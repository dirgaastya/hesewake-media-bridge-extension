//! What is playing on the phone, as Windows should see it. The phone's `position_ms` is its position at the moment it
//! sent the snapshot; this side keeps the moment it was *received* (its own monotonic clock, never compared with the
//! phone's) and moves the position on from there only while it plays, at its speed, up to the duration.

use crate::wire::{Action, Snapshot};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub struct Playing {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// The phone app that plays, e.g. `com.google.android.apps.youtube.music`.
    pub source: String,
    /// The picture, as the phone sent it (base64) with its type.
    pub artwork: Option<(String, String)>,
    pub duration: Option<Duration>,
    pub playing: bool,
    pub speed: f64,
    pub actions: Vec<Action>,
    anchor_ms: u64,
    anchor: Instant,
}

/// The state after a snapshot: something plays (or is paused), or nothing does.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Idle,
    Media(Box<Playing>),
}

impl State {
    /// `one_way` is how long the snapshot took to get here: the song has gone on by that much since the phone read its
    /// position (only while it plays, at its speed).
    pub fn from_snapshot(s: &Snapshot, received: Instant, one_way: Duration) -> State {
        if !s.active {
            return State::Idle;
        }
        State::Media(Box::new(Playing {
            title: s.title.clone(),
            artist: s.artist.clone(),
            album: s.album.clone(),
            source: s.source.clone(),
            artwork: s.artwork_mime.clone().zip(s.artwork_base64.clone()),
            duration: s.duration_ms.map(Duration::from_millis),
            playing: s.playing,
            speed: s.speed,
            actions: s.actions.clone(),
            anchor_ms: s.position_ms + if s.playing { (one_way.as_secs_f64() * 1000.0 * s.speed) as u64 } else { 0 },
            anchor: received,
        }))
    }
}

impl Playing {
    /// Where the song is at `now`.
    pub fn position(&self, now: Instant) -> Duration {
        let mut ms = self.anchor_ms as f64;
        if self.playing {
            ms += now.saturating_duration_since(self.anchor).as_secs_f64() * 1000.0 * self.speed;
        }
        let pos = Duration::from_millis(ms.max(0.0) as u64);
        self.duration.map_or(pos, |d| pos.min(d))
    }

    pub fn can(&self, a: Action) -> bool {
        self.actions.contains(&a)
    }

    /// The same song with something else changed? (The same text, so Windows need not be told the title again.)
    pub fn same_song(&self, other: &Playing) -> bool {
        (&self.title, &self.artist, &self.album, &self.source, &self.artwork) == (&other.title, &other.artist, &other.album, &other.source, &other.artwork)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Snapshot;

    fn snap(active: bool, position_ms: u64, playing: bool, speed: f64, duration_ms: Option<u64>) -> Snapshot {
        Snapshot {
            seq: 1,
            active,
            source: "com.example.player".into(),
            title: "A Song".into(),
            artist: "An Artist".into(),
            album: "An Album".into(),
            artwork_mime: None,
            artwork_base64: None,
            artwork_same: false,
            position_ms,
            duration_ms,
            playing,
            speed,
            actions: vec![Action::Play, Action::Pause],
        }
    }

    fn media(s: &Snapshot, at: Instant) -> Playing {
        match State::from_snapshot(s, at, Duration::ZERO) {
            State::Media(p) => *p,
            State::Idle => panic!("idle"),
        }
    }

    #[test]
    fn the_position_moves_on_while_it_plays() {
        let t0 = Instant::now();
        let p = media(&snap(true, 42_000, true, 1.0, Some(200_000)), t0);
        assert_eq!(p.position(t0), Duration::from_secs(42));
        assert_eq!(p.position(t0 + Duration::from_secs(10)), Duration::from_secs(52));
    }

    #[test]
    fn it_stands_still_when_paused_and_follows_the_speed() {
        let t0 = Instant::now();
        let paused = media(&snap(true, 42_000, false, 1.0, Some(200_000)), t0);
        assert_eq!(paused.position(t0 + Duration::from_secs(30)), Duration::from_secs(42));
        let fast = media(&snap(true, 10_000, true, 2.0, Some(200_000)), t0);
        assert_eq!(fast.position(t0 + Duration::from_secs(5)), Duration::from_secs(20));
        let slow = media(&snap(true, 10_000, true, 0.5, None), t0);
        assert_eq!(slow.position(t0 + Duration::from_secs(10)), Duration::from_secs(15));
        let stopped = media(&snap(true, 10_000, true, 0.0, None), t0);
        assert_eq!(stopped.position(t0 + Duration::from_secs(10)), Duration::from_secs(10), "speed 0 does not move");
    }

    #[test]
    fn it_never_goes_past_the_end() {
        let t0 = Instant::now();
        let p = media(&snap(true, 199_000, true, 1.0, Some(200_000)), t0);
        assert_eq!(p.position(t0 + Duration::from_secs(60)), Duration::from_secs(200));
        let unknown = media(&snap(true, 199_000, true, 1.0, None), t0);
        assert_eq!(unknown.position(t0 + Duration::from_secs(60)), Duration::from_secs(259), "no duration, no end");
    }

    #[test]
    fn a_clock_that_looks_backwards_does_not_move_the_song_back() {
        let t0 = Instant::now() + Duration::from_secs(10);
        let p = media(&snap(true, 5_000, true, 1.0, None), t0);
        assert_eq!(p.position(t0 - Duration::from_secs(5)), Duration::from_secs(5));
    }

    #[test]
    fn the_time_a_snapshot_took_is_added_while_it_plays() {
        let t0 = Instant::now();
        let s = snap(true, 42_000, true, 1.0, Some(200_000));
        let State::Media(p) = State::from_snapshot(&s, t0, Duration::from_millis(35)) else { panic!() };
        assert_eq!(p.position(t0), Duration::from_millis(42_035));
        let fast = snap(true, 42_000, true, 2.0, None);
        let State::Media(p) = State::from_snapshot(&fast, t0, Duration::from_millis(35)) else { panic!() };
        assert_eq!(p.position(t0), Duration::from_millis(42_070), "at twice the speed it has gone twice as far");
        let paused = snap(true, 42_000, false, 1.0, None);
        let State::Media(p) = State::from_snapshot(&paused, t0, Duration::from_millis(35)) else { panic!() };
        assert_eq!(p.position(t0), Duration::from_secs(42), "a paused song has not moved");
    }

    #[test]
    fn nothing_playing_is_idle() {
        assert_eq!(State::from_snapshot(&snap(false, 0, false, 1.0, None), Instant::now(), Duration::ZERO), State::Idle);
    }

    #[test]
    fn what_the_phone_can_do_is_kept_and_the_same_song_is_recognized() {
        let t0 = Instant::now();
        let a = media(&snap(true, 1000, true, 1.0, None), t0);
        let b = media(&snap(true, 9000, false, 1.0, None), t0);
        assert!(a.can(Action::Pause) && !a.can(Action::Seek));
        assert!(a.same_song(&b), "only the position and the state changed");
        let mut other = snap(true, 0, true, 1.0, None);
        other.title = "Another".into();
        assert!(!a.same_song(&media(&other, t0)));
    }
}
