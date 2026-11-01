//! The gate from MEDIA_BRIDGE_ANDROID_NOTCH_PLAN.md, for an extension (a separate process, not the notch):
//!
//!   1. this process publishes a Windows media session (SMTC) with made-up metadata, state Paused so that it does not
//!      take the media keys from the user's player;
//!   2. a second process (this exe started with `--read`, like the notch is another process) reads the sessions through
//!      GSMTC and must find it, with its title, artist and position;
//!   3. the reader asks it to play, and this process must receive the button press (a command from a Windows control).
//!
//! Nothing here touches the notch, its settings or the user's media. Run: `cargo run --release`.

use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::core::{factory, w, Result, HSTRING};
use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{GlobalSystemMediaTransportControlsSessionManager, GlobalSystemMediaTransportControlsSessionPlaybackStatus};
use windows::Media::{
    MediaPlaybackStatus, MediaPlaybackType, SystemMediaTransportControls, SystemMediaTransportControlsButton, SystemMediaTransportControlsButtonPressedEventArgs,
    SystemMediaTransportControlsTimelineProperties,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::ISystemMediaTransportControlsInterop;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, PeekMessageW, RegisterClassW, TranslateMessage, CW_USEDEFAULT, MSG, PM_REMOVE, WINDOW_EX_STYLE, WNDCLASSW, WS_POPUP,
};

const TITLE: &str = "Hesewake Gate Test Song";
const ARTIST: &str = "Synthetic Artist";

unsafe extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    DefWindowProcW(h, m, w, l)
}

/// A window nobody sees: SMTC wants a window handle to belong to.
fn hidden_window() -> Result<HWND> {
    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let class = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: w!("HesewakeSmtcGate"), ..Default::default() };
        RegisterClassW(&class);
        CreateWindowExW(WINDOW_EX_STYLE(0), w!("HesewakeSmtcGate"), w!("gate"), WS_POPUP, CW_USEDEFAULT, CW_USEDEFAULT, 1, 1, None, None, hinst, None)
    }
}

fn pump(until: Instant, mut done: impl FnMut() -> bool) {
    unsafe {
        let mut msg = MSG::default();
        while Instant::now() < until && !done() {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn publish(hwnd: HWND, presses: Arc<AtomicU32>) -> Result<SystemMediaTransportControls> {
    let interop = factory::<SystemMediaTransportControls, ISystemMediaTransportControlsInterop>()?;
    let smtc: SystemMediaTransportControls = unsafe { interop.GetForWindow(hwnd)? };
    smtc.SetIsEnabled(true)?;
    smtc.SetIsPlayEnabled(true)?;
    smtc.SetIsPauseEnabled(true)?;
    smtc.SetIsNextEnabled(true)?;
    smtc.SetIsPreviousEnabled(true)?;
    smtc.SetPlaybackStatus(MediaPlaybackStatus::Paused)?;
    let du = smtc.DisplayUpdater()?;
    du.SetType(MediaPlaybackType::Music)?;
    let music = du.MusicProperties()?;
    music.SetTitle(&HSTRING::from(TITLE))?;
    music.SetArtist(&HSTRING::from(ARTIST))?;
    du.Update()?;
    let tl = SystemMediaTransportControlsTimelineProperties::new()?;
    tl.SetStartTime((Duration::ZERO).into())?;
    tl.SetEndTime(Duration::from_secs(200).into())?;
    tl.SetPosition(Duration::from_secs(42).into())?;
    smtc.UpdateTimelineProperties(&tl)?;
    smtc.ButtonPressed(&TypedEventHandler::new(move |_, args: &Option<SystemMediaTransportControlsButtonPressedEventArgs>| {
        if let Some(a) = args {
            if a.Button()? == SystemMediaTransportControlsButton::Play {
                presses.fetch_add(1, Ordering::SeqCst);
            }
        }
        Ok(())
    }))?;
    Ok(smtc)
}

/// The second process: list what GSMTC knows, find the gate's session and ask it to play.
fn read() -> Result<()> {
    let mgr = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()?.get()?;
    let sessions = mgr.GetSessions()?;
    println!("sessions seen by the reader: {}", sessions.Size()?);
    let mut found = false;
    for s in sessions {
        let props = s.TryGetMediaPropertiesAsync()?.get()?;
        let title = props.Title()?.to_string();
        let st = s.GetPlaybackInfo()?.PlaybackStatus()?;
        println!("  - {:?} | {} | {:?}", s.SourceAppUserModelId()?.to_string(), title, st == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing);
        if title == TITLE {
            found = true;
            let tl = s.GetTimelineProperties()?;
            println!("FOUND title={:?} artist={:?} position={}s end={}s", title, props.Artist()?.to_string(), Duration::from(tl.Position()?).as_secs(), Duration::from(tl.EndTime()?).as_secs());
            let info = s.GetPlaybackInfo()?;
            let ctl = info.Controls()?;
            println!("controls: play={} pause={} next={} previous={}", ctl.IsPlayEnabled()?, ctl.IsPauseEnabled()?, ctl.IsNextEnabled()?, ctl.IsPreviousEnabled()?);
            println!("TryPlay accepted: {}", s.TryPlayAsync()?.get()?);
        }
    }
    println!("{}", if found { "READER: gate session found" } else { "READER: gate session NOT found" });
    Ok(())
}

fn main() -> Result<()> {
    if std::env::args().any(|a| a == "--read") {
        return read();
    }
    let hwnd = hidden_window()?;
    let presses = Arc::new(AtomicU32::new(0));
    let smtc = publish(hwnd, presses.clone())?;
    println!("published: enabled={} status=Paused title={TITLE:?}", smtc.IsEnabled()?);
    // give Windows a moment to register the session, then read it from another process
    pump(Instant::now() + Duration::from_millis(1500), || false);
    let out = Command::new(std::env::current_exe().unwrap()).arg("--read").output().expect("start the reader");
    println!("{}", String::from_utf8_lossy(&out.stdout).trim_end());
    if !out.stderr.is_empty() {
        println!("reader stderr: {}", String::from_utf8_lossy(&out.stderr));
    }
    pump(Instant::now() + Duration::from_secs(3), || presses.load(Ordering::SeqCst) > 0);
    let n = presses.load(Ordering::SeqCst);
    println!("PUBLISHER: Play button presses received from the other process: {n}");
    println!("{}", if n > 0 { "GATE PASSED" } else { "GATE FAILED" });
    Ok(())
}
