//! The sender's loop and the two traits it runs on, as in luch-kms: wait for the newest picture, encode it, send the
//! access unit at once. One thread, nothing queued between the stages: a picture is either encoded now or dropped.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::pace::{repeat_after, Pacer};

/// A picture from the screen. On Windows `frame` is a D3D11 texture that stays on the GPU.
pub struct Captured<F> {
    pub frame: F,
    /// When the picture was on the screen (the flip, not our read), in the clock of `Clock`.
    pub captured_us: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CaptureError {
    /// No new picture within the timeout: the screen is still.
    Timeout,
    /// The capture must be opened again: a mode change, a switch to the secure desktop (UAC, lock screen), a
    /// full-screen game taking the output (DXGI_ERROR_ACCESS_LOST and friends).
    Reinit(String),
    Fatal(String),
}

pub trait Capture {
    /// Cheap to clone (a reference-counted texture), so a still screen can be encoded again.
    type Frame: Clone;
    fn size(&self) -> (u32, u32);
    fn next(&mut self, timeout: Duration) -> Result<Captured<Self::Frame>, CaptureError>;
}

/// One encoded access unit, Annex-B, parameter sets inside every IDR (PROTOCOL.md section 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub data: Vec<u8>,
    pub idr: bool,
}

pub trait Encoder {
    type Frame;
    fn name(&self) -> &str;
    /// Encodes one picture and returns its access unit. `idr` forces a keyframe. Hardware encoders set up with one
    /// frame in flight (async depth 1) return the picture's own packet, never an older one.
    fn encode(&mut self, frame: &Self::Frame, idr: bool) -> Result<Packet, String>;
}

/// Microseconds on one monotonic clock: QueryPerformanceCounter on Windows, the same clock DXGI stamps flips with.
pub trait Clock {
    fn now_us(&self) -> u64;
}

/// What the receiver asks for, set by the control channel's thread.
#[derive(Default)]
pub struct Signals {
    pub want_idr: AtomicBool,
    pub stop: AtomicBool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub encoded: u64,
    pub repeated: u64,
    pub skipped: u64,
    pub reopened: u64,
}

/// How long to wait before opening the capture again after a reinit, so a mode change can settle.
const REOPEN_PAUSE: Duration = Duration::from_millis(200);

