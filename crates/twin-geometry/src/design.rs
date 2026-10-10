//! A design: a [`ModelSpec`] plus the parameter [`History`] that drives it.
//!
//! Every commit made through a `Design` records the hash of the geometry built
//! from its parameters. Because commits are content-addressed, the geometry
//! hash is part of the commit ID, so two machines that agree on an ID agree on
//! the geometry as well.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use twin_core::Twin;

use crate::model::{Model, ModelError, ModelSpec};
use crate::{CommitId, Content, History, HistoryError};

#[derive(Debug, Error, PartialEq)]
pub enum DesignError {
    #[error(transparent)]
    History(#[from] HistoryError),
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error("stored design is invalid: {0}")]
    Corrupt(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Design {
    spec: ModelSpec,
    history: History,
}

#[derive(Serialize, Deserialize)]
struct Snapshot {
    spec: ModelSpec,
    history: String,
}

/// Outcome of re-building a commit's geometry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verification {
    /// The rebuilt geometry matches the recorded hash.
    Match,
    /// The commit has no recorded geometry (made without a model).
    NoRecord,
    /// The rebuilt geometry differs from the recorded hash.
    Mismatch { recorded: String, rebuilt: String },
}

impl Design {
    /// Start a design. The initial parameters must build with `spec`.
    pub fn new(spec: ModelSpec, initial: Content, message: &str) -> Result<Self, DesignError> {
        let hash = Model::build(&spec, &initial)?.geometry_hash();
        let history = History::new_with(initial, message, Some(hash));
        Ok(Self { spec, history })
    }

    pub fn spec(&self) -> &ModelSpec {
        &self.spec
    }

    pub fn history(&self) -> &History {
        &self.history
    }

    /// Edit a working parameter. Use [`Design::preview`] to see the result.
    pub fn set(&mut self, key: impl Into<String>, value: f64) {
        self.history.set(key, value);
    }

    /// Build the geometry for the current working parameters.
    pub fn preview(&self) -> Result<Model, ModelError> {
        Model::build(&self.spec, self.history.working())
    }

    /// Commit the working parameters, recording their geometry hash. Fails
    /// without committing if the parameters do not build.
    pub fn commit(&mut self, message: &str) -> Result<CommitId, DesignError> {
        let hash = self.preview()?.geometry_hash();
        Ok(self.history.commit_changes_with(message, Some(hash))?)
    }

    pub fn create_branch(&mut self, name: &str) -> Result<(), DesignError> {
        Ok(self.history.create_branch(name)?)
    }

    pub fn switch(&mut self, name: &str) -> Result<(), DesignError> {
        Ok(self.history.switch(name)?)
    }

    /// Merge `from` into the current branch. The merged parameters must build;
    /// if they do not, nothing is merged.
    pub fn merge(&mut self, from: &str, message: &str) -> Result<CommitId, DesignError> {
        let spec = &self.spec;
        self.history.merge_with(from, message, |merged| {
            Ok(Some(Model::build(spec, merged)?.geometry_hash()))
        })
    }

    /// Rebuild the geometry of a commit and compare it with the recorded hash.
    pub fn verify(&self, id: &CommitId) -> Result<Verification, DesignError> {
        let commit = self.history.commit(id)?;
        let Some(recorded) = commit.geometry.clone() else {
            return Ok(Verification::NoRecord);
        };
        let rebuilt = Model::build(&self.spec, &commit.content)?.geometry_hash();
        Ok(if rebuilt == recorded {
            Verification::Match
        } else {
            Verification::Mismatch { recorded, rebuilt }
        })
    }

    /// Point a twin at this design: its parameters become the working
    /// parameters, and `geometry_ref` becomes the head commit ID. Parameters
    /// the design does not own are left alone.
    pub fn link_twin(&self, twin: &mut Twin) -> Result<(), DesignError> {
        for (name, value) in self.history.working() {
            twin.set_parameter(name.clone(), *value)
                .map_err(|e| DesignError::Corrupt(e.to_string()))?;
        }
        twin.geometry_ref = Some(self.history.head_commit().as_str().to_string());
        Ok(())
    }

    /// True if `twin` points at this design's current head commit.
    pub fn is_linked(&self, twin: &Twin) -> bool {
        twin.geometry_ref.as_deref() == Some(self.history.head_commit().as_str())
    }

    pub fn to_json(&self) -> Result<String, DesignError> {
        serde_json::to_string(&Snapshot {
            spec: self.spec.clone(),
            history: self.history.to_json()?,
        })
        .map_err(|e| DesignError::Corrupt(e.to_string()))
    }

