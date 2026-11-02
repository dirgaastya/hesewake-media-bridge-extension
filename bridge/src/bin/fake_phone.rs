//! A pretend phone, for trying Media Bridge without one. It finds the PC on the local network (or takes --host and
//! --port), pairs (it approves on its side at once; the PC's side is for you), keeps the token in `fake-phone.json` in the
//! current folder, and then plays made-up songs: it sends a snapshot every second and does what the commands ask.
//!
//!   fake-phone                      find the PC and connect (pairs the first time)
//!   fake-phone --host 127.0.0.1 --port 50000
//!   fake-phone --forget             throw the stored token away

use hesewake_media_bridge::wire::{self, FrameBuf};
use image::codecs::jpeg::JpegEncoder;
use mdns_sd::{ServiceDaemon, ServiceEvent};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

const FILE: &str = "fake-phone.json";
const SONGS: [(&str, &str, &str, u64); 3] = [("Fake Song One", "The Test Band", "Pretend Album", 183_000), ("Another Made-Up Tune", "Synthetic Artist", "Pretend Album", 241_000), ("Third Imaginary Track", "The Test Band", "Second Pretend Album", 207_000)];

/// The real phone pins the certificate's fingerprint; this one takes any and checks the pin itself afterwards.
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

/// A made-up cover: 512 px, a different gradient for each song, as the phone's JPEG (q75) in standard base64.
fn cover(song: usize) -> String {
    let img = image::RgbImage::from_fn(512, 512, |x, y| {
        let (a, b) = ((x * 255 / 511) as u8, (y * 255 / 511) as u8);
        match song % 3 {
            0 => image::Rgb([a, b, 160]),
            1 => image::Rgb([220 - b / 2, a, b]),
            _ => image::Rgb([b, 200 - a / 2, a]),
        }
    });
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 75).encode_image(&img).expect("a JPEG");
    wire::b64std_encode(&out)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    (s.len() % 2 == 0).then(|| ()).and_then(|_| (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect())
}

fn find_pc() -> Option<(String, u16)> {
    let daemon = ServiceDaemon::new().ok()?;
    let rx = daemon.browse("_hesewake-media._tcp.local.").ok()?;
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        if let Ok(ServiceEvent::ServiceResolved(info)) = rx.recv_timeout(Duration::from_millis(300)) {
            if let Some(addr) = info.get_addresses().iter().find(|a| a.is_ipv4()) {
                println!("found {} at {}:{}", info.get_fullname(), addr, info.get_port());
                let _ = daemon.shutdown();
                return Some((addr.to_string(), info.get_port()));
            }
        }
    }
    let _ = daemon.shutdown();
    None
}

struct Conn {
    tls: StreamOwned<ClientConnection, TcpStream>,
    fb: FrameBuf,
}

impl Conn {
    fn send(&mut self, kind: &str, fields: Value) {
        let f = wire::frame(kind, fields).expect("a frame");
        self.tls.write_all(&f).expect("write");
        self.tls.flush().expect("flush");
    }

    /// The next frame, or None if nothing came within the read timeout. Exits when the PC closes the connection.
    fn recv(&mut self) -> Option<Value> {
        let mut buf = [0u8; 4096];
        loop {
            if let Ok(Some(p)) = self.fb.next() {
                return serde_json::from_slice(&p).ok();
            }
            match self.tls.read(&mut buf) {
                Ok(0) => {
                    println!("the PC closed the connection");
                    std::process::exit(0);
                }
                Ok(n) => self.fb.push(&buf[..n]),
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => return None,
                Err(e) => {
                    println!("connection ended: {e}");
                    std::process::exit(0);
                }
            }
        }
    }

    fn wait(&mut self, what: &str) -> Value {
        for _ in 0..600 {
            if let Some(v) = self.recv() {
                return v;
            }
        }
        println!("nothing came: {what}");
        std::process::exit(1);
    }
}

