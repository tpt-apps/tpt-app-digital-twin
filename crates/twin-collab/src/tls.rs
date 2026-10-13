//! Encrypted transport for LAN use, with trust on first use.
//!
//! The server presents a self-signed certificate. [`ServerIdentity`] creates
//! it once per host, and its fingerprint (the SHA-256 of the certificate) is
//! what clients pin. A client that has never seen the server accepts whatever
//! certificate it is given and reports the fingerprint, so the caller can pin
//! it. A client that has a pin refuses any other certificate.
//!
//! Trust on first use means the very first connection to a server is not
//! authenticated: someone on the network could present their own certificate
//! then. Compare the fingerprint out of band if that matters. Certificate
//! chains and names are not checked, because the pin replaces them.
//!
//! Each connection runs in one thread. The socket has a short read timeout, so
//! the thread can take turns between reading from the client and writing what
//! the hub queued for it, without sharing the TLS stream between threads.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, ServerConfig, ServerConnection,
    SignatureScheme, StreamOwned,
};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use tpt_vertex_collab::protocol::{ClientMessage, ServerMessage};
use tpt_vertex_collab::server::{ConnId, Outbound};

use crate::hub::TwinHub;

/// Name the server certificate is issued for. Clients do not check it, because
/// the pin replaces name checking; it is there so the certificate is well formed.
const SERVER_NAME: &str = "twin.local";
/// How long a server connection waits for client data before it looks for
/// queued messages to write.
pub(crate) const POLL: Duration = Duration::from_millis(20);

/// The fingerprint of a certificate: SHA-256 of its DER encoding, as hex.
pub fn fingerprint(cert_der: &[u8]) -> String {
    Sha256::digest(cert_der)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A server's certificate and private key. Create one per host with
/// [`ServerIdentity::generate`], keep it private, and reuse it so clients keep
/// recognising the server.
pub struct ServerIdentity {
    cert: Vec<u8>,
    key: Vec<u8>,
}

impl ServerIdentity {
    /// A new self-signed certificate and key.
    pub fn generate() -> io::Result<Self> {
        let key = rcgen::KeyPair::generate().map_err(io::Error::other)?;
        let cert = rcgen::CertificateParams::new(vec![SERVER_NAME.to_string()])
            .map_err(io::Error::other)?
            .self_signed(&key)
            .map_err(io::Error::other)?;
        Ok(Self {
            cert: cert.der().to_vec(),
            key: key.serialize_der(),
        })
    }

    /// Rebuild an identity from the bytes [`cert_der`](Self::cert_der) and
    /// [`key_der`](Self::key_der) returned, for example when the host restarts.
    pub fn from_der(cert: Vec<u8>, key: Vec<u8>) -> Self {
        Self { cert, key }
    }

    pub fn cert_der(&self) -> &[u8] {
        &self.cert
    }

    /// The private key, in PKCS#8 DER. Keep it secret.
    pub fn key_der(&self) -> &[u8] {
        &self.key
    }

    /// The fingerprint clients pin.
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.cert)
    }
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

/// Accepts the server certificate if it matches the pin, or if there is no pin.
/// Records the fingerprint it saw either way.
#[derive(Debug)]
struct PinVerifier {
    provider: Arc<CryptoProvider>,
    pinned: Option<String>,
    observed: Arc<Mutex<Option<String>>>,
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let seen = fingerprint(end_entity.as_ref());
        *self.observed.lock().unwrap() = Some(seen.clone());
        match &self.pinned {
            Some(pin) if *pin != seen => Err(rustls::Error::General(
                "the server certificate does not match the pinned fingerprint".into(),
            )),
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

pub(crate) fn server_config(identity: &ServerIdentity) -> io::Result<ServerConfig> {
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key.clone()));
    ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .with_no_client_auth()
        .with_single_cert(vec![CertificateDer::from(identity.cert.clone())], key)
        .map_err(io::Error::other)
}

