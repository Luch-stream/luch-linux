//! MirrorGroup: the Group Engine's unit. One group = one CaptureSession, one
//! EncoderSession, one FrameBus, N independent receivers (spec §8).
//!
//! Lifecycle: `start()` builds capture/encoder/bus, attaches the initial receivers and
//! runs until every receiver is gone (auto-stop) or `stop()` is called. `add()`/`remove()`
//! only ever touch the receiver map and its tasks — capture and encoder keep running.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use futures_util::StreamExt;
use gstreamer as gst;
use gstreamer::prelude::*;
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, Notify};
use tokio::task::JoinHandle;

use crate::display::{Layout, Size};

use super::capture::CaptureSession;
use super::encoder::{self, EncSession};
use super::frame_bus::FrameBus;
use super::transport::{ReceiverTransport, TransportStats};
use super::{probe_stages, spawn_kms, wire, Cmd, VIRTUAL_SETUP};

/// How long a new receiver may take to answer `start` with `ready`.
const READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Static capture/source parameters of a group: decided once at creation and never
/// changed for running receivers (spec §14: no dynamic re-profiling in the MVP).
pub struct GroupParams {
    pub source: ashpd::desktop::screencast::SourceType,
    pub size: Size,
    pub kms: bool,
    pub mirror169: bool,
    pub niri_virtual: bool,
    /// Stream rate of this group: 0 keeps the screen's own rate, otherwise 30 or 60.
    /// Fixed at creation (spec §14).
    pub fps: u32,
    /// Sound only (source "audio"): no screen capture, no video stream (PROTOCOL.md 4d).
    pub audio_only: bool,
    /// The `audio` part of `start` (limits::start_audio), or None for the picture only.
    /// Group-wide: decided once, from the receiver that starts the show (spec §14).
    pub audio: Option<Value>,
}

impl GroupParams {
    fn label(&self) -> &'static str {
        if self.audio_only {
            "audio"
        } else if self.kms {
            if self.mirror169 {
                "fast169"
            } else {
                "fast"
            }
        } else if self.niri_virtual {
            "virtual"
        } else {
            match self.source {
                ashpd::desktop::screencast::SourceType::WINDOW => "window",
                ashpd::desktop::screencast::SourceType::VIRTUAL => "virtual",
                _ => "monitor",
            }
        }
    }
}

/// A receiver about to join a group. Control channel and its event broadcast come from
/// the daemon's connection layer; the group never opens control channels itself.
pub struct NewReceiver {
    pub id: String,
    pub name: String,
    pub address: Ipv4Addr,
    pub ifname: String,
    /// Decoder preference from the receiver's welcome (`welcome.bench.prefer`).
    pub prefer: String,
    /// Web receiver: its two media queues. `None` = UDP (Android TV).
    pub ws: Option<WsMedia>,
    pub cmd: mpsc::Sender<Cmd>,
    pub events: broadcast::Sender<Value>,
}

/// A web receiver's media queues, bounded per kind: video stays shallow so latency cannot
/// grow, audio cannot take the slot a video frame needs. The socket writer is shared.
#[derive(Clone)]
pub struct WsMedia {
    pub video: mpsc::Sender<bytes::Bytes>,
    pub audio: mpsc::Sender<bytes::Bytes>,
}

/// A receiver whose task ended on its own: `reason` is what the receiver said in `stop`
/// (limits::stop_reason), or None when its channel just closed.
pub struct EndedReceiver {
    pub id: String,
    pub name: String,
    pub reason: Option<&'static str>,
}

/// One group member with its independent delivery task and per-receiver statistics
/// (spec §5.7: delivery metrics live here, encoder metrics in the EncoderSession).
pub struct GroupReceiver {
    pub id: String,
    pub name: String,
    pub address: String,
    pub transport: &'static str, // "udp" | "ws"
    pub stats: Arc<TransportStats>,
    /// The receiver's own latest `stats` message (decoder numbers).
    pub rx_stats: Arc<StdMutex<Value>>,
    rtt_bits: Arc<AtomicU64>,
    cmd: mpsc::Sender<Cmd>,
    task: JoinHandle<()>,
}

impl GroupReceiver {
    pub fn rtt_ms(&self) -> f64 {
        f64::from_bits(self.rtt_bits.load(Ordering::Relaxed))
    }
}

/// Static per-group facts every per-receiver `sender_stats` tick needs (the receiver
/// shows them in its overlay). The virtual monitor appears only after the group placed it.
struct SenderInfo {
    size: Size,
    hevc: bool,
    virtual_monitor: StdMutex<Option<String>>,
}

/// The group. Owned by the daemon's GroupManager (`HashMap` in `State`); all mutating
/// methods take `&mut self`, so adding, removing and stopping can never race there —
/// the only concurrent actor is each receiver's own task, which reaches the shared
/// state through Arc clones and flags, never through the group itself.
pub struct MirrorGroup {
    id: String,
    codec: String,
    size: Size,
    audio_only: bool,
    source_label: &'static str,
    capture: Arc<CaptureSession>,
    enc: EncSession,
    bus: FrameBus,
    /// The `audio` part of `start`, fixed at creation; None streams the picture only.
    audio: Option<Value>,
    /// The GNOME layout from before this group extended the desktop: restored once the
    /// virtual monitor is gone, so the physical monitors keep their order (main.rs).
    layout: Option<Layout>,
    receivers: HashMap<String, GroupReceiver>,
    /// Receivers whose task ended on its own (stop event / channel closed);
    /// collected by `reap()` so the map never holds stale entries after a mutation.
    ended: Arc<StdMutex<Vec<EndedReceiver>>>,
    /// Wakes the manager to recheck the group: a receiver ended, or the pipeline died.
    /// Membership itself lives only in `receivers` (reaped), so there is no counter to
    /// drift out of sync with it.
    stopped: Arc<Notify>,
    /// Set by the gst bus watchdog on a fatal pipeline error or EOS: the manager must
    /// stop the whole group even though receivers may still look attached.
    pipeline_dead: Arc<AtomicBool>,
    watchdog: Option<JoinHandle<()>>,
    info: Arc<SenderInfo>,
}

