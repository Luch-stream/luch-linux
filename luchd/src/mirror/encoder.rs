//! EncoderSession: the single encoder instance of a group. Owns the VA-API encoder,
//! assigns the group-wide sequence, debounces IDR requests, and publishes EncodedFrames
//! to the group's FrameBus.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::frame::EncodedFrame;
use super::frame_bus::FrameBus;
use super::{F_CONFIG, F_IDR, KIND_AUDIO, KIND_VIDEO};

/// Minimum spacing between forced IDRs: one receiver's keyframe request must not turn
/// the group-wide stream into an IDR-per-frame sequence.
pub const IDR_COOLDOWN: Duration = Duration::from_millis(500);

/// Created EncoderSession instances currently alive. Test hook for the
/// "exactly one encoder per group" guarantee (spec 15.3).
static INSTANCES: AtomicUsize = AtomicUsize::new(0);

pub fn live_instances() -> usize {
    INSTANCES.load(Ordering::SeqCst)
}

/// Shared handle used by the capture callbacks, the KMS forwarding thread and the
/// per-receiver transports: all publishing is &self (atomics + bus), only the lazy
/// encoder open needs the internal mutex.
pub type EncSession = Arc<EncoderSession>;

#[cfg(test)]
pub fn create(bus: FrameBus, size: crate::display::Size) -> EncSession {
    create_with_fps(bus, size, super::fps())
}

/// Encoder for a group with an explicit stream rate: 0 forwards every captured frame,
/// any other value is the target fps used for rate control and reported in the profile.
pub fn create_with_fps(bus: FrameBus, size: crate::display::Size, fps: u32) -> EncSession {
    Arc::new(EncoderSession::new(bus, size, fps))
}

/// Group-wide encoder counters (spec 5.7 "group stats"): video frames published,
/// payload bytes, summed host latency. Per-receiver delivery numbers live in transport.rs.
#[derive(Default)]
pub struct PublishCounters {
    pub frames: AtomicU64,
    pub bytes: AtomicU64,
    pub lat_sum: AtomicU64,
}

pub struct EncoderSession {
    bus: FrameBus,
    enc: std::sync::Mutex<Option<crate::enc::Encoder>>,
    size: crate::display::Size,
    video_seq: AtomicU32,
    audio_seq: AtomicU32,
    /// Latch: a receiver (or a lagging websocket) asked for an IDR.
    want_idr: Arc<AtomicBool>,
    /// Group-level IDR accounting: asks that reached the latch, and asks absorbed by it
    /// (a request while one is already pending, or a request inside the cooldown).
    /// Twenty receivers falling behind at once must still produce one keyframe.
    idr_requests: AtomicU64,
    idr_suppressed: AtomicU64,
    /// Latch: the next audio frame must be preceded by an OpusHead config frame.
    want_head: Arc<AtomicBool>,
    last_idr: std::sync::Mutex<Option<Instant>>,
    cooldown: Duration,
    pub counters: PublishCounters,
    /// Target stream rate, fixed at group creation (0 = uncapped).
    pub fps: u32,
    /// Video publish spacing over the recent window: (last pts, deltas of the last 600).
    pacing: std::sync::Mutex<(Option<u32>, std::collections::VecDeque<u32>)>,
}

impl EncoderSession {
    pub fn new(bus: FrameBus, size: crate::display::Size, fps: u32) -> Self {
        INSTANCES.fetch_add(1, Ordering::SeqCst);
        EncoderSession {
            bus,
            enc: std::sync::Mutex::new(None),
            size,
            fps,
            video_seq: AtomicU32::new(0),
            audio_seq: AtomicU32::new(0),
            want_idr: Arc::new(AtomicBool::new(false)),
            idr_requests: AtomicU64::new(0),
            idr_suppressed: AtomicU64::new(0),
            want_head: Arc::new(AtomicBool::new(true)), // first audio frame carries OpusHead
            last_idr: std::sync::Mutex::new(None),
            cooldown: IDR_COOLDOWN,
            counters: PublishCounters::default(),
            pacing: std::sync::Mutex::new((None, std::collections::VecDeque::with_capacity(600))),
        }
    }

