//! Parameter sync over TLS: replicas exchange their operation logs.
//!
//! A host shares one [`TwinReplica`] and accepts peers over TLS (see
//! [`crate::tls`] for the pinning rules). The protocol is:
//!
//! 1. The peer sends `Hello` with the session code the host shows.
//! 2. The host checks the code. A wrong code closes the connection.
//! 3. The host sends `Snapshot`: the feature and its whole op log.
//! 4. After that, either side sends `Ops` with the ops it made. Each side
//!    applies the ops it has not seen, and the host relays them to the other
//!    peers.
//!
//! The session code is a shared secret typed by the people who join. It is not
//! a substitute for pinning: pinning is what protects the connection, and the
//! code keeps strangers on the same network out. Codes are compared as plain
//! strings, and nothing limits how often one is tried, so use a long code.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use rustls::{ServerConfig, ServerConnection, StreamOwned};
use serde::{Deserialize, Serialize};

use crate::tls::{self, SecureWire, ServerIdentity};
use crate::{FeatureKey, Op, TwinReplica};

/// What a peer may do in a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Access {
    /// May change parameters, which the host relays to the others.
    Editor,
    /// May watch. Its changes are ignored by the host.
    Viewer,
}

/// Messages between sync peers, one JSON line each.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SyncMessage {
    /// First message from a peer: the session code.
    Hello {
        code: String,
        /// The name the peer shows the others. Optional.
        #[serde(default)]
        name: String,
    },
    /// Host to peer, once the code is accepted: everything the host has, and
    /// what this peer may do.
    Snapshot {
        feature: FeatureKey,
        ops: Vec<Op>,
        access: Access,
    },
    /// Ops one side made, or relayed from another peer.
    Ops { ops: Vec<Op> },
    /// Host to peer: the session code was wrong. The host closes after this.
    Refused,
}

/// The codes a host accepts: one lets a peer edit, the other only watch.
struct Codes {
    editor: String,
    viewer: String,
}

/// A peer's sender for relaying lines to it, by connection number.
type Peers = Arc<Mutex<Vec<Peer>>>;

/// A connected, authorised peer: its connection number, name and send queue.
struct Peer {
    conn: u64,
    name: String,
    tx: Sender<String>,
}

/// A running host: the shared replica, and the peers connected to it.
pub struct SyncHost {
    addr: SocketAddr,
    replica: Arc<Mutex<TwinReplica>>,
    peers: Peers,
    /// Set when the host stops: the listener closes and peer threads end.
    stop: Arc<AtomicBool>,
}

