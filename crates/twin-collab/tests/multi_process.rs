//! Several peers over a hub that runs in its own process.
//!
//! The server is the `twin_collab_serve` binary, started as a child process.
//! Each peer here is a `Wire` connection with its own `LocalReplica`, so the
//! only thing shared between peers is the socket. The partition is a real
//! socket closing and reopening.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use tpt_vertex_collab::clock::ReplicaId;
use tpt_vertex_collab::crdt::{FeatureKey, LocalReplica, Op, ParamValue};
use tpt_vertex_collab::protocol::{ClientMessage, ServerMessage};
use twin_collab::hub::advance_clock;
use twin_collab::transport::Wire;

const ROOM: &str = "twin";
const OWNER: &str = "tok-owner";
const EDITOR: &str = "tok-editor";
const VIEWER: &str = "tok-viewer";

/// A child server process. Killed when dropped.
struct Server {
    child: Child,
    addr: SocketAddr,
}

impl Server {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_twin_collab_serve"))
            .args(["127.0.0.1:0", OWNER, EDITOR, VIEWER])
            .stdout(Stdio::piped())
            .spawn()
            .expect("server process starts");
        let stdout = child.stdout.take().unwrap();
        let mut first = String::new();
        BufReader::new(stdout).read_line(&mut first).unwrap();
        let addr = first
            .trim()
            .strip_prefix("listening ")
            .unwrap_or_else(|| panic!("unexpected first line: {first:?}"))
            .parse()
            .unwrap();
        Self { child, addr }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn send_ops(wire: &mut Wire, ops: Vec<Op>) {
    wire.send(&ClientMessage::Ops { ops }).unwrap();
}

/// One participant: a socket (which may be down), a replica, and the ops it
/// made while it could not reach the server.
struct Peer {
    addr: SocketAddr,
    token: &'static str,
    id: u64,
    wire: Option<Wire>,
    replica: LocalReplica,
    feature: Option<FeatureKey>,
    unsent: Vec<Op>,
}

impl Peer {
    fn connect(addr: SocketAddr, token: &'static str, id: u64) -> Self {
        let mut peer = Self {
            addr,
            token,
            id,
            wire: None,
            replica: LocalReplica::new(ReplicaId(id)),
            feature: None,
            unsent: Vec::new(),
        };
        peer.join();
        peer
    }

    /// Open a socket, join, and take the snapshot. Ops made offline are then
    /// re-applied on top of the snapshot and sent.
    fn join(&mut self) {
        let mut wire = Wire::connect(self.addr).expect("connect to server");
        wire.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        wire.send(&ClientMessage::Join {
            room: ROOM.into(),
            token: self.token.into(),
            replica: ReplicaId(self.id),
            display_name: format!("peer-{}", self.id),
        })
        .unwrap();
        let snapshot = match wire.recv().unwrap() {
            Some(ServerMessage::Welcome { snapshot, .. }) => snapshot,
            other => panic!("expected Welcome, got {other:?}"),
        };
        advance_clock(&mut self.replica, &snapshot);
        self.replica.doc = snapshot;
        if self.feature.is_none() {
            self.feature = self.replica.doc.ordered_keys().first().copied();
        }
        for op in std::mem::take(&mut self.unsent) {
            self.replica.receive(&op);
            send_ops(&mut wire, vec![op]);
        }
        self.wire = Some(wire);
    }

    /// Drop the socket. Edits made from now on are kept until `join`.
    fn disconnect(&mut self) {
        self.wire = None;
    }

    /// Set a parameter. Sent now if connected, otherwise kept for `join`.
    fn set(&mut self, name: &str, value: f64) {
        let key = self.feature.expect("twin is hosted");
        let op = self.replica.set_param(key, name, ParamValue::Number(value));
        match self.wire.as_mut() {
            Some(wire) => send_ops(wire, vec![op]),
            None => self.unsent.push(op),
        }
    }

    /// Read messages until `done` holds for this peer. Panics on timeout.
    fn pump_until(&mut self, done: impl Fn(&Peer) -> bool) {
        while !done(self) {
            let wire = self.wire.as_mut().expect("pump needs a connection");
            match wire.recv() {
                Ok(Some(ServerMessage::Ops { ops, .. })) => {
                    for op in &ops {
                        self.replica.receive(op);
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => panic!("server closed the connection"),
                Err(e) => panic!("timed out waiting for changes: {e}"),
            }
        }
    }

    /// Host a twin: an empty room, so this owner creates the feature and
    /// parameters and sends them. Waits until the server has them.
    fn host(&mut self, params: &[(&str, f64)]) {
        let (key, add) = self.replica.add_feature("twin", "0");
        self.feature = Some(key);
        let mut ops = vec![add];
        for (name, value) in params {
            ops.push(
                self.replica
                    .set_param(key, name, ParamValue::Number(*value)),
            );
        }
        let wire = self.wire.as_mut().unwrap();
        send_ops(wire, ops);
        // A resync is answered after the ops, on the same connection.
        wire.send(&ClientMessage::Resync).unwrap();
        match wire.recv().unwrap() {
            Some(ServerMessage::Snapshot { .. }) => {}
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    fn params(&self) -> BTreeMap<String, f64> {
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
}

#[test]
fn edits_reach_every_process_and_offline_edits_converge_after_heal() {
    let server = Server::start();
    let addr = server.addr;

    let mut owner = Peer::connect(addr, OWNER, 1);
    owner.host(&[("mass_kg", 1.0), ("stiffness_n_per_m", 4.0)]);
    let mut viewer = Peer::connect(addr, VIEWER, 3);
    let mut editor = Peer::connect(addr, EDITOR, 2);
    assert_eq!(viewer.params()["mass_kg"], 1.0);
    assert_eq!(editor.params(), owner.params());

    // Connected edit: the viewer sees it.
    editor.set("mass_kg", 2.0);
    viewer.pump_until(|p| p.params().get("mass_kg") == Some(&2.0));

    // Partition: the editor's socket closes. Both sides keep editing.
    editor.disconnect();
    editor.set("mass_kg", 7.0);
    editor.set("damping_n_s_per_m", 0.5);
    owner.set("mass_kg", 3.0);
    owner.set("stiffness_n_per_m", 9.0);
    // Let the owner's edits reach the viewer before the editor heals.
    viewer.pump_until(|p| p.params().get("stiffness_n_per_m") == Some(&9.0));

    // Heal: the editor rejoins, its kept ops are sent, and everyone catches up.
    editor.join();
    editor
        .wire
        .as_mut()
        .unwrap()
        .send(&ClientMessage::Resync)
        .unwrap();
    let wire = editor.wire.as_mut().unwrap();
    loop {
        match wire.recv().unwrap() {
            Some(ServerMessage::Snapshot { .. }) => break,
            Some(ServerMessage::Ops { ops, .. }) => {
                for op in &ops {
                    editor.replica.receive(op);
                }
            }
            _ => {}
        }
    }
    owner.pump_until(|p| p.params().get("damping_n_s_per_m") == Some(&0.5));
    viewer.pump_until(|p| p.params().get("damping_n_s_per_m") == Some(&0.5));

    let expected = editor.params();
    assert_eq!(expected["damping_n_s_per_m"], 0.5);
    assert_eq!(owner.params(), expected, "owner converges");
    assert_eq!(viewer.params(), expected, "viewer converges");
    assert_eq!(expected["stiffness_n_per_m"], 9.0);
}