    /// Shared latch: set by keyframe requests and lagging transports. A request that
    /// finds the latch already set is merged into it, not queued again.
    pub fn request_idr(&self) {
        if self.want_idr.swap(true, Ordering::Relaxed) {
            self.idr_suppressed.fetch_add(1, Ordering::Relaxed);
        } else {
            self.idr_requests.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// (requests that created a pending IDR, requests merged into one) for `GroupStats`.
    pub fn idr_counters(&self) -> (u64, u64) {
        (
            self.idr_requests.load(Ordering::Relaxed),
            self.idr_suppressed.load(Ordering::Relaxed),
        )
    }

    /// True while a keyframe was requested and not produced yet: the capture throttle
    /// must not drop the frame that will carry it (a lost IDR/SPS breaks every decoder).
    pub fn idr_pending(&self) -> bool {
        self.want_idr.load(Ordering::Relaxed)
    }

    /// Decides whether a pending IDR request may fire now, applying the group-wide
    /// cooldown. Testable with an explicit clock.
    pub fn idr_due(&self, now: Instant) -> bool {
        if !self.want_idr.swap(false, Ordering::Relaxed) {
            return false;
        }
        let mut last = self.last_idr.lock().unwrap();
        match *last {
            Some(t) if now.duration_since(t) < self.cooldown => {
                // Too soon after the last IDR: keep the request pending for the next frame.
                self.want_idr.store(true, Ordering::Relaxed);
                self.idr_suppressed.fetch_add(1, Ordering::Relaxed);
                false
            }
            _ => {
                *last = Some(now);
                true
            }
        }
    }

    fn next_seq(&self, kind: u8) -> u32 {
        let c = if kind == KIND_VIDEO {
            &self.video_seq
        } else {
            &self.audio_seq
        };
        c.fetch_add(1, Ordering::Relaxed)
    }

    /// Current group-wide sequence counters (next seq per kind), for `GroupStats`.
    pub fn seqs(&self) -> (u32, u32) {
        (
            self.video_seq.load(Ordering::Relaxed),
            self.audio_seq.load(Ordering::Relaxed),
        )
    }

    /// Record one published video frame's pts for the pacing window.
    fn note_pts(&self, pts_us: u32) {
        let mut p = self.pacing.lock().unwrap();
        if let Some(last) = p.0 {
            let d = pts_us.saturating_sub(last);
            p.1.push_back(d);
            if p.1.len() > 600 {
                p.1.pop_front();
            }
        }
        p.0 = Some(pts_us);
    }

    /// Published video spacing in ms — average and worst over the recent window. This is
    /// the objective pacing number: a smooth stream has avg ≈ 1000/fps and a small max.
    pub fn pacing_ms(&self) -> (f64, f64) {
        let p = self.pacing.lock().unwrap();
        let d = &p.1;
        if d.is_empty() {
            return (0.0, 0.0);
        }
        let sum: u64 = d.iter().map(|x| *x as u64).sum();
        let max = *d.iter().max().unwrap();
        (sum as f64 / d.len() as f64 / 1000.0, max as f64 / 1000.0)
    }

    /// Wrap `data` as a frame of `kind` with the group-wide seq and publish it.
    pub fn publish(&self, kind: u8, flags: u8, pts_us: u32, host_lat: u16, data: &[u8]) {
        let f = EncodedFrame::new(
            kind,
            flags,
            self.next_seq(kind),
            pts_us,
            host_lat,
            Arc::from(data),
        );
        if kind == KIND_VIDEO {
            self.note_pts(pts_us);
            self.counters.frames.fetch_add(1, Ordering::Relaxed);
            self.counters
                .bytes
                .fetch_add(data.len() as u64, Ordering::Relaxed);
            self.counters
                .lat_sum
                .fetch_add(host_lat as u64, Ordering::Relaxed);
            if flags & F_IDR != 0 {
                // A receiver may have (re)started its audio decoder: resend the config.
                self.want_head.store(true, Ordering::Relaxed);
            }
        }
        self.bus.publish(&f);
    }

    /// Audio path: prepend OpusHead as an F_CONFIG frame whenever a video IDR (or a new
    /// subscriber) asked for it. Keeps the wire behaviour of the old `need_head` flag.
    pub fn publish_audio(&self, data: &[u8], pts_us: u32, host_lat: u16) {
        if self.want_head.swap(false, Ordering::Relaxed) {
            self.publish(KIND_AUDIO, F_CONFIG, pts_us, 0, &super::opus_head());
        }
        self.publish(KIND_AUDIO, 0, pts_us, host_lat, data);
    }

    /// Encode one captured video frame and publish the result(s). Called on the capture
    /// thread; the encoder is opened lazily on the first frame. `lat_now` is sampled
    /// inside the publish callback — after encoding — so the on-wire host_lat keeps its
    /// capture-to-encoded meaning for the receiver overlay and D-Bus stats.
    pub fn encode_video(
        &self,
        frame: &crate::enc::Nv12Dmabuf,
        pts_us: i64,
        lat_now: impl Fn() -> u16,
    ) -> Result<(), String> {
        {
            let mut enc = self.enc.lock().unwrap();
            if enc.is_none() {
                // `fps = 0` means "every captured frame", but the encoder still needs a real
                // rate: at 1 fps its one-frame VBV becomes a whole second of bits.
                let fps = if self.fps == 0 { super::FPS } else { self.fps };
                *enc = Some(crate::enc::Encoder::open(
                    &render_node(),
                    self.size.width,
                    self.size.height,
                    fps,
                    super::kbps_for(self.size.width, self.size.height),
                )?);
            }
        }
        self.note_pts(pts_us as u32);
        let force = self.idr_due(Instant::now());
        let bus = self.bus.clone();
        let counters = &self.counters;
        let video_seq = &self.video_seq;
        let want_head = &self.want_head;
        self.enc.lock().unwrap().as_mut().unwrap().encode(
            frame,
            pts_us,
            force,
            move |bytes, idr| {
                let flags = if idr { F_IDR } else { 0 };
                let host_lat = lat_now();
                counters.frames.fetch_add(1, Ordering::Relaxed);
                counters
                    .bytes
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                counters
                    .lat_sum
                    .fetch_add(host_lat as u64, Ordering::Relaxed);
                let f = EncodedFrame::new(
                    KIND_VIDEO,
                    flags,
                    video_seq.fetch_add(1, Ordering::Relaxed),
                    pts_us as u32,
                    host_lat,
                    Arc::from(bytes),
                );
                if idr {
                    want_head.store(true, Ordering::Relaxed);
                }
                bus.publish(&f);
            },
        )?;
        Ok(())
    }
}

fn render_node() -> String {
    let mut nodes: Vec<_> = std::fs::read_dir("/dev/dri")
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().to_string_lossy().into_owned())
        .filter(|p| p.contains("renderD"))
        .collect();
    nodes.sort();
    nodes
        .into_iter()
        .next()
        .unwrap_or_else(|| "/dev/dri/renderD128".into())
}

impl Drop for EncoderSession {
    fn drop(&mut self) {
        INSTANCES.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard};
    use std::time::Duration;