    pub fn from_json(json: &str) -> Result<Self, DesignError> {
        let snap: Snapshot =
            serde_json::from_str(json).map_err(|e| DesignError::Corrupt(e.to_string()))?;
        Ok(Self {
            spec: snap.spec,
            history: History::from_json(&snap.history)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Step, Value};

    fn params(pairs: &[(&str, f64)]) -> Content {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    fn spec() -> ModelSpec {
        ModelSpec {
            steps: vec![Step::Block {
                width: Value::Param("w".into()),
                depth: Value::Param("d".into()),
                height: Value::Const(1.0),
            }],
        }
    }

    fn design() -> Design {
        Design::new(spec(), params(&[("w", 2.0), ("d", 3.0)]), "start").unwrap()
    }

    #[test]
    fn commits_record_geometry_and_verify() {
        let mut d = design();
        d.set("w", 4.0);
        let id = d.commit("wider").unwrap();
        let recorded = d.history().commit(&id).unwrap().geometry.clone().unwrap();
        assert_eq!(recorded, d.preview().unwrap().geometry_hash());
        assert_eq!(d.verify(&id).unwrap(), Verification::Match);
        let root = d.history().log().last().unwrap().id.clone();
        assert_eq!(d.verify(&root).unwrap(), Verification::Match);
    }

    #[test]
    fn same_inputs_give_the_same_commit_id_and_other_geometry_does_not() {
        let mut a = design();
        let mut b = design();
        a.set("w", 4.0);
        b.set("w", 4.0);
        assert_eq!(a.commit("x").unwrap(), b.commit("x").unwrap());

        // Same parameters and message, but the model is taller.
        let taller = ModelSpec {
            steps: vec![Step::Block {
                width: Value::Param("w".into()),
                depth: Value::Param("d".into()),
                height: Value::Const(9.0),
            }],
        };
        let mut c = Design::new(taller, params(&[("w", 2.0), ("d", 3.0)]), "start").unwrap();
        c.set("w", 4.0);
        assert_ne!(a.history().log()[0].id, c.commit("x").unwrap());
    }

    #[test]
    fn unbuildable_parameters_are_not_committed() {
        let mut d = design();
        d.set("w", -1.0);
        assert!(matches!(d.commit("bad"), Err(DesignError::Model(_))));
        assert_eq!(d.history().log().len(), 1);
    }

    #[test]
    fn merge_records_geometry_of_the_merged_parameters() {
        let mut d = design();
        d.create_branch("deep").unwrap();
        d.set("w", 4.0);
        d.commit("wider").unwrap();
        d.switch("deep").unwrap();
        d.set("d", 6.0);
        d.commit("deeper").unwrap();
        d.switch("main").unwrap();
        let id = d.merge("deep", "both").unwrap();
        assert_eq!(d.history().working(), &params(&[("w", 4.0), ("d", 6.0)]));
        assert_eq!(d.verify(&id).unwrap(), Verification::Match);
        let m = Model::build(d.spec(), d.history().working()).unwrap();
        assert!((m.volume() - 24.0).abs() < 1e-9);
    }

    #[test]
    fn json_round_trip_and_tamper_detection() {
        let mut d = design();
        d.set("w", 4.0);
        d.commit("wider").unwrap();
        let back = Design::from_json(&d.to_json().unwrap()).unwrap();
        assert_eq!(back, d);
        let tampered = d.to_json().unwrap().replace("wider", "narrower");
        assert!(Design::from_json(&tampered).is_err());
    }

    #[test]
    fn verify_reports_a_mismatch_when_geometry_changes() {
        let mut d = design();
        d.set("w", 4.0);
        let id = d.commit("wider").unwrap();
        // Same history, different spec: the rebuilt geometry no longer matches.
        let other = Design {
            spec: ModelSpec {
                steps: vec![Step::Block {
                    width: Value::Param("w".into()),
                    depth: Value::Param("d".into()),
                    height: Value::Const(9.0),
                }],
            },
            history: d.history().clone(),
        };
        assert!(matches!(
            other.verify(&id).unwrap(),
            Verification::Mismatch { .. }
        ));
    }

    #[test]
    fn link_copies_parameters_and_tracks_the_head() {
        let mut d = design();
        let mut twin = Twin::new("linked").unwrap();
        twin.set_parameter("mass_kg", 2.5).unwrap();
        d.link_twin(&mut twin).unwrap();
        assert_eq!(twin.parameters["w"], 2.0);
        assert_eq!(twin.parameters["mass_kg"], 2.5, "unowned parameters stay");
        assert!(d.is_linked(&twin));

        d.set("w", 4.0);
        d.commit("wider").unwrap();
        assert!(!d.is_linked(&twin), "stale after a new commit");
        d.link_twin(&mut twin).unwrap();
        assert!(d.is_linked(&twin));
        assert_eq!(twin.parameters["w"], 4.0);
    }
}
