//! Who may pair, and the phones that are paired. No sockets and no clock of its own (the time comes in), so every rule is
//! tested directly.
//!
//! A phone can only start pairing while the user has pressed "Pair a phone" (the pairing mode lasts two minutes), only one
//! pairing is open at a time, and five failures in a row lock pairing for a minute. What is kept of a paired phone is a
//! hash of its token, never the token.

use crate::wire::{b64_decode, b64_encode, same_bytes};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

/// How long "Pair a phone" stays open, and how long each side has to decide.
pub const MODE: Duration = Duration::from_secs(120);
const MAX_FAILS: u32 = 5;
const LOCK: Duration = Duration::from_secs(60);
pub const MAX_DEVICES: usize = 8;
/// The longest name of a phone, in characters.
pub const MAX_NAME: usize = 32;

#[derive(Debug, PartialEq)]
pub enum Refusal {
    /// The user has not asked to pair a phone.
    NotPairing,
    /// Another pairing is open.
    Busy,
    /// Too many failures in a row.
    Locked,
}

#[derive(Default)]
pub struct Gate {
    mode_until: Option<Instant>,
    pending: bool,
    fails: u32,
    locked_until: Option<Instant>,
}

impl Gate {
    /// The user pressed "Pair a phone".
    pub fn open(&mut self, now: Instant) {
        self.mode_until = Some(now + MODE);
    }

    pub fn close(&mut self) {
        self.mode_until = None;
    }

    /// Time left in the pairing mode.
    pub fn mode_left(&self, now: Instant) -> Option<Duration> {
        self.mode_until.and_then(|t| t.checked_duration_since(now)).filter(|d| !d.is_zero())
    }

    /// A phone sent `pair_hello`: may it go on?
    pub fn begin(&mut self, now: Instant) -> Result<(), Refusal> {
        if self.locked_until.is_some_and(|t| now < t) {
            return Err(Refusal::Locked);
        }
        if self.mode_left(now).is_none() {
            return Err(Refusal::NotPairing);
        }
        if self.pending {
            return Err(Refusal::Busy);
        }
        self.pending = true;
        Ok(())
    }