    use super::super::frame_bus::FrameBus;
    use super::super::{F_CONFIG, F_IDR, KIND_AUDIO, KIND_VIDEO};
    use super::*;

    /// The instance counter is process-global: serialize the tests that touch it.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn locked() -> MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn session() -> EncoderSession {
        EncoderSession::new(FrameBus::new(), crate::display::Size::default(), 60)
    }

    #[test]
    fn exactly_one_live_instance_per_session() {
        let _g = locked();
        assert_eq!(live_instances(), 0);
        {
            let _s = session();
            assert_eq!(live_instances(), 1);
            let _s2 = session();
            assert_eq!(live_instances(), 2);
        }
        assert_eq!(live_instances(), 0);
    }

    #[test]
    fn sequence_is_group_wide_and_per_kind() {
        let _g = locked();
        let s = session();
        assert_eq!(s.next_seq(KIND_VIDEO), 0);
        assert_eq!(s.next_seq(KIND_VIDEO), 1);
        assert_eq!(s.next_seq(KIND_AUDIO), 0); // audio has its own sequence
        assert_eq!(s.next_seq(KIND_VIDEO), 2);
    }

    #[tokio::test]
    async fn publish_wraps_payload_and_subscribers_see_it() {
        let _g = locked();
        let bus = FrameBus::new();
        let s = EncoderSession::new(bus.clone(), crate::display::Size::default(), 60);
        let mut rx = bus.subscribe_video();
        s.publish(KIND_VIDEO, F_IDR, 5_000, 12, &[9, 8, 7]);
        let f = rx.recv().await.unwrap();
        assert_eq!(f.seq, 0);
        assert!(f.flags & F_IDR != 0);
        assert_eq!(f.pts_us, 5_000);
        assert_eq!(&*f.data, &[9, 8, 7]);
    }

