//! LAN-only real-time multi-user editing over CRDTs.
//!
//! This first slice covers the twin's parameters. Each replica keeps an op
//! log; two replicas converge by exchanging logs (`pull`), in any order and
//! any number of times. Parameters are last-writer-wins registers from
//! `tpt-vertex-collab`, ordered by a hybrid clock, so concurrent edits to the
//! same parameter resolve the same way on every replica.
//!
//! [`hub`] runs sessions through `tpt-vertex-collab`'s `SyncHub` in-process,
//! with roles (owner, editor, viewer) and offline edits. [`TwinReplica`] is the
//! peer-to-peer log exchange used before the hub. There is still no network
//! transport. Geometry is shared as one text register (the model spec as JSON),
//! so a concurrent edit to the geometry replaces the whole spec, the same way
//! a parameter edit replaces one value. Sensors are not shared yet.

/// Name of the text register that holds the shared geometry spec.
pub const GEOMETRY_KEY: &str = "geometry.spec";

pub mod hub;
pub mod sync;
pub mod tls;
pub mod transport;

pub use hub::{Client, SessionError, TwinHub, deliver};
pub use transport::{Wire, serve};

use std::collections::BTreeMap;

use tpt_vertex_collab::clock::ReplicaId;
pub use tpt_vertex_collab::crdt::{FeatureKey, Op};
use tpt_vertex_collab::crdt::{LocalReplica, ParamValue};
use twin_core::{Twin, TwinError};

/// One collaborator's copy of a twin's parameters.
#[derive(Debug)]
pub struct TwinReplica {
    replica: LocalReplica,
    feature: FeatureKey,
    /// Every op this replica has applied, local or remote. Exchanged by `pull`.
    log: Vec<Op>,
}

impl TwinReplica {
    /// Start sharing `twin` from replica `id`. The twin's parameters become
    /// the first ops in the log. Other replicas get them by `pull`.
    pub fn host(id: u64, twin: &Twin) -> Self {
        let mut replica = LocalReplica::new(ReplicaId(id));
        let (feature, add) = replica.add_feature("twin", "0");
        let mut this = Self {
            replica,
            feature,
            log: vec![add],
        };
        for (name, value) in &twin.parameters {
            this.set_parameter(name, *value);
        }
        this
    }

    /// Start a replica that has none of `source`'s ops yet. Call `pull` to
    /// fill it in.
    pub fn join(id: u64, source: &TwinReplica) -> Self {
        let mut this = Self {
            replica: LocalReplica::new(ReplicaId(id)),
            feature: source.feature,
            log: Vec::new(),
        };
        this.pull(source);
        this
    }

    /// Set a parameter on this replica. The op is kept for other replicas.
    pub fn set_parameter(&mut self, name: &str, value: f64) {
        let op = self
            .replica
            .set_param(self.feature, name, ParamValue::Number(value));
        self.log.push(op);
    }

    /// Set a text value on this replica (last writer wins, like a parameter).
    /// The op is kept for other replicas.
    pub fn set_text(&mut self, name: &str, text: &str) {
        let op = self
            .replica
            .set_param(self.feature, name, ParamValue::Text(text.to_string()));
        self.log.push(op);
    }

    /// The text value called `name`, if one is set and is text.
    pub fn text(&self, name: &str) -> Option<String> {
        match &self
            .replica
            .doc
            .feature(self.feature)?
            .params
            .get(name)?
            .value
        {
            ParamValue::Text(text) => Some(text.clone()),
            _ => None,
        }
    }

    /// Share the geometry: the JSON of the design's model spec. Concurrent
    /// edits resolve as any other register does: the newer write wins whole.
    pub fn set_geometry(&mut self, spec_json: &str) {
        self.set_text(GEOMETRY_KEY, spec_json);
    }

    /// The shared geometry spec (JSON), if a replica has set one.
    pub fn geometry(&self) -> Option<String> {
        self.text(GEOMETRY_KEY)
    }

