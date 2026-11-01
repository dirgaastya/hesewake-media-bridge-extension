//! What the notch is told to show for the "Phone" slide: the paired phones, the pairing request with its code and the
//! Approve / Reject buttons, and a page per phone. State in, JSON pages out; nothing here touches the notch or the server,
//! so the pages and what a click means are tested directly.

use crate::pairing::Device;
use serde_json::{json, Value};
use std::time::Duration;

/// What a click asks the program to do.
#[derive(Debug, PartialEq)]
pub enum Do {
    OpenPairing,
    ClosePairing,
    Decide(bool),
    Forget(String),
    /// A new name for a phone: its id and what the user typed.
    Rename(String, String),
}

#[derive(Clone, Debug, PartialEq)]
enum View {
    Home,
    Device(String),
}

/// The song for the card at the top of the page: its words, the picture (a small JPEG as standard base64) and whether it plays.
#[derive(Clone, Debug, PartialEq)]
pub struct Hero {
    pub title: String,
    pub sub: String,
    pub image: Option<String>,
    pub playing: bool,
}

pub struct Ui {
    rev: u64,
    hero: Option<Hero>,
    devices: Vec<Device>,
    connected: Option<String>,
    playing: Option<(String, String)>,
    pairing_left: Option<Duration>,
    /// The six digits of the pairing that waits for the user's decision.
    request: Option<String>,
    /// This PC's name, and the QR code of the invitation (side and rows of `0` and `1`) while pairing is open.
    computer: String,
    invite: Option<(usize, Vec<String>)>,
    view: View,
}

