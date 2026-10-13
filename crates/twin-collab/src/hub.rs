//! In-process sessions over `tpt-vertex-collab`'s `SyncHub`.
//!
//! The hub speaks messages (`ClientMessage` / `ServerMessage`), so a session
//! can be tested without a socket. A transport only has to carry those
//! messages. Here the caller routes them: `handle` returns what to deliver, and
//! [`deliver`] hands it to the clients.
//!
//! Rules this layer adds on top of the hub:
//! - Only the owner hosts a twin. A joiner to an empty room is refused, so two
//!   people cannot race to create the twin's feature.
//! - A viewer's local edit is refused before it is sent.
//! - Edits made while disconnected are kept and sent after reconnecting.

use std::collections::BTreeMap;

use tpt_vertex_collab::clock::{HybridClock, ReplicaId};
use tpt_vertex_collab::crdt::{CrdtDoc, FeatureKey, LocalReplica, Op, ParamValue};
use tpt_vertex_collab::protocol::{AccessLevel, ClientMessage, ServerMessage};
use tpt_vertex_collab::server::{ConnId, MemoryAuth, Outbound, SyncHub};
use twin_core::Twin;

/// The hub used by a session. In-memory authentication for now.
pub type TwinHub = SyncHub<MemoryAuth>;

const TWIN_KIND: &str = "twin";

#[derive(Debug, Clone, PartialEq)]
pub enum SessionError {
    /// The hub refused the join (bad token, or no grant for the room).
    Rejected(String),
    /// The room has no twin yet, so only its owner can join.
    NotHosted,
    /// The client's access level does not allow the action.
    Forbidden,
    /// The hub sent something other than the expected reply.
    Protocol(String),
}

/// One participant's view of a twin.
#[derive(Debug)]
pub struct Client {
    conn: ConnId,
    room: String,
    replica: LocalReplica,
    level: AccessLevel,
    feature: Option<FeatureKey>,
    /// Ops made locally that the hub may not have yet.
    pending: Vec<Op>,
    /// Parameters whose offline edit lost on the last reconnect.
    lost: Vec<String>,
}

impl Client {
    /// Host a new twin in `room`. The caller must be an owner, and the room
    /// must be empty. The twin's parameters are sent to the hub.
    pub fn host(
        hub: &mut TwinHub,
        conn: ConnId,
        room: &str,
        token: &str,
        replica: u64,
        twin: &Twin,
    ) -> Result<(Self, Vec<Outbound>), SessionError> {
        let mut client = Self::join(hub, conn, room, token, replica)?;
        if client.level != AccessLevel::Owner {
            return Err(SessionError::Forbidden);
        }
        if client.feature.is_some() {
            return Err(SessionError::Protocol("room is already hosted".into()));
        }
        let (key, add) = client.replica.add_feature(TWIN_KIND, "0");
        client.feature = Some(key);
        client.pending.push(add);
        for (name, value) in &twin.parameters {
            let op = client
                .replica
                .set_param(key, name, ParamValue::Number(*value));
            client.pending.push(op);
        }
        let outs = client.flush(hub);
        Ok((client, outs))
    }

    /// Join a hosted twin. Fails if the room has no twin yet.
    pub fn connect(
        hub: &mut TwinHub,
        conn: ConnId,
        room: &str,
        token: &str,
        replica: u64,
    ) -> Result<Self, SessionError> {
        let client = Self::join(hub, conn, room, token, replica)?;
        if client.feature.is_none() {
            return Err(SessionError::NotHosted);
        }
        Ok(client)
    }

    /// Rejoin after a disconnect, with a new connection. Local edits made
    /// since the last join are kept and sent by the next `flush`.
    pub fn reconnect(
        self,
        hub: &mut TwinHub,
        conn: ConnId,
        token: &str,
    ) -> Result<Self, SessionError> {
        let pending = self.pending.clone();
        let mut client = Self::join(hub, conn, &self.room, token, self.replica.id.0)?;
        if client.feature.is_none() {
            return Err(SessionError::NotHosted);
        }
        // Re-apply the local edits on top of the fresh snapshot.
        for op in &pending {
            client.replica.receive(op);
        }
        // An edit loses when a newer write to the same parameter is already in
        // the room. Report those, so the caller can tell the user.
        for op in &pending {
            let Op::SetParam {
                key, name, value, ..
            } = op
            else {
                continue;
            };
            if client.replica.doc.param(*key, name) != Some(value) && !client.lost.contains(name) {
                client.lost.push(name.clone());
            }
        }
        client.pending = pending;
        Ok(client)
    }

