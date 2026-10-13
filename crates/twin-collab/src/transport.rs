//! A line-based TCP transport for the hub.
//!
//! Each message is one line of JSON, in the serde encoding of
//! `ClientMessage` (client to server) or `ServerMessage` (server to client).
//! The server runs the [`TwinHub`] behind a mutex. Replies and broadcasts are
//! written while the mutex is held, so every connection sees messages in the
//! order the hub produced them.
//!
//! Security, per the architecture notes:
//! - This plain transport binds loopback only. It is not encrypted, so it
//!   never takes a LAN address. LAN use goes through `tls::serve_tls`.
//! - Joins are checked by the hub's token table.
//!
//! The server threads run until the process exits. There is no shutdown.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use tpt_vertex_collab::protocol::{ClientMessage, ServerMessage};
use tpt_vertex_collab::server::{ConnId, Outbound};

use crate::hub::TwinHub;

type Writers = Arc<Mutex<HashMap<ConnId, TcpStream>>>;

/// Start serving `hub` on `addr`. Returns the bound address (useful with port 0).
///
/// Refuses any non-loopback address. `allow_lan` has no effect here: LAN use
/// needs the encrypted transport in `tls`.
pub fn serve(hub: TwinHub, addr: &str, _allow_lan: bool) -> io::Result<SocketAddr> {
    let listener = TcpListener::bind(addr)?;
    let bound = listener.local_addr()?;
    // This transport is not encrypted, so it never leaves the machine. LAN use
    // goes through `tls::serve_tls`.
    if !bound.ip().is_loopback() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{bound} is not a loopback address; plain transport is loopback only, use tls::serve_tls for LAN"
            ),
        ));
    }
    let hub = Arc::new(Mutex::new(hub));
    let writers: Writers = Arc::new(Mutex::new(HashMap::new()));
    thread::spawn(move || {
        for (conn, stream) in (1..).zip(listener.incoming().flatten()) {
            let Ok(write_half) = stream.try_clone() else {
                continue;
            };
            writers.lock().unwrap().insert(conn, write_half);
            let (hub, writers) = (hub.clone(), writers.clone());
            thread::spawn(move || handle_connection(conn, stream, hub, writers));
        }
    });
    Ok(bound)
}

fn handle_connection(conn: ConnId, stream: TcpStream, hub: Arc<Mutex<TwinHub>>, writers: Writers) {
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<ClientMessage>(&line) else {
            // A line that is not a message ends the connection.
            break;
        };
        let mut hub = hub.lock().unwrap();
        let outs = hub.handle(conn, msg);
        deliver(&writers, outs);
    }
    // Connection closed: tell the room, and forget the writer.
    let mut hub = hub.lock().unwrap();
    let outs = hub.disconnect(conn);
    deliver(&writers, outs);
    writers.lock().unwrap().remove(&conn);
}

/// Write each outbound message as one JSON line. Must be called while holding
/// the hub lock, so the order of lines matches the order the hub produced.
fn deliver(writers: &Writers, outs: Vec<Outbound>) {
    let mut writers = writers.lock().unwrap();
    for out in outs {
        let Some(stream) = writers.get_mut(&out.to) else {
            continue;
        };
        let mut line = serde_json::to_string(&out.message).expect("messages are serialisable");
        line.push('\n');
        if stream.write_all(line.as_bytes()).is_err() {
            writers.remove(&out.to);
        }
    }
}

/// A client's connection to a server. Sends and receives whole messages.
pub struct Wire {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl Wire {
    pub fn connect(addr: SocketAddr) -> io::Result<Self> {
        let writer = TcpStream::connect(addr)?;
        let reader = BufReader::new(writer.try_clone()?);
        Ok(Self { reader, writer })
    }

    /// Limit how long `recv` waits. Shared by the reader and the writer.
    pub fn set_read_timeout(&self, timeout: Option<std::time::Duration>) -> io::Result<()> {
        self.writer.set_read_timeout(timeout)
    }

    pub fn send(&mut self, msg: &ClientMessage) -> io::Result<()> {
        let mut line = serde_json::to_string(msg)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        self.writer.write_all(line.as_bytes())
    }

    /// Read the next message from the server. Returns `None` when it closes.
    pub fn recv(&mut self) -> io::Result<Option<ServerMessage>> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        serde_json::from_str(line.trim_end())
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::{TwinHub, advance_clock};
    use std::time::Duration;
    use tpt_vertex_collab::clock::ReplicaId;
    use tpt_vertex_collab::crdt::{CrdtDoc, LocalReplica, ParamValue};
    use tpt_vertex_collab::protocol::AccessLevel;
    use tpt_vertex_collab::server::MemoryAuth;
    use twin_core::Twin;

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

    fn join(replica: u64, token: &str) -> ClientMessage {
        ClientMessage::Join {
            room: "arm".into(),
            token: token.into(),
            replica: ReplicaId(replica),
            display_name: format!("replica-{replica}"),
        }
    }