/// Runs until `signals.stop`, a fatal error, or the capture changing size (the encoder would need a new session).
/// `send` gets each access unit with its capture time and its host latency (capture to encoded) in 0.1 ms.
pub fn pump<C, E>(
    mut open: impl FnMut() -> Result<C, String>,
    enc: &mut E,
    clock: &impl Clock,
    fps: u32,
    signals: &Signals,
    mut send: impl FnMut(&Packet, u64, u16),
) -> Result<Counts, String>
where
    C: Capture,
    E: Encoder<Frame = C::Frame>,
{
    let mut cap = open()?;
    let size = cap.size();
    let mut pacer = Pacer::new(fps);
    let repeat = repeat_after(fps);
    let mut last: Option<C::Frame> = None;
    let mut counts = Counts::default();
    let mut idr = true;
    while !signals.stop.load(Ordering::Relaxed) {
        let next = cap.next(repeat);
        // The control thread may have stopped the session while we waited.
        if signals.stop.load(Ordering::Relaxed) {
            break;
        }
        let (frame, captured_us) = match next {
            Ok(c) => {
                if !pacer.admit(c.captured_us) {
                    counts.skipped += 1;
                    last = Some(c.frame);
                    continue;
                }
                last = Some(c.frame.clone());
                (c.frame, c.captured_us)
            }
            Err(CaptureError::Timeout) => match &last {
                Some(f) => {
                    counts.repeated += 1;
                    (f.clone(), clock.now_us())
                }
                None => continue,
            },
            Err(CaptureError::Reinit(why)) => {
                eprintln!("[luch-win] capture reopens: {why}");
                drop(cap);
                std::thread::sleep(REOPEN_PAUSE);
                cap = open()?;
                if cap.size() != size {
                    return Err(format!(
                        "the screen is now {}x{}, was {}x{}",
                        cap.size().0,
                        cap.size().1,
                        size.0,
                        size.1
                    ));
                }
                counts.reopened += 1;
                last = None;
                idr = true;
                continue;
            }
            Err(CaptureError::Fatal(e)) => return Err(e),
        };
        idr |= signals.want_idr.swap(false, Ordering::Relaxed);
        let pkt = enc.encode(&frame, idr)?;
        idr = false;
        let host = (clock.now_us().saturating_sub(captured_us) / 100).min(u16::MAX as u64) as u16;
        counts.encoded += 1;
        send(&pkt, captured_us, host);
    }
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    /// A clock the fake capture moves forward.
    #[derive(Clone, Default)]
    struct FakeClock(Rc<Cell<u64>>);
    impl Clock for FakeClock {
        fn now_us(&self) -> u64 {
            self.0.get()
        }
    }

    enum Step {
        Frame(u32, u64),
        Timeout,
        Reinit,
        Resize,
        Fatal,
    }

    struct FakeCapture {
        steps: Rc<std::cell::RefCell<VecDeque<Step>>>,
        clock: FakeClock,
        signals: Rc<Signals>,
        size: (u32, u32),
        /// The mode the next open sees.
        mode: Rc<Cell<(u32, u32)>>,
    }

    impl Capture for FakeCapture {
        type Frame = u32;
        fn size(&self) -> (u32, u32) {
            self.size
        }
        fn next(&mut self, timeout: Duration) -> Result<Captured<u32>, CaptureError> {
            let step = self.steps.borrow_mut().pop_front();
            match step {
                Some(Step::Frame(id, at)) => {
                    self.clock.0.set(at + 3000); // 3 ms to encode
                    Ok(Captured {
                        frame: id,
                        captured_us: at,
                    })
                }
                Some(Step::Timeout) => {
                    self.clock
                        .0
                        .set(self.clock.0.get() + timeout.as_micros() as u64);
                    Err(CaptureError::Timeout)
                }
                Some(Step::Reinit) => Err(CaptureError::Reinit("ACCESS_LOST".into())),
                Some(Step::Resize) => {
                    self.mode.set((1280, 720));
                    Err(CaptureError::Reinit("mode change".into()))
                }
                Some(Step::Fatal) => Err(CaptureError::Fatal("device removed".into())),
                None => {
                    self.signals.stop.store(true, Ordering::Relaxed);
                    Err(CaptureError::Timeout)
                }
            }
        }
    }

    struct FakeEncoder;
    impl Encoder for FakeEncoder {
        type Frame = u32;
        fn name(&self) -> &str {
            "fake"
        }
        fn encode(&mut self, frame: &u32, idr: bool) -> Result<Packet, String> {
            Ok(Packet {
                data: frame.to_be_bytes().to_vec(),
                idr,
            })
        }
    }

    /// Frame id, idr, capture time, host latency.
    type Sent = (u32, bool, u64, u16);
    /// After how many packets to do what to the signals.
    type Midway<'a> = Option<(usize, &'a dyn Fn(&Signals))>;

    /// What was sent, and how often the capture was opened.
    fn run(steps: Vec<Step>, mid: Midway) -> (Result<Counts, String>, Vec<Sent>, usize) {
        let clock = FakeClock::default();
        let signals = Rc::new(Signals::default());
        let steps = Rc::new(std::cell::RefCell::new(VecDeque::from(steps)));
        let opened = Rc::new(Cell::new(0usize));
        let (c, s, st, o) = (
            clock.clone(),
            signals.clone(),
            steps.clone(),
            opened.clone(),
        );
        let mode = Rc::new(Cell::new((1920, 1080)));
        let open = move || {
            o.set(o.get() + 1);
            Ok(FakeCapture {
                steps: st.clone(),
                clock: c.clone(),
                signals: s.clone(),
                size: mode.get(),
                mode: mode.clone(),
            })
        };
        let mut sent = Vec::new();
        let r = pump(
            open,
            &mut FakeEncoder,
            &clock,
            60,
            &signals,
            |p, at, host| {
                sent.push((
                    u32::from_be_bytes(p.data[..4].try_into().unwrap()),
                    p.idr,
                    at,
                    host,
                ));
                if let Some((n, f)) = mid {
                    if sent.len() == n {
                        f(&signals);
                    }
                }
            },
        );
        (r, sent, opened.get())
    }

    #[test]
    fn the_first_frame_is_an_idr_and_latency_is_capture_to_encoded() {
        let (r, sent, _) = run(vec![Step::Frame(1, 0), Step::Frame(2, 16_667)], None);
        assert_eq!(r.unwrap().encoded, 2);
        assert_eq!(sent, vec![(1, true, 0, 30), (2, false, 16_667, 30)]);
    }

    #[test]
    fn a_keyframe_request_makes_the_next_frame_an_idr() {
        let steps = (0..4).map(|i| Step::Frame(i, i as u64 * 16_667)).collect();
        let ask = |s: &Signals| s.want_idr.store(true, Ordering::Relaxed);
        let (_, sent, _) = run(steps, Some((2, &ask)));
        let idr: Vec<bool> = sent.iter().map(|s| s.1).collect();
        assert_eq!(idr, vec![true, false, true, false]);
    }

    #[test]
    fn a_still_screen_sends_the_last_picture_again() {
        let (r, sent, _) = run(vec![Step::Frame(7, 0), Step::Timeout, Step::Timeout], None);
        assert_eq!(r.unwrap().repeated, 2);
        assert_eq!(sent.iter().map(|s| s.0).collect::<Vec<_>>(), vec![7, 7, 7]);
        assert!(sent[2].2 > sent[1].2, "a repeat is stamped when it is sent");
    }

    #[test]
    fn nothing_is_sent_before_the_first_picture() {
        let (r, sent, _) = run(vec![Step::Timeout, Step::Timeout], None);
        assert_eq!(r.unwrap().encoded, 0);
        assert!(sent.is_empty());
    }

    #[test]
    fn pictures_above_the_session_rate_are_skipped() {
        // 240 Hz for a quarter second.
        let steps = (0..60).map(|i| Step::Frame(i, i as u64 * 4_167)).collect();
        let (r, sent, _) = run(steps, None);
        let c = r.unwrap();
        assert!((14..=16).contains(&sent.len()), "{}", sent.len());
        assert_eq!(c.skipped as usize + sent.len(), 60);
    }

    #[test]
    fn a_lost_capture_is_opened_again_and_restarts_with_an_idr() {
        let steps = vec![
            Step::Frame(1, 0),
            Step::Frame(2, 16_667),
            Step::Reinit,
            Step::Timeout, // nothing to repeat: the old picture is from the old capture
            Step::Frame(3, 600_000),
        ];
        let (r, sent, opened) = run(steps, None);
        assert_eq!(opened, 2);
        assert_eq!(r.unwrap().reopened, 1);
        assert_eq!(
            sent.iter().map(|s| (s.0, s.1)).collect::<Vec<_>>(),
            vec![(1, true), (2, false), (3, true)]
        );
    }

    #[test]
    fn a_new_screen_size_ends_the_session() {
        let (r, _, opened) = run(vec![Step::Frame(1, 0), Step::Resize], None);
        assert_eq!(r.unwrap_err(), "the screen is now 1280x720, was 1920x1080");
        assert_eq!(opened, 2);
    }

    #[test]
    fn a_fatal_error_ends_the_session() {
        let (r, sent, _) = run(vec![Step::Frame(1, 0), Step::Fatal], None);
        assert_eq!(r.unwrap_err(), "device removed");
        assert_eq!(sent.len(), 1);
    }
}