fn short(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `2026-11-01` from unix seconds.
fn date(unix: u64) -> String {
    let z = (unix / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

impl Default for Ui {
    fn default() -> Ui {
        Ui { rev: 0, hero: None, devices: Vec::new(), connected: None, playing: None, pairing_left: None, request: None, computer: String::new(), invite: None, view: View::Home }
    }
}

impl Ui {
    pub fn set_devices(&mut self, d: Vec<Device>) {
        self.devices = d;
        if let View::Device(id) = &self.view {
            if !self.devices.iter().any(|x| &x.id == id) {
                self.view = View::Home;
            }
        }
    }

    pub fn set_connected(&mut self, id: Option<String>) {
        self.connected = id;
    }

    pub fn set_playing(&mut self, now: Option<(String, String)>) {
        self.playing = now;
    }

    pub fn set_hero(&mut self, hero: Option<Hero>) {
        self.hero = hero;
    }

    /// How long "Pair a phone" has left. When it has run out the code goes too.
    pub fn set_pairing_left(&mut self, left: Option<Duration>) {
        self.pairing_left = left;
        if left.is_none() {
            self.invite = None;
        }
    }

    pub fn set_computer(&mut self, name: &str) {
        self.computer = short(name, 40);
    }

    pub fn set_invite(&mut self, invite: Option<(usize, Vec<String>)>) {
        self.invite = invite;
    }

    pub fn set_request(&mut self, code: Option<String>) {
        self.request = code;
    }

    pub fn has_request(&self) -> bool {
        self.request.is_some()
    }

    pub fn pairing_open(&self) -> bool {
        self.pairing_left.is_some()
    }

    fn next_rev(&mut self) -> u64 {
        self.rev += 1;
        self.rev
    }

    fn name_of(&self, id: &str) -> String {
        self.devices.iter().find(|d| d.id == id).map_or_else(|| "Phone".to_string(), |d| d.name.clone())
    }

    /// The text for the Settings page's status line.
    pub fn status(&self) -> String {
        let who = self.connected.as_deref().map(|id| self.name_of(id));
        match (&self.request, who, &self.playing) {
            (Some(_), _, _) => "Pairing request".to_string(),
            (None, Some(w), Some((t, _))) => short(&format!("{w}: {t}"), 60),
            (None, Some(w), None) => short(&format!("{w} connected"), 60),
            (None, None, _) if self.pairing_left.is_some() => "Waiting for a phone".to_string(),
            (None, None, _) if self.devices.is_empty() => "No phone paired".to_string(),
            _ => "Waiting for a paired phone".to_string(),
        }
    }

    /// The page to show now: a pairing request comes first, then the phone that was clicked, then the list.
    pub fn page(&mut self) -> Value {
        let rev = self.next_rev();
        if let Some(code) = &self.request {
            return json!({
                "t": "detail", "rev": rev, "for": "pair", "title": "Pair this phone?",
                "meta": [{"k": "Code", "v": code}],
                "body": "Check that your phone shows the same six digits, then approve here and on the phone.",
                "buttons": [{"id": "approve", "label": "Approve"}, {"id": "reject", "label": "Reject"}],
                "foot": "Both sides must approve"
            });
        }
        if let Some(left) = self.pairing_left {
            let mut page = json!({
                "t": "detail", "rev": rev, "for": "pairing", "title": "Pair a phone",
                "meta": [{"k": "PC", "v": if self.computer.is_empty() { "this PC" } else { self.computer.as_str() }}, {"k": "Time left", "v": format!("{}:{:02}", left.as_secs() / 60, left.as_secs() % 60)}],
                "body": "On your phone, open Hesewake Media Bridge and pick this PC, or scan the code.",
                "buttons": [{"id": "pair_cancel", "label": "Cancel"}],
                "foot": "Phones can only pair while this page is open"
            });
            if let Some((size, rows)) = &self.invite {
                page["matrix"] = json!({"size": size, "rows": rows});
            }
            return page;
        }
        if let View::Device(id) = self.view.clone() {
            if let Some(d) = self.devices.iter().find(|d| d.id == id) {
                let on = self.connected.as_deref() == Some(id.as_str());
                return json!({
                    "t": "detail", "rev": rev, "for": id, "title": short(&d.name, 60),
                    "meta": [{"k": "Status", "v": if on { "Connected" } else { "Offline" }}, {"k": "Paired", "v": date(d.added)}],
                    "body": "Forget this phone to take away its access. It has to pair again to come back.",
                    "input": {"id": "rename", "label": "Name", "value": d.name, "max": crate::pairing::MAX_NAME, "button": "Save"},
                    "buttons": [{"id": format!("forget:{id}"), "label": "Forget"}]
                });
            }
        }
        let rows: Vec<Value> = self
            .devices
            .iter()
            .map(|d| {
                let on = self.connected.as_deref() == Some(d.id.as_str());
                json!({"id": d.id, "text": short(&d.name, 60), "sub": if on { "Connected" } else { "Offline" }, "tone": if on { "good" } else { "muted" }})
            })
            .collect();
        let action = json!({"id": "pair", "label": "Pair a phone"});
        let mut page = json!({"t": "slide", "rev": rev, "title": "Pho|ne", "actions": [action], "rows": rows});
        // the song that plays, as a card with its picture at the top
        if let (Some(h), Some(on)) = (&self.hero, self.connected.as_deref()) {
            let phone = self.name_of(on);
            let mut hero = json!({"title": short(&h.title, 80), "sub": short(&h.sub, 80), "text": format!("{phone} \u{b7} {}", if h.playing { "Playing" } else { "Paused" })});
            if let Some(img) = &h.image {
                hero["image"] = json!({"mime": "image/jpeg", "data": img});
            }
            page["hero"] = hero;
        }
        if self.devices.is_empty() {
            page["state"] = json!("empty");
            page["message"] = json!("No phone is paired yet. Press Pair a phone.");
        }
        page
    }

    /// A row was clicked: a phone opens its page.
    pub fn row(&mut self, id: &str) -> bool {
        if self.devices.iter().any(|d| d.id == id) {
            self.view = View::Device(id.to_string());
            return true;
        }
        false
    }

    pub fn back(&mut self) {
        self.view = View::Home;
    }

    /// The user confirmed the text of a page's text box. Only the phone's name can be typed.
    pub fn input(&mut self, id: &str, value: &str) -> Option<Do> {
        match (&self.view, id) {
            (View::Device(dev), "rename") => Some(Do::Rename(dev.clone(), value.to_string())),
            _ => None,
        }
    }

    /// A button was clicked.
    pub fn action(&mut self, id: &str) -> Option<Do> {
        match id {
            "pair" => Some(Do::OpenPairing),
            "pair_cancel" => Some(Do::ClosePairing),
            "approve" if self.request.is_some() => Some(Do::Decide(true)),
            "reject" if self.request.is_some() => Some(Do::Decide(false)),
            f if f.starts_with("forget:") && self.devices.iter().any(|d| d.id == f[7..]) => {
                self.view = View::Home;
                Some(Do::Forget(f[7..].to_string()))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pairing::Devices;

    fn phones(n: u8) -> Vec<Device> {
        let mut d = Devices::default();
        for i in 0..n {
            d.add(&[i + 1; 32], 1_793_491_200 + i as u64 * 86_400);
        }
        d.list().to_vec()
    }

    #[test]
    fn dates() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(1_793_491_200), "2026-11-01");
        assert_eq!(date(951_782_400), "2000-02-29");
        assert_eq!(date(1_709_164_800 + 86_399), "2024-02-29");
    }

    #[test]
    fn an_empty_home_says_what_to_do() {
        let mut ui = Ui::default();
        let p = ui.page();
        assert_eq!((p["t"].as_str(), p["state"].as_str(), p["actions"][0]["id"].as_str()), (Some("slide"), Some("empty"), Some("pair")));
        assert!(p["message"].as_str().unwrap().contains("Pair a phone"));
        assert_eq!(ui.status(), "No phone paired");
        // while pairing is open the page is the invitation, with a QR code when there is one
        ui.set_computer("THINKPAD");
        ui.set_pairing_left(Some(Duration::from_secs(95)));
        let p = ui.page();
        assert_eq!((p["t"].as_str(), p["for"].as_str(), p["buttons"][0]["id"].as_str()), (Some("detail"), Some("pairing"), Some("pair_cancel")));
        assert_eq!((p["meta"][0]["v"].as_str(), p["meta"][1]["v"].as_str()), (Some("THINKPAD"), Some("1:35")));
        assert!(p.get("matrix").is_none(), "no network address, no code");
        ui.set_invite(Some((2, vec!["10".into(), "01".into()])));
        let p = ui.page();
        assert_eq!((p["matrix"]["size"].as_u64(), p["matrix"]["rows"][1].as_str()), (Some(2), Some("01")));
        assert_eq!(ui.status(), "Waiting for a phone");
        // when the time is out the code goes with it
        ui.set_pairing_left(None);
        assert_eq!(ui.page()["t"], "slide");
        ui.set_pairing_left(Some(Duration::from_secs(5)));
        assert!(ui.page().get("matrix").is_none(), "the old code is not shown again");
    }

    #[test]
    fn the_list_shows_who_is_connected_and_what_plays() {
        let mut ui = Ui::default();
        let d = phones(2);
        let (a, b) = (d[0].id.clone(), d[1].id.clone());
        ui.set_devices(d);
        ui.set_connected(Some(b.clone()));
        ui.set_playing(Some(("A Song".into(), "An Artist".into())));
        ui.set_hero(Some(Hero { title: "A Song".into(), sub: "An Artist".into(), image: Some("AAAA".into()), playing: true }));
        let p = ui.page();
        assert!(p.get("state").is_none(), "rows, not an empty page");
        let rows = p["rows"].as_array().unwrap();
        assert_eq!((rows[0]["id"].as_str(), rows[0]["sub"].as_str()), (Some(a.as_str()), Some("Offline")));
        assert_eq!((rows[1]["text"].as_str(), rows[1]["sub"].as_str(), rows[1]["tone"].as_str()), (Some("Phone 2"), Some("Connected"), Some("good")));
        assert_eq!(rows[0]["tone"], "muted", "offline is quiet");
        // the card at the top: the song, which phone, and the picture
        assert_eq!((p["hero"]["title"].as_str(), p["hero"]["sub"].as_str(), p["hero"]["text"].as_str()), (Some("A Song"), Some("An Artist"), Some("Phone 2 \u{b7} Playing")));
        assert_eq!((p["hero"]["image"]["mime"].as_str(), p["hero"]["image"]["data"].as_str()), (Some("image/jpeg"), Some("AAAA")));
        ui.set_hero(Some(Hero { title: "A Song".into(), sub: "An Artist".into(), image: None, playing: false }));
        let p = ui.page();
        assert_eq!(p["hero"]["text"], "Phone 2 \u{b7} Paused");
        assert!(p["hero"].get("image").is_none(), "no picture, no image");
        ui.set_hero(None);
        assert!(ui.page().get("hero").is_none());
        ui.set_hero(Some(Hero { title: "A Song".into(), sub: String::new(), image: None, playing: true }));
        assert_eq!(ui.status(), "Phone 2: A Song");
        ui.set_playing(None);
        assert_eq!(ui.status(), "Phone 2 connected");
        ui.set_connected(None);
        assert_eq!(ui.status(), "Waiting for a paired phone");
    }

    #[test]
    fn a_pairing_request_takes_over_until_it_is_decided() {
        let mut ui = Ui::default();
        ui.set_devices(phones(1));
        ui.set_request(Some("667512".into()));
        let p = ui.page();
        assert_eq!((p["t"].as_str(), p["for"].as_str(), p["meta"][0]["v"].as_str()), (Some("detail"), Some("pair"), Some("667512")));
        assert_eq!(p["buttons"][0]["id"], "approve");
        assert_eq!(ui.status(), "Pairing request");
        assert_eq!(ui.action("approve"), Some(Do::Decide(true)));
        assert_eq!(ui.action("reject"), Some(Do::Decide(false)));
        ui.set_request(None);
        assert_eq!(ui.action("approve"), None, "nothing to approve any more");
        assert_eq!(ui.page()["t"], "slide");
    }

    #[test]
    fn a_phone_has_a_page_and_can_be_forgotten() {
        let mut ui = Ui::default();
        let d = phones(2);
        let id = d[0].id.clone();
        ui.set_devices(d);
        assert!(ui.row(&id));
        assert!(!ui.row("not-a-phone"));
        let p = ui.page();
        assert_eq!((p["t"].as_str(), p["title"].as_str(), p["meta"][1]["v"].as_str()), (Some("detail"), Some("Phone 1"), Some("2026-11-01")));
        let forget = p["buttons"][0]["id"].as_str().unwrap().to_string();
        assert_eq!(ui.action("forget:nobody"), None, "only a phone that is paired");
        assert_eq!(ui.action(&forget), Some(Do::Forget(id.clone())));
        assert_eq!(ui.page()["t"], "slide", "back at the list");
        ui.row(&id);
        ui.back();
        assert_eq!(ui.page()["t"], "slide");
        // a phone that disappears while its page is open
        ui.row(&id);
        ui.set_devices(phones(0));
        assert_eq!(ui.page()["t"], "slide");
    }

    #[test]
    fn a_phone_is_renamed_from_its_page() {
        let mut ui = Ui::default();
        let d = phones(2);
        let id = d[1].id.clone();
        ui.set_devices(d);
        assert_eq!(ui.input("rename", "Pixel 8"), None, "only on a phone's page");
        assert!(ui.row(&id));
        let p = ui.page();
        let inp = &p["input"];
        assert_eq!((inp["id"].as_str(), inp["value"].as_str(), inp["max"].as_u64(), inp["button"].as_str()), (Some("rename"), Some("Phone 2"), Some(32), Some("Save")));
        assert_eq!(ui.input("rename", "Pixel 8"), Some(Do::Rename(id.clone(), "Pixel 8".into())));
        assert_eq!(ui.input("something-else", "x"), None);
        // the name it was given shows in the page and the list
        let mut dv = Devices::default();
        dv.add(&[1; 32], 1_793_491_200);
        dv.add(&[2; 32], 1_793_577_600);
        assert_eq!(dv.rename(&id, "Pixel 8"), Some("Pixel 8".to_string()));
        ui.set_devices(dv.list().to_vec());
        assert_eq!(ui.page()["title"], "Pixel 8");
        assert_eq!(ui.page()["input"]["value"], "Pixel 8");
    }

    #[test]
    fn pairing_buttons_and_revisions() {
        let mut ui = Ui::default();
        assert_eq!(ui.action("pair"), Some(Do::OpenPairing));
        assert_eq!(ui.action("pair_cancel"), Some(Do::ClosePairing));
        assert_eq!(ui.action("something-else"), None);
        let (a, b) = (ui.page()["rev"].as_u64().unwrap(), ui.page()["rev"].as_u64().unwrap());
        assert!(b > a, "every page is newer than the one before");
    }

    #[test]
    fn long_names_are_cut_for_the_notch() {
        let mut ui = Ui::default();
        let mut d = phones(1);
        ui.set_devices(d.drain(..).collect());
        ui.set_connected(Some(ui.devices[0].id.clone()));
        ui.set_playing(Some(("T".repeat(200), "A".into())));
        assert!(ui.status().chars().count() <= 60);
        ui.set_hero(Some(Hero { title: "T".repeat(200), sub: "S".repeat(200), image: None, playing: true }));
        let p = ui.page();
        assert!(p["hero"]["title"].as_str().unwrap().chars().count() <= 80 && p["hero"]["sub"].as_str().unwrap().chars().count() <= 80, "the card's lines are cut");
    }
}