    /// Apply every op from `other` that this replica does not have yet. Pull
    /// in both directions to converge. Repeating a pull changes nothing.
    pub fn pull(&mut self, other: &TwinReplica) {
        for op in &other.log {
            if !self.log.contains(op) {
                self.replica.receive(op);
                self.log.push(op.clone());
            }
        }
    }

    /// The current numeric parameters, by name. Non-numeric values are skipped.
    pub fn parameters(&self) -> BTreeMap<String, f64> {
        let Some(state) = self.replica.doc.feature(self.feature) else {
            return BTreeMap::new();
        };
        state
            .params
            .iter()
            .filter_map(|(name, reg)| match reg.value {
                ParamValue::Number(v) => Some((name.clone(), v)),
                _ => None,
            })
            .collect()
    }

    /// Write the converged parameters into a copy of `base`. The result is
    /// validated, so a parameter that is not a finite number is refused.
    pub fn apply_to(&self, base: &Twin) -> Result<Twin, TwinError> {
        let mut twin = base.clone();
        for (name, value) in self.parameters() {
            twin.set_parameter(name, value)?;
        }
        Ok(twin)
    }

    pub fn log_len(&self) -> usize {
        self.log.len()
    }

    /// Every op this replica has applied, in order.
    pub fn ops(&self) -> &[Op] {
        &self.log
    }

    /// The feature the parameters belong to. Replicas that share it see the
    /// same parameters.
    pub fn feature(&self) -> FeatureKey {
        self.feature
    }

    /// The op made by the latest `set_parameter`, to send to other replicas.
    pub fn last_op(&self) -> Option<&Op> {
        self.log.last()
    }

    /// Start replica `id` from a snapshot of another replica: its feature and
    /// its complete op log.
    pub fn from_snapshot(id: u64, feature: FeatureKey, ops: &[Op]) -> Self {
        let mut this = Self {
            replica: LocalReplica::new(ReplicaId(id)),
            feature,
            log: Vec::new(),
        };
        this.receive_ops(ops);
        this
    }

    /// Apply the ops this replica does not have yet, and return them. Ops it
    /// already has are ignored, so the same ops can arrive more than once.
    pub fn receive_ops(&mut self, ops: &[Op]) -> Vec<Op> {
        let mut fresh = Vec::new();
        for op in ops {
            if !self.log.contains(op) {
                self.replica.receive(op);
                self.log.push(op.clone());
                fresh.push(op.clone());
            }
        }
        fresh
    }
}