/// Serve `hub` over TLS on `addr`. Returns the bound address.
///
/// Refuses a non-loopback address unless `allow_lan` is true.
pub fn serve_tls(
    hub: TwinHub,
    addr: &str,
    allow_lan: bool,
    identity: &ServerIdentity,
) -> io::Result<SocketAddr> {
    let listener = TcpListener::bind(addr)?;
    let bound = listener.local_addr()?;
    if !bound.ip().is_loopback() && !allow_lan {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{bound} is not a loopback address; pass allow_lan to bind it"),
        ));
    }
    let config = Arc::new(server_config(identity)?);
    let hub = Arc::new(Mutex::new(hub));
    let writers: Writers = Arc::new(Mutex::new(HashMap::new()));
    thread::spawn(move || {
        for (conn, tcp) in (1..).zip(listener.incoming().flatten()) {
            let (config, hub, writers) = (config.clone(), hub.clone(), writers.clone());
            thread::spawn(move || handle_connection(conn, tcp, config, hub, writers));
        }
    });
    Ok(bound)
}

/// Per connection, the queue of lines the hub wants written to it.
type Writers = Arc<Mutex<HashMap<ConnId, Sender<String>>>>;

fn handle_connection(
    conn: ConnId,
    tcp: TcpStream,
    config: Arc<ServerConfig>,
    hub: Arc<Mutex<TwinHub>>,
    writers: Writers,
) {
    let Ok(session) = ServerConnection::new(config) else {
        return;
    };
    if tcp.set_read_timeout(Some(POLL)).is_err() {
        return;
    }
    let mut stream = StreamOwned::new(session, tcp);
    let (tx, rx) = mpsc::channel::<String>();
    writers.lock().unwrap().insert(conn, tx);

    let mut pending: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    'serve: loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                pending.extend_from_slice(&buf[..n]);
                while let Some(end) = pending.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = pending.drain(..=end).collect();
                    let text = String::from_utf8_lossy(&line[..line.len() - 1]);
                    let Ok(msg) = serde_json::from_str::<ClientMessage>(&text) else {
                        // A line that is not a message ends the connection.
                        break 'serve;
                    };
                    let mut hub = hub.lock().unwrap();
                    let outs = hub.handle(conn, msg);
                    deliver(&writers, outs);
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
        while let Ok(line) = rx.try_recv() {
            if stream.write_all(line.as_bytes()).is_err() || stream.flush().is_err() {
                break 'serve;
            }
        }
    }

    // Connection closed: tell the room, and forget the queue.
    let mut hub = hub.lock().unwrap();
    let outs = hub.disconnect(conn);
    deliver(&writers, outs);
    writers.lock().unwrap().remove(&conn);
}

/// Queue each outbound message on its connection. Must be called while holding
/// the hub lock, so the order of lines matches the order the hub produced.
fn deliver(writers: &Writers, outs: Vec<Outbound>) {
    let writers = writers.lock().unwrap();
    for out in outs {
        let Some(tx) = writers.get(&out.to) else {
            continue;
        };
        let mut line = serde_json::to_string(&out.message).expect("messages are serialisable");
        line.push('\n');
        // A closed queue means the connection is gone; its disconnect cleans up.
        let _ = tx.send(line);
    }
}

/// A client's encrypted connection to a server.
pub struct SecureWire {
    stream: StreamOwned<ClientConnection, TcpStream>,
    pending: Vec<u8>,
}