    /// Parameters whose local edit, made while disconnected, lost to a newer
    /// write when this client reconnected. Their values are now the room's.
    pub fn lost_on_reconnect(&self) -> &[String] {
        &self.lost
    }

    fn join(
        hub: &mut TwinHub,
        conn: ConnId,
        room: &str,
        token: &str,
        replica: u64,
    ) -> Result<Self, SessionError> {
        let outs = hub.handle(
            conn,
            ClientMessage::Join {
                room: room.to_string(),
                token: token.to_string(),
                replica: ReplicaId(replica),
                display_name: format!("replica-{replica}"),
            },
        );
        let reply = outs
            .into_iter()
            .find(|o| o.to == conn)
            .ok_or_else(|| SessionError::Protocol("no reply to join".into()))?;
        match reply.message {
            ServerMessage::Welcome {
                level, snapshot, ..
            } => {
                let mut replica = LocalReplica::new(ReplicaId(replica));
                let feature = snapshot.ordered_keys().first().copied();
                advance_clock(&mut replica, &snapshot);
                replica.doc = snapshot;
                Ok(Self {
                    conn,
                    room: room.to_string(),
                    replica,
                    level,
                    feature,
                    pending: Vec::new(),
                    lost: Vec::new(),
                })
            }
            ServerMessage::Rejected { reason } => Err(SessionError::Rejected(reason)),
            other => Err(SessionError::Protocol(format!("unexpected {other:?}"))),
        }
    }

    /// Set a parameter. Refused for viewers. The op is sent now if the client
    /// can reach the hub, otherwise it waits for `flush` after reconnecting.
    pub fn set_parameter(
        &mut self,
        hub: &mut TwinHub,
        name: &str,
        value: f64,
    ) -> Result<Vec<Outbound>, SessionError> {
        if !self.level.can_edit() {
            return Err(SessionError::Forbidden);
        }
        let key = self.feature.ok_or(SessionError::NotHosted)?;
        let op = self.replica.set_param(key, name, ParamValue::Number(value));
        self.pending.push(op);
        Ok(self.flush(hub))
    }

    /// Send pending ops to the hub. Ops are kept until they are sent.
    pub fn flush(&mut self, hub: &mut TwinHub) -> Vec<Outbound> {
        if self.pending.is_empty() || !self.level.can_edit() {
            return Vec::new();
        }
        let ops = self.pending.clone();
        let outs = hub.handle(self.conn, ClientMessage::Ops { ops });
        // The hub replies with a rejection if it refused the ops; keep them then.
        if outs
            .iter()
            .any(|o| o.to == self.conn && matches!(o.message, ServerMessage::Rejected { .. }))
        {
            return outs;
        }
        self.pending.clear();
        outs
    }

    /// Apply a message the hub delivered to this client.
    pub fn receive(&mut self, message: &ServerMessage) {
        match message {
            ServerMessage::Ops { ops, .. } => {
                for op in ops {
                    self.replica.receive(op);
                }
                if self.feature.is_none() {
                    self.feature = self.replica.doc.ordered_keys().first().copied();
                }
            }
            ServerMessage::Snapshot { snapshot } => {
                self.replica.doc.merge(snapshot);
                if self.feature.is_none() {
                    self.feature = snapshot.ordered_keys().first().copied();
                }
            }
            _ => {}
        }
    }

    pub fn conn(&self) -> ConnId {
        self.conn
    }