/// Exchange logs both ways, so both replicas hold the same ops.
pub fn sync(a: &mut TwinReplica, b: &mut TwinReplica) {
    a.pull(b);
    b.pull(a);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn twin() -> Twin {
        let mut t = Twin::new("arm").unwrap();
        t.set_parameter("mass_kg", 1.0).unwrap();
        t.set_parameter("stiffness_n_per_m", 4.0).unwrap();
        t
    }

    fn pair() -> (TwinReplica, TwinReplica) {
        let a = TwinReplica::host(1, &twin());
        let b = TwinReplica::join(2, &a);
        (a, b)
    }

    #[test]
    fn a_joined_replica_sees_the_hosted_parameters() {
        let (a, b) = pair();
        assert_eq!(b.parameters(), a.parameters());
        assert_eq!(a.parameters()["mass_kg"], 1.0);
    }

    #[test]
    fn edits_to_different_parameters_both_survive() {
        let (mut a, mut b) = pair();
        a.set_parameter("mass_kg", 2.0);
        b.set_parameter("damping_n_s_per_m", 0.5);
        sync(&mut a, &mut b);
        assert_eq!(a.parameters(), b.parameters());
        assert_eq!(a.parameters()["mass_kg"], 2.0);
        assert_eq!(a.parameters()["damping_n_s_per_m"], 0.5);
    }

    #[test]
    fn repeated_pulls_change_nothing() {
        let (mut a, mut b) = pair();
        a.set_parameter("mass_kg", 2.0);
        sync(&mut a, &mut b);
        let (params, len) = (b.parameters(), b.log_len());
        b.pull(&a);
        sync(&mut a, &mut b);
        assert_eq!(b.parameters(), params);
        assert_eq!(b.log_len(), len);
    }

    #[test]
    fn partitioned_edits_converge_after_heal() {
        let (mut a, mut b) = pair();
        // Partition: neither side sees the other while both edit.
        a.set_parameter("mass_kg", 2.0);
        a.set_parameter("mass_kg", 3.0);
        a.set_parameter("stiffness_n_per_m", 9.0);
        b.set_parameter("mass_kg", 5.0);
        b.set_parameter("damping_n_s_per_m", 0.3);
        // Heal.
        sync(&mut a, &mut b);
        assert_eq!(a.parameters(), b.parameters());
        // a's second write to mass is newer than b's single write, so a wins.
        assert_eq!(a.parameters()["mass_kg"], 3.0);
        assert_eq!(a.parameters()["stiffness_n_per_m"], 9.0);
        assert_eq!(a.parameters()["damping_n_s_per_m"], 0.3);
    }

    #[test]
    fn equal_clocks_are_broken_by_replica_id() {
        let (mut a, mut b) = pair();
        // Both make exactly one write to mass from the same starting clock.
        a.set_parameter("mass_kg", 2.0);
        b.set_parameter("mass_kg", 5.0);
        sync(&mut a, &mut b);
        assert_eq!(a.parameters(), b.parameters());
        // Replica 2 (b) has the higher id, so its value wins the tie.
        assert_eq!(a.parameters()["mass_kg"], 5.0);
    }

    #[test]
    fn three_replicas_converge_in_any_pull_order() {
        let mut a = TwinReplica::host(1, &twin());
        let mut b = TwinReplica::join(2, &a);
        let mut c = TwinReplica::join(3, &a);
        a.set_parameter("mass_kg", 2.0);
        b.set_parameter("damping_n_s_per_m", 0.1);
        c.set_parameter("stiffness_n_per_m", 7.0);
        // Chain the pulls in an order that is not the write order.
        c.pull(&b);
        b.pull(&a);
        a.pull(&c);
        b.pull(&a);
        c.pull(&b);
        assert_eq!(a.parameters(), b.parameters());
        assert_eq!(b.parameters(), c.parameters());
        assert_eq!(a.parameters()["mass_kg"], 2.0);
        assert_eq!(a.parameters()["damping_n_s_per_m"], 0.1);
        assert_eq!(a.parameters()["stiffness_n_per_m"], 7.0);
    }

    #[test]
    fn geometry_reaches_a_joined_replica_and_is_not_a_parameter() {
        let (mut a, mut b) = pair();
        assert_eq!(b.geometry(), None);
        a.set_geometry(r#"{"steps":[]}"#);
        b.pull(&a);
        assert_eq!(b.geometry().as_deref(), Some(r#"{"steps":[]}"#));
        assert!(!b.parameters().contains_key(GEOMETRY_KEY));
    }

    #[test]
    fn concurrent_geometry_edits_resolve_to_one_whole_spec() {
        let (mut a, mut b) = pair();
        a.set_geometry("spec-a");
        b.set_geometry("spec-b");
        sync(&mut a, &mut b);
        assert_eq!(a.geometry(), b.geometry());
        // Equal clocks are broken by replica id: replica 2 (b) wins.
        assert_eq!(a.geometry().as_deref(), Some("spec-b"));
    }

    #[test]
    fn geometry_edits_leave_parameters_alone() {
        let (mut a, mut b) = pair();
        a.set_parameter("mass_kg", 2.0);
        b.set_geometry("spec");
        sync(&mut a, &mut b);
        assert_eq!(a.parameters(), b.parameters());
        assert_eq!(a.parameters()["mass_kg"], 2.0);
        assert_eq!(a.geometry().as_deref(), Some("spec"));
    }

    #[test]
    fn converged_parameters_write_back_into_a_validated_twin() {
        let (mut a, mut b) = pair();
        b.set_parameter("mass_kg", 3.0);
        sync(&mut a, &mut b);
        let base = twin();
        let merged = a.apply_to(&base).unwrap();
        assert_eq!(merged.parameters["mass_kg"], 3.0);
        assert_eq!(merged.id, base.id, "the twin's identity is kept");
    }
}