    #[test]
    fn idr_requests_debounce_with_cooldown() {
        let _g = locked();
        let s = session();
        let t0 = Instant::now();
        s.request_idr();
        assert!(s.idr_due(t0)); // first request fires
        assert!(!s.idr_due(t0)); // nothing pending
        s.request_idr();
        s.request_idr(); // repeated requests collapse into one
        assert!(!s.idr_due(t0 + Duration::from_millis(100))); // inside cooldown: deferred
        assert!(s.idr_due(t0 + Duration::from_millis(600))); // after cooldown: fires
        assert!(!s.idr_due(t0 + Duration::from_millis(700)));
    }

    #[test]
    fn twenty_lagging_receivers_share_one_pending_idr() {
        let _g = locked();
        let s = session();
        let t0 = Instant::now();
        for _ in 0..20 {
            s.request_idr();
        }
        let (requests, suppressed) = s.idr_counters();
        assert_eq!(requests, 1, "one request is enough for the whole group");
        assert_eq!(suppressed, 19);
        assert!(s.idr_due(t0));
        assert!(!s.idr_due(t0)); // nothing pending: the group got exactly one keyframe
    }

    #[tokio::test]
    async fn video_idr_schedules_opus_head_before_next_audio() {
        let _g = locked();
        let bus = FrameBus::new();
        let s = EncoderSession::new(bus.clone(), crate::display::Size::default(), 60);
        let mut v = bus.subscribe_video();
        let mut a = bus.subscribe_audio();
        s.publish(KIND_VIDEO, F_IDR, 1_000, 0, &[1]);
        assert!(v.recv().await.unwrap().is_keyframe());
        s.publish_audio(&[2, 2], 2_000, 5);
        let head = a.recv().await.unwrap();
        assert_eq!(head.kind, KIND_AUDIO);
        assert!(head.flags & F_CONFIG != 0);
        assert_eq!(&head.data[..8], b"OpusHead");
        let data = a.recv().await.unwrap();
        assert_eq!(data.flags, 0);
        assert_eq!(&*data.data, &[2, 2]);
    }

    #[test]
    fn pacing_reports_the_recent_video_spacing() {
        let s = EncoderSession::new(FrameBus::new(), crate::display::Size::default(), 60);
        for pts in [0u32, 33_333, 66_666, 99_999] {
            s.publish(KIND_VIDEO, 0, pts, 0, &[0]);
        }
        let (avg, max) = s.pacing_ms();
        assert!((avg - 33.333).abs() < 0.05, "avg {avg}");
        assert!((max - 33.333).abs() < 0.05, "max {max}");
    }
}
