//! Hesewake Media Bridge, the extension: the notch starts this program (docs/extensions.md in the notch repository) and it
//! connects back over the notch's pipe. It listens for the phone, shows what the phone plays as a Windows media session,
//! and keeps one slide in the notch for pairing and for the paired phones.

use hesewake_media_bridge::advert::Advert;
use hesewake_media_bridge::invite;
use hesewake_media_bridge::log;
use hesewake_media_bridge::identity::Identity;
use hesewake_media_bridge::media::{Playing, State};
use hesewake_media_bridge::notch::{FromNotch, Link};
use hesewake_media_bridge::server::{Event, Server, Timing};
use hesewake_media_bridge::smtc::{self, Control, Publisher, View};
use hesewake_media_bridge::thumb::Thumbs;
use hesewake_media_bridge::ui::{Do, Hero, Ui};
use hesewake_media_bridge::wire::{b64std_decode, Action};
use serde_json::json;
use std::sync::mpsc::{channel, Sender};
use std::time::{Duration, Instant};

const ID: &str = "media-bridge";
const NAME: &str = "Media Bridge";

enum Msg {
    Notch(FromNotch),
    Server(Event),
    Control(Control),
    Tick,
}

fn forward<T: Send + 'static>(rx: std::sync::mpsc::Receiver<T>, tx: Sender<Msg>, wrap: fn(T) -> Msg) {
    std::thread::spawn(move || {
        while let Ok(x) = rx.recv() {
            if tx.send(wrap(x)).is_err() {
                break;
            }
        }
    });
}

/// What Windows is shown for a song at `now`.
fn view_of(p: &Playing, now: Instant) -> View {
    View {
        title: p.title.clone(),
        artist: p.artist.clone(),
        album: p.album.clone(),
        artwork: p.artwork.as_ref().and_then(|(_, b64)| b64std_decode(b64)),
        position: p.position(now),
        duration: p.duration,
        playing: p.playing,
        speed: p.speed,
        can_play: p.can(Action::Play),
        can_pause: p.can(Action::Pause),
        can_next: p.can(Action::Next),
        can_previous: p.can(Action::Previous),
        can_seek: p.can(Action::Seek),
    }
}

fn data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("APPDATA").unwrap_or_else(|_| ".".into())).join("hesewake-media-bridge")
}