impl Drop for SyncHost {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl SyncHost {
    /// Start sharing `replica` on `addr`. Refuses a non-loopback address unless
    /// `allow_lan` is true, as [`crate::tls::serve_tls`] does.
    pub fn start(
        replica: TwinReplica,
        identity: &ServerIdentity,
        addr: &str,
        allow_lan: bool,
        editor_code: &str,
        viewer_code: &str,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        let bound = listener.local_addr()?;
        if !bound.ip().is_loopback() && !allow_lan {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{bound} is not a loopback address; pass allow_lan to bind it"),
            ));
        }
        let config = Arc::new(tls::server_config(identity)?);
        let replica = Arc::new(Mutex::new(replica));
        let peers: Peers = Arc::new(Mutex::new(Vec::new()));
        let codes = Arc::new(Codes {
            editor: editor_code.to_string(),
            viewer: viewer_code.to_string(),
        });
        // Accept without blocking, so the loop can notice a stop request.
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        {
            let (replica, peers, codes, stop) =
                (replica.clone(), peers.clone(), codes.clone(), stop.clone());
            thread::spawn(move || {
                let mut conn = 0;
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((tcp, _)) => {
                            conn += 1;
                            let (config, replica, peers, codes, stop) = (
                                config.clone(),
                                replica.clone(),
                                peers.clone(),
                                codes.clone(),
                                stop.clone(),
                            );
                            let serve =
                                move || serve_peer(conn, tcp, config, replica, peers, codes, stop);
                            thread::spawn(serve);
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(20));
                        }
                        Err(_) => break,
                    }
                }
            });
        }
        Ok(Self {
            addr: bound,
            replica,
            peers,
            stop,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Set a parameter on the host and send the op to every peer.
    pub fn set_parameter(&self, name: &str, value: f64) {
        let op = {
            let mut replica = self.replica.lock().unwrap();
            replica.set_parameter(name, value);
            replica.last_op().cloned()
        };
        if let Some(op) = op {
            broadcast(&self.peers, SyncMessage::Ops { ops: vec![op] }, None);
        }
    }

    /// The host's current parameters.
    pub fn parameters(&self) -> BTreeMap<String, f64> {
        self.replica.lock().unwrap().parameters()
    }

    /// Share a new geometry spec (JSON) from the host and send it to every peer.
    pub fn set_geometry(&self, spec_json: &str) {
        let op = {
            let mut replica = self.replica.lock().unwrap();
            replica.set_geometry(spec_json);
            replica.last_op().cloned()
        };
        if let Some(op) = op {
            broadcast(&self.peers, SyncMessage::Ops { ops: vec![op] }, None);
        }
    }

    /// The shared geometry spec (JSON) as the host sees it, if any.
    pub fn geometry(&self) -> Option<String> {
        self.replica.lock().unwrap().geometry()
    }

    /// How many peers are connected and authorised.
    pub fn peer_count(&self) -> usize {
        self.peers.lock().unwrap().len()
    }

    /// The names of the connected peers, in the order they joined.
    pub fn peer_names(&self) -> Vec<String> {
        self.peers
            .lock()
            .unwrap()
            .iter()
            .map(|p| p.name.clone())
            .collect()
    }
}

fn line(msg: &SyncMessage) -> String {
    let mut text = serde_json::to_string(msg).expect("sync messages are serialisable");
    text.push('\n');
    text
}

/// Queue `msg` for every peer except `except`.
fn broadcast(peers: &Peers, msg: SyncMessage, except: Option<u64>) {
    let text = line(&msg);
    for peer in peers.lock().unwrap().iter() {
        if Some(peer.conn) != except {
            // A closed queue means that peer has gone; it is removed when its
            // thread ends.
            let _ = peer.tx.send(text.clone());
        }
    }
}

fn serve_peer(
    conn: u64,
    tcp: TcpStream,
    config: Arc<ServerConfig>,
    replica: Arc<Mutex<TwinReplica>>,
    peers: Peers,
    codes: Arc<Codes>,
    stop: Arc<AtomicBool>,
) {
    let Ok(session) = ServerConnection::new(config) else {
        return;
    };
    if tcp.set_read_timeout(Some(tls::POLL)).is_err() {
        return;
    }
    let mut stream = StreamOwned::new(session, tcp);
    let mut pending: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];

    // The first line must be a Hello with the right code.
    let mut access: Option<Access> = None;
    let (tx, rx) = mpsc::channel::<String>();
    'serve: loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                pending.extend_from_slice(&buf[..n]);
                while let Some(end) = pending.iter().position(|&b| b == b'\n') {
                    let text: Vec<u8> = pending.drain(..=end).collect();
                    let text = String::from_utf8_lossy(&text[..text.len() - 1]).into_owned();
                    let Ok(msg) = serde_json::from_str::<SyncMessage>(&text) else {
                        break 'serve;
                    };
                    match msg {
                        SyncMessage::Hello { code: given, name } if access.is_none() => {
                            let granted = if given == codes.editor {
                                Access::Editor
                            } else if given == codes.viewer {
                                Access::Viewer
                            } else {
                                let _ = stream.write_all(line(&SyncMessage::Refused).as_bytes());
                                let _ = stream.flush();
                                break 'serve;
                            };
                            access = Some(granted);
                            // Snapshot and registration happen under the replica lock,
                            // so an op applied meanwhile is either in the snapshot or
                            // relayed to this peer. The lock order is always replica then
                            // peers, and a duplicate op is ignored by `receive_ops`.
                            let snapshot = {
                                let replica = replica.lock().unwrap();
                                let snapshot = SyncMessage::Snapshot {
                                    feature: replica.feature(),
                                    ops: replica.ops().to_vec(),
                                    access: granted,
                                };
                                peers.lock().unwrap().push(Peer {
                                    conn,
                                    name,
                                    tx: tx.clone(),
                                });
                                snapshot
                            };
                            let _ = tx.send(line(&snapshot));
                        }
                        // Viewers' changes are ignored, not relayed.
                        SyncMessage::Ops { .. } if access == Some(Access::Viewer) => {}
                        SyncMessage::Ops { ops } if access == Some(Access::Editor) => {
                            let fresh = replica.lock().unwrap().receive_ops(&ops);
                            if !fresh.is_empty() {
                                broadcast(&peers, SyncMessage::Ops { ops: fresh }, Some(conn));
                            }
                        }
                        _ => break 'serve,
                    }
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
        while let Ok(text) = rx.try_recv() {
            if stream.write_all(text.as_bytes()).is_err() || stream.flush().is_err() {
                break 'serve;
            }
        }
    }
    peers.lock().unwrap().retain(|p| p.conn != conn);
}

