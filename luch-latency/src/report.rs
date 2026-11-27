//! The analysis as a file to attach to an issue: JSON for tools, Markdown for people.

use serde_json::{json, Map, Value};

use crate::analyze::{Analysis, Summary};
use crate::pattern::Meta;

fn round(v: f64, digits: i32) -> f64 {
    let k = 10f64.powi(digits);
    (v * k).round() / k
}

fn summary(s: &Summary) -> Value {
    json!({
        "count": s.count, "min": round(s.min, 1), "p05": round(s.p05, 1), "p50": round(s.p50, 1),
        "p95": round(s.p95, 1), "max": round(s.max, 1), "mean": round(s.mean, 1), "jitter": round(s.jitter, 1),
    })
}

fn meta(m: &Option<Meta>) -> Value {
    m.as_ref().map_or(Value::Null, |m| {
        Value::Object(
            m.fields
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect::<Map<_, _>>(),
        )
    })
}

pub fn json(a: &Analysis) -> Value {
    json!({
        "tool": concat!("luch-latency/", env!("CARGO_PKG_VERSION")),
        "video": {"frames": a.frames, "width": a.width, "height": a.height, "fps": round(a.camera_fps, 2),
                  "time_scale": round(a.time_scale, 3)},
        "latency_ms": summary(&a.latency),
        "resolution_ms": round(1000.0 / a.camera_fps / a.time_scale, 2),
        "sender_hz": round(a.sender_hz, 2),
        "tv_frames_shown": round(a.tv_frames_shown, 3),
        "tv": {"hz": a.tv_hz.map(|v| round(v, 2)), "overlay_skipped": a.tv_overlay_skipped.map(|v| round(v, 3)),
               "pacing_ms": a.tv_pacing_ms.map(|v| round(v, 2))},
        "readable": a.readable.iter().map(|(k, v)| json!({"pattern": k, "share": round(*v, 3)})).collect::<Vec<_>>(),
        "sender": meta(&a.sender),
        "receiver": meta(&a.receiver),
        "warnings": a.warnings,
        "samples": a.samples.iter().map(|(n, ms)| json!([n, round(*ms, 2)])).collect::<Vec<_>>(),
    })
}

/// What the QR codes' short keys mean (luchd/src/latency.rs, LatencyPattern.java), in the order people look for them.
const SENDER_KEYS: [(&str, &str); 10] = [
    ("v", "Sender"),
    ("m", "Capture mode"),
    ("c", "Codec"),
    ("b", "Bitrate, Mbit/s"),
    ("r", "Stream size"),
    ("p", "Ping, ms"),
    ("h", "Capture + encode, ms (luchd's own figure)"),
    ("e", "luchd's estimate without the displays, ms"),
    ("hz", "Computer's display, Hz (measured by the page)"),
    ("x", "Refreshes the page missed"),
];
const RECEIVER_KEYS: [(&str, &str); 8] = [
    ("v", "Receiver app"),
    ("mo", "Model"),
    ("a", "Android"),
    ("d", "Decoder"),
    ("dm", "Display mode"),
    ("hz", "TV refresh, Hz (measured by the receiver)"),
    ("s", "Picture surface"),
    ("g", "Low-latency display mode asked for"),
];

fn meta_rows(out: &mut String, m: &Meta, keys: &[(&str, &str)]) {
    for (k, name) in keys {
        if let Some(v) = m.get(k) {
            out.push_str(&format!("| {name} | {v} |\n"));
        }
    }
    for (k, v) in &m.fields {
        if !keys.iter().any(|(key, _)| key == k) {
            out.push_str(&format!("| {k} | {v} |\n"));
        }
    }
}

pub fn markdown(a: &Analysis) -> String {
    let mut s = String::from("## Luch photon-to-photon latency\n\n");
    let res = 1000.0 / a.camera_fps / a.time_scale;
    let l = &a.latency;
    if l.count > 0 {
        s.push_str(&format!(
            "**{:.1} ms** median from the computer's screen to the TV's, {:.1} ms at the 95th percentile \
             ({} frames; one camera frame is {:.1} ms).\n\n",
            l.p50, l.p95, l.count, res
        ));
        s.push_str(
            "| min | p5 | p50 | p95 | max | mean | jitter (sd) |\n|---|---|---|---|---|---|---|\n",
        );
        s.push_str(&format!(
            "| {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} |\n\n",
            l.min, l.p05, l.p50, l.p95, l.max, l.mean, l.jitter
        ));
        if let Some(est) = a
            .sender
            .as_ref()
            .and_then(|m| m.get("est"))
            .and_then(|v| v.parse::<f64>().ok())
        {
            s.push_str(&format!(
                "luchd's own estimate (capture, encode, network, decode) was {est:.1} ms: the remaining {:.1} ms are \
                 the two displays and the receiver's compositor.\n\n",
                l.p50 - est
            ));
        }
    } else {
        s.push_str("**No latency could be measured.**\n\n");
    }
    for w in &a.warnings {
        s.push_str(&format!("> ⚠ {w}\n\n"));
    }
    s.push_str("| Measured | |\n|---|---|\n");
    s.push_str(&format!(
        "| Camera | {}x{}, {:.1} fps, {} frames{} |\n",
        a.width,
        a.height,
        a.camera_fps,
        a.frames,
        if a.time_scale != 1.0 {
            format!(", clock ×{:.2}", a.time_scale)
        } else {
            String::new()
        }
    ));
    if a.sender_hz > 0.0 {
        s.push_str(&format!(
            "| Computer's display, as filmed | {:.2} Hz |\n",
            a.sender_hz
        ));
    }
    if l.count > 0 {
        s.push_str(&format!(
            "| Frames the TV showed of those the computer showed | {:.0}% |\n",
            a.tv_frames_shown * 100.0
        ));
    }
    if let Some(hz) = a.tv_hz {
        s.push_str(&format!("| TV refresh, as filmed | {hz:.2} Hz |\n"));
    }
    if let Some(v) = a.tv_overlay_skipped {
        s.push_str(&format!(
            "| TV refreshes the receiver did not draw | {:.1}% |\n",
            v * 100.0
        ));
    }
    if let Some(v) = a.tv_pacing_ms {
        s.push_str(&format!("| TV frame pacing (sd, ms) | {v:.1} |\n"));
    }
    for (k, v) in &a.readable {
        s.push_str(&format!(
            "| Frames with the {k} readable | {:.0}% |\n",
            v * 100.0
        ));
    }
    if let Some(m) = &a.sender {
        s.push_str("\n| Computer | |\n|---|---|\n");
        meta_rows(&mut s, m, &SENDER_KEYS);
    }
    if let Some(m) = &a.receiver {
        s.push_str("\n| TV | |\n|---|---|\n");
        meta_rows(&mut s, m, &RECEIVER_KEYS);
    }
    s.push_str(&format!(
        "\n<sub>{} · per-frame numbers in the JSON report</sub>\n",
        concat!("luch-latency ", env!("CARGO_PKG_VERSION"))
    ));
    s
}