fn main() {
    let pipe = std::env::args().skip_while(|a| a != "--pipe").nth(1).or_else(|| std::env::var("HESEWAKE_NOTCH_PIPE").ok());
    let Some(pipe) = pipe else {
        eprintln!("the notch starts this program and gives it the pipe's name (HESEWAKE_NOTCH_PIPE), or pass --pipe <name>");
        std::process::exit(2);
    };
    let dir = data_dir();
    log::init(dir.join("bridge.log"));
    log::note(&format!("started {}", env!("CARGO_PKG_VERSION")));
    let identity = Identity::load_or_create(&dir).expect("a TLS identity");
    let (ev_tx, ev_rx) = channel();
    let tls = hesewake_media_bridge::tls::server_config(&identity).expect("a TLS configuration");
    let server = Server::start(tls, identity.fingerprint, Some(dir.join("devices.json")), ev_tx, "0.0.0.0:0".parse().unwrap(), Timing::default()).expect("a port to listen on");
    let computer = std::env::var("COMPUTERNAME").unwrap_or_default();
    let _advert = Advert::start(&computer, server.port()).map_err(|e| eprintln!("not announced on the network: {e}")).ok();
    let (link, notch_rx) = match Link::connect(&pipe, ID, NAME, env!("CARGO_PKG_VERSION")) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let (ctl_tx, ctl_rx) = channel();
    // the name Windows and the notch show as the source of the song
    smtc::set_app_id(smtc::APP_ID);
    let publisher = Publisher::start(ctl_tx).map_err(|e| eprintln!("no media session: {e}")).ok();

    let (tx, rx) = channel::<Msg>();
    forward(notch_rx, tx.clone(), Msg::Notch);
    forward(ev_rx, tx.clone(), Msg::Server);
    forward(ctl_rx, tx.clone(), Msg::Control);
    let tick = tx.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        if tick.send(Msg::Tick).is_err() {
            break;
        }
    });

    let mut ui = Ui::default();
    ui.set_computer(&computer);
    ui.set_devices(server.devices());
    // the QR code of the invitation: where this PC is, and the fingerprint the phone pins
    let invite_code = |server: &Server| invite::lan_address().and_then(|ip| invite::matrix(&invite::payload(&ip.to_string(), server.port(), &identity.fingerprint)));
    let mut visible = false;
    // how long a snapshot takes to get here, from the pings; the song's position is moved on by it
    let mut one_way = Duration::ZERO;
    // when the phone was last lost (for the "connected" toast)
    let mut gone_at = None::<Instant>;
    // the phone the page considers connected: one that signs in again while it is (a second connection replaces the first, and
    // its Connected comes before the old one's Disconnected) is not a new arrival
    let mut live = None::<String>;
    // the song's picture for the page, made once per picture; and what the page last said about the song
    let mut thumbs = Thumbs::default();
    let mut last_hero = None::<(String, String, bool, Option<String>)>;
    let page = |ui: &mut Ui| link.send(&ui.page());
    let status = |ui: &Ui| link.send(&json!({"t": "status", "text": ui.status()}));
    let toast = |title: &str, sub: &str| link.send(&json!({"t": "toast", "title": title, "sub": sub}));
    page(&mut ui);
    status(&ui);

    while let Ok(msg) = rx.recv() {
        let now = Instant::now();
        match msg {
            Msg::Notch(FromNotch::Quit) => break,
            Msg::Notch(FromNotch::Visible(on)) => {
                visible = on;
                if on {
                    link.send(&json!({"t": "badge", "on": false}));
                    ui.set_pairing_left(server.pairing_left());
                    page(&mut ui);
                }
            }
            Msg::Notch(FromNotch::Row(id)) => {
                if ui.row(&id) {
                    page(&mut ui);
                }
            }
            Msg::Notch(FromNotch::Back) => {
                // the invitation page is the one thing that is not left by going back to the list, so Back ends the pairing
                if ui.pairing_open() && !ui.has_request() {
                    server.close_pairing();
                    ui.set_pairing_left(None);
                }
                ui.back();
                page(&mut ui);
            }
            Msg::Notch(FromNotch::Action(id)) => {
                match ui.action(&id) {
                    Some(Do::OpenPairing) => {
                        server.open_pairing();
                        ui.set_pairing_left(server.pairing_left());
                        ui.set_invite(invite_code(&server));
                    }
                    Some(Do::ClosePairing) => server.close_pairing(),
                    Some(Do::Decide(yes)) => server.decide(yes),
                    Some(Do::Forget(id)) => {
                        server.forget(&id);
                        ui.set_devices(server.devices());
                    }
                    Some(Do::Rename(id, name)) => {
                        server.rename(&id, &name);
                        ui.set_devices(server.devices());
                    }
                    None => {}
                }
                ui.set_pairing_left(server.pairing_left());
                page(&mut ui);
                status(&ui);
            }
            Msg::Notch(FromNotch::Input(id, value)) => {
                // a name typed in the box of a phone's page: the page comes back with the name it was given
                if let Some(Do::Rename(phone, name)) = ui.input(&id, &value) {
                    server.rename(&phone, &name);
                    ui.set_devices(server.devices());
                    page(&mut ui);
                    status(&ui);
                }
            }
            // its settings are in the panel of the Settings window, not in a window of its own
            Msg::Notch(_) => {}
            Msg::Server(Event::PairRequest { code }) => {
                ui.set_request(Some(code.clone()));
                toast("Pairing request", &format!("Code {code}"));
                link.send(&json!({"t": "badge", "on": true}));
                page(&mut ui);
                status(&ui);
            }
            Msg::Server(Event::PairEnded { paired }) => {
                ui.set_request(None);
                ui.set_devices(server.devices());
                ui.set_pairing_left(server.pairing_left());
                if paired {
                    toast("Phone paired", "It can connect from now on");
                }
                page(&mut ui);
                status(&ui);
            }
            Msg::Server(Event::Connected { id, name }) => {
                ui.set_devices(server.devices());
                let again = live.as_deref() == Some(id.as_str());
                live = Some(id.clone());
                ui.set_connected(Some(id));
                // a phone that comes back within a minute and a half is the same stay, not news: no toast every time
                if !again && gone_at.is_none_or(|t| t.elapsed() > Duration::from_secs(90)) {
                    toast(&format!("{name} connected"), "Media Bridge");
                }
                page(&mut ui);
                status(&ui);
            }
            Msg::Server(Event::Forgotten { name, .. }) => {
                // the phone took itself off the list; the Disconnected that follows clears what it was playing
                ui.set_devices(server.devices());
                toast(&format!("{name} removed itself"), "Media Bridge");
                page(&mut ui);
                status(&ui);
            }
            Msg::Server(Event::Disconnected { why, .. }) => {
                gone_at = Some(Instant::now());
                log::note(&format!("page told: disconnected ({why})"));
                // a phone that signs in again is already the connected one by now
                if server.connected().is_none() {
                    live = None;
                    one_way = Duration::ZERO;
                    ui.set_connected(None);
                    ui.set_playing(None);
                    ui.set_hero(None);
                    thumbs.clear();
                    last_hero = None;
                    if let Some(p) = &publisher {
                        p.hide();
                    }
                }
                page(&mut ui);
                status(&ui);
            }
            Msg::Server(Event::Latency(d)) => one_way = d,
            Msg::Server(Event::Snapshot(s)) => match State::from_snapshot(&s, now, one_way) {
                State::Idle => {
                    if let Some(p) = &publisher {
                        p.hide();
                    }
                    ui.set_playing(None);
                    ui.set_hero(None);
                    thumbs.clear();
                    last_hero = None;
                    page(&mut ui);
                    status(&ui);
                }
                State::Media(p) => {
                    if let Some(pb) = &publisher {
                        pb.show(view_of(&p, now));
                    }
                    let now_playing = (p.title.clone(), p.artist.clone());
                    let image = thumbs.get(s.artwork_base64.as_deref());
                    let sig = (p.title.clone(), p.artist.clone(), p.playing, image.clone());
                    // the notch's page and status change with the song, with play and pause and with the picture that
                    // arrives for it, not with every second of it
                    if last_hero.as_ref() != Some(&sig) {
                        let sub = [p.artist.as_str(), p.album.as_str()].iter().filter(|x| !x.is_empty()).copied().collect::<Vec<_>>().join(" \u{b7} ");
                        ui.set_hero(Some(Hero { title: p.title.clone(), sub, image, playing: p.playing }));
                        last_hero = Some(sig);
                        ui.set_playing(Some(now_playing));
                        page(&mut ui);
                        status(&ui);
                    }
                }
            },
            Msg::Server(Event::Ack { .. }) => {}
            Msg::Control(c) => {
                let (action, pos) = match c {
                    Control::Play => (Action::Play, None),
                    Control::Pause => (Action::Pause, None),
                    Control::Next => (Action::Next, None),
                    Control::Previous => (Action::Previous, None),
                    Control::Seek(d) => (Action::Seek, Some(d.as_millis() as u64)),
                };
                let _ = server.send_command(action, pos);
            }
            Msg::Tick => {
                let left = server.pairing_left();
                let changed = left.is_some() != ui.pairing_open() || (left.is_some() && visible);
                ui.set_pairing_left(left);
                if changed && !ui.has_request() {
                    if visible {
                        page(&mut ui);
                    }
                    status(&ui);
                }
            }
        }
    }
    link.send(&json!({"t": "bye"}));
    // give the bye a moment to reach the pipe before the process ends
    std::thread::sleep(Duration::from_millis(200));
}