impl SecureWire {
    /// Connect to `addr`. If `pinned` is given, the server's certificate must
    /// have that fingerprint. Returns the connection and the fingerprint the
    /// server presented, so a first-time client can pin it.
    pub fn connect(addr: SocketAddr, pinned: Option<&str>) -> io::Result<(Self, String)> {
        let mut tcp = TcpStream::connect(addr)?;
        let observed = Arc::new(Mutex::new(None));
        let verifier = PinVerifier {
            provider: provider(),
            pinned: pinned.map(str::to_string),
            observed: observed.clone(),
        };
        let config = ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(io::Error::other)?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth();
        let name = ServerName::try_from(SERVER_NAME).map_err(io::Error::other)?;
        let mut session =
            ClientConnection::new(Arc::new(config), name).map_err(io::Error::other)?;
        // Finish the handshake now, so a refused certificate is reported here.
        while session.is_handshaking() {
            session.complete_io(&mut tcp)?;
        }
        let fingerprint = observed
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| io::Error::other("no server certificate was seen"))?;
        let wire = Self {
            stream: StreamOwned::new(session, tcp),
            pending: Vec::new(),
        };
        Ok((wire, fingerprint))
    }

    pub fn send(&mut self, msg: &ClientMessage) -> io::Result<()> {
        self.send_json(msg)
    }

    /// Read the next message from the server. Returns `None` when it closes.
    pub fn recv(&mut self) -> io::Result<Option<ServerMessage>> {
        self.recv_json()
    }

    /// Send one message as a JSON line. Any serialisable type works, so the
    /// sync protocol uses the same wire.
    pub fn send_json<T: Serialize>(&mut self, msg: &T) -> io::Result<()> {
        let mut line = serde_json::to_string(msg)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        self.stream.write_all(line.as_bytes())?;
        self.stream.flush()
    }

    /// Read the next JSON line as a `T`. Returns `None` when the server closes.
    pub fn recv_json<T: DeserializeOwned>(&mut self) -> io::Result<Option<T>> {
        let mut buf = [0u8; 4096];
        loop {
            if let Some(end) = self.pending.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.pending.drain(..=end).collect();
                let text = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
                return serde_json::from_str(&text)
                    .map(Some)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
            }
            let n = self.stream.read(&mut buf)?;
            if n == 0 {
                return if self.pending.is_empty() {
                    Ok(None)
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "connection closed in the middle of a message",
                    ))
                };
            }
            self.pending.extend_from_slice(&buf[..n]);
        }
    }

    /// Limit how long `recv` waits. `None` waits forever.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.stream.sock.set_read_timeout(timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_vertex_collab::clock::ReplicaId;
    use tpt_vertex_collab::protocol::AccessLevel;
    use tpt_vertex_collab::server::MemoryAuth;

    fn auth() -> MemoryAuth {
        let mut a = MemoryAuth::new();
        a.add_token("tok-owner", "alice");
        a.grant("arm", "alice", AccessLevel::Owner);
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

    #[test]
    fn a_first_client_learns_the_fingerprint_and_a_pinned_one_joins() {
        let identity = ServerIdentity::generate().unwrap();
        let addr = serve_tls(TwinHub::new(auth()), "127.0.0.1:0", false, &identity).unwrap();

        // First contact: no pin, so the client sees the fingerprint and can pin it.
        let (mut owner, seen) = SecureWire::connect(addr, None).unwrap();
        assert_eq!(seen, identity.fingerprint());
        owner.send(&join(1, "tok-owner")).unwrap();
        owner
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        match owner.recv().unwrap() {
            Some(ServerMessage::Welcome { level, .. }) => assert_eq!(level, AccessLevel::Owner),
            other => panic!("expected Welcome, got {other:?}"),
        }

        // Later contact with the pin: accepted.
        let (_, again) = SecureWire::connect(addr, Some(&identity.fingerprint())).unwrap();
        assert_eq!(again, identity.fingerprint());
    }

    #[test]
    fn a_different_certificate_is_refused_once_pinned() {
        let identity = ServerIdentity::generate().unwrap();
        let addr = serve_tls(TwinHub::new(auth()), "127.0.0.1:0", false, &identity).unwrap();
        let impostor = ServerIdentity::generate().unwrap();
        let err = SecureWire::connect(addr, Some(&impostor.fingerprint()))
            .err()
            .expect("the pin must refuse the server");
        assert!(err.to_string().contains("pinned fingerprint"), "{err}");
    }

    #[test]
    fn an_identity_survives_a_round_trip_through_its_bytes() {
        let identity = ServerIdentity::generate().unwrap();
        let restored =
            ServerIdentity::from_der(identity.cert_der().to_vec(), identity.key_der().to_vec());
        assert_eq!(restored.fingerprint(), identity.fingerprint());
        // The restored key still serves: a client pinned to it connects.
        let addr = serve_tls(TwinHub::new(auth()), "127.0.0.1:0", false, &restored).unwrap();
        assert!(SecureWire::connect(addr, Some(&identity.fingerprint())).is_ok());
    }

    #[test]
    fn lan_addresses_need_the_opt_in() {
        let identity = ServerIdentity::generate().unwrap();
        let err = serve_tls(TwinHub::new(auth()), "0.0.0.0:0", false, &identity).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }
}
