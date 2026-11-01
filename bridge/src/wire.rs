//! The wire protocol with the phone (MEDIA_BRIDGE_ANDROID_NOTCH_PLAN.md in the notch repository): frames, messages and
//! the pairing code. Data in, data out, no sockets: everything here is tested without a network.
//! The phone is trusted only after pairing, but every frame is still checked before it is used.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Biggest frame in either direction.
pub const MAX_FRAME: usize = 1_048_576;
/// Longest text field (bytes of UTF-8), artwork excepted.
pub const MAX_TEXT: usize = 4096;
/// Biggest artwork after decoding.
pub const MAX_ARTWORK: usize = 262_144;
const DOMAIN: &[u8] = b"hesewake-media-pair-v1";

// ---------------------------------------------------------------- base64url without padding
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |acc, (i, &b)| acc | (b as u32) << (16 - 8 * i));
        let chars = chunk.len() + 1;
        for i in 0..chars {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// Decodes base64url, with or without padding. None for anything else.
pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim_end_matches('=');
    if s.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let v = ALPHABET.iter().position(|&a| a == c)? as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    // leftover bits must be zero, so one text has one decoding
    (acc == 0).then_some(out)
}

// ---------------------------------------------------------------- the pairing code
/// Six digits both sides show, from the server certificate's fingerprint and both nonces.
pub fn pairing_code(fingerprint: &[u8], client_nonce: &[u8], server_nonce: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(DOMAIN);
    h.update(fingerprint);
    h.update(client_nonce);
    h.update(server_nonce);
    let d = h.finalize();
    format!("{:06}", u32::from_be_bytes([d[0], d[1], d[2], d[3]]) % 1_000_000)
}

pub fn fingerprint(der: &[u8]) -> [u8; 32] {
    Sha256::digest(der).into()
}

/// Same bytes, in time that does not depend on where they differ.
pub fn same_bytes(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---------------------------------------------------------------- frames
/// A frame announced an impossible length: close the connection.
#[derive(Debug, PartialEq)]
pub struct BadLength;

/// Bytes read from the socket, cut into frames (u32 big-endian length, then UTF-8 JSON).
#[derive(Default)]
pub struct FrameBuf {
    buf: Vec<u8>,
}

impl FrameBuf {
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// The next whole payload; `Ok(None)` while one is still arriving. The length is checked before anything is kept.
    pub fn next(&mut self) -> Result<Option<Vec<u8>>, BadLength> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let n = u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
        if n == 0 || n > MAX_FRAME {
            return Err(BadLength);
        }
        if self.buf.len() < 4 + n {
            return Ok(None);
        }
        let payload = self.buf[4..4 + n].to_vec();
        self.buf.drain(..4 + n);
        Ok(Some(payload))
    }
}

/// A frame for the wire, with `v` and `type` added. None if it would be over the limit.
pub fn frame(kind: &str, mut fields: Value) -> Option<Vec<u8>> {
    let obj = fields.as_object_mut()?;
    obj.insert("v".into(), json!(1));
    obj.insert("type".into(), json!(kind));
    let body = Value::Object(std::mem::take(obj)).to_string().into_bytes();
    if body.len() > MAX_FRAME {
        return None;
    }
    let mut out = (body.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(&body);
    Some(out)
}

// ---------------------------------------------------------------- server -> phone
pub fn pair_challenge(server_nonce: &[u8; 32]) -> Vec<u8> {
    frame("pair_challenge", json!({"server_nonce": b64_encode(server_nonce)})).unwrap_or_default()
}

pub fn pair_decision(approved: bool) -> Vec<u8> {
    frame("pair_decision", json!({"approved": approved})).unwrap_or_default()
}

pub fn pair_result(token: &[u8; 32]) -> Vec<u8> {
    frame("pair_result", json!({"approved": true, "token": b64_encode(token)})).unwrap_or_default()
}

pub fn auth_result(approved: bool) -> Vec<u8> {
    frame("auth_result", json!({"approved": approved})).unwrap_or_default()
}

pub fn pong(id: &str) -> Vec<u8> {
    frame("pong", json!({"id": id})).unwrap_or_default()
}

pub fn ping(id: &str) -> Vec<u8> {
    frame("ping", json!({"id": id})).unwrap_or_default()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Play,
    Pause,
    Next,
    Previous,
    Seek,
}

impl Action {
    pub fn name(self) -> &'static str {
        match self {
            Action::Play => "play",
            Action::Pause => "pause",
            Action::Next => "next",
            Action::Previous => "previous",
            Action::Seek => "seek",
        }
    }

    fn from_name(s: &str) -> Option<Action> {
        [Action::Play, Action::Pause, Action::Next, Action::Previous, Action::Seek].into_iter().find(|a| a.name() == s)
    }
}

