//! The server the phone connects to: TLS, pairing (both sides approve), the token for later connections, the snapshots
//! that come in and the commands that go out. It reports what happens as `Event`s and takes decisions from the UI; it
//! knows nothing about the notch or Windows media.
//!
//! One thread accepts, one thread per connection. A connection thread does everything for its phone, writing as well as
//! reading, by waiting for data a short while at a time (`Timing::poll`), so a commands is sent within that while.

use crate::latency::Latency;
use crate::pairing::{Device, Devices, Gate};
use crate::wire::{self, Action, FrameBuf, FromPhone, Snapshot};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// At most this many connections at once; the rest are turned away.
const MAX_CONNECTIONS: usize = 6;

/// What happened, for the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A phone asks to pair; these six digits must be the same on its screen.
    PairRequest { code: String },
    /// That pairing is over.
    PairEnded { paired: bool },
    /// A paired phone is connected and signed in.
    Connected { id: String, name: String },
    /// Why it ended: `closed by the phone`, `bad frame`, `idle`, `write failed`, `seq went back`, `unexpected message` or
    /// `replaced or forgotten`.
    Disconnected { id: String, why: &'static str },
    /// The phone asked to be forgotten and it was: it is gone from the list and its token is dead.
    Forgotten { id: String, name: String },
    Snapshot(Snapshot),
    /// How long a message from the phone takes to get here now (one way), as far as the pings can tell.
    Latency(Duration),
    /// The phone's answer to a command.
    Ack { id: String, accepted: bool, reason: String },
}

#[derive(Clone, Copy)]
pub struct Timing {
    /// A new connection must say what it wants within this long (the TLS handshake included).
    pub hello: Duration,
    /// Each side has this long to decide about a pairing.
    pub decision: Duration,
    pub ping_every: Duration,
    /// A phone that sends nothing valid for this long is dropped.
    pub idle: Duration,
    /// How long a read waits before the connection looks at what is to be sent.
    pub poll: Duration,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing { hello: Duration::from_secs(5), decision: Duration::from_secs(120), ping_every: Duration::from_secs(10), idle: Duration::from_secs(35), poll: Duration::from_millis(50) }
    }
}

struct Active {
    id: String,
    tx: Sender<Vec<u8>>,
    generation: u64,
}

struct Inner {
    cfg: Arc<ServerConfig>,
    fingerprint: [u8; 32],
    devices: Mutex<Devices>,
    devices_file: Option<PathBuf>,
    gate: Mutex<Gate>,
    /// Where the user's decision about the pairing in progress goes.
    decision: Mutex<Option<Sender<bool>>>,
    active: Mutex<Option<Active>>,
    events: Sender<Event>,
    timing: Timing,
    generation: AtomicU64,
    connections: AtomicU64,
}

pub struct Server {
    inner: Arc<Inner>,
    port: u16,
}

fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    // a system that cannot give random bytes cannot pair safely: stop, do not carry on with zeros
    getrandom::getrandom(&mut b).expect("the system's random numbers");
    b
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A random version 4 UUID in lower case (8-4-4-4-12): the phone refuses any other form of a command's id.
fn uuid_v4() -> String {
    let mut b = random::<16>();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex(&b);
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Server {
    /// Listen on `addr` (port 0 = any) and start accepting. `devices_file` keeps the paired phones between runs.
    pub fn start(cfg: Arc<ServerConfig>, fingerprint: [u8; 32], devices_file: Option<PathBuf>, events: Sender<Event>, addr: SocketAddr, timing: Timing) -> std::io::Result<Server> {
        let listener = TcpListener::bind(addr)?;
        let port = listener.local_addr()?.port();
        let devices = devices_file.as_ref().and_then(|f| std::fs::read_to_string(f).ok()).map(|t| Devices::parse(&t)).unwrap_or_default();
        let inner = Arc::new(Inner {
            cfg,
            fingerprint,
            devices: Mutex::new(devices),
            devices_file,
            gate: Mutex::new(Gate::default()),
            decision: Mutex::new(None),
            active: Mutex::new(None),
            events,
            timing,
            generation: AtomicU64::new(1),
            connections: AtomicU64::new(0),
        });
        let accept = inner.clone();
        std::thread::spawn(move || {
            for tcp in listener.incoming().flatten() {
                if accept.connections.load(Ordering::SeqCst) >= MAX_CONNECTIONS as u64 {
                    continue; // dropped: the phone sees the connection close
                }
                accept.connections.fetch_add(1, Ordering::SeqCst);
                let conn = accept.clone();
                std::thread::spawn(move || {
                    conn.handle(tcp);
                    conn.connections.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        Ok(Server { inner, port })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// "Pair a phone": phones may start pairing for the next two minutes.
    pub fn open_pairing(&self) {
        if let Ok(mut g) = self.inner.gate.lock() {
            g.open(Instant::now());
        }
    }

    pub fn close_pairing(&self) {
        if let Ok(mut g) = self.inner.gate.lock() {
            g.close();
        }
    }

    pub fn pairing_left(&self) -> Option<Duration> {
        self.inner.gate.lock().ok().and_then(|g| g.mode_left(Instant::now()))
    }

    /// The user's answer about the pairing in progress (the code on the screen is the same as on the phone, or not).
    pub fn decide(&self, approved: bool) {
        if let Some(tx) = self.inner.decision.lock().ok().and_then(|mut d| d.take()) {
            let _ = tx.send(approved);
        }
    }

    pub fn devices(&self) -> Vec<Device> {
        self.inner.devices.lock().map(|d| d.list().to_vec()).unwrap_or_default()
    }

    /// Who is connected now.
    pub fn connected(&self) -> Option<String> {
        self.inner.active.lock().ok().and_then(|a| a.as_ref().map(|a| a.id.clone()))
    }

    /// A new name for a phone. The name it was given (cleaned), or None if there is no such phone or no name.
    pub fn rename(&self, id: &str, name: &str) -> Option<String> {
        let done = self.inner.devices.lock().ok().and_then(|mut d| d.rename(id, name));
        if done.is_some() {
            self.inner.save_devices();
        }
        done
    }

    /// "Forget this phone": its token stops working and it is disconnected.
    pub fn forget(&self, id: &str) -> bool {
        let found = self.inner.devices.lock().map(|mut d| d.forget(id)).unwrap_or(false);
        if found {
            self.inner.save_devices();
            if let Ok(mut a) = self.inner.active.lock() {
                if a.as_ref().is_some_and(|x| x.id == id) {
                    *a = None; // the connection notices and ends
                }
            }
        }
        found
    }

    /// A command for the phone that is connected; its id, so the answer can be matched. None if no phone is there.
    pub fn send_command(&self, action: Action, position_ms: Option<u64>) -> Option<String> {
        let id = uuid_v4();
        let a = self.inner.active.lock().ok()?;
        a.as_ref()?.tx.send(wire::command(&id, action, position_ms)).ok()?;
        crate::log::note(&format!("command {} sent ({})", action.name(), &id[..8]));
        Some(id)
    }
}

/// One connection's socket and what has been read from it.
struct Conn {
    tls: StreamOwned<ServerConnection, TcpStream>,
    fb: FrameBuf,
    buf: Vec<u8>,
}

enum Poll {
    Frame(FromPhone),
    /// Nothing yet.
    Idle,
    Closed,
    /// A frame that must not be: the connection ends.
    Bad,
}

impl Conn {
    fn poll(&mut self) -> Poll {
        loop {
            match self.fb.next() {
                Err(_) => return Poll::Bad,
                Ok(Some(p)) => {
                    return match wire::parse(&p) {
                        Ok(m) => Poll::Frame(m),
                        Err(_) => Poll::Bad,
                    }
                }
                Ok(None) => {}
            }
            match self.tls.read(&mut self.buf) {
                Ok(0) => return Poll::Closed,
                Ok(n) => self.fb.push(&self.buf[..n]),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => return Poll::Idle,
                Err(_) => return Poll::Closed,
            }
        }
    }

    /// The next frame before `deadline` (a closed or broken connection is None as well).
    fn next(&mut self, deadline: Instant) -> Option<FromPhone> {
        while Instant::now() < deadline {
            match self.poll() {
                Poll::Frame(m) => return Some(m),
                Poll::Idle => {}
                Poll::Closed | Poll::Bad => return None,
            }
        }
        None
    }

    fn send(&mut self, bytes: &[u8]) -> bool {
        !bytes.is_empty() && self.tls.write_all(bytes).and_then(|_| self.tls.flush()).is_ok()
    }
}

impl Inner {
    /// Write the list down. True if it is kept (also when there is no file to keep it in).
    fn save_devices(&self) -> bool {
        let (Some(file), Ok(d)) = (&self.devices_file, self.devices.lock()) else { return self.devices_file.is_none() };
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::write(file, d.serialize()).is_ok()
    }

    /// Take a phone's access away for good. False, and nothing changed, if there is no such phone or the change cannot be kept.
    fn revoke(&self, id: &str) -> bool {
        let backup = {
            let Ok(mut d) = self.devices.lock() else { return false };
            let before = d.clone();
            if !d.forget(id) {
                return false;
            }
            before
        };
        if self.save_devices() {
            return true;
        }
        if let Ok(mut d) = self.devices.lock() {
            *d = backup;
        }
        false
    }

    fn emit(&self, e: Event) {
        let _ = self.events.send(e);
    }

    fn handle(&self, tcp: TcpStream) {
        let _ = tcp.set_nodelay(true);
        let _ = tcp.set_read_timeout(Some(self.timing.poll));
        let _ = tcp.set_write_timeout(Some(Duration::from_secs(5)));
        let Ok(session) = ServerConnection::new(self.cfg.clone()) else { return };
        let mut c = Conn { tls: StreamOwned::new(session, tcp), fb: FrameBuf::default(), buf: vec![0u8; 16 * 1024] };
        let who = match c.next(Instant::now() + self.timing.hello) {
            Some(FromPhone::PairHello { client_nonce, device_name }) => self.pair(&mut c, &client_nonce, device_name.as_deref()),
            Some(FromPhone::Auth { token, device_name }) => self.auth(&mut c, &token, device_name.as_deref()),
            _ => None,
        };
        if let Some(id) = who {
            self.session(&mut c, id);
        }
    }

    /// A phone asks to pair. Some(id) once it is paired (the connection goes on as that phone).
    fn pair(&self, c: &mut Conn, client_nonce: &[u8; 32], device_name: Option<&str>) -> Option<String> {
        let started = Instant::now();
        if self.gate.lock().ok()?.begin(started).is_err() {
            return None;
        }
        let paired = self.pair_inner(c, client_nonce, device_name, started);
        if let Ok(mut g) = self.gate.lock() {
            g.end(paired.is_some(), Instant::now());
        }
        if let Ok(mut d) = self.decision.lock() {
            *d = None;
        }
        self.emit(Event::PairEnded { paired: paired.is_some() });
        paired
    }

    fn pair_inner(&self, c: &mut Conn, client_nonce: &[u8; 32], device_name: Option<&str>, started: Instant) -> Option<String> {
        let server_nonce = random::<32>();
        if !c.send(&wire::pair_challenge(&server_nonce)) {
            return None;
        }
        let (tx, rx) = channel::<bool>();
        *self.decision.lock().ok()? = Some(tx);
        self.emit(Event::PairRequest { code: wire::pairing_code(&self.fingerprint, client_nonce, &server_nonce) });
        let deadline = started + self.timing.decision;
        let (mut phone, mut user): (Option<bool>, Option<bool>) = (None, None);
        while (phone.is_none() || user.is_none()) && Instant::now() < deadline {
            if user.is_none() {
                user = rx.try_recv().ok();
            }
            if phone.is_none() {
                match c.poll() {
                    Poll::Frame(FromPhone::PairDecision { approved }) => phone = Some(approved),
                    Poll::Idle => {}
                    _ => return None,
                }
            } else if user.is_none() {
                user = rx.recv_timeout(self.timing.poll).ok();
            }
        }
        let approved = phone == Some(true) && user == Some(true);
        let device = if approved {
            let token = random::<32>();
            let added = self.devices.lock().ok().and_then(|mut d| d.add_named(&token, now_unix(), device_name).map(|d| d.id.clone()));
            added.map(|id| (id, token))
        } else {
            None
        };
        if !c.send(&wire::pair_decision(device.is_some())) {
            return None;
        }
        let (id, token) = device?;
        self.save_devices();
        c.send(&wire::pair_result(&token)).then_some(id)
    }

    /// A phone that was paired before. Some(id) if its token is good.
    fn auth(&self, c: &mut Conn, token: &[u8; 32], device_name: Option<&str>) -> Option<String> {
        let id = self.devices.lock().ok()?.verify(token).map(|d| d.id.clone());
        // a phone that has not been given a name of its own takes the one it brings
        if let (Some(id), Some(name)) = (&id, device_name) {
            if self.devices.lock().ok().and_then(|mut d| d.adopt_name(id, name)).is_some() {
                self.save_devices();
            }
        }
        c.send(&wire::auth_result(id.is_some()));
        if id.is_none() {
            // a guess costs time
            std::thread::sleep(Duration::from_millis(300));
        }
        id
    }

    /// Signed in: snapshots in, commands out, until it ends.
    fn session(&self, c: &mut Conn, id: String) {
        let (tx, rx) = channel::<Vec<u8>>();
        let generation = self.generation.fetch_add(1, Ordering::SeqCst);
        // a phone that signs in replaces the one that was connected
        if let Ok(mut a) = self.active.lock() {
            *a = Some(Active { id: id.clone(), tx, generation });
        }
        let name = self.devices.lock().ok().and_then(|d| d.list().iter().find(|x| x.id == id).map(|x| x.name.clone())).unwrap_or_default();
        crate::log::note(&format!("phone {id} connected"));
        self.emit(Event::Connected { id: id.clone(), name });
        // what the log says when it ends: why, how long, how many frames and the longest wait between two
        let (started, mut frames, mut longest, mut last_frame, mut why) = (Instant::now(), 0u64, Duration::ZERO, Instant::now(), "closed by the phone");
        let (mut seen, mut pinged, mut last_seq) = (Instant::now(), Instant::now(), None::<u64>);
        // a few quick pings first, so the delay is known when the first song arrives; then one now and then
        let (mut latency, mut sent, mut burst) = (Latency::default(), Vec::<(String, Instant)>::new(), 4u32);
        let mut burst_at = Instant::now();
        let mut keep = wire::ArtworkKeep::default();
        let mut last_state = None::<(bool, bool)>;
        loop {
            if !self.active.lock().is_ok_and(|a| a.as_ref().is_some_and(|a| a.generation == generation)) {
                why = "replaced or forgotten";
                break;
            }
            while let Ok(frame) = rx.try_recv() {
                if !c.send(&frame) {
                    break;
                }
            }
            match c.poll() {
                Poll::Closed => break,
                Poll::Bad => {
                    why = "bad frame";
                    break;
                }
                Poll::Idle => {}
                Poll::Frame(m) => {
                    let now = Instant::now();
                    longest = longest.max(now.saturating_duration_since(last_frame));
                    (seen, last_frame, frames) = (now, now, frames + 1);
                    match m {
                        FromPhone::Ping { id } => {
                            if !c.send(&wire::pong(&id)) {
                                why = "write failed";
                                break;
                            }
                        }
                        FromPhone::Pong { id } => {
                            if let Some(i) = sent.iter().position(|(p, _)| *p == id) {
                                let (_, at) = sent.remove(i);
                                if let Some(d) = latency.add(at.elapsed()) {
                                    self.emit(Event::Latency(d));
                                }
                            }
                        }
                        FromPhone::Snapshot(mut s) => {
                            // `seq` only goes up: an old or repeated one is a phone that is confused, or something else
                            if last_seq.is_some_and(|l| s.seq <= l) {
                                why = "seq went back";
                                break;
                            }
                            last_seq = Some(s.seq);
                            keep.resolve(&mut s);
                            // what the phone says it plays changes now and then: noted, without the song
                            let now_state = (s.active, s.playing);
                            if last_state != Some(now_state) {
                                crate::log::note(&format!("phone reports active={} playing={} source={}", s.active, s.playing, s.source));
                                last_state = Some(now_state);
                            }
                            self.emit(Event::Snapshot(s));
                        }
                        FromPhone::Ack { id, accepted, reason } => {
                            crate::log::note(&format!("ack {} accepted={accepted} {reason}", id.get(..8).unwrap_or("")));
                            self.emit(Event::Ack { id, accepted, reason });
                        }
                        // pairing or signing in again on a signed-in connection
                        FromPhone::Forget => {
                            let name = self.devices.lock().ok().and_then(|d| d.list().iter().find(|x| x.id == id).map(|x| x.name.clone())).unwrap_or_default();
                            let done = self.revoke(&id);
                            c.send(&wire::forget_result(done));
                            if done {
                                crate::log::note(&format!("phone {id} asked to be forgotten"));
                                self.emit(Event::Forgotten { id: id.clone(), name });
                                why = "forgotten by the phone";
                                break;
                            }
                        }
                        FromPhone::PairHello { .. } | FromPhone::PairDecision { .. } | FromPhone::Auth { .. } => {
                            why = "unexpected message";
                            break;
                        }
                    }
                }
            }
            let due = if burst > 0 { burst_at.elapsed() >= Duration::from_millis(150) } else { pinged.elapsed() >= self.timing.ping_every };
            if due {
                let id = hex(&random::<8>());
                if sent.len() >= 8 {
                    sent.remove(0);
                }
                sent.push((id.clone(), Instant::now()));
                if burst > 0 {
                    burst -= 1;
                    burst_at = Instant::now();
                }
                pinged = Instant::now();
                if !c.send(&wire::ping(&id)) {
                    why = "write failed";
                    break;
                }
            }
            if seen.elapsed() >= self.timing.idle {
                why = "idle";
                break;
            }
        }
        if let Ok(mut a) = self.active.lock() {
            if a.as_ref().is_some_and(|a| a.generation == generation) {
                *a = None;
            }
        }
        crate::log::note(&format!("phone {id} gone: {why} after {} s, {frames} frames, longest wait {} ms", started.elapsed().as_secs(), longest.as_millis()));
        self.emit(Event::Disconnected { id, why });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use std::sync::mpsc::Receiver;
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme};
    use serde_json::{json, Value};

    /// A phone that accepts any certificate, like the real one does until it has pinned the fingerprint; the test checks
    /// the fingerprint itself.
    #[derive(Debug)]
    struct AnyCert;

    impl ServerCertVerifier for AnyCert {
        fn verify_server_cert(&self, _: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(m, c, d, &rustls::crypto::ring::default_provider().signature_verification_algorithms)
        }
        fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(m, c, d, &rustls::crypto::ring::default_provider().signature_verification_algorithms)
        }
        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes()
        }
    }

    struct Phone {
        tls: StreamOwned<ClientConnection, TcpStream>,
        fb: FrameBuf,
    }

    impl Phone {
        fn connect(port: u16) -> Phone {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let cfg = ClientConfig::builder_with_provider(provider).with_safe_default_protocol_versions().unwrap().dangerous().with_custom_certificate_verifier(Arc::new(AnyCert)).with_no_client_auth();
            let tcp = TcpStream::connect(("127.0.0.1", port)).unwrap();
            tcp.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
            let conn = ClientConnection::new(Arc::new(cfg), ServerName::try_from("localhost").unwrap()).unwrap();
            let mut tls = StreamOwned::new(conn, tcp);
            // the handshake would wait for the first read or write; finish it now so the certificate can be looked at
            while tls.conn.is_handshaking() {
                tls.conn.complete_io(&mut tls.sock).unwrap();
            }
            Phone { tls, fb: FrameBuf::default() }
        }

        fn send(&mut self, kind: &str, fields: Value) {
            let f = wire::frame(kind, fields).unwrap();
            self.tls.write_all(&f).unwrap();
            self.tls.flush().unwrap();
        }

        /// The next frame the server sends, None if it closed the connection or nothing came in 4 seconds.
        fn recv(&mut self) -> Option<Value> {
            let mut buf = [0u8; 4096];
            loop {
                if let Ok(Some(p)) = self.fb.next() {
                    return serde_json::from_slice(&p).ok();
                }
                match self.tls.read(&mut buf) {
                    Ok(n) if n > 0 => self.fb.push(&buf[..n]),
                    _ => return None,
                }
            }
        }

        fn fingerprint(&self) -> [u8; 32] {
            wire::fingerprint(self.tls.conn.peer_certificates().unwrap()[0].as_ref())
        }
    }

    fn fast() -> Timing {
        Timing { hello: Duration::from_millis(600), decision: Duration::from_secs(3), ping_every: Duration::from_millis(300), idle: Duration::from_millis(900), poll: Duration::from_millis(10) }
    }

    struct Rig {
        server: Server,
        events: Receiver<Event>,
        fp: [u8; 32],
    }

    fn rig(timing: Timing, file: Option<PathBuf>) -> Rig {
        let id = Identity::generate().unwrap();
        let (tx, events) = channel();
        let server = Server::start(crate::tls::server_config(&id).unwrap(), id.fingerprint, file, tx, "127.0.0.1:0".parse().unwrap(), timing).unwrap();
        Rig { server, events, fp: id.fingerprint }
    }

    impl Rig {
        /// The next event that is not a latency report (those come on their own as pings are answered).
        fn event(&self) -> Event {
            loop {
                match self.events.recv_timeout(Duration::from_secs(4)).expect("an event") {
                    Event::Latency(_) => {}
                    e => return e,
                }
            }
        }

        /// The next latency report.
        fn latency(&self) -> Duration {
            loop {
                if let Event::Latency(d) = self.events.recv_timeout(Duration::from_secs(4)).expect("a latency report") {
                    return d;
                }
            }
        }

        fn no_event(&self) {
            assert!(self.events.recv_timeout(Duration::from_millis(250)).is_err(), "no event expected");
        }
    }

    const NONCE: [u8; 32] = [3; 32];

    fn pair(r: &Rig) -> ([u8; 32], String) {
        pair_as(r, None)
    }

    /// Pair a phone to the end, with the name it gives (if any); returns its token and the code both sides saw.
    fn pair_as(r: &Rig, name: Option<&str>) -> ([u8; 32], String) {
        r.server.open_pairing();
        let mut p = Phone::connect(r.server.port());
        assert_eq!(p.fingerprint(), r.fp, "the phone sees the certificate it will pin");
        let mut hello = json!({"client_nonce": wire::b64_encode(&NONCE)});
        if let Some(n) = name {
            hello["device_name"] = json!(n);
        }
        p.send("pair_hello", hello);
        let ch = p.recv().expect("challenge");
        assert_eq!(ch["type"], "pair_challenge");
        let server_nonce = wire::b64_decode(ch["server_nonce"].as_str().unwrap()).unwrap();
        let phone_code = wire::pairing_code(&r.fp, &NONCE, &server_nonce);
        let Event::PairRequest { code } = r.event() else { panic!("a pairing request") };
        assert_eq!(code, phone_code, "both sides show the same six digits");
        p.send("pair_decision", json!({"approved": true}));
        r.server.decide(true);
        assert_eq!(p.recv().unwrap()["approved"], true);
        let res = p.recv().unwrap();
        assert_eq!((res["type"].as_str(), res["approved"].as_bool()), (Some("pair_result"), Some(true)));
        assert_eq!(res["capabilities"], json!(["artwork_delta_v1", "forget_v1"]));
        let token: [u8; 32] = wire::b64_decode(res["token"].as_str().unwrap()).unwrap().try_into().unwrap();
        assert!(matches!(r.event(), Event::PairEnded { paired: true }));
        assert!(matches!(r.event(), Event::Connected { .. }), "after pairing the phone is signed in");
        (token, code)
    }

    fn sign_in(r: &Rig, token: &[u8; 32]) -> Phone {
        sign_in_as(r, token, None)
    }

    fn sign_in_as(r: &Rig, token: &[u8; 32], name: Option<&str>) -> Phone {
        let mut p = Phone::connect(r.server.port());
        let mut auth = json!({"token": wire::b64_encode(token)});
        if let Some(n) = name {
            auth["device_name"] = json!(n);
        }
        p.send("auth", auth);
        assert_eq!(p.recv().unwrap(), json!({"v": 1, "type": "auth_result", "approved": true, "capabilities": ["artwork_delta_v1", "forget_v1"]}));
        p
    }

    fn snapshot(seq: u64) -> Value {
        json!({"seq": seq, "active": true, "source": "com.example.player", "title": "A Song", "artist": "An Artist", "position_ms": 1000, "duration_ms": 200000, "playing": true, "speed": 1, "actions": ["play", "pause", "next", "previous", "seek"]})
    }

    #[test]
    fn a_phone_pairs_with_both_approvals_and_signs_in_with_its_token() {
        let r = rig(fast(), None);
        let (token, _) = pair(&r);
        assert_eq!(r.server.devices().len(), 1);
        assert_eq!(r.server.pairing_left(), None, "a pairing closes the mode");
        drop(r.server.connected());
        // another connection, later: the token
        let mut p = sign_in(&r, &token);
        assert!(matches!(r.event(), Event::Disconnected { .. }), "the first connection of this phone is replaced");
        assert!(matches!(r.event(), Event::Connected { .. }));
        // snapshots in, commands out, answers back
        p.send("snapshot", snapshot(1));
        let Event::Snapshot(s) = r.event() else { panic!("a snapshot") };
        assert_eq!((s.title.as_str(), s.position_ms, s.playing), ("A Song", 1000, true));
        let id = r.server.send_command(Action::Seek, Some(30000)).expect("a phone is connected");
        assert!(id.len() == 36 && id.chars().enumerate().all(|(i, c)| if [8, 13, 18, 23].contains(&i) { c == '-' } else { c.is_ascii_hexdigit() && !c.is_ascii_uppercase() }) && &id[14..15] == "4", "the phone only takes a UUID as a command's id");
        let c = p.recv().unwrap();
        assert_eq!((c["type"].as_str(), c["action"].as_str(), c["position_ms"].as_u64(), c["id"].as_str()), (Some("command"), Some("seek"), Some(30000), Some(id.as_str())));
        p.send("ack", json!({"id": id, "accepted": true}));
        assert_eq!(r.event(), Event::Ack { id, accepted: true, reason: String::new() });
    }

    /// The next frame that is not a ping (the server pings now and then, on its own).
    fn reply(p: &mut Phone) -> Value {
        loop {
            let v = p.recv().expect("a reply");
            if v["type"] != "ping" {
                return v;
            }
        }
    }

    fn drain(r: &Rig) {
        while r.events.recv_timeout(Duration::from_millis(150)).is_ok() {}
    }

    #[test]
    fn a_phone_pairs_with_the_name_it_gives_and_signing_in_keeps_a_chosen_name() {
        let r = rig(fast(), None);
        let (token, _) = pair_as(&r, Some("Pixel 9"));
        assert_eq!(r.server.devices()[0].name, "Pixel 9");
        let id = r.server.devices()[0].id.clone();
        let _p = sign_in_as(&r, &token, Some("Pixel 9 Pro"));
        assert_eq!(r.server.devices()[0].name, "Pixel 9", "the name from pairing stays");
        r.server.rename(&id, "Dirga's phone");
        let _q = sign_in_as(&r, &token, Some("Pixel 9"));
        assert_eq!(r.server.devices()[0].name, "Dirga's phone", "a name typed in stays");

        // a phone paired before it gave names takes the one it has when it signs in
        let r = rig(fast(), None);
        let (token, _) = pair(&r);
        assert_eq!(r.server.devices()[0].name, "Phone 1");
        let _p = sign_in_as(&r, &token, Some("Galaxy A57"));
        assert_eq!(r.server.devices()[0].name, "Galaxy A57");
    }

    #[test]
    fn a_picture_that_is_said_to_be_the_same_is_the_one_from_before_on_this_connection_only() {
        let r = rig(fast(), None);
        let (token, _) = pair(&r);
        let mut p = sign_in(&r, &token);
        drain(&r);
        let send = |p: &mut Phone, seq: u64, extra: Value| {
            let mut s = snapshot(seq);
            s.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            p.send("snapshot", s);
        };
        let art = |r: &Rig| {
            let Event::Snapshot(s) = r.event() else { panic!("a snapshot") };
            (s.artwork_mime, s.artwork_base64)
        };
        send(&mut p, 1, json!({"artwork_mime": "image/png", "artwork_base64": "AAAA"}));
        assert_eq!(art(&r), (Some("image/png".to_string()), Some("AAAA".to_string())));
        send(&mut p, 2, json!({"artwork_same": true}));
        assert_eq!(art(&r), (Some("image/png".to_string()), Some("AAAA".to_string())), "the same picture comes back");
        send(&mut p, 3, json!({"artwork_mime": "image/jpeg", "artwork_base64": "BBBB"}));
        assert_eq!(art(&r).1.as_deref(), Some("BBBB"), "another picture for the same title is another picture");
        send(&mut p, 4, json!({}));
        assert_eq!(art(&r), (None, None), "no picture, no flag: none, and nothing is inherited");
        send(&mut p, 5, json!({"artwork_same": true}));
        assert_eq!(art(&r), (None, None));
        send(&mut p, 6, json!({"artwork_mime": "image/png", "artwork_base64": "AAAA"}));
        art(&r);
        let mut q = sign_in(&r, &token);
        drain(&r);
        send(&mut q, 1, json!({"artwork_same": true}));
        assert_eq!(art(&r), (None, None), "a new connection does not inherit the picture of the old one");
    }

    #[test]
    fn a_phone_that_asks_to_be_forgotten_is_forgotten_and_told_so() {
        let dir = std::env::temp_dir().join(format!("hesewake-bridge-forget-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("devices.json");
        let r = rig(fast(), Some(file.clone()));
        let (token, _) = pair(&r);
        let other = {
            let mut d = r.server.devices();
            assert_eq!(d.len(), 1);
            d.remove(0).id
        };
        let mut p = sign_in(&r, &token);
        drain(&r);
        p.send("forget", json!({}));
        assert_eq!(reply(&mut p), json!({"v": 1, "type": "forget_result", "approved": true}));
        assert!(matches!(r.event(), Event::Forgotten { ref id, .. } if *id == other));
        assert!(matches!(r.event(), Event::Disconnected { .. }));
        assert!(r.server.devices().is_empty());
        assert!(!std::fs::read_to_string(&file).unwrap().contains(&other), "gone from the file as well");
        let mut again = Phone::connect(r.server.port());
        again.send("auth", json!({"token": wire::b64_encode(&token)}));
        assert_eq!(again.recv().unwrap()["approved"], false, "the old token is turned away");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_forget_that_cannot_be_kept_is_refused_and_nothing_is_lost() {
        let dir = std::env::temp_dir().join(format!("hesewake-bridge-forget-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let blocker = dir.join("not-a-folder");
        std::fs::write(&blocker, "x").unwrap();
        // a file where the folder should be: nothing can be saved there
        let r = rig(fast(), Some(blocker.join("devices.json")));
        let (token, _) = pair(&r);
        let mut p = sign_in(&r, &token);
        drain(&r);
        p.send("forget", json!({}));
        assert_eq!(reply(&mut p), json!({"v": 1, "type": "forget_result", "approved": false}));
        assert_eq!(r.server.devices().len(), 1, "it is still paired, since the change could not be made lasting");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nobody_pairs_unless_the_user_asked() {
        let r = rig(fast(), None);
        let mut p = Phone::connect(r.server.port());
        p.send("pair_hello", json!({"client_nonce": wire::b64_encode(&NONCE)}));
        assert!(p.recv().is_none(), "pairing mode is off: the connection just closes");
        r.no_event();
        assert!(r.server.devices().is_empty());
    }

    #[test]
    fn either_side_can_say_no() {
        let r = rig(fast(), None);
        // the user says no
        r.server.open_pairing();
        let mut p = Phone::connect(r.server.port());
        p.send("pair_hello", json!({"client_nonce": wire::b64_encode(&NONCE)}));
        p.recv().expect("challenge");
        assert!(matches!(r.event(), Event::PairRequest { .. }));
        p.send("pair_decision", json!({"approved": true}));
        r.server.decide(false);
        assert_eq!(p.recv().unwrap()["approved"], false);
        assert!(p.recv().is_none(), "and no token");
        assert!(matches!(r.event(), Event::PairEnded { paired: false }));
        // the phone says no
        let mut p = Phone::connect(r.server.port());
        p.send("pair_hello", json!({"client_nonce": wire::b64_encode(&NONCE)}));
        p.recv().expect("challenge");
        assert!(matches!(r.event(), Event::PairRequest { .. }));
        p.send("pair_decision", json!({"approved": false}));
        r.server.decide(true);
        assert_eq!(p.recv().unwrap()["approved"], false);
        assert!(matches!(r.event(), Event::PairEnded { paired: false }));
        assert!(r.server.devices().is_empty());
    }

    #[test]
    fn a_pairing_nobody_finishes_times_out() {
        let t = Timing { decision: Duration::from_millis(500), ..fast() };
        let r = rig(t, None);
        r.server.open_pairing();
        let mut p = Phone::connect(r.server.port());
        p.send("pair_hello", json!({"client_nonce": wire::b64_encode(&NONCE)}));
        p.recv().expect("challenge");
        assert!(matches!(r.event(), Event::PairRequest { .. }));
        assert_eq!(p.recv().unwrap()["approved"], false, "no decision in time: refused");
        assert!(matches!(r.event(), Event::PairEnded { paired: false }));
    }

    #[test]
    fn only_one_pairing_at_a_time() {
        let r = rig(fast(), None);
        r.server.open_pairing();
        let mut a = Phone::connect(r.server.port());
        a.send("pair_hello", json!({"client_nonce": wire::b64_encode(&NONCE)}));
        a.recv().expect("challenge");
        let mut b = Phone::connect(r.server.port());
        b.send("pair_hello", json!({"client_nonce": wire::b64_encode(&[4u8; 32])}));
        assert!(b.recv().is_none(), "the second is turned away");
        r.server.decide(false);
    }

    #[test]
    fn a_wrong_token_is_refused_and_forgetting_ends_a_phone() {
        let r = rig(fast(), None);
        let (token, _) = pair(&r);
        let mut bad = Phone::connect(r.server.port());
        bad.send("auth", json!({"token": wire::b64_encode(&[0u8; 32])}));
        assert_eq!(bad.recv().unwrap()["approved"], false);
        assert!(bad.recv().is_none());
        let mut p = sign_in(&r, &token);
        assert!(matches!(r.event(), Event::Disconnected { .. }));
        assert!(matches!(r.event(), Event::Connected { .. }));
        let id = r.server.devices()[0].id.clone();
        assert!(r.server.forget(&id));
        assert!(matches!(r.event(), Event::Disconnected { .. }), "forgetting disconnects it");
        assert!(p.recv().is_none());
        let mut again = Phone::connect(r.server.port());
        again.send("auth", json!({"token": wire::b64_encode(&token)}));
        assert_eq!(again.recv().unwrap()["approved"], false, "the token no longer works");
        assert!(r.server.send_command(Action::Play, None).is_none(), "nobody to send to");
    }

    #[test]
    fn paired_phones_survive_a_restart() {
        let file = std::env::temp_dir().join(format!("hesewake-bridge-devices-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&file);
        let (token, id) = {
            let r = rig(fast(), Some(file.clone()));
            let (t, _) = pair(&r);
            (t, r.server.devices()[0].id.clone())
        };
        let r2 = rig(fast(), Some(file.clone()));
        assert_eq!(r2.server.devices()[0].id, id);
        let _p = sign_in(&r2, &token);
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn bad_behaviour_ends_the_connection() {
        let r = rig(fast(), None);
        let (token, _) = pair(&r);
        // a snapshot whose number does not go up
        let mut p = sign_in(&r, &token);
        r.event();
        r.event();
        p.send("snapshot", snapshot(5));
        assert!(matches!(r.event(), Event::Snapshot(_)));
        p.send("snapshot", snapshot(5));
        assert!(matches!(r.event(), Event::Disconnected { .. }), "a repeated seq ends it");
        // pairing again on a signed-in connection
        let mut p = sign_in(&r, &token);
        r.event();
        p.send("pair_hello", json!({"client_nonce": wire::b64_encode(&NONCE)}));
        assert!(matches!(r.event(), Event::Disconnected { .. }));
        // a frame that is not one
        let mut p = sign_in(&r, &token);
        r.event();
        p.tls.write_all(&[0, 0, 0, 3, b'n', b'o', b'!']).unwrap();
        assert!(matches!(r.event(), Event::Disconnected { .. }));
        // an announced length that is too big
        let mut p = sign_in(&r, &token);
        r.event();
        p.tls.write_all(&((wire::MAX_FRAME + 1) as u32).to_be_bytes()).unwrap();
        assert!(matches!(r.event(), Event::Disconnected { .. }));
    }

    #[test]
    fn silence_is_noticed_and_pings_keep_a_phone_alive() {
        let r = rig(fast(), None);
        let (token, _) = pair(&r);
        let mut p = sign_in(&r, &token);
        r.event();
        r.event();
        // answers every ping for a while: stays connected past the idle limit
        let until = Instant::now() + Duration::from_millis(1500);
        let mut pings = 0;
        while Instant::now() < until {
            if let Some(m) = p.recv().filter(|m| m["type"] == "ping") {
                pings += 1;
                p.send("pong", json!({"id": m["id"]}));
            }
        }
        assert!(pings >= 2, "the server pings ({pings})");
        assert!(r.server.connected().is_some());
        // then goes quiet
        assert!(matches!(r.event(), Event::Disconnected { .. }), "35 s of nothing (here 0.9) ends it");
    }

    #[test]
    fn the_round_trip_of_the_pings_is_measured() {
        let r = rig(fast(), None);
        let (token, _) = pair(&r);
        let mut p = sign_in(&r, &token);
        // a phone that answers each ping after a delay of its own: about 40 ms there and back
        let until = Instant::now() + Duration::from_secs(3);
        let mut answered = 0;
        while Instant::now() < until && answered < 3 {
            if let Some(m) = p.recv().filter(|m| m["type"] == "ping") {
                std::thread::sleep(Duration::from_millis(40));
                p.send("pong", json!({"id": m["id"]}));
                answered += 1;
            }
        }
        let d = r.latency();
        assert!(d >= Duration::from_millis(15) && d <= Duration::from_millis(120), "one way is about half of 40 ms plus the work of the test: {d:?}");
        // a pong for a ping that was never sent says nothing
        p.send("pong", json!({"id": "never-sent"}));
        assert!(!matches!(r.events.recv_timeout(Duration::from_millis(300)), Ok(Event::Latency(_))), "no report from a made-up pong");
    }

    #[test]
    fn a_connection_that_says_nothing_is_dropped() {
        let r = rig(fast(), None);
        let mut p = Phone::connect(r.server.port());
        assert!(p.recv().is_none());
        r.no_event();
    }

    #[test]
    fn a_phone_that_asks_to_ping_gets_a_pong() {
        let r = rig(fast(), None);
        let (token, _) = pair(&r);
        let mut p = sign_in(&r, &token);
        r.event();
        r.event();
        p.send("ping", json!({"id": "abc"}));
        loop {
            let m = p.recv().expect("something");
            if m["type"] == "pong" {
                assert_eq!(m["id"], "abc");
                break;
            }
        }
    }
}
