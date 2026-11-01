//! The pipe to the notch (docs/extensions.md in the notch repository). Frames are a little-endian u32 length and JSON.
//! The pipe is opened for overlapped I/O: on a plain handle a read that waits would block every write, and this program
//! has to send (a new page, a toast) while it waits to hear from the notch.

use serde_json::{json, Value};
use std::ffi::c_void;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};
use windows::core::HSTRING;
use windows::Win32::Foundation::{CloseHandle, ERROR_IO_PENDING, ERROR_PIPE_BUSY, GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{CreateFileW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE, OPEN_EXISTING};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::PeekNamedPipe;
use windows::Win32::System::Threading::CreateEventW;

const MAX_FRAME: usize = 64 * 1024;

/// What the notch says to us.
#[derive(Debug, Clone, PartialEq)]
pub enum FromNotch {
    /// The slide came into view (true) or left it.
    Visible(bool),
    Tab(String),
    Row(String),
    Action(String),
    Step(i32),
    Back,
    /// The user pressed Settings for this extension.
    Config,
    /// The user confirmed the text of a page's text box: its id and the text.
    Input(String, String),
    Lang(String),
    /// Exit: the notch is closing or the extension was switched off. Also what a closed pipe turns into.
    Quit,
}

pub fn parse(v: &Value) -> Option<FromNotch> {
    let id = || v.get("id").and_then(Value::as_str).map(str::to_string);
    Some(match v.get("t")?.as_str()? {
        "visible" => FromNotch::Visible(v.get("on").and_then(Value::as_bool)?),
        "tab" => FromNotch::Tab(id()?),
        "row" => FromNotch::Row(id()?),
        "action" => FromNotch::Action(id()?),
        "step" => FromNotch::Step(v.get("dir").and_then(Value::as_i64)?.signum() as i32),
        "back" => FromNotch::Back,
        "config" => FromNotch::Config,
        "input" => FromNotch::Input(id()?, v.get("value")?.as_str()?.to_string()),
        "lang" => FromNotch::Lang(v.get("lang")?.as_str()?.to_string()),
        "quit" => FromNotch::Quit,
        _ => return None,
    })
}

/// One frame for the notch; None if it is too big.
pub fn frame(v: &Value) -> Option<Vec<u8>> {
    let body = v.to_string().into_bytes();
    if body.len() > MAX_FRAME {
        return None;
    }
    let mut out = (body.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(&body);
    Some(out)
}

/// Cuts the bytes read from the pipe into frames.
#[derive(Default)]
pub struct Frames(Vec<u8>);

impl Frames {
    pub fn push(&mut self, data: &[u8]) {
        self.0.extend_from_slice(data);
    }

    /// The next whole frame as JSON; Err if the announced length is impossible.
    pub fn next(&mut self) -> Result<Option<Value>, ()> {
        if self.0.len() < 4 {
            return Ok(None);
        }
        let n = u32::from_le_bytes([self.0[0], self.0[1], self.0[2], self.0[3]]) as usize;
        if n == 0 || n > MAX_FRAME {
            return Err(());
        }
        if self.0.len() < 4 + n {
            return Ok(None);
        }
        let v = serde_json::from_slice(&self.0[4..4 + n]).unwrap_or(Value::Null);
        self.0.drain(..4 + n);
        Ok(Some(v))
    }
}

/// Run one overlapped operation to its end: the byte count, or None if it failed or was cancelled.
fn overlapped(h: HANDLE, start: impl FnOnce(*mut OVERLAPPED) -> windows::core::Result<()>) -> Option<usize> {
    unsafe {
        let ev = CreateEventW(None, true, false, None).ok()?;
        let mut ov = OVERLAPPED { hEvent: ev, ..Default::default() };
        let mut n = 0u32;
        let ok = match start(&mut ov) {
            Ok(()) => GetOverlappedResult(h, &ov, &mut n, true).is_ok(),
            Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => GetOverlappedResult(h, &ov, &mut n, true).is_ok(),
            Err(_) => false,
        };
        let _ = CloseHandle(ev);
        ok.then_some(n as usize)
    }
}

fn read_some(h: HANDLE, buf: &mut [u8]) -> Option<usize> {
    overlapped(h, |ov| unsafe { ReadFile(h, Some(&mut *buf), None, Some(ov)) }).filter(|&n| n > 0)
}

fn write_all(h: HANDLE, bytes: &[u8]) -> bool {
    let mut off = 0;
    while off < bytes.len() {
        match overlapped(h, |ov| unsafe { WriteFile(h, Some(&bytes[off..]), None, Some(ov)) }) {
            Some(n) if n > 0 => off += n,
            _ => return false,
        }
    }
    true
}

fn wait_for_data(h: HANDLE, deadline: Instant) -> bool {
    loop {
        let mut avail = 0u32;
        if unsafe { PeekNamedPipe(h, None, 0, None, Some(&mut avail), None) }.is_err() {
            return false;
        }
        if avail > 0 {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn open(pipe: &str) -> Result<HANDLE, String> {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        match unsafe { CreateFileW(&HSTRING::from(pipe), GENERIC_READ.0 | GENERIC_WRITE.0, FILE_SHARE_NONE, None, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED, None) } {
            Ok(h) => return Ok(h),
            Err(e) if Instant::now() < until && (e.code() == ERROR_PIPE_BUSY.to_hresult() || e.code().0 as u32 == 0x8007_0002) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(format!("could not open the notch's pipe: {e}")),
        }
    }
}

/// The connection to the notch: `send` queues a message, what the notch says comes out of the receiver `connect` returned.
pub struct Link {
    out: Sender<Vec<u8>>,
    handle: isize,
}

impl Link {
    /// Connect, say hello, wait for the welcome. A refusal (`reject`) is an error with the reason.
    pub fn connect(pipe: &str, id: &str, name: &str, version: &str) -> Result<(Link, Receiver<FromNotch>), String> {
        let h = open(pipe)?;
        let hello = frame(&json!({"t": "hello", "proto": 1, "id": id, "name": name, "version": version})).ok_or("hello too big")?;
        if !write_all(h, &hello) {
            unsafe {
                let _ = CloseHandle(h);
            }
            return Err("the notch closed the pipe".into());
        }
        let (mut frames, mut buf) = (Frames::default(), vec![0u8; 8192]);
        let deadline = Instant::now() + Duration::from_secs(5);
        let first = loop {
            match frames.next() {
                Ok(Some(v)) => break v,
                Ok(None) => {}
                Err(_) => return Err("the notch sent a broken frame".into()),
            }
            if !wait_for_data(h, deadline) {
                return Err("the notch did not answer".into());
            }
            match read_some(h, &mut buf) {
                Some(n) => frames.push(&buf[..n]),
                None => return Err("the notch closed the pipe".into()),
            }
        };
        if first["t"] != "welcome" {
            return Err(format!("the notch said no: {}", first["reason"].as_str().unwrap_or("?")));
        }
        let (in_tx, in_rx) = channel::<FromNotch>();
        let (out_tx, out_rx) = channel::<Vec<u8>>();
        let hraw = h.0 as isize;
        std::thread::spawn(move || {
            let h = HANDLE(hraw as *mut c_void);
            while let Ok(bytes) = out_rx.recv() {
                if !write_all(h, &bytes) {
                    break;
                }
            }
        });
        std::thread::spawn(move || {
            let h = HANDLE(hraw as *mut c_void);
            let (mut frames, mut buf) = (frames, vec![0u8; 8192]);
            'read: loop {
                loop {
                    match frames.next() {
                        Ok(Some(v)) => {
                            if let Some(m) = parse(&v) {
                                let quit = m == FromNotch::Quit;
                                if in_tx.send(m).is_err() || quit {
                                    break 'read;
                                }
                            }
                        }
                        Ok(None) => break,
                        Err(_) => break 'read,
                    }
                }
                match read_some(h, &mut buf) {
                    Some(n) => frames.push(&buf[..n]),
                    None => break,
                }
            }
            // a closed pipe means the notch is gone: the same as being told to quit
            let _ = in_tx.send(FromNotch::Quit);
        });
        Ok((Link { out: out_tx, handle: hraw }, in_rx))
    }

    pub fn send(&self, v: &Value) {
        if let Some(f) = frame(v) {
            let _ = self.out.send(f);
        }
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        unsafe {
            let h = HANDLE(self.handle as *mut c_void);
            let _ = CancelIoEx(h, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_the_notch_says_is_understood() {
        let p = |j: Value| parse(&j);
        assert_eq!(p(json!({"t": "visible", "on": true})), Some(FromNotch::Visible(true)));
        assert_eq!(p(json!({"t": "tab", "id": "all"})), Some(FromNotch::Tab("all".into())));
        assert_eq!(p(json!({"t": "row", "id": "128"})), Some(FromNotch::Row("128".into())));
        assert_eq!(p(json!({"t": "action", "id": "pair"})), Some(FromNotch::Action("pair".into())));
        assert_eq!(p(json!({"t": "step", "dir": -1})), Some(FromNotch::Step(-1)));
        assert_eq!(p(json!({"t": "back"})), Some(FromNotch::Back));
        assert_eq!(p(json!({"t": "config"})), Some(FromNotch::Config));
        assert_eq!(p(json!({"t": "input", "id": "rename", "value": "Pixel 8"})), Some(FromNotch::Input("rename".into(), "Pixel 8".into())));
        assert_eq!(p(json!({"t": "input", "id": "rename"})), None, "no text, no message");
        assert_eq!(p(json!({"t": "lang", "lang": "id"})), Some(FromNotch::Lang("id".into())));
        assert_eq!(p(json!({"t": "quit"})), Some(FromNotch::Quit));
        assert_eq!(p(json!({"t": "from-the-future"})), None, "newer messages are ignored");
        assert_eq!(p(json!({"t": "row"})), None, "a row without an id");
        assert_eq!(p(json!({"nope": 1})), None);
    }

    #[test]
    fn frames_are_little_endian_and_split_anywhere() {
        let a = frame(&json!({"t": "back"})).unwrap();
        assert_eq!(&a[..4], &(a.len() as u32 - 4).to_le_bytes());
        let two = [a.clone(), frame(&json!({"t": "quit"})).unwrap()].concat();
        let mut f = Frames::default();
        for c in two.chunks(2) {
            f.push(c);
        }
        assert_eq!(f.next(), Ok(Some(json!({"t": "back"}))));
        assert_eq!(f.next(), Ok(Some(json!({"t": "quit"}))));
        assert_eq!(f.next(), Ok(None));
        let mut big = Frames::default();
        big.push(&((MAX_FRAME + 1) as u32).to_le_bytes());
        assert_eq!(big.next(), Err(()));
        assert!(frame(&json!({"x": "a".repeat(MAX_FRAME)})).is_none());
    }
}
