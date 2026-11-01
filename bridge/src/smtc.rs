//! The phone's media as a Windows media session (SMTC). Windows, the notch's Music slide and its lyrics read it like any
//! other player; the buttons they press come back as `Control`s, to be sent to the phone. The audio is not here.
//!
//! The session belongs to a window, so one thread makes a hidden one and runs its messages; the rest of the program talks
//! to that thread through a channel.

use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::core::{factory, w, HSTRING};
use windows::Foundation::TypedEventHandler;
use windows::Media::{
    MediaPlaybackStatus, MediaPlaybackType, PlaybackPositionChangeRequestedEventArgs, SystemMediaTransportControls, SystemMediaTransportControlsButton,
    SystemMediaTransportControlsButtonPressedEventArgs, SystemMediaTransportControlsTimelineProperties,
};
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream, RandomAccessStreamReference};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::ISystemMediaTransportControlsInterop;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, PeekMessageW, RegisterClassW, TranslateMessage, CW_USEDEFAULT, MSG, PM_REMOVE, WINDOW_EX_STYLE,
    WNDCLASSW, WS_POPUP,
};

/// The name this program's media session goes by. Windows and the notch show it as the source of the song, so it is
/// what the user reads ("Media from phone"), not the name of the exe. Set it before the session is made.
pub const APP_ID: &str = "Media from phone";

pub fn set_app_id(id: &str) {
    unsafe {
        let _ = windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(id));
    }
}

/// A button pressed in Windows (or in the notch), for the phone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Control {
    Play,
    Pause,
    Next,
    Previous,
    Seek(Duration),
}

/// How far the position Windows would work out for itself may be from the phone's before it is told again.
const SLACK: Duration = Duration::from_millis(200);

/// What Windows was last told about the timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct Told {
    at: Instant,
    position: Duration,
    playing: bool,
    speed: f64,
    duration: Option<Duration>,
    can_seek: bool,
}

impl Told {
    fn of(v: &View, at: Instant) -> Told {
        Told { at, position: v.position, playing: v.playing, speed: v.speed, duration: v.duration, can_seek: v.can_seek }
    }

    /// Where Windows puts the song now on its own: a playing song moves on at its speed.
    fn expected(&self, now: Instant) -> Duration {
        let moved = if self.playing { now.saturating_duration_since(self.at).mul_f64(self.speed.max(0.0)) } else { Duration::ZERO };
        let pos = self.position + moved;
        self.duration.map_or(pos, |d| pos.min(d))
    }
}

/// Must Windows be told the timeline again? A playing song moves on by itself, so a snapshot that agrees with that
/// (within `SLACK`) is left out: the small second-to-second differences of the network would only make the position, and
/// the lyrics that follow it, jump about. A change of state, speed, length or the right to seek always goes through.
pub fn timeline_needs_update(told: Option<&Told>, v: &View, now: Instant) -> bool {
    let Some(t) = told else { return true };
    t.playing != v.playing || t.speed != v.speed || t.duration != v.duration || t.can_seek != v.can_seek || t.expected(now).abs_diff(v.position.min(v.duration.unwrap_or(v.position))) > SLACK
}

/// What Windows is shown.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// The picture's bytes (PNG or JPEG), already decoded.
    pub artwork: Option<Vec<u8>>,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub playing: bool,
    pub speed: f64,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_next: bool,
    pub can_previous: bool,
    pub can_seek: bool,
}

enum Msg {
    Show(Box<View>),
    Hide,
    Stop,
}

pub struct Publisher {
    tx: Sender<Msg>,
    thread: Option<JoinHandle<()>>,
}

unsafe extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    DefWindowProcW(h, m, w, l)
}

impl Publisher {
    /// Start the thread and the session (hidden until something is shown). Buttons come out of `controls`.
    pub fn start(controls: Sender<Control>) -> Result<Publisher, String> {
        let (tx, rx) = channel::<Msg>();
        let (ready_tx, ready_rx) = channel::<Result<(), String>>();
        let thread = std::thread::spawn(move || run(rx, controls, ready_tx));
        ready_rx.recv().map_err(|_| "the media thread ended".to_string())??;
        Ok(Publisher { tx, thread: Some(thread) })
    }

