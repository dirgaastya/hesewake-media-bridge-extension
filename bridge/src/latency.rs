//! How long a message from the phone takes to get here, so that the song's position is where the phone really is and
//! the lyrics are on time. The phone sends the position it had when it sent the snapshot; by the time it arrives the song
//! has gone on by the one-way travel time. That time is half the round trip of a ping, and the smallest of the recent round
//! trips is the best guess (a slow one only says that something else was using the network at the moment).

use std::collections::VecDeque;
use std::time::Duration;

/// Round trips that count; the rest are a bad moment, not the network.
const MAX_RTT: Duration = Duration::from_secs(2);
/// The one-way time is never taken as more than this: a phone this far away is not in the same room.
const MAX_ONE_WAY: Duration = Duration::from_millis(400);
/// A new estimate is passed on only when it moved this much.
const STEP: Duration = Duration::from_millis(5);
const KEEP: usize = 8;

#[derive(Default)]
pub struct Latency {
    rtts: VecDeque<Duration>,
    reported: Option<Duration>,
}

impl Latency {
    /// The best guess of the one-way time now.
    pub fn one_way(&self) -> Option<Duration> {
        self.rtts.iter().min().map(|r| (*r / 2).min(MAX_ONE_WAY))
    }

    /// A ping came back after `rtt`. The new estimate, if it differs from the last one passed on by `STEP` or more.
    pub fn add(&mut self, rtt: Duration) -> Option<Duration> {
        if rtt > MAX_RTT {
            return None;
        }
        if self.rtts.len() == KEEP {
            self.rtts.pop_front();
        }
        self.rtts.push_back(rtt);
        let now = self.one_way()?;
        let moved = self.reported.map_or(true, |r| now.abs_diff(r) >= STEP);
        moved.then(|| {
            self.reported = Some(now);
            now
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn the_estimate_is_half_the_smallest_recent_round_trip() {
        let mut l = Latency::default();
        assert_eq!(l.one_way(), None);
        assert_eq!(l.add(ms(40)), Some(ms(20)), "the first one is always passed on");
        assert_eq!(l.add(ms(90)), None, "a slow one does not raise it");
        assert_eq!(l.add(ms(30)), Some(ms(15)));
        assert_eq!(l.one_way(), Some(ms(15)));
    }

    #[test]
    fn small_changes_are_not_passed_on() {
        let mut l = Latency::default();
        l.add(ms(40));
        assert_eq!(l.add(ms(38)), None, "1 ms is noise");
        assert_eq!(l.add(ms(30)), Some(ms(15)));
        assert_eq!(l.add(ms(29)), None);
    }

    #[test]
    fn an_old_good_round_trip_is_forgotten_after_eight() {
        let mut l = Latency::default();
        l.add(ms(10));
        for _ in 0..KEEP {
            l.add(ms(60));
        }
        assert_eq!(l.one_way(), Some(ms(30)), "the network is slower now than it was");
    }

    #[test]
    fn nonsense_is_ignored_or_capped() {
        let mut l = Latency::default();
        assert_eq!(l.add(Duration::from_secs(3)), None, "a round trip of seconds says nothing about the network");
        assert_eq!(l.one_way(), None);
        l.add(ms(1800));
        assert_eq!(l.one_way(), Some(ms(400)), "capped");
        let mut z = Latency::default();
        assert_eq!(z.add(Duration::ZERO), Some(Duration::ZERO));
    }
}
