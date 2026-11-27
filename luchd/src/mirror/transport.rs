//! ReceiverTransport: delivers the group's shared EncodedFrames to one receiver.
//! UDP unicast for Android TV receivers, websocket messages for the web receiver.
//! Each receiver gets its own sender task and queue isolation: a slow or dead
//! receiver can only ever stall its own task, never the encoder or other receivers.

use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::{broadcast, mpsc};

use super::encoder::IDR_COOLDOWN;
use super::frame::{seq_newer, EncodedFrame};
use super::{header, BURST, BURST_GAP, HEADER, SHARD};

/// Shards per burst before a short pause, so a big IDR doesn't overflow the Wi-Fi queue.
/// The pause lives in the per-receiver sender task: capture and encoding never sleep.
const BURST_LOCAL: usize = BURST;
const BURST_GAP_LOCAL: Duration = BURST_GAP;

/// Delivery state of one receiver (spec §10): it streams, falls behind, then withholds
/// dependent frames until the group-wide keyframe arrives and streaming resumes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DeliveryState {
    Streaming = 0,
    Lagging = 1,
    AwaitingIdr = 2,
}

impl DeliveryState {
    pub fn as_str(self) -> &'static str {
        match self {
            DeliveryState::Streaming => "streaming",
            DeliveryState::Lagging => "lagging",
            DeliveryState::AwaitingIdr => "awaiting_idr",
        }
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => DeliveryState::Lagging,
            2 => DeliveryState::AwaitingIdr,
            _ => DeliveryState::Streaming,
        }
    }
}

/// Per-receiver delivery numbers (spec 5.7), kept apart from the encoder's own metrics.
#[derive(Default)]
pub struct TransportStats {
    /// Frames actually handed to the wire.
    pub frames: AtomicU64,
    pub bytes: AtomicU64,
    /// Datagrams / messages the wire refused (send error, full websocket channel).
    pub send_errors: AtomicU64,
    /// Frames dropped at send time: the receiver could not take them.
    pub dropped: AtomicU64,
    /// Frames skipped because the receiver was a full queue behind and waiting for an
    /// IDR, plus the frames the broadcast bus already discarded for it (lag).
    pub skipped: AtomicU64,
    /// Group IDRs this receiver asked for (merged and rate-limited by the encoder).
    pub idr_requests: AtomicU64,
    /// Current send rate in bits per second, refreshed by the group's one-second tick.
    pub rate_bps: AtomicU64,
    /// `DeliveryState` as u8.
    pub state: AtomicU8,
    /// Seq of the last video frame actually sent.
    pub last_seq: AtomicU64,
    warned: AtomicBool,
}

impl TransportStats {
    pub fn state(&self) -> DeliveryState {
        DeliveryState::from_u8(self.state.load(Ordering::Relaxed))
    }
}

/// Where one receiver's frames go.
pub enum ReceiverTransport {
    /// Connected sockets pinned to the receiver's interface, DSCP-marked (video, audio).
    Udp { video: UdpSocket, audio: UdpSocket },
    /// Web receiver: whole frames as binary websocket messages. Video and audio have
    /// separate bounded channels: the socket writer is shared, but a burst of audio (200
    /// messages a second) must not take the slot a video frame needs, and the video
    /// queue stays shallow so latency cannot grow behind sound.
    Ws {
        video: mpsc::Sender<Bytes>,
        audio: mpsc::Sender<Bytes>,
    },
}

impl ReceiverTransport {
    /// Sockets actually used by the group. Test builds skip the device pin:
    /// `SO_BINDTODEVICE` needs privileges, and loopback integration tests cannot have them.
    #[cfg(not(test))]
    pub fn udp_for(
        ifname: &str,
        video: std::net::SocketAddrV4,
        audio: std::net::SocketAddrV4,
    ) -> std::io::Result<Self> {
        Self::udp_with(Some(ifname), video, audio)
    }

    #[cfg(test)]
    pub fn udp_for(
        _ifname: &str,
        video: std::net::SocketAddrV4,
        audio: std::net::SocketAddrV4,
    ) -> std::io::Result<Self> {
        Self::udp_with(None, video, audio)
    }

