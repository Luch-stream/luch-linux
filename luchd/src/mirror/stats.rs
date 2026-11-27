//! Aggregate group statistics (spec §10): encoder metrics from the EncoderSession,
//! transport subscriptions from the FrameBus, delivery metrics per receiver.
//! All reads are lock-free apart from each receiver's latest `stats` message,
//! which is a short-lived clone.

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use super::capture;
use super::encoder;
use super::group::{GroupReceiver, MirrorGroup};

/// D-Bus `GroupStats` payload for one group.
pub fn group_stats_json(g: &MirrorGroup) -> Value {
    let c = g.enc_counters();
    let (video_seq, audio_seq) = g.encoder_seqs();
    let (pace_avg, pace_max) = g.encoder_pacing();
    let (idr_requests, idr_suppressed) = g.encoder_idr();
    json!({
        "id": g.id(),
        "source": g.source_label(),
        "codec": g.codec(),
        "resolution": format!("{}x{}", g.size().width, g.size().height),
        "fps_target": g.fps(),
        "receiver_count": g.receiver_count(),
        "audio": g.audio_profile(),
        "receivers": g.receiver_ids(),
        "capture": g.capture_state(),
        "capture_instances": capture::live_captures(),
        "encoder": {
            "frames": c.frames.load(Ordering::Relaxed),
            "bytes": c.bytes.load(Ordering::Relaxed),
            "live_instances": encoder::live_instances(),
            "seq": {"video": video_seq, "audio": audio_seq},
            "pacing_ms": {"avg": (pace_avg * 10.0).round() / 10.0,
                          "max": (pace_max * 10.0).round() / 10.0},
            "idr": {"requests": idr_requests, "suppressed": idr_suppressed},
        },
        "transport": {
            "video_subscribers": g.bus().video_subscribers(),
            "audio_subscribers": g.bus().audio_subscribers(),
        },
    })
}

/// The per-receiver `sender_stats` payload: the sender side of one stream. Shared by the
/// on-screen overlay tick (group) and D-Bus `Stats`, so the two shapes cannot drift.
pub fn sender_stats_json(
    hevc: bool,
    size: crate::display::Size,
    fps: f64,
    mbps: f64,
    host_ms: f64,
    rtt_ms: f64,
    virtual_monitor: Option<String>,
) -> Value {
    json!({
        "t": "sender_stats",
        "rtt_ms": super::round1(rtt_ms),
        "host_ms": super::round1(host_ms),
        "fps": super::round1(fps),
        "mbps": super::round1(mbps),
        "virtual_monitor": virtual_monitor,
        "encoder": if hevc { "hevc_vaapi" } else { "h264_vaapi" },
        "res": format!("{}x{}", size.width, size.height),
    })
}

/// Per-receiver delivery metrics (spec §5.7): bytes/frames sent, send errors, dropped
/// vs lag-skipped frames, IDR requests, send rate, delivery state, RTT from control
/// pings, and the receiver's own decoder `stats` as-is.
pub fn receiver_stats_json(r: &GroupReceiver) -> Value {
    json!({
        "id": r.id,
        "name": r.name,
        "address": r.address,
        "transport": r.transport,
        "connected": true, // present in the group's map; reaped receivers are gone
        "state": r.stats.state().as_str(),
        "bytes": r.stats.bytes.load(Ordering::Relaxed),
        "frames": r.stats.frames.load(Ordering::Relaxed),
        "send_errors": r.stats.send_errors.load(Ordering::Relaxed),
        "dropped": r.stats.dropped.load(Ordering::Relaxed),
        "skipped": r.stats.skipped.load(Ordering::Relaxed),
        "idr_requests": r.stats.idr_requests.load(Ordering::Relaxed),
        "rate_mbps": (r.stats.rate_bps.load(Ordering::Relaxed) as f64 / 1e6 * 10.0).round() / 10.0,
        "last_seq": r.stats.last_seq.load(Ordering::Relaxed) as u32,
        "rtt_ms": r.rtt_ms(),
        "rx_stats": r.rx_stats.lock().unwrap().clone(),
    })
}
