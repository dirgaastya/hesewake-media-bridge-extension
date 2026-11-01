//! A small log of what happens to the phone's connection, so that a connection that keeps dropping can be explained. It holds
//! times, names of phones and reasons, never the songs: no titles, artists or pictures. It is kept in the program's own folder
//! and cut to a fresh file when it gets big.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// The log is started over when it is bigger than this.
const MAX_BYTES: u64 = 200_000;
/// A line is cut to this many characters.
const MAX_LINE: usize = 240;

pub struct Log {
    path: PathBuf,
    lock: Mutex<()>,
}

impl Log {
    pub fn new(path: PathBuf) -> Log {
        Log { path, lock: Mutex::new(()) }
    }

    /// Add a line with the time (`HH:MM:SS`, local) in front. Never fails the caller: a log that cannot be written is no log.
    pub fn write(&self, line: &str) {
        self.write_at(&stamp(), line);
    }

    fn write_at(&self, time: &str, line: &str) {
        let Ok(_guard) = self.lock.lock() else { return };
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() > MAX_BYTES) {
            let _ = std::fs::rename(&self.path, self.path.with_extension("old"));
        }
        let clean: String = line.chars().map(|c| if c.is_control() { ' ' } else { c }).take(MAX_LINE).collect();
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = writeln!(f, "{time} {clean}");
        }
    }
}

fn stamp() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:02}:{:02}:{:02}", t.wHour, t.wMinute, t.wSecond)
}

static LOG: OnceLock<Log> = OnceLock::new();

/// Start logging to `path` (once per run).
pub fn init(path: PathBuf) {
    let _ = LOG.set(Log::new(path));
}

/// A line for the log; nothing happens if `init` was not called.
pub fn note(line: &str) {
    if let Some(l) = LOG.get() {
        l.write(line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hesewake-bridge-log-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.join("bridge.log")
    }

    #[test]
    fn lines_are_added_with_the_time_and_cleaned() {
        let p = scratch("lines");
        let log = Log::new(p.clone());
        log.write_at("12:00:01", "phone connected");
        log.write_at("12:00:09", "bad\u{7}\nline");
        log.write_at("12:00:10", &"x".repeat(1000));
        let text = std::fs::read_to_string(&p).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "12:00:01 phone connected");
        assert_eq!(lines[1], "12:00:09 bad  line", "control characters become spaces and never break a line");
        assert_eq!(lines[2].len(), "12:00:10 ".len() + MAX_LINE, "a long line is cut");
        assert_eq!(lines.len(), 3);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn a_big_log_starts_over_and_keeps_the_old_one_aside() {
        let p = scratch("rotate");
        let log = Log::new(p.clone());
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "x".repeat(MAX_BYTES as usize + 1)).unwrap();
        log.write_at("12:00:00", "fresh");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "12:00:00 fresh\n");
        assert_eq!(std::fs::metadata(p.with_extension("old")).unwrap().len(), MAX_BYTES + 1);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn the_time_looks_like_a_time() {
        let s = stamp();
        assert_eq!((s.len(), s.as_bytes()[2], s.as_bytes()[5]), (8, b':', b':'));
    }
}