fn caps_of(v: &Value) -> Vec<String> {
    v["capabilities"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

fn arg(name: &str) -> Option<String> {
    std::env::args().skip_while(|a| a != name).nth(1)
}

fn main() {
    if std::env::args().any(|a| a == "--forget") {
        let _ = std::fs::remove_file(FILE);
        println!("token forgotten");
        return;
    }
    let (host, port) = match (arg("--host"), arg("--port").and_then(|p| p.parse::<u16>().ok())) {
        (Some(h), Some(p)) => (h, p),
        _ => find_pc().unwrap_or_else(|| {
            println!("no PC found on the network; is Media Bridge running and Windows Firewall open for it? (or pass --host and --port)");
            std::process::exit(1);
        }),
    };
    let stored: Option<Value> = std::fs::read_to_string(FILE).ok().and_then(|t| serde_json::from_str(&t).ok());
    let name = arg("--name").unwrap_or_else(|| "Fake Phone".to_string());
    // the same every run, as a real phone's id is: pairing again does not make a second entry
    let id = arg("--id").unwrap_or_else(|| "fake-phone-0001".to_string());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = ClientConfig::builder_with_provider(provider).with_safe_default_protocol_versions().unwrap().dangerous().with_custom_certificate_verifier(Arc::new(AnyCert)).with_no_client_auth();
    let tcp = TcpStream::connect((host.as_str(), port)).expect("connect");
    tcp.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    let mut tls = StreamOwned::new(ClientConnection::new(Arc::new(cfg), ServerName::try_from("localhost").unwrap()).unwrap(), tcp);
    while tls.conn.is_handshaking() {
        let _ = tls.conn.complete_io(&mut tls.sock);
    }
    let fingerprint = wire::fingerprint(tls.conn.peer_certificates().expect("a certificate")[0].as_ref());
    let mut c = Conn { tls, fb: FrameBuf::default() };

    let caps: Vec<String>;
    match stored {
        Some(s) if s["fingerprint"].as_str().and_then(unhex).as_deref() != Some(&fingerprint[..]) => {
            println!("the PC's certificate is not the one that was paired with; run with --forget to pair again");
            std::process::exit(1);
        }
        Some(s) => {
            let token = s["token"].as_str().unwrap_or("");
            c.send("auth", json!({"token": token, "device_name": name, "device_id": id}));
            let r = c.wait("auth_result");
            if r["approved"] != true {
                println!("the PC does not know this phone any more (forgotten?); run with --forget to pair again");
                std::process::exit(1);
            }
            caps = caps_of(&r);
            println!("signed in");
        }
        None => {
            let nonce: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_mul(7).wrapping_add(std::process::id() as u8));
            c.send("pair_hello", json!({"client_nonce": wire::b64_encode(&nonce), "device_name": name, "device_id": id}));
            let ch = c.wait("pair_challenge");
            if ch["type"] != "pair_challenge" {
                println!("the PC will not pair now: press \"Pair a phone\" in the notch first");
                std::process::exit(1);
            }
            let server_nonce = wire::b64_decode(ch["server_nonce"].as_str().unwrap_or("")).expect("a nonce");
            println!("code on this phone: {}   (the notch must show the same; approve there)", wire::pairing_code(&fingerprint, &nonce, &server_nonce));
            c.send("pair_decision", json!({"approved": true}));
            let d = c.wait("pair_decision");
            if d["approved"] != true {
                println!("the PC said no");
                std::process::exit(1);
            }
            let res = c.wait("pair_result");
            caps = caps_of(&res);
            let token = res["token"].as_str().expect("a token").to_string();
            std::fs::write(FILE, json!({"token": token, "fingerprint": hex(&fingerprint)}).to_string()).expect("keep the token");
            println!("paired; token kept in {FILE}");
        }
    }

    println!("the PC offers: {caps:?}");
    if std::env::args().any(|a| a == "--forget-on-pc") {
        c.send("forget", json!({}));
        let r = c.wait("forget_result");
        println!("forget_result: {r}");
        if r["approved"] == true {
            let _ = std::fs::remove_file(FILE);
        }
        return;
    }
    let delta = caps.iter().any(|x| x == "artwork_delta_v1");

    // play made-up songs: a snapshot every second, commands from the PC
    let (mut song, mut pos, mut playing, mut seq) = (0usize, 0u64, true, 1u64);
    // the song whose cover the PC has on this connection
    let mut sent_cover: Option<usize> = None;
    let mut last = Instant::now();
    let covers: Vec<String> = (0..SONGS.len()).map(cover).collect();
    println!("playing {:?}; Ctrl+C to stop", SONGS[song].0);
    loop {
        if let Some(m) = c.recv() {
            match m["type"].as_str().unwrap_or("") {
                "ping" => c.send("pong", json!({"id": m["id"]})),
                "command" => {
                    let action = m["action"].as_str().unwrap_or("");
                    println!("command: {action}");
                    match action {
                        "play" => playing = true,
                        "pause" => playing = false,
                        "next" => (song, pos) = ((song + 1) % SONGS.len(), 0),
                        "previous" => (song, pos) = ((song + SONGS.len() - 1) % SONGS.len(), 0),
                        "seek" => pos = m["position_ms"].as_u64().unwrap_or(0).min(SONGS[song].3),
                        _ => {}
                    }
                    c.send("ack", json!({"id": m["id"], "accepted": true}));
                }
                _ => {}
            }
        }
        if last.elapsed() >= Duration::from_secs(1) {
            if playing {
                pos += last.elapsed().as_millis() as u64;
                if pos >= SONGS[song].3 {
                    (song, pos) = ((song + 1) % SONGS.len(), 0);
                }
            }
            last = Instant::now();
            let (title, artist, album, dur) = SONGS[song];
            let art = if delta && sent_cover == Some(song) {
                json!({"artwork_same": true})
            } else {
                sent_cover = Some(song);
                json!({"artwork_mime": "image/jpeg", "artwork_base64": covers[song]})
            };
            let mut snap = json!({"seq": seq, "active": true, "source": "com.example.fakeplayer", "title": title, "artist": artist, "album": album,
                    "position_ms": pos, "duration_ms": dur, "playing": playing, "speed": 1, "actions": ["play", "pause", "next", "previous", "seek"]});
            snap.as_object_mut().unwrap().extend(art.as_object().unwrap().clone());
            c.send("snapshot", snap);
            seq += 1;
        }
    }
}