    fn udp_with(
        device: Option<&str>,
        video: std::net::SocketAddrV4,
        audio: std::net::SocketAddrV4,
    ) -> std::io::Result<Self> {
        use socket2::{Domain, Protocol, Socket, Type};
        let open = |to: &std::net::SocketAddrV4, dscp: u32| -> std::io::Result<UdpSocket> {
            let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
            if let Some(ifname) = device {
                s.bind_device(Some(ifname.as_bytes()))?;
            }
            s.set_tos_v4(dscp << 2)?; // CS5 video / CS6 audio -> Wi-Fi WMM queues
            s.set_send_buffer_size(1 << 20)?;
            // Never park a tokio worker thread on a dead receiver: a full send buffer
            // surfaces as WouldBlock, counted as a send error, the frame is dropped.
            s.set_nonblocking(true)?;
            s.connect(&(*to).into())?;
            Ok(s.into())
        };
        Ok(ReceiverTransport::Udp {
            video: open(&video, 40)?,
            audio: open(&audio, 48)?,
        })
    }

    pub fn ws(video: mpsc::Sender<Bytes>, audio: mpsc::Sender<Bytes>) -> Self {
        ReceiverTransport::Ws { video, audio }
    }
}

/// Build the on-wire message(s) for one frame. UDP: one datagram per shard with the shared
/// Luch v1 header. WebSocket: one whole-frame binary message (idx 0, shards 1).
/// Pacing pauses (`BURST_GAP`) sit in the caller's per-receiver task, never on the encode path.
fn shard_packets(f: &EncodedFrame) -> Arc<[Bytes]> {
    f.udp_shards
        .get_or_init(|| {
            let shards = f.data.len().div_ceil(SHARD).max(1);
            let pkts: Vec<Bytes> = f
                .data
                .chunks(SHARD)
                .enumerate()
                .map(|(i, chunk)| {
                    let mut pkt = Vec::with_capacity(HEADER + SHARD);
                    pkt.extend_from_slice(&header(
                        f.kind,
                        f.flags,
                        i as u16,
                        shards as u16,
                        f.seq,
                        f.pts_us,
                        f.data.len() as u32,
                        f.host_lat,
                    ));
                    pkt.extend_from_slice(chunk); // last shard is not padded; receiver uses frame_len
                    Bytes::from(pkt)
                })
                .collect();
            Arc::from(pkts)
        })
        .clone()
}

/// One whole-frame websocket message: header + payload, framing is reliable and ordered.
pub fn ws_message(f: &EncodedFrame) -> Bytes {
    f.ws_wire
        .get_or_init(|| {
            let mut msg = Vec::with_capacity(HEADER + f.data.len());
            msg.extend_from_slice(&header(
                f.kind,
                f.flags,
                0,
                1,
                f.seq,
                f.pts_us,
                f.data.len() as u32,
                f.host_lat,
            ));
            msg.extend_from_slice(&f.data);
            Bytes::from(msg)
        })
        .clone()
}

/// `true` when the whole-frame message was queued. A full channel means the browser
/// lags; the caller enters AwaitingIdr and asks for a group keyframe.
fn ws_try_send(tx: &mpsc::Sender<Bytes>, f: &EncodedFrame) -> bool {
    tx.try_send(ws_message(f)).is_ok()
}

/// Group-wide keyframe request. A lost frame breaks the decoder chain of the whole group,
/// so the IDR must be asked of the real capture: the encoder latch on portal sources
/// (consumed by `idr_due`) and the KMS grabber on `fast` sources, whose frames never pass
/// through `idr_due` at all.
#[derive(Clone)]
pub struct GroupIdr(Arc<dyn Fn() + Send + Sync>);