    pub fn level(&self) -> AccessLevel {
        self.level
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// The current numeric parameters of the twin, by name.
    pub fn parameters(&self) -> BTreeMap<String, f64> {
        let Some(key) = self.feature else {
            return BTreeMap::new();
        };
        let Some(state) = self.replica.doc.feature(key) else {
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

    /// The room's document as this client sees it.
    pub fn doc(&self) -> &CrdtDoc {
        &self.replica.doc
    }
}

/// Deliver hub output to the clients it is addressed to. Messages for
/// connections that are not in `clients` are dropped.
pub fn deliver(outs: Vec<Outbound>, clients: &mut [&mut Client]) {
    for out in outs {
        if let Some(c) = clients.iter_mut().find(|c| c.conn == out.to) {
            c.receive(&out.message);
        }
    }
}

/// Move this replica's clock past every clock in `doc`, so a local write made
/// after a snapshot is newer than the writes it already holds. Without this,
/// a fresh replica's first edit could lose to an older value.
///
/// `LocalReplica` does not expose its clock, so this applies a no-op
/// `SetPosition` that repeats the current position at the highest clock. It
/// changes no value, only the clock.
pub fn advance_clock(replica: &mut LocalReplica, doc: &CrdtDoc) {
    let mut max: Option<HybridClock> = None;
    for key in doc.ordered_keys() {
        let Some(state) = doc.feature(key) else {
            continue;
        };
        let clocks = state
            .add_tags
            .iter()
            .copied()
            .chain(state.params.values().map(|r| r.clock))
            .chain(state.position.iter().map(|r| r.clock))
            .chain(state.kind.iter().map(|r| r.clock));
        for c in clocks {
            if max.is_none_or(|m| c > m) {
                max = Some(c);
            }
        }
    }
    let (Some(clock), Some(key)) = (max, doc.ordered_keys().first().copied()) else {
        return;
    };
    let position = match doc.feature(key).and_then(|f| f.position.as_ref()) {
        Some(reg) => match &reg.value {
            ParamValue::Text(s) => s.clone(),
            _ => String::new(),
        },
        None => String::new(),
    };
    replica.receive(&Op::SetPosition {
        key,
        position,
        clock,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth() -> MemoryAuth {
        let mut a = MemoryAuth::new();
        a.add_token("tok-owner", "alice");
        a.add_token("tok-editor", "bob");
        a.add_token("tok-viewer", "carol");
        a.grant("arm", "alice", AccessLevel::Owner);
        a.grant("arm", "bob", AccessLevel::Editor);
        a.grant("arm", "carol", AccessLevel::Viewer);
        a
    }

    fn twin() -> Twin {
        let mut t = Twin::new("arm").unwrap();
        t.set_parameter("mass_kg", 1.0).unwrap();
        t.set_parameter("stiffness_n_per_m", 4.0).unwrap();
        t
    }

    /// Owner hosts; editor and viewer join. Returns all three clients.
    fn room(hub: &mut TwinHub) -> (Client, Client, Client) {
        let (owner, outs) = Client::host(hub, 1, "arm", "tok-owner", 1, &twin()).unwrap();
        assert!(outs.iter().all(|o| o.to != 1), "host gets no echo");
        let editor = Client::connect(hub, 2, "arm", "tok-editor", 2).unwrap();
        let viewer = Client::connect(hub, 3, "arm", "tok-viewer", 3).unwrap();
        (owner, editor, viewer)
    }

    #[test]
    fn bad_token_is_refused() {
        let mut hub = SyncHub::new(auth());
        let err = Client::connect(&mut hub, 9, "arm", "wrong", 9).unwrap_err();
        assert!(matches!(err, SessionError::Rejected(_)));
    }

    #[test]
    fn joiners_to_an_empty_room_are_refused() {
        let mut hub = SyncHub::new(auth());
        let err = Client::connect(&mut hub, 2, "arm", "tok-editor", 2).unwrap_err();
        assert_eq!(err, SessionError::NotHosted);
    }

    #[test]
    fn only_the_owner_can_host() {
        let mut hub = SyncHub::new(auth());
        let err = Client::host(&mut hub, 2, "arm", "tok-editor", 2, &twin()).unwrap_err();
        assert_eq!(err, SessionError::Forbidden);
    }

    #[test]
    fn joiners_see_the_hosted_parameters() {
        let mut hub = SyncHub::new(auth());
        let (owner, editor, viewer) = room(&mut hub);
        assert_eq!(editor.parameters(), owner.parameters());
        assert_eq!(viewer.parameters()["mass_kg"], 1.0);
        assert_eq!(viewer.level(), AccessLevel::Viewer);
    }

    #[test]
    fn an_editor_edit_reaches_the_owner_and_viewer() {
        let mut hub = SyncHub::new(auth());
        let (mut owner, mut editor, mut viewer) = room(&mut hub);
        let outs = editor.set_parameter(&mut hub, "mass_kg", 2.5).unwrap();
        deliver(outs, &mut [&mut owner, &mut viewer]);
        assert_eq!(owner.parameters()["mass_kg"], 2.5);
        assert_eq!(viewer.parameters()["mass_kg"], 2.5);
        assert_eq!(editor.parameters()["mass_kg"], 2.5);
    }

    #[test]
    fn a_viewer_cannot_edit() {
        let mut hub = SyncHub::new(auth());
        let (mut owner, _editor, mut viewer) = room(&mut hub);
        let err = viewer.set_parameter(&mut hub, "mass_kg", 9.0).unwrap_err();
        assert_eq!(err, SessionError::Forbidden);
        assert_eq!(viewer.pending_len(), 0);
        deliver(Vec::new(), &mut [&mut owner]);
        assert_eq!(owner.parameters()["mass_kg"], 1.0);
    }

    #[test]
    fn concurrent_edits_from_two_editors_converge() {
        let mut hub = SyncHub::new(auth());
        let (mut owner, mut editor, _viewer) = room(&mut hub);
        // A second editor, on its own connection.
        let mut editor2 = Client::connect(&mut hub, 4, "arm", "tok-editor", 4).unwrap();
        let o1 = editor.set_parameter(&mut hub, "mass_kg", 2.0).unwrap();
        let o2 = editor2.set_parameter(&mut hub, "mass_kg", 3.0).unwrap();
        let o3 = editor2
            .set_parameter(&mut hub, "damping_n_s_per_m", 0.2)
            .unwrap();
        deliver(o1, &mut [&mut owner, &mut editor2]);
        deliver(o2, &mut [&mut owner, &mut editor]);
        deliver(o3, &mut [&mut owner, &mut editor]);
        assert_eq!(owner.parameters(), editor.parameters());
        assert_eq!(owner.parameters(), editor2.parameters());
        assert_eq!(owner.parameters()["damping_n_s_per_m"], 0.2);
        // editor2's second write is newer than editor's single write.
        assert_eq!(owner.parameters()["mass_kg"], 3.0);
    }

    #[test]
    fn offline_edits_are_sent_after_reconnecting() {
        let mut hub = SyncHub::new(auth());
        let (mut owner, mut editor, _viewer) = room(&mut hub);
        // Disconnect, then edit while offline. The hub is not told.
        let left = hub.disconnect(2);
        deliver(left, &mut [&mut owner]);
        let key = editor.feature.unwrap();
        let op = editor
            .replica
            .set_param(key, "height_m", ParamValue::Number(0.7));
        editor.pending.push(op);
        assert_eq!(owner.parameters().get("height_m"), None);
        // Reconnect on a new connection: the kept edit is sent.
        let mut back = editor.reconnect(&mut hub, 5, "tok-editor").unwrap();
        assert_eq!(back.pending_len(), 1);
        let outs = back.flush(&mut hub);
        deliver(outs, &mut [&mut owner]);
        assert_eq!(owner.parameters()["height_m"], 0.7);
        assert_eq!(back.pending_len(), 0);
    }

    #[test]
    fn an_offline_edit_that_loses_is_reported_on_reconnect() {
        let mut hub = SyncHub::new(auth());
        let (mut owner, mut editor, _viewer) = room(&mut hub);
        let left = hub.disconnect(2);
        deliver(left, &mut [&mut owner]);
        // The editor edits offline; the owner then writes the same parameter
        // twice, so its clock is ahead of the editor's.
        let key = editor.feature.unwrap();
        let op = editor
            .replica
            .set_param(key, "mass_kg", ParamValue::Number(7.0));
        editor.pending.push(op);
        owner.set_parameter(&mut hub, "mass_kg", 8.0).unwrap();
        owner.set_parameter(&mut hub, "mass_kg", 9.0).unwrap();

        let back = editor.reconnect(&mut hub, 5, "tok-editor").unwrap();
        assert_eq!(back.lost_on_reconnect(), ["mass_kg".to_string()]);
        assert_eq!(back.parameters()["mass_kg"], 9.0);
    }

    #[test]
    fn an_offline_edit_that_wins_is_not_reported() {
        let mut hub = SyncHub::new(auth());
        let (mut owner, mut editor, _viewer) = room(&mut hub);
        let left = hub.disconnect(2);
        deliver(left, &mut [&mut owner]);
        let key = editor.feature.unwrap();
        let op = editor
            .replica
            .set_param(key, "mass_kg", ParamValue::Number(7.0));
        editor.pending.push(op);

        let back = editor.reconnect(&mut hub, 5, "tok-editor").unwrap();
        assert!(back.lost_on_reconnect().is_empty());
        assert_eq!(back.parameters()["mass_kg"], 7.0);
    }

    #[test]
    fn a_rejected_flush_keeps_the_ops() {
        let mut hub = SyncHub::new(auth());
        let (_owner, editor, _viewer) = room(&mut hub);
        let mut editor = editor;
        // Kick the editor out, then ask it to flush. The hub has no member for
        // this connection, so the ops are refused and stay pending.
        hub.disconnect(2);
        editor.pending.push(editor.replica.set_param(
            editor.feature.unwrap(),
            "x",
            ParamValue::Number(1.0),
        ));
        let outs = editor.flush(&mut hub);
        assert!(
            outs.iter()
                .any(|o| matches!(o.message, ServerMessage::Rejected { .. }))
        );
        assert_eq!(editor.pending_len(), 1);
    }
}
