//! The port the server listens on is kept between runs. A phone remembers the address it paired with; a port that changes at every
//! start would make that address wrong each time the program restarts, and the phone would have to find the PC again by name.

use std::path::Path;

const FILE: &str = "port.txt";

/// The port of the last run, or 0 (any free port) if there is none or it is not one that is safe to ask for.
pub fn remembered(dir: &Path) -> u16 {
    std::fs::read_to_string(dir.join(FILE)).ok().and_then(|t| t.trim().parse::<u16>().ok()).filter(|p| *p >= 1024).unwrap_or(0)
}

/// Keep the port for the next run. A failure to write is not worth stopping for: the next run takes any free port.
pub fn remember(dir: &Path, port: u16) {
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(dir.join(FILE), port.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("hesewake-bridge-port-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_port_comes_back_and_a_missing_or_bad_one_means_any() {
        let d = scratch("roundtrip");
        assert_eq!(remembered(&d), 0, "nothing kept yet");
        remember(&d, 50321);
        assert_eq!(remembered(&d), 50321);
        std::fs::write(d.join(FILE), "80").unwrap();
        assert_eq!(remembered(&d), 0, "a low port is not asked for");
        std::fs::write(d.join(FILE), "not a number").unwrap();
        assert_eq!(remembered(&d), 0);
        std::fs::write(d.join(FILE), "70000").unwrap();
        assert_eq!(remembered(&d), 0, "too big for a port");
        let _ = std::fs::remove_dir_all(&d);
    }
}