/// niri output name for a group: short and ASCII. A receiver-supplied id can be any UTF-8,
/// so it is filtered by characters, never sliced by bytes.
fn output_label(id: &str) -> String {
    id.chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect()
}

impl MirrorGroup {
    /// Build capture, encoder and bus, attach the initial receivers, start streaming.
    /// Codec profile: HEVC only on the KMS path when every initial receiver prefers it,
    /// H.264 otherwise (spec §14).
    pub async fn start(
        id: String,
        params: GroupParams,
        initial: Vec<NewReceiver>,
    ) -> Result<Self, String> {
        let capture = CaptureSession::begin(
            params.source,
            params.size,
            params.kms,
            params.mirror169,
            params.niri_virtual,
            params.audio_only,
            &format!("LUCH-g-{}", output_label(&id)),
        )
        .await?;
        Self::launch(id, params, capture, initial).await
    }

    /// The rest of start(), split out so tests can inject a display-less capture.
    async fn launch(
        id: String,
        params: GroupParams,
        capture: CaptureSession,
        initial: Vec<NewReceiver>,
    ) -> Result<Self, String> {
        let hevc = choose_codec(params.kms, &initial) == "video/hevc";
        let codec = if hevc { "video/hevc" } else { "video/avc" }.to_string();
        let virtual_source = params.source == ashpd::desktop::screencast::SourceType::VIRTUAL
            && !params.niri_virtual;
        let virtual_setup = if virtual_source {
            Some(VIRTUAL_SETUP.lock().await)
        } else {
            None
        };
        let layout = if virtual_source {
            Some(Layout::capture().await?)
        } else {
            None
        };

        let capture = Arc::new(capture);
        let bus = FrameBus::new();
        let enc = encoder::create_with_fps(bus.clone(), params.size, params.fps);
        let audio = params.audio.clone();
        let source_label = params.label();
        wire(&capture.pipeline, enc.clone(), params.fps);
        let (stopped, pipeline_dead) = (Arc::new(Notify::new()), Arc::new(AtomicBool::new(false)));
        if params.kms {
            // A helper that dies on its own (crash, unplugged screen, KMS lost) ends the
            // group through the same flag the gst watchdog uses.
            let (dead, wake) = (pipeline_dead.clone(), stopped.clone());
            match spawn_kms(enc.clone(), hevc, params.size, params.fps, move || {
                dead.store(true, Ordering::SeqCst);
                wake.notify_waiters();
            }) {
                Ok(g) => capture.install_kms(g),
                Err(e) => {
                    capture.close().await;
                    return Err(e);
                }
            }
        }

        let mut group = MirrorGroup {
            id,
            codec,
            size: params.size,
            audio_only: params.audio_only,
            source_label,
            capture,
            enc,
            bus,
            audio,
            layout,
            receivers: HashMap::new(),
            ended: Arc::new(StdMutex::new(Vec::new())),
            stopped,
            pipeline_dead,
            watchdog: None,
            info: Arc::new(SenderInfo {
                size: params.size,
                hevc,
                virtual_monitor: StdMutex::new(None),
            }),
        };
        for r in initial {
            if let Err(e) = group.add(r).await {
                group.abort().await;
                return Err(e);
            }
        }
        if let Err(e) = group.capture.pipeline.set_state(gst::State::Playing) {
            group.abort().await;
            return Err(format!("pipeline: {e}"));
        }
        if let Some(layout) = &group.layout {
            match layout.place_new_right(group.size).await {
                Ok(connector) => *group.info.virtual_monitor.lock().unwrap() = Some(connector),
                Err(e) => {
                    group.abort().await;
                    return Err(e);
                }
            }
        }
        drop(virtual_setup);
        if std::env::var_os("LUCH_PROBE").is_some() {
            probe_stages(&group.capture.pipeline);
        }
        // Gst bus watchdog: a fatal pipeline error or EOS stops the whole group.
        if let Some(gst_bus) = group.capture.pipeline.bus() {
            let stopped = group.stopped.clone();
            let pipeline_dead = group.pipeline_dead.clone();
            group.watchdog = Some(tokio::spawn(async move {
                use gstreamer::MessageView::*;
                let mut stream = gst_bus.stream();
                while let Some(msg) = stream.next().await {
                    if matches!(msg.view(), Error(_) | Eos(_)) {
                        break;
                    }
                }
                pipeline_dead.store(true, Ordering::SeqCst);
                stopped.notify_waiters();
            }));
        }
        eprintln!(
            "[luchd] group {} ({} {}x{} {}) started, {} receiver(s)",
            group.id,
            group.source_label,
            group.size.width,
            group.size.height,
            group.codec,
            group.receivers.len()
        );
        Ok(group)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn codec(&self) -> &str {
        &self.codec
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn source_label(&self) -> &'static str {
        self.source_label
    }

    pub fn bus(&self) -> &FrameBus {
        &self.bus
    }

    /// The `audio` part of `start`, fixed at creation (one profile per group).
    pub fn audio_profile(&self) -> Option<Value> {
        self.audio.clone()
    }

    /// (IDR requests that created a pending keyframe, requests merged into one).
    pub fn encoder_idr(&self) -> (u64, u64) {
        self.enc.idr_counters()
    }

    pub fn enc_counters(&self) -> &encoder::PublishCounters {
        &self.enc.counters
    }

    /// Stream rate of this group (0 = the screen's own rate); fixed at creation.
    pub fn fps(&self) -> u32 {
        self.enc.fps
    }

    /// Current group-wide sequence counters (next seq to be assigned per kind).
    pub fn encoder_seqs(&self) -> (u32, u32) {
        self.enc.seqs()
    }

    /// Published video spacing (avg, max) in ms, for `GroupStats`.
    pub fn encoder_pacing(&self) -> (f64, f64) {
        self.enc.pacing_ms()
    }

    /// Capture pipeline state for `GroupStats` ("playing", "paused", ...).
    pub fn capture_state(&self) -> &'static str {
        match self.capture.pipeline.current_state() {
            gst::State::Playing => "playing",
            gst::State::Paused => "paused",
            gst::State::Ready => "ready",
            _ => "null",
        }
    }