    fn recv(wire: &mut Wire) -> ServerMessage {
        wire.recv().unwrap().expect("server closed the connection")
    }

    fn welcome(wire: &mut Wire) -> (AccessLevel, CrdtDoc) {
        match recv(wire) {
            ServerMessage::Welcome {
                level, snapshot, ..
            } => (level, snapshot),
            other => panic!("expected Welcome, got {other:?}"),
        }
    }

    /// A local replica that starts from a snapshot, with its clock advanced.
    fn replica_from(id: u64, snapshot: CrdtDoc) -> LocalReplica {
        let mut r = LocalReplica::new(ReplicaId(id));
        advance_clock(&mut r, &snapshot);
        r.doc = snapshot;
        r
    }

    #[test]
    fn refuses_a_lan_address_without_opt_in() {
        let hub = TwinHub::new(auth());
        let err = serve(hub, "0.0.0.0:0", false).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn loopback_join_is_welcomed_and_bad_tokens_are_rejected() {
        let addr = serve(TwinHub::new(auth()), "127.0.0.1:0", false).unwrap();
        assert!(addr.ip().is_loopback());

        let mut bad = Wire::connect(addr).unwrap();
        bad.send(&join(9, "wrong")).unwrap();
        assert!(matches!(recv(&mut bad), ServerMessage::Rejected { .. }));

        let mut owner = Wire::connect(addr).unwrap();
        owner.send(&join(1, "tok-owner")).unwrap();
        let (level, _) = welcome(&mut owner);
        assert_eq!(level, AccessLevel::Owner);
    }

    #[test]
    fn an_edit_over_the_wire_reaches_another_connection() {
        let addr = serve(TwinHub::new(auth()), "127.0.0.1:0", false).unwrap();

        // The owner hosts the twin: an empty room, so it builds the feature
        // and parameters locally and sends them.
        let mut owner = Wire::connect(addr).unwrap();
        owner.send(&join(1, "tok-owner")).unwrap();
        let (_, snapshot) = welcome(&mut owner);
        let mut owner_replica = replica_from(1, snapshot);
        let twin = {
            let mut t = Twin::new("arm").unwrap();
            t.set_parameter("mass_kg", 1.0).unwrap();
            t
        };
        let (key, add) = owner_replica.add_feature("twin", "0");
        let mut ops = vec![add];
        for (name, value) in &twin.parameters {
            ops.push(owner_replica.set_param(key, name, ParamValue::Number(*value)));
        }
        owner.send(&ClientMessage::Ops { ops }).unwrap();
        // Wait until the hub has applied the ops: a resync is answered after
        // them, on the same connection.
        owner.send(&ClientMessage::Resync).unwrap();
        assert!(matches!(recv(&mut owner), ServerMessage::Snapshot { .. }));

        // A viewer joins and gets the hosted twin in its snapshot.
        let mut viewer = Wire::connect(addr).unwrap();
        viewer.send(&join(3, "tok-viewer")).unwrap();
        let (_, viewer_snapshot) = welcome(&mut viewer);
        let mut viewer_replica = replica_from(3, viewer_snapshot);
        let viewer_key = viewer_replica.doc.ordered_keys()[0];
        assert_eq!(
            viewer_replica.doc.param(viewer_key, "mass_kg"),
            Some(&ParamValue::Number(1.0))
        );

        // An editor joins, edits, and the viewer receives the change.
        let mut editor = Wire::connect(addr).unwrap();
        editor.send(&join(2, "tok-editor")).unwrap();
        let (_, editor_snapshot) = welcome(&mut editor);
        let mut editor_replica = replica_from(2, editor_snapshot);
        let op = editor_replica.set_param(viewer_key, "mass_kg", ParamValue::Number(2.5));
        editor.send(&ClientMessage::Ops { ops: vec![op] }).unwrap();

        viewer
            .writer
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        match recv(&mut viewer) {
            ServerMessage::Ops { ops, .. } => {
                for op in &ops {
                    viewer_replica.receive(op);
                }
            }
            other => panic!("expected Ops, got {other:?}"),
        }
        assert_eq!(
            viewer_replica.doc.param(viewer_key, "mass_kg"),
            Some(&ParamValue::Number(2.5))
        );
    }

    #[test]
    fn a_dropped_connection_is_announced_to_the_room() {
        let addr = serve(TwinHub::new(auth()), "127.0.0.1:0", false).unwrap();
        let mut owner = Wire::connect(addr).unwrap();
        owner.send(&join(1, "tok-owner")).unwrap();
        welcome(&mut owner);
        let mut viewer = Wire::connect(addr).unwrap();
        viewer.send(&join(3, "tok-viewer")).unwrap();
        welcome(&mut viewer);

        drop(viewer);
        owner
            .writer
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        match recv(&mut owner) {
            ServerMessage::Left { replica } => assert_eq!(replica, ReplicaId(3)),
            other => panic!("expected Left, got {other:?}"),
        }
    }
}