/// A playback command for the phone; `position_ms` only for `Seek`.
pub fn command(id: &str, action: Action, position_ms: Option<u64>) -> Vec<u8> {
    let mut f = json!({"id": id, "action": action.name()});
    if action == Action::Seek {
        f["position_ms"] = json!(position_ms.unwrap_or(0));
    }
    frame("command", f).unwrap_or_default()
}

// ---------------------------------------------------------------- phone -> server
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub seq: u64,
    pub active: bool,
    /// Android package of the app that plays.
    pub source: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub artwork_mime: Option<String>,
    /// The artwork as it came (base64), already checked to decode to at most `MAX_ARTWORK` bytes.
    pub artwork_base64: Option<String>,
    pub position_ms: u64,
    pub duration_ms: Option<u64>,
    pub playing: bool,
    pub speed: f64,
    pub actions: Vec<Action>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FromPhone {
    PairHello { client_nonce: [u8; 32] },
    PairDecision { approved: bool },
    Auth { token: [u8; 32] },
    Snapshot(Snapshot),
    Ack { id: String, accepted: bool, reason: String },
    Ping { id: String },
    Pong { id: String },
}

/// What was wrong with a frame; all of them end the connection.
#[derive(Debug, PartialEq)]
pub enum WireError {
    NotJson,
    Version,
    Type,
    Field(&'static str),
}

fn text(v: &Value, key: &'static str) -> Result<String, WireError> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(s)) if s.len() <= MAX_TEXT => Ok(s.clone()),
        _ => Err(WireError::Field(key)),
    }
}

fn nonce(v: &Value, key: &'static str) -> Result<[u8; 32], WireError> {
    let bytes = v.get(key).and_then(Value::as_str).and_then(b64_decode).ok_or(WireError::Field(key))?;
    bytes.try_into().map_err(|_| WireError::Field(key))
}

fn parse_snapshot(v: &Value) -> Result<Snapshot, WireError> {
    let uint = |key: &'static str| match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(x) => x.as_u64().map(Some).ok_or(WireError::Field(key)),
    };
    let artwork_base64 = match v.get("artwork_base64") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            // base64 is a third bigger than what it holds: refuse before decoding something huge
            if s.len() > MAX_ARTWORK / 3 * 4 + 8 || b64_decode(s).is_none_or(|b| b.len() > MAX_ARTWORK) {
                return Err(WireError::Field("artwork_base64"));
            }
            Some(s.clone())
        }
        Some(_) => return Err(WireError::Field("artwork_base64")),
    };
    let artwork_mime = match v.get("artwork_mime") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s == "image/png" || s == "image/jpeg" => Some(s.clone()),
        Some(_) => return Err(WireError::Field("artwork_mime")),
    };
    let speed = match v.get("speed") {
        None | Some(Value::Null) => 1.0,
        Some(x) => x.as_f64().filter(|s| (0.0..=4.0).contains(s)).ok_or(WireError::Field("speed"))?,
    };
    let actions = match v.get("actions") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) if a.len() <= 5 => a.iter().map(|x| x.as_str().and_then(Action::from_name).ok_or(WireError::Field("actions"))).collect::<Result<_, _>>()?,
        Some(_) => return Err(WireError::Field("actions")),
    };
    Ok(Snapshot {
        seq: v.get("seq").and_then(Value::as_u64).ok_or(WireError::Field("seq"))?,
        active: v.get("active").and_then(Value::as_bool).ok_or(WireError::Field("active"))?,
        source: text(v, "source")?,
        title: text(v, "title")?,
        artist: text(v, "artist")?,
        album: text(v, "album")?,
        artwork_mime,
        artwork_base64,
        position_ms: uint("position_ms")?.unwrap_or(0),
        duration_ms: uint("duration_ms")?,
        playing: v.get("playing").and_then(Value::as_bool).unwrap_or(false),
        speed,
        actions,
    })
}