impl GroupIdr {
    pub fn new(f: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    pub fn request(&self) {
        (self.0)();
    }
}

/// Ask for a keyframe, at most once per encoder cooldown while the receiver keeps waiting.
/// A single request can be lost — the KMS side drops requests inside its own cooldown, and
/// an IDR can be lost on the wire — and with an infinite GOP nothing else produces the
/// next one, so waiting without retrying would freeze that receiver for good.
fn ask_idr(idr: &GroupIdr, last: &mut Option<Instant>, stats: &TransportStats) {
    let now = Instant::now();
    if last.is_some_and(|t| now.duration_since(t) < IDR_COOLDOWN) {
        return;
    }
    *last = Some(now);
    stats.idr_requests.fetch_add(1, Ordering::Relaxed);
    idr.request();
}

/// Mark the receiver as lagging and latch one group IDR request. The first withheld
/// dependent frame then promotes `Lagging` to `AwaitingIdr`.
fn enter_awaiting(stats: &TransportStats, idr: &GroupIdr, last: &mut Option<Instant>) {
    stats
        .state
        .store(DeliveryState::Lagging as u8, Ordering::Relaxed);
    ask_idr(idr, last, stats);
}

/// Per-receiver delivery loop: drains this receiver's subscriptions and sends.
/// A video `Lagged` means we fell a full queue behind: the backlog is already discarded
/// by the bus, dependent frames are withheld until the group's next keyframe, and only
/// that keyframe resumes the stream (spec §10). Audio keeps flowing throughout.
/// Returns when the bus closes (group stopped) or every send path is dead.
pub async fn run_receiver(
    tp: ReceiverTransport,
    mut vrx: broadcast::Receiver<EncodedFrame>,
    mut arx: broadcast::Receiver<EncodedFrame>,
    idr: GroupIdr,
    stats: Arc<TransportStats>,
    start_awaiting: bool,
) {
    let warned = &stats.warned;
    // Awaiting an IDR: withhold video until a keyframe newer than the last sent frame.
    // A receiver joining a running group starts that way: it subscribes mid-GOP and the
    // first frames it would see are P-frames it cannot decode.
    let mut awaiting = start_awaiting;
    if start_awaiting {
        stats
            .state
            .store(DeliveryState::AwaitingIdr as u8, Ordering::Relaxed);
    }
    let mut last_sent: Option<u32> = None;
    let mut last_idr: Option<Instant> = None;
    loop {
        tokio::select! {
            f = vrx.recv() => match f {
                Ok(f) => {
                    if awaiting {
                        let fresh = last_sent.is_none_or(|l| seq_newer(f.seq, l));
                        if !f.is_keyframe() || !fresh {
                            if stats.state() == DeliveryState::Lagging {
                                stats
                                    .state
                                    .store(DeliveryState::AwaitingIdr as u8, Ordering::Relaxed);
                            }
                            stats.skipped.fetch_add(1, Ordering::Relaxed);
                            ask_idr(&idr, &mut last_idr, &stats);
                            continue;
                        }
                        awaiting = false;
                        stats
                            .state
                            .store(DeliveryState::Streaming as u8, Ordering::Relaxed);
                    }
                    let sent = match &tp {
                        ReceiverTransport::Udp { video, .. } => {
                            let mut ok = true;
                            let pkts = shard_packets(&f);
                            for (i, pkt) in pkts.iter().enumerate() {
                                if i > 0 && i % BURST_LOCAL == 0 {
                                    tokio::time::sleep(BURST_GAP_LOCAL).await;
                                }
                                if let Err(e) = video.send(pkt) {
                                    ok = false;
                                    stats.send_errors.fetch_add(1, Ordering::Relaxed);
                                    if !warned.swap(true, Ordering::Relaxed) {
                                        eprintln!("[luchd] udp send (kind {}): {e}", f.kind);
                                    }
                                }
                            }
                            ok
                        }
                        ReceiverTransport::Ws { video, .. } => ws_try_send(video, &f),
                    };
                    if sent {
                        stats.frames.fetch_add(1, Ordering::Relaxed);
                        stats.bytes.fetch_add(f.data.len() as u64, Ordering::Relaxed);
                        stats.last_seq.store(f.seq as u64, Ordering::Relaxed);
                        last_sent = Some(f.seq);
                    } else {
                        // A lost frame breaks the decoder chain: recover through an IDR.
                        stats.dropped.fetch_add(1, Ordering::Relaxed);
                        awaiting = true;
                        enter_awaiting(&stats, &idr, &mut last_idr);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    stats.skipped.fetch_add(n as u64, Ordering::Relaxed);
                    if !awaiting {
                        awaiting = true;
                        enter_awaiting(&stats, &idr, &mut last_idr);
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            f = arx.recv() => match f {
                Ok(f) => {
                    // Audio has no frame dependencies: it is delivered even while video
                    // waits for a keyframe, so sound continues through the recovery.
                    let sent = match &tp {
                        ReceiverTransport::Udp { audio, .. } => {
                            let mut ok = true;
                            let pkts = shard_packets(&f);
                            for pkt in pkts.iter() {
                                if let Err(e) = audio.send(pkt) {
                                    ok = false;
                                    stats.send_errors.fetch_add(1, Ordering::Relaxed);
                                    if !warned.swap(true, Ordering::Relaxed) {
                                        eprintln!("[luchd] udp send (kind {}): {e}", f.kind);
                                    }
                                }
                            }
                            ok
                        }
                        ReceiverTransport::Ws { audio, .. } => ws_try_send(audio, &f),
                    };
                    if sent {
                        stats.frames.fetch_add(1, Ordering::Relaxed);
                        stats.bytes.fetch_add(f.data.len() as u64, Ordering::Relaxed);
                    } else {
                        // Audio has its own queue and its own socket on both transports: a
                        // refused audio frame says nothing about the video chain, so it must
                        // not withhold video or ask the group for a keyframe.
                        stats.dropped.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    // Audio only: skip ahead, video is unaffected.
                    stats.skipped.fetch_add(n as u64, Ordering::Relaxed);
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::encoder::{create, EncSession};
    use super::super::frame::EncodedFrame;
    use super::super::frame_bus::FrameBus;
    use super::super::{F_IDR, KIND_AUDIO, KIND_VIDEO};
    use super::*;

    fn frame(kind: u8, flags: u8, seq: u32, data: Vec<u8>) -> EncodedFrame {
        EncodedFrame::new(kind, flags, seq, 1234, 5, Arc::from(data))
    }

    /// Seq and flags of a queued websocket message (nothing else is needed here).
    fn wire(msg: &[u8]) -> (u8, u32) {
        (msg[1], u32::from_be_bytes(msg[8..12].try_into().unwrap()))
    }

    fn drain(rx: &mut mpsc::Receiver<Bytes>) -> Vec<(u8, u32)> {
        let mut got = Vec::new();
        while let Ok(m) = rx.try_recv() {
            got.push(wire(&m));
        }
        got
    }

    fn ws_receiver(
        bus: &FrameBus,
        enc: &EncSession,
        cap: usize,
    ) -> (
        Arc<TransportStats>,
        mpsc::Receiver<Bytes>,
        tokio::task::JoinHandle<()>,
    ) {
        let stats = Arc::new(TransportStats::default());
        let (tx, rx) = mpsc::channel(cap);
        let (atx, _arx) = mpsc::channel(8);
        let idr = {
            let enc = enc.clone();
            GroupIdr::new(move || enc.request_idr())
        };
        let handle = tokio::spawn(run_receiver(
            ReceiverTransport::Ws {
                video: tx,
                audio: atx,
            },
            bus.subscribe_video(),
            bus.subscribe_audio(),
            idr,
            stats.clone(),
            false,
        ));
        (stats, rx, handle)
    }

    #[test]
    fn wire_forms_are_built_once_per_frame_and_shared() {
        let f = frame(KIND_VIDEO, F_IDR, 7, vec![9; SHARD * 2 + 100]);
        // A second call returns the cached allocation instead of re-serializing.
        assert!(Arc::ptr_eq(&shard_packets(&f), &shard_packets(&f)));
        assert_eq!(ws_message(&f).as_ptr(), ws_message(&f).as_ptr());
    }

    #[tokio::test]
    async fn two_receivers_get_the_same_serialized_bytes() {
        let bus = FrameBus::new();
        let enc = create(bus.clone(), crate::display::Size::default());
        let (_s1, mut rx1, h1) = ws_receiver(&bus, &enc, 8);
        let (_s2, mut rx2, h2) = ws_receiver(&bus, &enc, 8);
        enc.publish(KIND_VIDEO, F_IDR, 1, 0, &[7u8; 64]);
        let (a, b) = (rx1.recv().await.unwrap(), rx2.recv().await.unwrap());
        assert_eq!(
            a.as_ptr(),
            b.as_ptr(),
            "one serialization, shared by every receiver"
        );
        h1.abort();
        h2.abort();
        let _ = h1.await;
        let _ = h2.await;
    }

    #[test]
    fn udp_frame_is_sharded_and_header_carries_group_seq() {
        let data: Vec<u8> = (0..SHARD * 2 + 100).map(|i| i as u8).collect();
        let f = frame(KIND_VIDEO, F_IDR, 7, data.clone());
        let pkts = shard_packets(&f);
        assert_eq!(pkts.len(), 3);
        let mut got = Vec::new();
        for (i, p) in pkts.iter().enumerate() {
            assert_eq!(
                &p[..8],
                &header(
                    KIND_VIDEO,
                    F_IDR,
                    i as u16,
                    3,
                    7,
                    1234,
                    data.len() as u32,
                    5
                )[..8]
            );
            assert_eq!(u32::from_be_bytes(p[8..12].try_into().unwrap()), 7);
            got.extend_from_slice(&p[HEADER..]);
        }
        assert_eq!(got, data);
    }

    #[tokio::test]
    async fn lagging_receiver_withholds_frames_until_a_newer_keyframe() {
        let bus = FrameBus::new();
        let enc = create(bus.clone(), crate::display::Size::default());
        let (stats, mut rx, handle) = ws_receiver(&bus, &enc, 64);
        let burst = super::super::frame_bus::VIDEO_QUEUE as u32 + 6; // seq 0..13
                                                                     // One synchronous burst overfills the bounded queue: the receiver falls behind.
        for _ in 0..burst {
            enc.publish(KIND_VIDEO, 0, 1, 0, &[0u8; 4]);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        // Lagging immediately, or already withholding the stale backlog: never streaming.
        assert_ne!(stats.state(), DeliveryState::Streaming);
        // Whatever reached the wire before the lag is old news now.
        drain(&mut rx);
        // Dependent frames after a lost frame are undecodable: they must be withheld.
        for _ in 0..3 {
            enc.publish(KIND_VIDEO, 0, 2, 0, &[0u8; 4]); // seq 14..16
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(stats.state(), DeliveryState::AwaitingIdr);
        assert!(drain(&mut rx).is_empty(), "P-frames escaped before the IDR");
        assert!(stats.skipped.load(Ordering::Relaxed) >= 3);
        assert!(stats.idr_requests.load(Ordering::Relaxed) >= 1);
        // The group keyframe resumes delivery; the following P-frame keeps flowing.
        enc.publish(KIND_VIDEO, F_IDR, 3, 0, &[1, 2, 3]); // seq 17
        enc.publish(KIND_VIDEO, 0, 4, 0, &[4]); // seq 18
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(drain(&mut rx), vec![(F_IDR, 17), (0, 18)]);
        assert_eq!(stats.state(), DeliveryState::Streaming);
        assert_eq!(stats.last_seq.load(Ordering::Relaxed), 18);
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn full_ws_channel_enters_awaiting_and_recovers_on_keyframe() {
        let bus = FrameBus::new();
        let enc = create(bus.clone(), crate::display::Size::default());
        let (stats, mut rx, handle) = ws_receiver(&bus, &enc, 1);
        enc.publish(KIND_VIDEO, 0, 1, 0, &[1]); // seq 0: fills the channel
        tokio::time::sleep(Duration::from_millis(30)).await;
        enc.publish(KIND_VIDEO, 0, 2, 0, &[2]); // seq 1: channel full -> dropped + IDR
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(stats.dropped.load(Ordering::Relaxed), 1);
        assert_ne!(stats.state(), DeliveryState::Streaming);
        assert!(stats.idr_requests.load(Ordering::Relaxed) >= 1);
        // The queued frame is intact; the dropped one never arrives.
        assert_eq!(drain(&mut rx), vec![(0, 0)]);
        enc.publish(KIND_VIDEO, 0, 3, 0, &[3]); // seq 2: withheld, state -> AwaitingIdr
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(stats.state(), DeliveryState::AwaitingIdr);
        assert!(drain(&mut rx).is_empty());
        enc.publish(KIND_VIDEO, F_IDR, 4, 0, &[9]); // seq 3: recovery
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(drain(&mut rx), vec![(F_IDR, 3)]);
        assert_eq!(stats.state(), DeliveryState::Streaming);
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn a_lost_keyframe_request_is_asked_again_while_the_receiver_waits() {
        let bus = FrameBus::new();
        let enc = create(bus.clone(), crate::display::Size::default());
        let asks = Arc::new(AtomicU64::new(0));
        let stats = Arc::new(TransportStats::default());
        let (tx, mut rx) = mpsc::channel(64);
        let idr = {
            let asks = asks.clone();
            GroupIdr::new(move || {
                asks.fetch_add(1, Ordering::Relaxed);
            })
        };
        let (atx, _arx) = mpsc::channel(8);
        let handle = tokio::spawn(run_receiver(
            ReceiverTransport::Ws {
                video: tx,
                audio: atx,
            },
            bus.subscribe_video(),
            bus.subscribe_audio(),
            idr,
            stats.clone(),
            false,
        ));
        let burst = super::super::frame_bus::VIDEO_QUEUE as u32 + 6;
        for _ in 0..burst {
            enc.publish(KIND_VIDEO, 0, 1, 0, &[0u8; 4]);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        let first = asks.load(Ordering::Relaxed);
        assert_eq!(first, 1, "one request on entering AwaitingIdr");
        // Withheld frames keep arriving: inside the cooldown nothing new is asked.
        for _ in 0..3 {
            enc.publish(KIND_VIDEO, 0, 2, 0, &[0u8; 4]);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(asks.load(Ordering::Relaxed), first);
        // After the cooldown the request is repeated: an IDR lost on the wire or dropped
        // by the KMS cooldown must not freeze the receiver forever.
        tokio::time::sleep(IDR_COOLDOWN + Duration::from_millis(50)).await;
        enc.publish(KIND_VIDEO, 0, 3, 0, &[0u8; 4]);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(asks.load(Ordering::Relaxed), first + 1);
        drain(&mut rx);
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn an_audio_backlog_does_not_evict_or_withhold_video() {
        let bus = FrameBus::new();
        let enc = create(bus.clone(), crate::display::Size::default());
        let stats = Arc::new(TransportStats::default());
        let (vtx, mut vrx) = mpsc::channel(4);
        let (atx, _arx) = mpsc::channel(1); // never drained: the audio queue stays full
        let idr = {
            let enc = enc.clone();
            GroupIdr::new(move || enc.request_idr())
        };
        let handle = tokio::spawn(run_receiver(
            ReceiverTransport::Ws {
                video: vtx,
                audio: atx,
            },
            bus.subscribe_video(),
            bus.subscribe_audio(),
            idr,
            stats.clone(),
            false,
        ));
        for _ in 0..3 {
            enc.publish(KIND_AUDIO, 0, 1, 0, &[0u8; 8]);
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            stats.dropped.load(Ordering::Relaxed) >= 1,
            "audio was refused"
        );
        assert_eq!(stats.state(), DeliveryState::Streaming);
        enc.publish(KIND_VIDEO, 0, 2, 0, &[1u8; 8]);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(drain(&mut vrx).len(), 1, "video keeps its own queue");
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn a_stale_keyframe_does_not_resume_the_stream() {
        let bus = FrameBus::new();
        let enc = create(bus.clone(), crate::display::Size::default());
        let (stats, mut rx, handle) = ws_receiver(&bus, &enc, 64);
        enc.publish(KIND_VIDEO, 0, 1, 0, &[0]); // seq 0
        enc.publish(KIND_VIDEO, 0, 2, 0, &[0]); // seq 1
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(drain(&mut rx), vec![(0, 0), (0, 1)]);
        for _ in 0..(super::super::frame_bus::VIDEO_QUEUE as u32 + 2) {
            enc.publish(KIND_VIDEO, 0, 3, 0, &[0]); // seq 2..: falls behind
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        drain(&mut rx);
        // An IDR with a seq we already passed cannot help: keep waiting.
        bus.publish(&EncodedFrame::new(
            KIND_VIDEO,
            F_IDR,
            1,
            0,
            0,
            Arc::from(vec![9u8]),
        ));
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(drain(&mut rx).is_empty());
        assert_ne!(stats.state(), DeliveryState::Streaming);
        // A fresh keyframe (seq 12) resumes.
        enc.publish(KIND_VIDEO, F_IDR, 4, 0, &[9]);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(drain(&mut rx), vec![(F_IDR, 12)]);
        assert_eq!(stats.state(), DeliveryState::Streaming);
        handle.abort();
        let _ = handle.await;
    }
}