/// A peer's connection to a host, holding its own replica of the twin.
pub struct SyncClient {
    wire: SecureWire,
    replica: TwinReplica,
    access: Access,
}

impl SyncClient {
    /// Connect to a host, pinned to `pinned` if given, and join with `code`.
    /// Returns the client and the fingerprint the host presented. Fails if the
    /// code is refused.
    pub fn connect(
        addr: SocketAddr,
        pinned: Option<&str>,
        code: &str,
        replica_id: u64,
    ) -> io::Result<(Self, String)> {
        Self::connect_as(addr, pinned, code, "peer", replica_id)
    }

    /// Like [`connect`](Self::connect), showing the host the name `name`.
    pub fn connect_as(
        addr: SocketAddr,
        pinned: Option<&str>,
        code: &str,
        name: &str,
        replica_id: u64,
    ) -> io::Result<(Self, String)> {
        let (mut wire, fingerprint) = SecureWire::connect(addr, pinned)?;
        wire.send_json(&SyncMessage::Hello {
            code: code.to_string(),
            name: name.to_string(),
        })?;
        wire.set_read_timeout(Some(Duration::from_secs(10)))?;
        let (replica, access) = match wire.recv_json()? {
            Some(SyncMessage::Snapshot {
                feature,
                ops,
                access,
            }) => (
                TwinReplica::from_snapshot(replica_id, feature, &ops),
                access,
            ),
            Some(SyncMessage::Refused) => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "the host refused the session code",
                ));
            }
            Some(other) => {
                return Err(io::Error::other(format!(
                    "expected a snapshot, got {other:?}"
                )));
            }
            None => {
                return Err(io::Error::other(
                    "the host closed before the session started",
                ));
            }
        };
        Ok((
            Self {
                wire,
                replica,
                access,
            },
            fingerprint,
        ))
    }

    /// Set a parameter and send the op to the host.
    pub fn set_parameter(&mut self, name: &str, value: f64) -> io::Result<()> {
        self.edit(|replica| replica.set_parameter(name, value))
    }

    /// Share a new geometry spec (JSON) and send the op to the host.
    pub fn set_geometry(&mut self, spec_json: &str) -> io::Result<()> {
        self.edit(|replica| replica.set_geometry(spec_json))
    }

    /// The shared geometry spec (JSON) as this peer sees it, if any.
    pub fn geometry(&self) -> Option<String> {
        self.replica.geometry()
    }

    /// Make a local edit, then send its op to the host. Refused for view-only
    /// sessions, before anything changes.
    fn edit(&mut self, change: impl FnOnce(&mut TwinReplica)) -> io::Result<()> {
        if self.access == Access::Viewer {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "this session is view-only",
            ));
        }
        change(&mut self.replica);
        let op = self
            .replica
            .last_op()
            .cloned()
            .expect("an op was just made");
        self.wire.send_json(&SyncMessage::Ops { ops: vec![op] })
    }

    /// Apply whatever the host has sent, without waiting for more than
    /// `timeout`. Returns how many ops were new.
    pub fn poll(&mut self, timeout: Duration) -> io::Result<usize> {
        self.wire.set_read_timeout(Some(timeout))?;
        let mut applied = 0;
        loop {
            match self.wire.recv_json() {
                Ok(Some(SyncMessage::Ops { ops })) => {
                    applied += self.replica.receive_ops(&ops).len()
                }
                Ok(Some(other)) => {
                    return Err(io::Error::other(format!("unexpected message {other:?}")));
                }
                Ok(None) => return Err(io::Error::other("the host closed the session")),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(applied);
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub fn parameters(&self) -> BTreeMap<String, f64> {
        self.replica.parameters()
    }

    /// What this peer may do in the session.
    pub fn access(&self) -> Access {
        self.access
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tls::ServerIdentity;
    use twin_core::Twin;

    fn twin() -> Twin {
        let mut t = Twin::new("arm").unwrap();
        t.set_parameter("mass_kg", 1.0).unwrap();
        t
    }

    fn wait_until(mut check: impl FnMut() -> bool) {
        for _ in 0..500 {
            if check() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("condition not reached in time");
    }

    #[test]
    fn peers_converge_on_each_others_edits_through_the_host() {
        let identity = ServerIdentity::generate().unwrap();
        let host = SyncHost::start(
            TwinReplica::host(1, &twin()),
            &identity,
            "127.0.0.1:0",
            false,
            "open-sesame",
            "watch",
        )
        .unwrap();

        let (mut a, _) =
            SyncClient::connect(host.addr(), Some(&identity.fingerprint()), "open-sesame", 2)
                .unwrap();
        let (mut b, _) =
            SyncClient::connect(host.addr(), Some(&identity.fingerprint()), "open-sesame", 3)
                .unwrap();
        assert_eq!(a.parameters()["mass_kg"], 1.0);

        // A's edit reaches the host and then B.
        a.set_parameter("mass_kg", 2.5).unwrap();
        wait_until(|| {
            b.poll(Duration::from_millis(20)).unwrap();
            b.parameters().get("mass_kg") == Some(&2.5)
        });
        assert_eq!(host.parameters()["mass_kg"], 2.5);

        // The host's edit reaches both peers.
        host.set_parameter("stiffness_n_per_m", 9.0);
        wait_until(|| {
            a.poll(Duration::from_millis(20)).unwrap();
            a.parameters().get("stiffness_n_per_m") == Some(&9.0)
        });
        wait_until(|| {
            b.poll(Duration::from_millis(20)).unwrap();
            b.parameters().get("stiffness_n_per_m") == Some(&9.0)
        });
    }

    #[test]
    fn a_wrong_session_code_is_refused() {
        let identity = ServerIdentity::generate().unwrap();
        let host = SyncHost::start(
            TwinReplica::host(1, &twin()),
            &identity,
            "127.0.0.1:0",
            false,
            "right",
            "watch",
        )
        .unwrap();
        let err = SyncClient::connect(host.addr(), Some(&identity.fingerprint()), "wrong", 2)
            .err()
            .expect("the host must refuse the code");
        assert!(err.to_string().contains("refused"), "{err}");
    }

    #[test]
    fn a_viewer_watches_but_its_changes_are_not_relayed() {
        let identity = ServerIdentity::generate().unwrap();
        let host = SyncHost::start(
            TwinReplica::host(1, &twin()),
            &identity,
            "127.0.0.1:0",
            false,
            "edit-me",
            "look-only",
        )
        .unwrap();
        let pin = identity.fingerprint();
        let (mut editor, _) =
            SyncClient::connect_as(host.addr(), Some(&pin), "edit-me", "alice", 2).unwrap();
        let (mut viewer, _) = SyncClient::connect(host.addr(), Some(&pin), "look-only", 3).unwrap();
        assert_eq!(editor.access(), Access::Editor);
        assert_eq!(viewer.access(), Access::Viewer);
        wait_until(|| host.peer_count() == 2);
        let mut names = host.peer_names();
        names.sort();
        assert_eq!(names, ["alice", "peer"]);

        // A viewer cannot edit at all.
        let err = viewer.set_parameter("mass_kg", 9.0).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);

        // An editor's change reaches the viewer.
        editor.set_parameter("mass_kg", 3.0).unwrap();
        wait_until(|| {
            viewer.poll(Duration::from_millis(20)).unwrap();
            viewer.parameters().get("mass_kg") == Some(&3.0)
        });
        assert_eq!(host.parameters()["mass_kg"], 3.0);

        // A viewer that sends ops anyway, over a raw connection, is ignored.
        let (mut raw, _) = SecureWire::connect(host.addr(), Some(&pin)).unwrap();
        raw.send_json(&SyncMessage::Hello {
            code: "look-only".into(),
            name: "raw".into(),
        })
        .unwrap();
        let Some(SyncMessage::Snapshot { feature, ops, .. }) = raw.recv_json().unwrap() else {
            panic!("expected a snapshot");
        };
        let mut forger = TwinReplica::from_snapshot(9, feature, &ops);
        forger.set_parameter("mass_kg", 99.0);
        let forged = forger.last_op().cloned().unwrap();
        raw.send_json(&SyncMessage::Ops { ops: vec![forged] })
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(host.parameters()["mass_kg"], 3.0);
    }

    #[test]
    fn geometry_is_shared_both_ways_and_viewers_cannot_change_it() {
        let identity = ServerIdentity::generate().unwrap();
        let host = SyncHost::start(
            TwinReplica::host(1, &twin()),
            &identity,
            "127.0.0.1:0",
            false,
            "edit-me",
            "look-only",
        )
        .unwrap();
        let pin = identity.fingerprint();
        let (mut editor, _) =
            SyncClient::connect_as(host.addr(), Some(&pin), "edit-me", "bob", 2).unwrap();
        let (mut viewer, _) = SyncClient::connect(host.addr(), Some(&pin), "look-only", 3).unwrap();

        // The editor's spec reaches the host and the viewer.
        editor.set_geometry("spec-from-editor").unwrap();
        wait_until(|| {
            viewer.poll(Duration::from_millis(20)).unwrap();
            viewer.geometry().as_deref() == Some("spec-from-editor")
        });
        assert_eq!(host.geometry().as_deref(), Some("spec-from-editor"));

        // The host's spec reaches the editor.
        host.set_geometry("spec-from-host");
        wait_until(|| {
            editor.poll(Duration::from_millis(20)).unwrap();
            editor.geometry().as_deref() == Some("spec-from-host")
        });

        // A viewer cannot share geometry, and nothing changes.
        let err = viewer.set_geometry("forged").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(host.geometry().as_deref(), Some("spec-from-host"));
    }

    #[test]
    fn dropping_the_host_closes_its_port() {
        let identity = ServerIdentity::generate().unwrap();
        let host = SyncHost::start(
            TwinReplica::host(1, &twin()),
            &identity,
            "127.0.0.1:0",
            false,
            "x",
            "y",
        )
        .unwrap();
        let addr = host.addr();
        drop(host);
        // The accept loop notices the stop within one poll, then the port is free.
        wait_until(|| std::net::TcpListener::bind(addr).is_ok());
    }

    #[test]
    fn a_lan_address_needs_the_opt_in() {
        let identity = ServerIdentity::generate().unwrap();
        let err = SyncHost::start(
            TwinReplica::host(1, &twin()),
            &identity,
            "0.0.0.0:0",
            false,
            "x",
            "y",
        )
        .err()
        .expect("must refuse");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }
}