    pub fn receiver_count(&self) -> usize {
        self.receivers.len()
    }

    pub fn receiver_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.receivers.keys().cloned().collect();
        ids.sort();
        ids
    }

    pub fn receivers(&self) -> impl Iterator<Item = &GroupReceiver> {
        self.receivers.values()
    }

    /// Fired when the group is empty (every receiver left) or the pipeline died.
    /// The manager uses it to remove the group from its tables and call stop().
    pub fn stopped_notify(&self) -> Arc<Notify> {
        self.stopped.clone()
    }

    /// True once the gst bus watchdog saw a fatal error or EOS: every receiver is
    /// effectively gone, even if their tasks have not noticed yet.
    pub fn pipeline_dead(&self) -> bool {
        self.pipeline_dead.load(Ordering::SeqCst)
    }

    /// Connector of the GNOME virtual monitor this group placed, if any (D-Bus stats).
    pub fn virtual_monitor(&self) -> Option<String> {
        self.info.virtual_monitor.lock().unwrap().clone()
    }

    /// Group numbers as JSON: id, codec, size, fps target, receivers, encoder counters,
    /// transport subscribers.
    pub fn stats_json(&self) -> Value {
        super::stats::group_stats_json(self)
    }

    /// Attach a receiver to the running group: control handshake (`start` -> `ready`),
    /// an independent transport subscribed to the shared bus, and a task merging its
    /// media delivery with its control events. Capture and encoder keep running.
    pub async fn add(&mut self, r: NewReceiver) -> Result<(), String> {
        if self.codec == "video/hevc" && r.prefer != "video/hevc" {
            return Err(format!(
                "incompatible codec: group streams {}, {} prefers {}",
                self.codec, r.id, r.prefer
            ));
        }
        if self.receivers.contains_key(&r.id) {
            return Err(format!("already in group: {}", r.id));
        }
        let NewReceiver {
            id,
            name,
            address,
            ifname,
            prefer: _,
            ws,
            cmd,
            events: _events,
        } = r;
        // Subscribe *before* sending start: the receiver may answer `ready` immediately,
        // and a broadcast message with no subscriber is lost. The same subscription then
        // goes into the receiver task, so no event is dropped between the handshake and it.
        let mut ready_events = _events.subscribe();
        // The audio profile is the group's, not this receiver's: one shared Opus stream
        // (spec §14), so a joiner gets what the group already encodes.
        let mut start = if self.audio_only {
            json!({"t": "start", "mode": "audio"})
        } else {
            json!({"t": "start", "mode": "mirror",
                "video": {"codec": self.codec, "width": self.size.width, "height": self.size.height, "fps": self.fps()}})
        };
        if let Some(a) = &self.audio {
            start["audio"] = a.clone();
        }
        cmd.send(Cmd::Send(start))
            .await
            .map_err(|_| format!("control channel closed: {id}"))?;
        let ready = tokio::time::timeout(READY_TIMEOUT, async {
            loop {
                match ready_events.recv().await {
                    Ok(v) if v["t"] == "ready" => return Ok(v),
                    Ok(v) if v["t"] == "error" || v["t"] == "stop" => {
                        return Err(format!("receiver {id}: {v}"))
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => return Err(format!("control channel closed: {id}")),
                }
            }
        })
        .await
        .map_err(|_| format!("receiver {id} did not answer start"))??;
        let port = |k: &str| {
            ready[k]
                .as_u64()
                .filter(|p| *p > 0 && *p < 65536)
                .map(|p| p as u16)
        };
        let (tp, kind) = if let Some(ws) = ws {
            (ReceiverTransport::ws(ws.video, ws.audio), "ws")
        } else {
            let (vp, ap) = port("video_port")
                .zip(port("audio_port"))
                .ok_or(format!("bad ready from {id}: {ready}"))?;
            (
                ReceiverTransport::udp_for(
                    &ifname,
                    SocketAddrV4::new(address, vp),
                    SocketAddrV4::new(address, ap),
                )
                .map_err(|e| format!("udp: {e}"))?,
                "udp",
            )
        };
        let stats = Arc::new(TransportStats::default());
        let rx_stats = Arc::new(StdMutex::new(Value::Null));
        let rtt_bits = Arc::new(AtomicU64::new(0));
        // A joiner subscribes mid-GOP: it must not be fed P-frames before its first
        // keyframe. A fresh group has nothing to withhold, its encoder starts with an IDR.
        let start_awaiting = self.enc.counters.frames.load(Ordering::Relaxed) > 0;
        let task = tokio::spawn(receiver_task(
            id.clone(),
            name.clone(),
            tp,
            self.bus.subscribe_video(),
            self.bus.subscribe_audio(),
            ready_events,
            self.enc.clone(),
            self.capture.clone(),
            stats.clone(),
            rx_stats.clone(),
            rtt_bits.clone(),
            cmd.clone(),
            self.info.clone(),
            self.ended.clone(),
            self.stopped.clone(),
            start_awaiting,
        ));
        self.receivers.insert(
            id.clone(),
            GroupReceiver {
                id: id.clone(),
                name,
                address: address.to_string(),
                transport: kind,
                stats,
                rx_stats,
                rtt_bits,
                cmd,
                task,
            },
        );
        eprintln!("[luchd] group {}: {} joined ({kind})", self.id, id);
        if self.enc.counters.frames.load(Ordering::Relaxed) > 0 {
            // A joiner subscribes at the current point of an infinite-GOP stream, so without
            // this it would show nothing until some other event produced a keyframe. The
            // initial receivers need none: a fresh encoder starts with a keyframe.
            self.enc.request_idr();
            self.capture.request_idr();
        }
        Ok(())
    }

    /// Move receivers whose task ended on its own out of the map, returning what ended
    /// (with the receiver's own stop reason, if it gave one). Called at the start of
    /// every mutation, and by the manager whenever it touches the group.
    pub fn reap(&mut self) -> Vec<EndedReceiver> {
        let ended = std::mem::take(&mut *self.ended.lock().unwrap());
        for e in &ended {
            if let Some(r) = self.receivers.remove(&e.id) {
                eprintln!("[luchd] group {}: {} left", self.id, e.id);
                drop(r);
            }
        }
        ended
    }

    /// Detach one receiver: its delivery stops, the rest of the group keeps running.
    /// Idempotent: removing an unknown id is Ok.
    pub async fn remove(&mut self, id: &str) -> Result<(), String> {
        self.reap();
        if let Some(r) = self.receivers.remove(id) {
            let _ = r.cmd.send(Cmd::Send(json!({"t": "stop"}))).await;
            r.task.abort();
            let _ = r.task.await;
            // Tell the manager to recheck: it reaps and, at zero receivers, stops the group.
            wake(&self.stopped);
            eprintln!("[luchd] group {}: {} left", self.id, id);
        }
        Ok(())
    }

    /// Stop everything: all receivers, the watchdog, the pipeline, the capture.
    /// Consuming, so it cannot run twice.
    pub async fn stop(mut self) {
        self.teardown().await;
        self.stopped.notify_waiters();
        eprintln!("[luchd] group {} stopped", self.id);
    }

    /// Teardown used by `stop()` and by a failed `start()`: receivers out, watchdog and bus
    /// gone, pipeline down, capture closed, virtual-monitor layout restored.
    async fn teardown(&mut self) {
        let ids: Vec<String> = self.receivers.keys().cloned().collect();
        for id in ids {
            let _ = self.remove(&id).await;
        }
        if let Some(w) = self.watchdog.take() {
            w.abort();
            let _ = w.await; // drop its bus stream before the pipeline goes away
        }
        let _ = self.capture.pipeline.set_state(gst::State::Null);
        self.capture.close().await;
        // The portal session is closed: Mutter has dropped the virtual monitor by now, so
        // the physical monitors get back the layout they had before extend (main.rs).
        if let (Some(layout), Some(connector)) = (&self.layout, self.virtual_monitor()) {
            if let Err(e) = layout.restore(&connector).await {
                eprintln!("[luchd] restore display layout: {e}");
            }
        }
    }

    /// Teardown used when start() fails partway.
    async fn abort(mut self) {
        self.teardown().await;
    }

    #[cfg(test)]
    pub(crate) fn enc(&self) -> EncSession {
        self.enc.clone()
    }
}