    /// The pairing that `begin` allowed is over. A success closes the mode; failures add up.
    pub fn end(&mut self, approved: bool, now: Instant) {
        self.pending = false;
        if approved {
            self.fails = 0;
            self.mode_until = None;
        } else {
            self.fails += 1;
            if self.fails >= MAX_FAILS {
                self.locked_until = Some(now + LOCK);
                self.fails = 0;
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// Unix seconds.
    pub added: u64,
    hash: [u8; 32],
}

fn hash_of(token: &[u8; 32]) -> [u8; 32] {
    Sha256::digest(token).into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A name as it is kept: control characters dropped, the ends trimmed, at most `MAX_NAME` characters.
fn clean_name(name: &str) -> String {
    let clean: String = name.chars().filter(|c| !c.is_control()).collect::<String>().trim().chars().take(MAX_NAME).collect();
    clean.trim().to_string()
}

/// "Phone 3": the name a phone gets when it did not give one, which the user has not changed.
fn is_numbered(name: &str) -> bool {
    name.strip_prefix("Phone ").is_some_and(|n| n.parse::<u32>().is_ok())
}

#[derive(Clone, Default, Debug, PartialEq)]
pub struct Devices {
    list: Vec<Device>,
}

impl Devices {
    pub fn list(&self) -> &[Device] {
        &self.list
    }

    /// Which paired phone has this token? Every phone is compared, whatever the answer, so the time says nothing.
    pub fn verify(&self, token: &[u8; 32]) -> Option<&Device> {
        let h = hash_of(token);
        let mut found = None;
        for d in &self.list {
            if same_bytes(&d.hash, &h) {
                found = Some(d);
            }
        }
        found
    }

    /// Remember a phone that was just paired. None if there is no room.
    pub fn add(&mut self, token: &[u8; 32], now_unix: u64) -> Option<&Device> {
        self.add_named(token, now_unix, None)
    }

    /// Like `add`, with the name the phone gave itself (cleaned; a number if it gave none or nothing is left of it).
    pub fn add_named(&mut self, token: &[u8; 32], now_unix: u64, name: Option<&str>) -> Option<&Device> {
        if self.list.len() >= MAX_DEVICES {
            return None;
        }
        let hash = hash_of(token);
        let mut id = hex(&hash[..4]);
        while self.list.iter().any(|d| d.id == id) {
            id.push('x');
        }
        let name = name.map(clean_name).filter(|n| !n.is_empty()).unwrap_or_else(|| format!("Phone {}", self.list.iter().filter_map(|d| d.name.strip_prefix("Phone ")?.parse::<u32>().ok()).max().unwrap_or(0) + 1));
        self.list.push(Device { id, name, added: now_unix, hash });
        self.list.last()
    }

    /// A new name for a phone: control characters dropped, the ends trimmed, at most `MAX_NAME` characters. The cleaned name
    /// if it was taken (it is never empty), None if there is no such phone or nothing is left of the name.
    pub fn rename(&mut self, id: &str, name: &str) -> Option<String> {
        let clean = clean_name(name);
        if clean.is_empty() {
            return None;
        }
        let d = self.list.iter_mut().find(|d| d.id == id)?;
        d.name = clean.clone();
        Some(clean)
    }

    /// The name a phone gives itself when it signs in. It is taken only by a phone that still has a numbered name ("Phone 2"),
    /// so a name that was given at pairing or typed in is left alone. The new name if it was taken.
    pub fn adopt_name(&mut self, id: &str, name: &str) -> Option<String> {
        let clean = clean_name(name);
        if clean.is_empty() {
            return None;
        }
        let d = self.list.iter_mut().find(|d| d.id == id)?;
        if !is_numbered(&d.name) || d.name == clean {
            return None;
        }
        d.name = clean.clone();
        Some(clean)
    }

    /// "Forget this phone": its token stops working.
    pub fn forget(&mut self, id: &str) -> bool {
        let before = self.list.len();
        self.list.retain(|d| d.id != id);
        self.list.len() != before
    }

    pub fn serialize(&self) -> String {
        let devices: Vec<Value> = self.list.iter().map(|d| json!({"id": d.id, "name": d.name, "added": d.added, "hash": b64_encode(&d.hash)})).collect();
        json!({"devices": devices}).to_string()
    }

    /// A file that is missing, broken or hand-edited gives the phones that are fine in it (never more than `MAX_DEVICES`).
    pub fn parse(text: &str) -> Devices {
        let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        let mut list: Vec<Device> = Vec::new();
        for d in v.get("devices").and_then(Value::as_array).into_iter().flatten().take(MAX_DEVICES) {
            let hash: Option<[u8; 32]> = d.get("hash").and_then(Value::as_str).and_then(b64_decode).and_then(|b| b.try_into().ok());
            let (Some(hash), Some(id)) = (hash, d.get("id").and_then(Value::as_str).filter(|s| !s.is_empty() && s.len() <= 16 && s.bytes().all(|c| c.is_ascii_alphanumeric()))) else {
                continue;
            };
            if list.iter().any(|x| x.id == id || x.hash == hash) {
                continue;
            }
            let name = d.get("name").and_then(Value::as_str).unwrap_or("Phone").chars().filter(|c| !c.is_control()).take(MAX_NAME).collect();
            list.push(Device { id: id.to_string(), name, added: d.get("added").and_then(Value::as_u64).unwrap_or(0), hash });
        }
        Devices { list }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn later(t: Instant, secs: u64) -> Instant {
        t + Duration::from_secs(secs)
    }

    #[test]
    fn a_phone_can_only_pair_while_the_user_asked_for_it() {
        let t0 = Instant::now();
        let mut g = Gate::default();
        assert_eq!(g.begin(t0), Err(Refusal::NotPairing), "nobody asked");
        g.open(t0);
        assert_eq!(g.mode_left(later(t0, 100)), Some(Duration::from_secs(20)));
        assert_eq!(g.begin(later(t0, 121)), Err(Refusal::NotPairing), "the two minutes are over");
        g.open(later(t0, 130));
        assert_eq!(g.begin(later(t0, 131)), Ok(()));
        assert_eq!(g.begin(later(t0, 132)), Err(Refusal::Busy), "one at a time");
        g.end(true, later(t0, 140));
        assert_eq!(g.begin(later(t0, 141)), Err(Refusal::NotPairing), "a success closes the mode");
    }

    #[test]
    fn a_failure_leaves_the_mode_open_until_five_in_a_row_lock_it() {
        let t0 = Instant::now();
        let mut g = Gate::default();
        for i in 0..4 {
            g.open(t0);
            assert_eq!(g.begin(t0), Ok(()), "try {i}");
            g.end(false, t0);
        }
        g.open(t0);
        assert_eq!(g.begin(t0), Ok(()));
        g.end(false, t0);
        g.open(t0);
        assert_eq!(g.begin(later(t0, 10)), Err(Refusal::Locked), "the fifth failure locks pairing");
        assert_eq!(g.begin(later(t0, 59)), Err(Refusal::Locked));
        assert_eq!(g.begin(later(t0, 61)), Ok(()), "unlocked after a minute (the mode opened earlier is still on)");
        // a success starts the count over
        g.end(true, later(t0, 63));
        g.open(later(t0, 64));
        g.begin(later(t0, 64)).unwrap();
        g.end(false, later(t0, 64));
        g.open(later(t0, 65));
        assert_eq!(g.begin(later(t0, 65)), Ok(()), "one failure after a success does not lock");
    }

    #[test]
    fn closing_the_mode_stops_new_pairings() {
        let t0 = Instant::now();
        let mut g = Gate::default();
        g.open(t0);
        g.close();
        assert_eq!(g.begin(t0), Err(Refusal::NotPairing));
        assert_eq!(g.mode_left(t0), None);
    }

    fn token(n: u8) -> [u8; 32] {
        [n; 32]
    }

    #[test]
    fn a_paired_phone_is_known_by_its_token_and_forgotten_by_its_id() {
        let mut d = Devices::default();
        let id = d.add(&token(1), 1000).unwrap().id.clone();
        d.add(&token(2), 2000).unwrap();
        assert_eq!(d.verify(&token(1)).map(|x| x.id.as_str()), Some(id.as_str()));
        assert_eq!(d.verify(&token(2)).map(|x| x.name.as_str()), Some("Phone 2"));
        assert_eq!(d.verify(&token(3)), None, "a token nobody was given");
        assert_eq!(d.verify(&[0; 32]), None, "and the all-zero token the phone sends when it has none");
        assert!(d.forget(&id));
        assert_eq!(d.verify(&token(1)), None, "forgotten: the token stops working");
        assert!(!d.forget(&id));
        assert_eq!(d.add(&token(9), 3000).unwrap().name, "Phone 3", "a name is not reused while a higher one exists");
    }

    #[test]
    fn a_phone_can_be_renamed_and_the_name_is_cleaned() {
        let mut d = Devices::default();
        let id = d.add(&token(1), 5).unwrap().id.clone();
        d.add(&token(2), 5).unwrap();
        assert_eq!(d.rename(&id, "  Pixel 8  "), Some("Pixel 8".to_string()));
        assert_eq!(d.list()[0].name, "Pixel 8");
        assert_eq!(d.rename(&id, "My\u{7} \nphone"), Some("My phone".to_string()), "control characters go");
        assert_eq!(d.rename(&id, &"x".repeat(80)).unwrap().chars().count(), MAX_NAME);
        assert_eq!(d.rename(&id, "   "), None, "nothing left of it");
        assert_eq!(d.rename(&id, "\u{7}\u{0}"), None);
        assert_eq!(d.list()[0].name.chars().count(), MAX_NAME, "a refused name changes nothing");
        assert_eq!(d.rename("nobody", "Name"), None);
        assert_eq!(d.list()[1].name, "Phone 2", "the other phone is not touched");
        // kept in the file
        let back = Devices::parse(&d.serialize());
        assert_eq!(back.list()[0].name, d.list()[0].name);
        // the token still works
        assert!(d.verify(&token(1)).is_some());
        // a new phone does not take the number of a renamed one wrongly
        assert_eq!(d.add(&token(3), 6).unwrap().name, "Phone 3");
    }

    #[test]
    fn there_is_room_for_eight_phones() {
        let mut d = Devices::default();
        for n in 0..MAX_DEVICES as u8 {
            assert!(d.add(&token(n), 0).is_some());
        }
        assert!(d.add(&token(100), 0).is_none());
        assert_eq!(d.list().len(), MAX_DEVICES);
    }

    #[test]
    fn the_file_holds_hashes_not_tokens() {
        let mut d = Devices::default();
        d.add(&token(7), 55);
        let text = d.serialize();
        assert!(!text.contains(&b64_encode(&token(7))), "the token itself is never written");
        let back = Devices::parse(&text);
        assert_eq!(back, d);
        assert!(back.verify(&token(7)).is_some());
    }

    #[test]
    fn a_broken_or_edited_file_gives_what_is_fine_in_it() {
        assert_eq!(Devices::parse(""), Devices::default());
        assert_eq!(Devices::parse("{ nope"), Devices::default());
        assert_eq!(Devices::parse("[]"), Devices::default());
        let h = b64_encode(&[5u8; 32]);
        let text = format!(
            r#"{{"devices":[{{"id":"ok1","hash":"{h}","name":"Mine\u0007","added":9}},{{"id":"ok1","hash":"{h}"}},{{"id":"","hash":"{h}"}},{{"id":"bad id!","hash":"{h}"}},{{"id":"short","hash":"AAAA"}},{{"hash":"{h}"}},{{"id":"ok2","hash":"{}"}}]}}"#,
            b64_encode(&[6u8; 32])
        );
        let d = Devices::parse(&text);
        assert_eq!(d.list().iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), vec!["ok1", "ok2"], "bad entries and duplicates are skipped");
        assert_eq!(d.list()[0].name, "Mine", "control characters are dropped");
        assert_eq!(d.list()[1].name, "Phone", "a missing name gets a plain one");
        let many: Vec<Value> = (0..20u8).map(|n| json!({"id": format!("d{n}"), "hash": b64_encode(&[n; 32])})).collect();
        assert_eq!(Devices::parse(&json!({"devices": many}).to_string()).list().len(), MAX_DEVICES);
    }
}

#[cfg(test)]
mod name_tests {
    use super::*;

    fn token(n: u8) -> [u8; 32] {
        [n; 32]
    }

    #[test]
    fn a_phone_that_gives_its_name_gets_it_and_one_that_does_not_gets_a_number() {
        let mut d = Devices::default();
        assert_eq!(d.add_named(&token(1), 0, Some("  Pixel\u{7} 9 ")).unwrap().name, "Pixel 9", "cleaned like a name typed in");
        assert_eq!(d.add_named(&token(2), 0, Some("   ")).unwrap().name, "Phone 1", "nothing left of it: a number");
        assert_eq!(d.add_named(&token(3), 0, None).unwrap().name, "Phone 2");
        let long = "x".repeat(100);
        assert_eq!(d.add_named(&token(4), 0, Some(&long)).unwrap().name.chars().count(), MAX_NAME);
    }

    #[test]
    fn the_name_a_phone_signs_in_with_never_replaces_a_name_that_was_chosen() {
        let mut d = Devices::default();
        let old = d.add(&token(1), 0).unwrap().id.clone();
        let named = d.add_named(&token(2), 0, Some("Pixel 9")).unwrap().id.clone();
        assert_eq!(d.adopt_name(&old, "Galaxy A57"), Some("Galaxy A57".to_string()), "a phone paired without a name gets the one it has now");
        assert_eq!(d.adopt_name(&old, "Something else"), None, "but only once: the name it has is its own now");
        assert_eq!(d.adopt_name(&named, "Other"), None, "the name it paired with stays");
        d.rename(&old, "Phone 7");
        assert_eq!(d.adopt_name(&old, "Again"), Some("Again".to_string()), "a name that looks like the numbered one counts as not chosen");
        assert_eq!(d.adopt_name("nobody", "X"), None);
        assert_eq!(d.adopt_name(&named, ""), None);
        d.rename(&named, "My phone");
        assert_eq!(d.adopt_name(&named, "Pixel 9"), None);
    }
}