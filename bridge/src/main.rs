//! Hesewake Media Bridge, the extension: the notch starts this program (docs/extensions.md in the notch repository) and it
//! connects back over the notch's pipe. It listens for the phone, shows what the phone plays as a Windows media session,
//! and keeps one slide in the notch for pairing and for the paired phones.

use hesewake_media_bridge::advert::Advert;
use hesewake_media_bridge::identity::Identity;
use hesewake_media_bridge::media::{Playing, State};
use hesewake_media_bridge::notch::{FromNotch, Link};
use hesewake_media_bridge::server::{Event, Server, Timing};
use hesewake_media_bridge::smtc::{self, Control, Publisher, View};
use hesewake_media_bridge::ui::{Do, Ui};
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
    ui.set_devices(server.devices());
    let (mut visible, mut last_title) = (false, None::<(String, String)>);
    // how long a snapshot takes to get here, from the pings; the song's position is moved on by it
    let mut one_way = Duration::ZERO;
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
                ui.back();
                page(&mut ui);
            }
            Msg::Notch(FromNotch::Action(id)) => {
                match ui.action(&id) {
                    Some(Do::OpenPairing) => server.open_pairing(),
                    Some(Do::ClosePairing) => server.close_pairing(),
                    Some(Do::Decide(yes)) => server.decide(yes),
                    Some(Do::Forget(id)) => {
                        server.forget(&id);
                        ui.set_devices(server.devices());
                    }
                    None => {}
                }
                ui.set_pairing_left(server.pairing_left());
                page(&mut ui);
                status(&ui);
            }
            Msg::Notch(FromNotch::Config) => {
                toast("Media Bridge", "Pair phones from the Phone slide");
            }
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
                ui.set_connected(Some(id));
                toast(&format!("{name} connected"), "Media Bridge");
                page(&mut ui);
                status(&ui);
            }
            Msg::Server(Event::Disconnected { .. }) => {
                // a phone that signs in again is already the connected one by now
                if server.connected().is_none() {
                    one_way = Duration::ZERO;
                    ui.set_connected(None);
                    ui.set_playing(None);
                    last_title = None;
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
                    last_title = None;
                    page(&mut ui);
                    status(&ui);
                }
                State::Media(p) => {
                    if let Some(pb) = &publisher {
                        pb.show(view_of(&p, now));
                    }
                    let now_playing = (p.title.clone(), p.artist.clone());
                    // the notch's page and status change with the song, not with every second of it
                    if last_title.as_ref() != Some(&now_playing) {
                        ui.set_playing(Some(now_playing.clone()));
                        last_title = Some(now_playing);
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