/// Group codec profile (spec §14): HEVC only on the KMS path and only when every
/// initial receiver prefers it; H.264 in every other case.
fn choose_codec(kms: bool, initial: &[NewReceiver]) -> &'static str {
    if kms && !initial.is_empty() && initial.iter().all(|r| r.prefer == "video/hevc") {
        "video/hevc"
    } else {
        "video/avc"
    }
}

/// Tell the manager to recheck the group: it reaps ended receivers and, at zero, stops
/// the group. Fired on every receiver end, so a receiver leaving mid-session is
/// promptly reflected over D-Bus — the notify only means "look again", never "last one".
fn wake(stopped: &Notify) {
    stopped.notify_waiters();
}

/// One receiver's life in the group: media frames from the shared bus on one arm of the
/// select, its control events on the other. It owns nothing of the group but Arc clones,
/// so it can never lock or stall the pipeline — and when it ends (receiver `stop`,
/// control channel closed, bus closed, or abort from remove()), it decrements the
/// group's live count; the last one out stops the group.
#[allow(clippy::too_many_arguments)]
async fn receiver_task(
    id: String,
    name: String,
    tp: ReceiverTransport,
    vrx: broadcast::Receiver<super::frame::EncodedFrame>,
    arx: broadcast::Receiver<super::frame::EncodedFrame>,
    mut events: broadcast::Receiver<Value>,
    enc: EncSession,
    capture: Arc<CaptureSession>,
    stats: Arc<TransportStats>,
    rx_stats: Arc<StdMutex<Value>>,
    rtt_bits: Arc<AtomicU64>,
    cmd: mpsc::Sender<Cmd>,
    info: Arc<SenderInfo>,
    ended: Arc<StdMutex<Vec<EndedReceiver>>>,
    stopped: Arc<Notify>,
    start_awaiting: bool,
) {
    let media = super::transport::run_receiver(
        tp,
        vrx,
        arx,
        {
            let enc = enc.clone();
            let capture = capture.clone();
            super::transport::GroupIdr::new(move || {
                enc.request_idr();
                capture.request_idr();
            })
        },
        stats.clone(),
        start_awaiting,
    );
    tokio::pin!(media);
    // Once a second, the numbers behind the receiver's on-screen overlay: the sender
    // side of *this* stream (fps/mbps/host latency of the group encoder) plus its RTT.
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let (mut last_frames, mut last_bytes, mut last_lat) = (0u64, 0u64, 0u64);
    let mut last_sent_bytes = 0u64;
    let mut tick_at = tokio::time::Instant::now();
    let mut reason = None;
    loop {
        tokio::select! {
            _ = &mut media => break,
            _ = tick.tick() => {
                let c = &enc.counters;
                let (frames, bytes, lat) = (
                    c.frames.load(Ordering::Relaxed),
                    c.bytes.load(Ordering::Relaxed),
                    c.lat_sum.load(Ordering::Relaxed),
                );
                let df = frames - last_frames;
                let host_ms = if df > 0 {
                    (lat - last_lat) as f64 / df as f64 / 10.0
                } else {
                    0.0
                };
                let mbps = (bytes - last_bytes) as f64 * 8.0 / 1e6;
                (last_frames, last_bytes, last_lat) = (frames, bytes, lat);
                // This receiver's own send rate (spec §5.7 "текущая скорость отправки").
                let sent = stats.bytes.load(Ordering::Relaxed);
                let now = tokio::time::Instant::now();
                let secs = now.duration_since(tick_at).as_secs_f64().max(1e-3);
                tick_at = now;
                stats.rate_bps.store(
                    ((sent.saturating_sub(last_sent_bytes)) as f64 * 8.0 / secs) as u64,
                    Ordering::Relaxed,
                );
                last_sent_bytes = sent;
                let rtt = f64::from_bits(rtt_bits.load(Ordering::Relaxed));
                let sender = super::stats::sender_stats_json(
                    info.hevc,
                    info.size,
                    df as f64,
                    mbps,
                    host_ms,
                    rtt,
                    info.virtual_monitor.lock().unwrap().clone(),
                );
                if cmd.send(Cmd::Send(sender)).await.is_err() {
                    break;
                }
            }
            ev = events.recv() => match ev {
                Ok(v) if v["t"] == "keyframe" => {
                    stats.idr_requests.fetch_add(1, Ordering::Relaxed);
                    enc.request_idr();
                    capture.request_idr();
                }
                Ok(v) if v["t"] == "stats" => *rx_stats.lock().unwrap() = v,
                Ok(v) if v["t"] == "_rtt" => {
                    rtt_bits.store(v["ms"].as_f64().unwrap_or(0.0).to_bits(), Ordering::Relaxed);
                }
                Ok(v) if v["t"] == "stop" => {
                    reason = Some(crate::limits::stop_reason(&v));
                    break;
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break, // control channel closed
            },
        }
    }
    ended
        .lock()
        .unwrap()
        .push(EndedReceiver { id, name, reason });
    wake(&stopped);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::{F_CONFIG, F_IDR, KIND_VIDEO};
    use std::time::Instant;

    /// A fake web receiver: media channel proves delivery; the test answers `start`
    /// with `ready` and can inject any control event afterwards.
    struct FakeRx {
        rx: NewReceiver,
        cmd: mpsc::Receiver<Cmd>,
        events: broadcast::Sender<Value>,
        media: mpsc::Receiver<bytes::Bytes>,
        _audio: mpsc::Receiver<bytes::Bytes>,
    }

    fn fake_rx(id: &str) -> FakeRx {
        fake_rx_cap(id, 64)
    }

    fn fake_rx_cap(id: &str, media_cap: usize) -> FakeRx {
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (events, _) = broadcast::channel(16);
        let (media_tx, media_rx) = mpsc::channel(media_cap);
        let (audio_tx, audio_rx) = mpsc::channel(media_cap);
        FakeRx {
            rx: NewReceiver {
                id: id.into(),
                name: format!("fake-{id}"),
                address: Ipv4Addr::LOCALHOST,
                ifname: "lo".into(),
                prefer: "video/avc".into(),
                ws: Some(WsMedia {
                    video: media_tx,
                    audio: audio_tx,
                }),
                cmd: cmd_tx,
                events: events.clone(),
            },
            cmd: cmd_rx,
            events,
            media: media_rx,
            _audio: audio_rx,
        }
    }

    fn udp_sock() -> tokio::net::UdpSocket {
        let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        s.set_nonblocking(true).unwrap();
        tokio::net::UdpSocket::from_std(s).unwrap()
    }

    /// One Luch v1 shard as a UDP receiver would parse it (payloads here are single-shard).
    async fn udp_frame(sock: &tokio::net::UdpSocket) -> (u8, u32, Vec<u8>) {
        let mut buf = [0u8; 2048];
        let (n, _) = tokio::time::timeout(Duration::from_secs(2), sock.recv_from(&mut buf))
            .await
            .expect("no UDP frame")
            .unwrap();
        let p = &buf[..n];
        let seq = u32::from_be_bytes(p[8..12].try_into().unwrap());
        let len = u32::from_be_bytes(p[16..20].try_into().unwrap()) as usize;
        assert_eq!(n, 24 + len, "single-shard frame");
        (p[1], seq, p[24..].to_vec())
    }

    /// Answer one `start` with `ready`, returning the start message for assertions.
    /// Keeps draining the command channel afterwards: the group's per-second
    /// `sender_stats` writes must never fail the receiver task in tests, exactly as in
    /// production where run_channel owns the other end.
    async fn answer_start(cmd: mpsc::Receiver<Cmd>, events: broadcast::Sender<Value>) -> Value {
        answer_start_with(cmd, events, json!({"t": "ready"})).await
    }

    async fn answer_start_with(
        mut cmd: mpsc::Receiver<Cmd>,
        events: broadcast::Sender<Value>,
        ready: Value,
    ) -> Value {
        let start = match cmd.recv().await.unwrap() {
            Cmd::Send(v) => v,
            other => panic!("expected Send(start), got {other:?}"),
        };
        events.send(ready).unwrap();
        tokio::spawn(async move { while cmd.recv().await.is_some() {} });
        start
    }

    async fn launch_test(id: &str, initial: Vec<NewReceiver>) -> MirrorGroup {
        launch_test_fps(id, initial, 60).await
    }

    async fn launch_test_fps(id: &str, initial: Vec<NewReceiver>, fps: u32) -> MirrorGroup {
        let _ = gstreamer::init(); // once per process; later calls are a no-op Err
        MirrorGroup::launch(
            id.into(),
            GroupParams {
                source: ashpd::desktop::screencast::SourceType::MONITOR,
                size: Size {
                    width: 1920,
                    height: 1080,
                },
                kms: false,
                mirror169: false,
                niri_virtual: false,
                audio_only: false,
                fps,
                audio: Some(json!({"codec": "audio/opus", "rate": 48000, "channels": 2, "frame_ms": crate::mirror::AUDIO_FRAME_MS})),
            },
            CaptureSession::for_test(),
            initial,
        )
        .await
        .unwrap()
    }

    async fn recv_frame(rx: &mut mpsc::Receiver<bytes::Bytes>) -> (u8, u32, u32) {
        let raw = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(raw.len() >= 24, "whole-frame wire format");
        (
            raw[1],                                              // flags
            u32::from_be_bytes(raw[8..12].try_into().unwrap()),  // seq
            u32::from_be_bytes(raw[12..16].try_into().unwrap()), // pts_us
        )
    }

    #[tokio::test]
    async fn start_two_receivers_one_encoder() {
        let a = fake_rx("a");
        let b = fake_rx("b");
        let ha = tokio::spawn(answer_start(a.cmd, a.events.clone()));
        let hb = tokio::spawn(answer_start(b.cmd, b.events.clone()));
        let group = launch_test("g-1", vec![a.rx, b.rx]).await;
        let (sa, sb) = (ha.await.unwrap(), hb.await.unwrap());
        assert_eq!(sa["video"]["codec"], sb["video"]["codec"]);
        assert_eq!(sa["video"]["width"], sb["video"]["width"]);
        assert_eq!(sa["video"]["height"], sb["video"]["height"]);
        assert_eq!(sa["video"]["codec"], "video/avc");
        assert_eq!(group.receiver_count(), 2);
        assert_eq!(group.receiver_ids(), vec!["a".to_string(), "b".to_string()]);
        group.stop().await;
    }

    #[tokio::test]
    async fn encode_once_both_receivers_same_frame() {
        let a = fake_rx("a");
        let b = fake_rx("b");
        tokio::spawn(answer_start(a.cmd, a.events.clone()));
        tokio::spawn(answer_start(b.cmd, b.events.clone()));
        let mut media_a = a.media;
        let mut media_b = b.media;
        let group = launch_test("g-2", vec![a.rx, b.rx]).await;
        let enc = group.enc();
        enc.publish(KIND_VIDEO, F_CONFIG, 42, 7, b"frame-one");
        let (fa, fb) = (
            recv_frame(&mut media_a).await,
            recv_frame(&mut media_b).await,
        );
        assert_eq!(fa, fb);
        assert_eq!(fa, (F_CONFIG, 0, 42));
        group.stop().await;
    }

    #[tokio::test]
    async fn add_during_streaming_subscribes_to_shared_seq() {
        let a = fake_rx("a");
        let b = fake_rx("b");
        tokio::spawn(answer_start(a.cmd, a.events.clone()));
        tokio::spawn(answer_start(b.cmd, b.events.clone()));
        let mut media_a = a.media;
        let mut media_b = b.media;
        let mut group = launch_test("g-3", vec![a.rx, b.rx]).await;
        group.enc().publish(KIND_VIDEO, 0, 1, 0, b"one"); // seq 0
        assert_eq!(recv_frame(&mut media_a).await, (0, 0, 1));
        assert_eq!(recv_frame(&mut media_b).await, (0, 0, 1));
        let c = fake_rx("c");
        tokio::spawn(answer_start(c.cmd, c.events.clone()));
        let mut media_c = c.media;
        group.add(c.rx).await.unwrap();
        assert_eq!(group.receiver_count(), 3);
        // The newcomer joined after frame 0 and waits for a keyframe: the next group IDR
        // reaches all three with the same group sequence.
        group.enc().publish(KIND_VIDEO, F_IDR, 2, 0, b"two"); // seq 1
        let same = (F_IDR, 1, 2);
        assert_eq!(recv_frame(&mut media_a).await, same);
        assert_eq!(recv_frame(&mut media_b).await, same);
        assert_eq!(recv_frame(&mut media_c).await, same);
        group.stop().await;
    }

    #[tokio::test]
    async fn a_receiver_can_join_mid_stream_and_others_keep_their_sequence() {
        let a = fake_rx("a");
        let b = fake_rx("b");
        tokio::spawn(answer_start(a.cmd, a.events.clone()));
        tokio::spawn(answer_start(b.cmd, b.events.clone()));
        let mut media_b = b.media;
        let mut group = launch_test("g-int", vec![a.rx, b.rx]).await;
        group.enc().publish(KIND_VIDEO, F_IDR, 1, 0, b"key"); // seq 0
        assert_eq!(recv_frame(&mut media_b).await.1, 0);

        // A third receiver joins while frames are flowing.
        let c = fake_rx("c");
        tokio::spawn(answer_start(c.cmd, c.events.clone()));
        let mut media_c = c.media;
        group.add(c.rx).await.unwrap();
        assert_eq!(group.receiver_count(), 3);
        group.enc().publish(KIND_VIDEO, 0, 2, 0, b"p-frame"); // seq 1
        assert_eq!(recv_frame(&mut media_b).await.1, 1);
        group.enc().publish(KIND_VIDEO, F_IDR, 3, 0, b"joiner-key"); // seq 2
        assert_eq!(recv_frame(&mut media_b).await.1, 2);
        // The newcomer's first frame is the group keyframe, not the P-frame before it.
        assert_eq!(recv_frame(&mut media_c).await, (F_IDR, 2, 3));

        // One of the original receivers leaves; the rest keep the same group sequence.
        group.remove("a").await.unwrap();
        group.enc().publish(KIND_VIDEO, 0, 4, 0, b"after"); // seq 3
        assert_eq!(recv_frame(&mut media_b).await, (0, 3, 4));
        assert_eq!(recv_frame(&mut media_c).await, (0, 3, 4));

        group.stop().await;
        // Teardown closed every media channel: no receiver task outlives the group.
        assert!(media_b.recv().await.is_none());
        assert!(media_c.recv().await.is_none());
    }

    #[tokio::test]
    async fn joining_a_streaming_group_requests_a_keyframe() {
        let a = fake_rx("a");
        tokio::spawn(answer_start(a.cmd, a.events.clone()));
        let mut group = launch_test("g-20", vec![a.rx]).await;
        assert!(!group.enc().idr_due(Instant::now())); // a fresh group needs no request
        group.enc().publish(KIND_VIDEO, 0, 1, 0, b"one");
        let b = fake_rx("b");
        tokio::spawn(answer_start(b.cmd, b.events.clone()));
        group.add(b.rx).await.unwrap();
        // The joiner starts mid-GOP: the group must produce a keyframe for it.
        assert!(group.enc().idr_due(Instant::now()));
        group.stop().await;
    }

    #[tokio::test]
    async fn a_lagging_web_receiver_asks_the_capture_and_not_only_the_encoder() {
        let a = fake_rx_cap("a", 1);
        tokio::spawn(answer_start(a.cmd, a.events.clone()));
        let group = launch_test("g-21", vec![a.rx]).await;
        assert_eq!(group.capture.idr_asks(), 0);
        // The 1-frame media channel refuses the second frame: the receiver lags. On the KMS
        // path only the capture can produce the next keyframe, so both must be asked.
        for _ in 0..4 {
            group.enc().publish(KIND_VIDEO, 0, 1, 0, &[0u8; 4]);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(group.capture.idr_asks() >= 1);
        assert!(group.enc().idr_due(Instant::now()));
        group.stop().await;
    }

    #[test]
    fn a_receiver_id_of_any_utf8_never_panics_the_group_label() {
        assert_eq!(output_label("g-web-0cbaa3fd"), "gweb0cba");
        assert_eq!(output_label("g-яяя"), "g");
        assert_eq!(output_label(""), "");
    }

    #[tokio::test]
    async fn keyframe_from_one_receiver_requests_group_idr() {
        let a = fake_rx("a");
        let b = fake_rx("b");
        tokio::spawn(answer_start(a.cmd, a.events.clone()));
        tokio::spawn(answer_start(b.cmd, b.events.clone()));
        let group = launch_test("g-4", vec![a.rx, b.rx]).await;
        let enc = group.enc();
        assert!(!enc.idr_due(Instant::now()));
        a.events.send(json!({"t": "keyframe"})).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if enc.idr_due(Instant::now()) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        group.stop().await;
    }

    #[tokio::test]
    async fn stop_from_one_receiver_leaves_the_rest_running() {
        let a = fake_rx("a");
        let b = fake_rx("b");
        tokio::spawn(answer_start(a.cmd, a.events.clone()));
        tokio::spawn(answer_start(b.cmd, b.events.clone()));
        let mut media_a = a.media;
        let mut media_b = b.media;
        let mut group = launch_test("g-5", vec![a.rx, b.rx]).await;
        a.events.send(json!({"t": "stop"})).unwrap();
        // Receiver a's task ends on its own; the group reaps it.
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                group.reap();
                if group.receiver_count() == 1 {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(group.receiver_ids(), vec!["b".to_string()]);
        group.enc().publish(KIND_VIDEO, 0, 9, 0, b"after");
        assert!(recv_frame(&mut media_b).await.2 == 9);
        // a's media channel was dropped with its task.
        assert!(
            tokio::time::timeout(Duration::from_millis(100), media_a.recv())
                .await
                .map(|m| m.is_none())
                .unwrap_or(true)
        );
        group.stop().await;
    }

    #[tokio::test]
    async fn last_receiver_out_stops_the_group() {
        let a = fake_rx("a");
        let b = fake_rx("b");
        tokio::spawn(answer_start(a.cmd, a.events.clone()));
        tokio::spawn(answer_start(b.cmd, b.events.clone()));
        let mut group = launch_test("g-6", vec![a.rx, b.rx]).await;
        let stopped = group.stopped_notify();
        let notified = stopped.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        group.remove("a").await.unwrap();
        group.remove("b").await.unwrap();
        assert_eq!(group.receiver_count(), 0);
        tokio::time::timeout(Duration::from_secs(2), &mut notified)
            .await
            .unwrap();
        group.stop().await;
    }

    #[tokio::test]
    async fn stop_group_closes_everything() {
        let a = fake_rx("a");
        let b = fake_rx("b");
        tokio::spawn(answer_start(a.cmd, a.events.clone()));
        tokio::spawn(answer_start(b.cmd, b.events.clone()));
        let mut media_a = a.media;
        let mut media_b = b.media;
        let group = launch_test("g-7", vec![a.rx, b.rx]).await;
        let enc = group.enc();
        group.stop().await;
        // Every receiver channel is closed once its task is gone.
        assert!(tokio::time::timeout(Duration::from_secs(2), media_a.recv())
            .await
            .unwrap()
            .is_none());
        assert!(tokio::time::timeout(Duration::from_secs(2), media_b.recv())
            .await
            .unwrap()
            .is_none());
        // The group released its encoder and bus; only this test's Arc survives.
        assert_eq!(Arc::strong_count(&enc), 1);
    }

    #[tokio::test]
    async fn mixed_udp_and_web_receivers_share_one_stream() {
        // A web receiver with a one-deep channel: easy to make it lag on purpose.
        let a = fake_rx_cap("a", 1);
        tokio::spawn(answer_start(a.cmd, a.events.clone()));
        let mut a_media = a.media;
        let mut group = launch_test("g-e", vec![a.rx]).await;
        group.enc().publish(KIND_VIDEO, 0, 1, 0, b"first"); // seq 0 fills the browser channel

        // An Android-TV-style receiver with real UDP sockets joins mid-stream.
        let v_sock = udp_sock();
        let au_sock = udp_sock();
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (events, _) = broadcast::channel(16);
        // Hold one sender for the receiver's lifetime: the group only subscribes, and a
        // dropped control-event channel means "receiver gone" (as in run_channel).
        let keep_events = events.clone();
        let ready = json!({
            "t": "ready",
            "video_port": v_sock.local_addr().unwrap().port(),
            "audio_port": au_sock.local_addr().unwrap().port(),
        });
        let ans = tokio::spawn(answer_start_with(cmd_rx, events.clone(), ready));
        group
            .add(NewReceiver {
                id: "tv".into(),
                name: "tv".into(),
                address: Ipv4Addr::LOCALHOST,
                ifname: "lo".into(),
                prefer: "video/avc".into(),
                ws: None,
                cmd: cmd_tx,
                events: keep_events,
            })
            .await
            .unwrap();
        // The control handshake is still the Luch v1 one the Android app speaks.
        let start = ans.await.unwrap();
        assert_eq!(start["mode"], "mirror");
        assert_eq!(start["video"]["codec"], "video/avc");
        assert_eq!(start["video"]["width"], 1920);
        assert_eq!(start["video"]["height"], 1080);
        assert_eq!(start["audio"]["codec"], "audio/opus");

        // Two more frames: the browser lags (its channel is full), UDP keeps delivering.
        group.enc().publish(KIND_VIDEO, F_IDR, 2, 0, b"second"); // seq 1
        group.enc().publish(KIND_VIDEO, 0, 3, 0, b"third"); // seq 2
        assert_eq!(udp_frame(&v_sock).await, (F_IDR, 1, b"second".to_vec()));
        assert_eq!(udp_frame(&v_sock).await, (0, 2, b"third".to_vec()));
        // The newcomer never saw the pre-join frame; the lagging browser only its queued one.
        assert_eq!(recv_frame(&mut a_media).await.1, 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(150), a_media.recv())
                .await
                .map(|m| m.is_none())
                .unwrap_or(true),
            "the lagging browser received frames it should have skipped"
        );
        group.stop().await;
    }

    #[tokio::test]
    async fn stats_expose_group_seq_capture_and_receiver_delivery_state() {
        let a = fake_rx("a");
        tokio::spawn(answer_start(a.cmd, a.events.clone()));
        let group = launch_test("g-d", vec![a.rx]).await;
        let v = group.stats_json();
        // The display-less test pipeline never prerolls, so it reports "ready"; a real
        // capture reports "playing". Either way the field is a state string.
        assert!(matches!(v["capture"].as_str(), Some("playing" | "ready")));
        assert_eq!(v["encoder"]["seq"]["video"], 0);
        group.enc().publish(KIND_VIDEO, 0, 1, 0, b"x");
        assert_eq!(group.stats_json()["encoder"]["seq"]["video"], 1);
        let r = crate::mirror::stats::receiver_stats_json(group.receivers().next().unwrap());
        assert!(r["state"].is_string());
        assert_eq!(r["connected"], true);
        assert!(r["dropped"].is_number() && r["skipped"].is_number());
        assert!(r["idr_requests"].is_number() && r["rate_mbps"].is_number());
        group.stop().await;
    }

    #[tokio::test]
    async fn two_groups_have_independent_seq() {
        let a = fake_rx("a");
        let c = fake_rx("c");
        tokio::spawn(answer_start(a.cmd, a.events.clone()));
        tokio::spawn(answer_start(c.cmd, c.events.clone()));
        let mut media_a = a.media;
        let mut media_c = c.media;
        let g1 = launch_test("g-8a", vec![a.rx]).await;
        let g2 = launch_test("g-8b", vec![c.rx]).await;
        g1.enc().publish(KIND_VIDEO, 0, 1, 0, b"g1-one");
        g1.enc().publish(KIND_VIDEO, 0, 2, 0, b"g1-two");
        g2.enc().publish(KIND_VIDEO, 0, 1, 0, b"g2-one");
        assert_eq!(recv_frame(&mut media_a).await.1, 0);
        assert_eq!(recv_frame(&mut media_a).await.1, 1);
        // g2 has its own counter: g1's frames never touched it.
        assert_eq!(recv_frame(&mut media_c).await.1, 0);
        g1.stop().await;
        g2.stop().await;
    }

    #[tokio::test]
    async fn remove_after_natural_stop_does_not_double_count() {
        let a = fake_rx("a");
        let b = fake_rx("b");
        tokio::spawn(answer_start(a.cmd, a.events.clone()));
        tokio::spawn(answer_start(b.cmd, b.events.clone()));
        let mut group = launch_test("g-9", vec![a.rx, b.rx]).await;
        let stopped = group.stopped_notify();
        let notified = stopped.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        // a leaves on its own; its task decrements the live count.
        a.events.send(json!({"t": "stop"})).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                group.reap();
                if group.receiver_count() == 1 {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        // Removing the same id again must not count it twice: b is still live.
        // (The notify fires on every receiver end by design — the watcher rechecks.)
        group.remove("a").await.unwrap();
        assert_eq!(group.receiver_count(), 1);
        group.remove("b").await.unwrap();
        assert_eq!(group.receiver_count(), 0);
        tokio::time::timeout(Duration::from_secs(2), &mut notified)
            .await
            .unwrap();
        group.stop().await;
    }

    #[test]
    fn hevc_only_when_kms_and_everyone_prefers_it() {
        let rx = |prefer: &str| NewReceiver {
            id: "x".into(),
            name: "x".into(),
            address: Ipv4Addr::LOCALHOST,
            ifname: "lo".into(),
            prefer: prefer.into(),
            ws: None,
            cmd: mpsc::channel(1).0,
            events: broadcast::channel(1).0,
        };
        assert_eq!(choose_codec(true, &[rx("video/hevc")]), "video/hevc");
        assert_eq!(
            choose_codec(true, &[rx("video/hevc"), rx("video/avc")]),
            "video/avc"
        );
        assert_eq!(choose_codec(false, &[rx("video/hevc")]), "video/avc");
        assert_eq!(choose_codec(true, &[]), "video/avc");
    }

    #[tokio::test]
    async fn group_profile_uses_the_chosen_stream_rate() {
        for fps in [30u32, 0] {
            let a = fake_rx("a");
            let ans = tokio::spawn(answer_start(a.cmd, a.events.clone()));
            let group = launch_test_fps("g-fps", vec![a.rx], fps).await;
            let start = ans.await.unwrap();
            assert_eq!(start["video"]["fps"], fps);
            assert_eq!(group.fps(), fps);
            assert_eq!(group.stats_json()["fps_target"], fps);
            group.stop().await;
        }
    }

    #[tokio::test]
    async fn adding_the_same_receiver_twice_is_rejected() {
        let a = fake_rx("a");
        tokio::spawn(answer_start(a.cmd, a.events.clone()));
        let mut group = launch_test("g-dup", vec![a.rx]).await;
        let again = fake_rx("a");
        let err = group.add(again.rx).await.unwrap_err();
        assert!(err.contains("already in group"), "{err}");
        group.stop().await;
    }
}
