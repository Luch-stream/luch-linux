//! Which captured pictures go to the encoder. A 144 Hz monitor flips more often than a 60 fps session wants, and a
//! still screen does not flip at all; the receiver needs neither a burst nor a gap.

use std::time::Duration;

/// Lets through at most `fps` pictures a second, on a grid of `1/fps`, the way Sunshine's pacing groups do
/// (src/platform/windows/display_base.cpp:276-339). A picture slightly early for its slot still goes (the flip clock
/// of the monitor and our clock drift); one that comes after a gap starts a new grid instead of a burst to catch up.
pub struct Pacer {
    interval_us: u64,
    next_us: Option<u64>,
}

/// How early a picture may be for its slot: a quarter of the interval.
const EARLY_DIV: u64 = 4;

impl Pacer {
    pub fn new(fps: u32) -> Self {
        Pacer {
            interval_us: 1_000_000 / fps.max(1) as u64,
            next_us: None,
        }
    }

    pub fn interval(&self) -> Duration {
        Duration::from_micros(self.interval_us)
    }

    /// Whether a picture captured at `now_us` is encoded. Each picture let through moves the slot on.
    pub fn admit(&mut self, now_us: u64) -> bool {
        let early = self.interval_us / EARLY_DIV;
        match self.next_us {
            Some(next) if now_us + early < next => false,
            Some(next) if now_us < next + self.interval_us => {
                self.next_us = Some(next + self.interval_us);
                true
            }
            // The first picture, or one after a gap: a new grid from here.
            _ => {
                self.next_us = Some(now_us + self.interval_us);
                true
            }
        }
    }
}

/// When a still screen gets its picture again: at half the session's frame rate (Sunshine's default
/// `minimum_fps_target`, src/video.cpp:2741-2743), so the receiver's decoder and its loss detection keep going and a
/// lost frame is replaced soon.
pub fn repeat_after(fps: u32) -> Duration {
    Duration::from_micros(2_000_000 / fps.max(1) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fast_monitor_is_cut_to_the_session_rate() {
        let mut p = Pacer::new(60);
        // A 144 Hz monitor for one second.
        let admitted = (0..144).filter(|i| p.admit(i * 1_000_000 / 144)).count();
        assert!((59..=61).contains(&admitted), "{admitted}");
    }

    #[test]
    fn the_same_rate_passes_whole_even_with_jitter() {
        let mut p = Pacer::new(60);
        let jitter = [0i64, 2000, -2000, 1500, -1000];
        let admitted = (0..120)
            .filter(|i| {
                let t = (i * 16_667) as i64 + 50_000 + jitter[*i as usize % jitter.len()];
                p.admit(t as u64)
            })
            .count();
        assert_eq!(admitted, 120);
    }

    #[test]
    fn after_a_pause_there_is_no_burst() {
        let mut p = Pacer::new(60);
        assert!(p.admit(0));
        // Nothing for a second, then pictures 1 ms apart: one goes, the next ones wait for their slot.
        assert!(p.admit(1_000_000));
        assert!(!p.admit(1_001_000));
        assert!(!p.admit(1_002_000));
        assert!(p.admit(1_016_000));
    }

    #[test]
    fn a_still_screen_repeats_at_half_rate() {
        assert_eq!(repeat_after(60), Duration::from_micros(33_333));
        assert_eq!(repeat_after(30), Duration::from_micros(66_666));
    }
}