    pub fn show(&self, v: View) {
        let _ = self.tx.send(Msg::Show(Box::new(v)));
    }

    /// Nothing plays any more: the session goes away.
    pub fn hide(&self) {
        let _ = self.tx.send(Msg::Hide);
    }
}

impl Drop for Publisher {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn err(e: windows::core::Error) -> String {
    format!("{e}")
}

fn run(rx: Receiver<Msg>, controls: Sender<Control>, ready: Sender<Result<(), String>>) {
    let setup = || -> Result<(HWND, SystemMediaTransportControls), String> {
        unsafe {
            let hinst = GetModuleHandleW(None).map_err(err)?;
            let class = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: w!("HesewakeMediaBridgeWindow"), ..Default::default() };
            RegisterClassW(&class);
            let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), w!("HesewakeMediaBridgeWindow"), w!("Hesewake Media Bridge"), WS_POPUP, CW_USEDEFAULT, CW_USEDEFAULT, 1, 1, None, None, hinst, None).map_err(err)?;
            let interop = factory::<SystemMediaTransportControls, ISystemMediaTransportControlsInterop>().map_err(err)?;
            let smtc: SystemMediaTransportControls = interop.GetForWindow(hwnd).map_err(err)?;
            smtc.SetIsEnabled(false).map_err(err)?;
            let tx = Mutex::new(controls.clone());
            smtc.ButtonPressed(&TypedEventHandler::new(move |_, args: &Option<SystemMediaTransportControlsButtonPressedEventArgs>| {
                let c = match args.as_ref().map(|a| a.Button()).transpose()? {
                    Some(SystemMediaTransportControlsButton::Play) => Control::Play,
                    Some(SystemMediaTransportControlsButton::Pause) => Control::Pause,
                    Some(SystemMediaTransportControlsButton::Next) => Control::Next,
                    Some(SystemMediaTransportControlsButton::Previous) => Control::Previous,
                    _ => return Ok(()),
                };
                if let Ok(tx) = tx.lock() {
                    let _ = tx.send(c);
                }
                Ok(())
            }))
            .map_err(err)?;
            let tx = Mutex::new(controls.clone());
            smtc.PlaybackPositionChangeRequested(&TypedEventHandler::new(move |_, args: &Option<PlaybackPositionChangeRequestedEventArgs>| {
                if let Some(a) = args {
                    if let Ok(tx) = tx.lock() {
                        let _ = tx.send(Control::Seek(a.RequestedPlaybackPosition()?.into()));
                    }
                }
                Ok(())
            }))
            .map_err(err)?;
            Ok((hwnd, smtc))
        }
    };
    let (hwnd, smtc) = match setup() {
        Ok(x) => x,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    let mut shown: Option<(String, String, String, bool)> = None;
    let mut told: Option<Told> = None;
    loop {
        // Windows messages for the window, then what the program asked for
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        match rx.recv_timeout(Duration::from_millis(25)) {
            Ok(Msg::Show(v)) => {
                let _ = apply(&smtc, &v, &mut shown, &mut told);
            }
            Ok(Msg::Hide) => {
                let _ = hide(&smtc);
                (shown, told) = (None, None);
            }
            Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
    let _ = hide(&smtc);
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

fn hide(smtc: &SystemMediaTransportControls) -> windows::core::Result<()> {
    smtc.SetPlaybackStatus(MediaPlaybackStatus::Closed)?;
    smtc.DisplayUpdater()?.ClearAll()?;
    smtc.DisplayUpdater()?.Update()?;
    smtc.SetIsEnabled(false)
}

fn apply(smtc: &SystemMediaTransportControls, v: &View, shown: &mut Option<(String, String, String, bool)>, told: &mut Option<Told>) -> windows::core::Result<()> {
    smtc.SetIsEnabled(true)?;
    smtc.SetIsPlayEnabled(v.can_play)?;
    smtc.SetIsPauseEnabled(v.can_pause)?;
    smtc.SetIsNextEnabled(v.can_next)?;
    smtc.SetIsPreviousEnabled(v.can_previous)?;
    smtc.SetPlaybackStatus(if v.playing { MediaPlaybackStatus::Playing } else { MediaPlaybackStatus::Paused })?;
    smtc.SetPlaybackRate(v.speed)?;
    // the text and the picture only when the song is another one: Windows redraws its controls at every Update
    let key = (v.title.clone(), v.artist.clone(), v.album.clone(), v.artwork.is_some());
    if shown.as_ref() != Some(&key) {
        let du = smtc.DisplayUpdater()?;
        du.SetType(MediaPlaybackType::Music)?;
        let music = du.MusicProperties()?;
        music.SetTitle(&HSTRING::from(v.title.as_str()))?;
        music.SetArtist(&HSTRING::from(v.artist.as_str()))?;
        music.SetAlbumTitle(&HSTRING::from(v.album.as_str()))?;
        match v.artwork.as_deref().filter(|b| !b.is_empty()) {
            Some(bytes) => {
                let stream = InMemoryRandomAccessStream::new()?;
                let writer = DataWriter::CreateDataWriter(&stream)?;
                writer.WriteBytes(bytes)?;
                writer.StoreAsync()?.get()?;
                writer.DetachStream()?;
                stream.Seek(0)?;
                du.SetThumbnail(&RandomAccessStreamReference::CreateFromStream(&stream)?)?;
            }
            None => du.SetThumbnail(None::<&RandomAccessStreamReference>)?,
        }
        du.Update()?;
        *shown = Some(key);
    }
    let now = Instant::now();
    if !timeline_needs_update(told.as_ref(), v, now) {
        return Ok(());
    }
    *told = Some(Told::of(v, now));
    let end = v.duration.unwrap_or(v.position);
    let tl = SystemMediaTransportControlsTimelineProperties::new()?;
    tl.SetStartTime(Duration::ZERO.into())?;
    tl.SetEndTime(end.into())?;
    tl.SetPosition(v.position.min(end).into())?;
    if v.can_seek && v.duration.is_some() {
        tl.SetMinSeekTime(Duration::ZERO.into())?;
        tl.SetMaxSeekTime(end.into())?;
    }
    smtc.UpdateTimelineProperties(&tl)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager;

    #[test]
    fn windows_is_told_the_timeline_only_when_it_matters() {
        let t0 = Instant::now();
        let v = |pos: u64, playing: bool| View { position: Duration::from_secs(pos), ..view("x", playing) };
        assert!(timeline_needs_update(None, &v(42, true), t0), "the first time");
        let told = Told::of(&v(42, true), t0);
        // 10 s later the phone says 52: exactly what Windows works out itself
        assert!(!timeline_needs_update(Some(&told), &View { position: Duration::from_millis(52_100), ..v(0, true) }, t0 + Duration::from_secs(10)), "100 ms off: left alone");
        assert!(timeline_needs_update(Some(&told), &View { position: Duration::from_millis(52_300), ..v(0, true) }, t0 + Duration::from_secs(10)), "300 ms off: told");
        assert!(timeline_needs_update(Some(&told), &v(90, true), t0 + Duration::from_secs(10)), "a seek is told at once");
        assert!(timeline_needs_update(Some(&told), &v(52, false), t0 + Duration::from_secs(10)), "pausing is told");
        let paused = Told::of(&v(42, false), t0);
        assert!(!timeline_needs_update(Some(&paused), &v(42, false), t0 + Duration::from_secs(30)), "a paused song stays where it is");
        assert!(timeline_needs_update(Some(&paused), &v(50, false), t0 + Duration::from_secs(30)), "unless it was moved");
        let mut other = v(42, true);
        other.speed = 1.5;
        assert!(timeline_needs_update(Some(&told), &other, t0), "a change of speed");
        other = v(42, true);
        other.duration = Some(Duration::from_secs(300));
        assert!(timeline_needs_update(Some(&told), &other, t0), "another length");
        // never past the end
        let end = Told::of(&View { duration: Some(Duration::from_secs(60)), ..v(59, true) }, t0);
        assert!(!timeline_needs_update(Some(&end), &View { duration: Some(Duration::from_secs(60)), ..v(60, true) }, t0 + Duration::from_secs(30)), "Windows stops at the end too");
    }

    fn view(title: &str, playing: bool) -> View {
        View {
            title: title.into(),
            artist: "Bridge Test Artist".into(),
            album: "Bridge Test Album".into(),
            artwork: None,
            position: Duration::from_secs(42),
            duration: Some(Duration::from_secs(200)),
            playing,
            speed: 1.0,
            can_play: true,
            can_pause: true,
            can_next: true,
            can_previous: true,
            can_seek: true,
        }
    }

    /// Our session as the rest of Windows sees it: its media properties and timeline, looked up by title.
    fn find(title: &str) -> Option<(String, String, u64, u64, bool)> {
        let mgr = GlobalSystemMediaTransportControlsSessionManager::RequestAsync().ok()?.get().ok()?;
        for s in mgr.GetSessions().ok()? {
            let props = s.TryGetMediaPropertiesAsync().ok()?.get().ok()?;
            if props.Title().ok()?.to_string() == title {
                let tl = s.GetTimelineProperties().ok()?;
                let playing = s.GetPlaybackInfo().ok()?.PlaybackStatus().ok()? == windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing;
                return Some((props.Artist().ok()?.to_string(), props.AlbumTitle().ok()?.to_string(), Duration::from(tl.Position().ok()?).as_secs(), Duration::from(tl.EndTime().ok()?).as_secs(), playing));
            }
        }
        None
    }

    fn wait_for<T>(mut f: impl FnMut() -> Option<T>) -> Option<T> {
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if let Some(x) = f() {
                return Some(x);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        None
    }

    /// Shows a made-up song in the user's Windows media controls for a few seconds, so it is run by hand:
    /// `cargo test --release the_session_is_seen -- --ignored`. It is paused, so it does not take the media keys.
    #[test]
    #[ignore = "puts a made-up session in the user's media controls"]
    fn the_session_is_seen_by_windows_and_its_buttons_come_back() {
        let (ctl_tx, ctl_rx) = channel();
        set_app_id(APP_ID);
        let p = Publisher::start(ctl_tx).expect("a publisher");
        let title = "Hesewake Bridge Test Song";
        p.show(view(title, false));
        let (artist, album, pos, end, playing) = wait_for(|| find(title)).expect("the session is listed");
        let source = GlobalSystemMediaTransportControlsSessionManager::RequestAsync().unwrap().get().unwrap().GetSessions().unwrap().into_iter().find(|s| s.TryGetMediaPropertiesAsync().unwrap().get().unwrap().Title().unwrap().to_string() == title).map(|s| s.SourceAppUserModelId().unwrap().to_string());
        assert_eq!(source.as_deref(), Some(APP_ID), "the source of the song, as the notch reads it");
        assert_eq!((artist.as_str(), album.as_str(), pos, end, playing), ("Bridge Test Artist", "Bridge Test Album", 42, 200, false));
        // another song replaces the text, and the playing state follows
        p.show(view(title, true));
        assert_eq!(wait_for(|| find(title).filter(|s| s.4)).map(|s| s.4), Some(true));
        // ask it to play and to go to 30 s from another API, the way the notch and Windows do
        let mgr = GlobalSystemMediaTransportControlsSessionManager::RequestAsync().unwrap().get().unwrap();
        let s = mgr.GetSessions().unwrap().into_iter().find(|s| s.TryGetMediaPropertiesAsync().unwrap().get().unwrap().Title().unwrap().to_string() == title).unwrap();
        p.show(view(title, false));
        std::thread::sleep(Duration::from_millis(300));
        assert!(s.TryPlayAsync().unwrap().get().unwrap());
        assert_eq!(ctl_rx.recv_timeout(Duration::from_secs(3)), Ok(Control::Play));
        assert!(s.TryChangePlaybackPositionAsync(30 * 10_000_000).unwrap().get().unwrap());
        assert_eq!(ctl_rx.recv_timeout(Duration::from_secs(3)), Ok(Control::Seek(Duration::from_secs(30))));
        // nothing plays any more: the session goes away
        p.hide();
        assert!(wait_for(|| find(title).is_none().then_some(())).is_some(), "the session is gone");
    }
}