/// Check one frame's payload.
pub fn parse(payload: &[u8]) -> Result<FromPhone, WireError> {
    let v: Value = serde_json::from_slice(payload).map_err(|_| WireError::NotJson)?;
    if v.get("v").and_then(Value::as_u64) != Some(1) {
        return Err(WireError::Version);
    }
    let id = || v.get("id").and_then(Value::as_str).filter(|s| !s.is_empty() && s.len() <= 128).map(str::to_string).ok_or(WireError::Field("id"));
    match v.get("type").and_then(Value::as_str).ok_or(WireError::Type)? {
        "pair_hello" => Ok(FromPhone::PairHello { client_nonce: nonce(&v, "client_nonce")? }),
        "pair_decision" => Ok(FromPhone::PairDecision { approved: v.get("approved").and_then(Value::as_bool).ok_or(WireError::Field("approved"))? }),
        "auth" => Ok(FromPhone::Auth { token: nonce(&v, "token")? }),
        "snapshot" => Ok(FromPhone::Snapshot(parse_snapshot(&v)?)),
        "ack" => Ok(FromPhone::Ack {
            id: id()?,
            accepted: v.get("accepted").and_then(Value::as_bool).ok_or(WireError::Field("accepted"))?,
            reason: text(&v, "reason")?,
        }),
        "ping" => Ok(FromPhone::Ping { id: id()? }),
        "pong" => Ok(FromPhone::Pong { id: id()? }),
        _ => Err(WireError::Type),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_roundtrips_and_matches_the_phone() {
        let bytes: Vec<u8> = (0..32).collect();
        // the same text the Python reference makes for bytes 32..64 and 64..96
        assert_eq!(b64_encode(&(32..64).collect::<Vec<u8>>()), "ICEiIyQlJicoKSorLC0uLzAxMjM0NTY3ODk6Ozw9Pj8");
        assert_eq!(b64_encode(&(64..96).collect::<Vec<u8>>()), "QEFCQ0RFRkdISUpLTE1OT1BRUlNUVVZXWFlaW1xdXl8");
        assert_eq!(b64_decode(&b64_encode(&bytes)), Some(bytes));
        for n in 0..40 {
            let data: Vec<u8> = (0..n as u8).map(|i| i.wrapping_mul(37)).collect();
            assert_eq!(b64_decode(&b64_encode(&data)), Some(data), "{n} bytes");
        }
        assert_eq!(b64_decode("AAAA="), b64_decode("AAAA"), "padding is tolerated");
        assert_eq!(b64_decode("A"), None);
        assert_eq!(b64_decode("AA+/"), None, "the standard alphabet is not base64url");
        assert_eq!(b64_decode("AB"), None, "stray bits: one text, one decoding");
        assert_eq!(b64_decode("A$AA"), None);
    }

    #[test]
    fn the_pairing_code_is_what_the_phone_computes() {
        let (fp, c, s): (Vec<u8>, Vec<u8>, Vec<u8>) = ((0..32).collect(), (32..64).collect(), (64..96).collect());
        assert_eq!(pairing_code(&fp, &c, &s), "667512", "vector from the Android project's reference receiver");
        assert_eq!(pairing_code(&[0xab; 32], &[0; 32], &[1; 32]), "203352");
        assert_ne!(pairing_code(&fp, &c, &s), pairing_code(&fp, &s, &c), "the order of the nonces matters");
        assert_eq!(pairing_code(&[0; 32], &[0; 32], &[0; 32]).len(), 6, "always six digits, zeros in front");
    }

    #[test]
    fn frames_match_the_reference_and_split_anywhere() {
        let want = "000000207b2276223a312c2274797065223a2270696e67222c226964223a22616263227d";
        let ours = ping("abc");
        let hex: String = ours.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex.len(), want.len());
        // key order may differ from the Python writer, so compare what is inside
        let v: Value = serde_json::from_slice(&ours[4..]).unwrap();
        assert_eq!(v, json!({"v": 1, "type": "ping", "id": "abc"}));
        assert_eq!(&ours[..4], &[0, 0, 0, ours.len() as u8 - 4], "big-endian length");
        let two = [ping("a"), pong("b")].concat();
        let mut fb = FrameBuf::default();
        for chunk in two.chunks(3) {
            fb.push(chunk);
        }
        assert_eq!(parse(&fb.next().unwrap().unwrap()), Ok(FromPhone::Ping { id: "a".into() }));
        assert_eq!(parse(&fb.next().unwrap().unwrap()), Ok(FromPhone::Pong { id: "b".into() }));
        assert_eq!(fb.next(), Ok(None));
    }

    #[test]
    fn a_wrong_length_ends_the_connection_before_anything_is_kept() {
        for n in [0u32, (MAX_FRAME + 1) as u32, u32::MAX] {
            let mut fb = FrameBuf::default();
            fb.push(&n.to_be_bytes());
            assert_eq!(fb.next(), Err(BadLength), "{n}");
        }
        let mut fb = FrameBuf::default();
        fb.push(&(MAX_FRAME as u32).to_be_bytes());
        assert_eq!(fb.next(), Ok(None), "the biggest allowed frame is waited for");
        assert!(frame("snapshot", json!({"x": "a".repeat(MAX_FRAME)})).is_none(), "and never sent");
    }

    #[test]
    fn pairing_and_auth_messages() {
        let n = [7u8; 32];
        let hello = frame("pair_hello", json!({"client_nonce": b64_encode(&n)})).unwrap();
        assert_eq!(parse(&hello[4..]), Ok(FromPhone::PairHello { client_nonce: n }));
        let hello_short = frame("pair_hello", json!({"client_nonce": b64_encode(&n[..31])})).unwrap();
        assert_eq!(parse(&hello_short[4..]), Err(WireError::Field("client_nonce")), "32 bytes exactly");
        let auth = frame("auth", json!({"token": b64_encode(&n)})).unwrap();
        assert_eq!(parse(&auth[4..]), Ok(FromPhone::Auth { token: n }));
        let d = frame("pair_decision", json!({"approved": true})).unwrap();
        assert_eq!(parse(&d[4..]), Ok(FromPhone::PairDecision { approved: true }));
        assert_eq!(parse(&frame("pair_decision", json!({"approved": "yes"})).unwrap()[4..]), Err(WireError::Field("approved")));
        // what the server sends is readable by a phone
        let c: Value = serde_json::from_slice(&pair_challenge(&n)[4..]).unwrap();
        assert_eq!((c["type"].as_str(), c["v"].as_u64(), c["server_nonce"].as_str()), (Some("pair_challenge"), Some(1), Some(b64_encode(&n).as_str())));
        let r: Value = serde_json::from_slice(&pair_result(&n)[4..]).unwrap();
        assert_eq!((r["approved"].as_bool(), r["token"].as_str()), (Some(true), Some(b64_encode(&n).as_str())));
    }

    fn snap(extra: Value) -> Result<Snapshot, WireError> {
        let mut base = json!({"seq": 1, "active": true, "source": "com.google.android.apps.youtube.music", "title": "A Song", "artist": "An Artist", "album": "An Album",
            "position_ms": 42000, "duration_ms": 200000, "playing": true, "speed": 1, "actions": ["play", "pause", "next", "previous", "seek"]});
        for (k, v) in extra.as_object().unwrap() {
            base[k] = v.clone();
        }
        match parse(frame("snapshot", base).unwrap()[4..].as_ref()) {
            Ok(FromPhone::Snapshot(s)) => Ok(s),
            Ok(other) => panic!("{other:?}"),
            Err(e) => Err(e),
        }
    }

    #[test]
    fn snapshots_are_checked_field_by_field() {
        let s = snap(json!({})).unwrap();
        assert_eq!((s.seq, s.active, s.title.as_str(), s.position_ms, s.duration_ms, s.playing, s.speed), (1, true, "A Song", 42000, Some(200000), true, 1.0));
        assert_eq!(s.actions, vec![Action::Play, Action::Pause, Action::Next, Action::Previous, Action::Seek]);
        let idle = match parse(frame("snapshot", json!({"seq": 9, "active": false})).unwrap()[4..].as_ref()) {
            Ok(FromPhone::Snapshot(s)) => s,
            other => panic!("{other:?}"),
        };
        assert!(!idle.active && idle.title.is_empty() && idle.duration_ms.is_none(), "nothing playing is a snapshot too");
        assert_eq!(snap(json!({"duration_ms": null})).unwrap().duration_ms, None, "the duration may be null");
        for (extra, field) in [
            (json!({"seq": -1}), "seq"),
            (json!({"seq": "1"}), "seq"),
            (json!({"active": 1}), "active"),
            (json!({"position_ms": -5}), "position_ms"),
            (json!({"position_ms": 1.5}), "position_ms"),
            (json!({"title": 5}), "title"),
            (json!({"title": "x".repeat(MAX_TEXT + 1)}), "title"),
            (json!({"speed": 5}), "speed"),
            (json!({"speed": -1}), "speed"),
            (json!({"actions": ["play", "explode"]}), "actions"),
            (json!({"actions": serde_json::to_value(vec!["play"; 6]).unwrap()}), "actions"),
            (json!({"artwork_mime": "image/gif"}), "artwork_mime"),
            (json!({"artwork_base64": 7}), "artwork_base64"),
            (json!({"artwork_base64": "!!!"}), "artwork_base64"),
        ] {
            assert_eq!(snap(extra.clone()), Err(WireError::Field(field)), "{extra}");
        }
        assert!(snap(json!({"title": "x".repeat(MAX_TEXT)})).is_ok(), "the longest allowed title");
        assert!(snap(json!({"future_field": [1, 2, 3]})).is_ok(), "unknown fields are ignored");
    }

    #[test]
    fn artwork_is_limited_before_and_after_decoding() {
        let ok = b64_encode(&vec![1u8; MAX_ARTWORK]);
        assert!(snap(json!({"artwork_mime": "image/png", "artwork_base64": ok})).is_ok());
        let big = b64_encode(&vec![1u8; MAX_ARTWORK + 1]);
        assert_eq!(snap(json!({"artwork_base64": big})), Err(WireError::Field("artwork_base64")));
        let huge = "A".repeat(MAX_ARTWORK * 2);
        assert_eq!(snap(json!({"artwork_base64": huge})), Err(WireError::Field("artwork_base64")), "refused without decoding it");
    }

    #[test]
    fn broken_frames_are_errors_not_panics() {
        assert_eq!(parse(b"nope"), Err(WireError::NotJson));
        assert_eq!(parse(b"[]"), Err(WireError::Version));
        assert_eq!(parse(br#"{"type":"ping","id":"a"}"#), Err(WireError::Version), "no version");
        assert_eq!(parse(br#"{"v":2,"type":"ping","id":"a"}"#), Err(WireError::Version));
        assert_eq!(parse(br#"{"v":1}"#), Err(WireError::Type));
        assert_eq!(parse(br#"{"v":1,"type":"launch_missiles"}"#), Err(WireError::Type));
        assert_eq!(parse(br#"{"v":1,"type":"ping"}"#), Err(WireError::Field("id")));
        assert_eq!(parse(br#"{"v":1,"type":"ack","id":"x"}"#), Err(WireError::Field("accepted")));
        assert_eq!(parse(&[0xff, 0xfe]), Err(WireError::NotJson));
    }

    #[test]
    fn commands_and_ack() {
        let c: Value = serde_json::from_slice(&command("id-1", Action::Seek, Some(30000))[4..]).unwrap();
        assert_eq!(c, json!({"v": 1, "type": "command", "id": "id-1", "action": "seek", "position_ms": 30000}));
        let p: Value = serde_json::from_slice(&command("id-2", Action::Pause, Some(5))[4..]).unwrap();
        assert!(p.get("position_ms").is_none(), "only a seek has a position");
        let a = frame("ack", json!({"id": "id-1", "accepted": false, "reason": "no player"})).unwrap();
        assert_eq!(parse(&a[4..]), Ok(FromPhone::Ack { id: "id-1".into(), accepted: false, reason: "no player".into() }));
    }

    #[test]
    fn comparing_tokens() {
        assert!(same_bytes(&[1, 2, 3], &[1, 2, 3]));
        assert!(!same_bytes(&[1, 2, 3], &[1, 2, 4]));
        assert!(!same_bytes(&[1, 2, 3], &[1, 2]));
        assert!(same_bytes(&[], &[]));
    }
}
