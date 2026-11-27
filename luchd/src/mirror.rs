//! Screen mirroring: xdg-desktop-portal ScreenCast -> PipeWire -> VA-API H.264 + Opus -> Luch v1 UDP.
//!
//! Latency rules borrowed from Sunshine/Moonlight: newest frame only, no B-frames, one reference,
//! one-frame CPB, IDR only on request, packets sent straight from the encoder thread.

use std::{
    os::fd::OwnedFd,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use ashpd::desktop::{
    screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType, StartCastOptions},
    PersistMode,
};

pub mod capture;
pub mod encoder;
pub mod frame;
pub mod frame_bus;
pub mod group;
pub mod stats;
pub mod transport;

use futures_util::StreamExt;
use gstreamer::{self as gst, prelude::*};
use gstreamer_app as gst_app;
use serde_json::Value;

use crate::display::Size;

pub(crate) static VIRTUAL_SETUP: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const UHD: (u32, u32) = (3840, 2160);
pub const WIDTH: u32 = 1920;
pub const HEIGHT: u32 = 1080;
pub const FPS: u32 = 60;

/// Stream frame-rate target for new groups. Only three rates are meaningful: the screen's
/// own rate (`LUCH_FPS=0`, no cap), 60 (default: bandwidth and decoder headroom) and 30.
/// Anything else falls back to 60 — other values cannot be produced from a fixed-refresh
/// source without duplicating frames. Read once.
pub fn fps() -> u32 {
    static FPS_ENV: std::sync::LazyLock<u32> = std::sync::LazyLock::new(|| {
        std::env::var("LUCH_FPS")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|v| allowed_fps(*v))
            .unwrap_or(FPS)
    });
    *FPS_ENV
}

/// The three rates a fixed-refresh source can actually deliver: its own (`0`), 60 and 30.
pub(crate) fn allowed_fps(v: u32) -> bool {
    matches!(v, 0 | 30 | 60)
}
/// Phase-locked frame-rate gate: accepts at most one frame per 1/fps of stream time and
/// keeps the grid aligned to the accepted frame (144 Hz -> every 3rd frame at 60). Forced
/// frames (IDRs) always pass, and a far clock jump (capture restart) re-phases the grid.
pub(crate) struct FpsGate {
    slice: u64,
    due: Option<u64>,
}

impl FpsGate {
    pub(crate) fn new(fps: u32) -> Self {
        FpsGate {
            slice: if fps > 0 { 1_000_000 / fps as u64 } else { 0 },
            due: None,
        }
    }

    /// `pts_us` is the capture timestamp; `force` marks a frame that must not be dropped.
    pub(crate) fn accept(&mut self, pts_us: u64, force: bool) -> bool {
        if self.slice == 0 {
            return true;
        }
        let take = match self.due {
            None => true,
            Some(d) => pts_us + self.slice / 10 >= d || d.saturating_sub(pts_us) >= 5 * self.slice,
        };
        if !take && !force {
            return false;
        }
        self.due = Some(pts_us.saturating_add(self.slice));
        true
    }
}

pub const VIDEO_KBPS: u32 = 20_000; // at 1920x1080; other sizes scale, see `kbps_for`

/// Video bitrate for a frame size: grows with the square root of the pixel count (4K gets twice
/// the 1080p rate, not four times: Wi-Fi and TV decoders choke on 80 Mbit/s), never below 1080p's.
pub fn kbps_for(w: u32, h: u32) -> u32 {
    let ratio = (w as f64 * h as f64) / (WIDTH as f64 * HEIGHT as f64);
    (VIDEO_KBPS as f64 * ratio.sqrt()).max(VIDEO_KBPS as f64) as u32
}
pub const AUDIO_BPS: u32 = 128_000;
pub const AUDIO_FRAME_MS: u32 = 5;

const HEADER: usize = 24;
const SHARD: usize = 1376; // HEADER + SHARD = 1400, safe under a 1500 MTU

pub const KIND_VIDEO: u8 = 1;
pub const KIND_AUDIO: u8 = 2;
pub const F_IDR: u8 = 1;
pub const F_CONFIG: u8 = 2;

fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.trim().split_once(['x', 'X'])?;
    let (w, h) = (w.trim().parse::<u32>().ok()?, h.trim().parse::<u32>().ok()?);
    // even sizes (4:2:0 chroma), within what encoders and receivers take
    (w >= 64 && h >= 64 && w <= 8192 && h <= 8192).then_some((w & !1, h & !1))
}

/// Frame size of a session. `LUCH_SIZE` unset: 1920x1080 as before. `auto`: what the receiver announced
/// as `max` in its welcome (e.g. a 4K TV). `WxH`: that size, never larger than the receiver's `max`.
pub fn session_size(env: Option<&str>, receiver_max: Option<&str>) -> (u32, u32) {
    let max = receiver_max.and_then(parse_size);
    match env.map(str::trim) {
        None | Some("") => (WIDTH, HEIGHT),
        // A receiver's announced max can be more than its decoder takes (a Google TV Streamer says 4096x2304 and
        // drops a 4096x2304@60 HEVC session), so `auto` stops at UHD.
        Some("auto") => max
            .map(|(w, h)| (w.min(UHD.0), h.min(UHD.1)))
            .unwrap_or((WIDTH, HEIGHT)),
        Some(v) => match (parse_size(v), max) {
            (Some((w, h)), Some((mw, mh))) => (w.min(mw), h.min(mh)),
            (Some(size), None) => size,
            _ => (WIDTH, HEIGHT),
        },
    }
}

/// Restores the monitor's mode when the session ends.
struct ModeGuard {
    output: String,
    orig: String,
}

impl Drop for ModeGuard {
    fn drop(&mut self) {
        let _ = std::process::Command::new("niri")
            .args(["msg", "output", &self.output, "mode", &self.orig])
            .output();
        if let Ok(d) = crate::config_dir() {
            let _ = std::fs::remove_file(d.join("restore-mode"));
        }
    }
}

/// After a crash or restart mid-session: put the monitor's mode back and drop headless outputs we made.
pub fn restore_leftovers() {
    use std::process::Command;
    if let Some(d) = crate::config_dir().ok().map(|d| d.join("restore-mode")) {
        if let Some((out, mode)) = std::fs::read_to_string(&d).ok().and_then(|t| {
            t.trim()
                .split_once(' ')
                .map(|(a, b)| (a.to_string(), b.to_string()))
        }) {
            let _ = Command::new("niri")
                .args(["msg", "output", &out, "mode", &mode])
                .output();
        }
        let _ = std::fs::remove_file(d);
    }
    let Ok(o) = Command::new("niri").args(["msg", "-j", "outputs"]).output() else {
        return;
    };
    let Ok(v) = serde_json::from_slice::<Value>(&o.stdout) else {
        return;
    };
    for name in v
        .as_object()
        .into_iter()
        .flat_map(|m| m.keys())
        .filter(|n| n.starts_with("LUCH-"))
    {
        let _ = Command::new("niri-virtual")
            .args(["msg", "remove-virtual-output", name])
            .output();
    }
}

fn mode_spec(m: &Value) -> Option<(u64, u64, String)> {
    let (w, h, r) = (
        m["width"].as_u64()?,
        m["height"].as_u64()?,
        m["refresh_rate"].as_u64()?,
    );
    Some((w, h, format!("{w}x{h}@{:.3}", r as f64 / 1000.0)))
}

/// Switch the biggest real output to a 16:9 mode at least `want_w` wide (else the widest 16:9),
/// highest refresh first. `None` when already 16:9, or Niri doesn't answer.
async fn switch_to_16x9(want_w: u32) -> Option<ModeGuard> {
    let out = tokio::process::Command::new("niri")
        .args(["msg", "-j", "outputs"])
        .output()
        .await
        .ok()?;
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    let area = |o: &Value| {
        o["modes"]
            .get(o["current_mode"].as_u64().unwrap_or(0) as usize)
            .and_then(mode_spec)
            .map_or(0, |(w, h, _)| w * h)
    };
    let (name, o) = v
        .as_object()?
        .iter()
        .filter(|(n, _)| !n.starts_with("LUCH-"))
        .max_by_key(|(_, o)| area(o))?;
    let modes = o["modes"].as_array()?;
    let (cw, ch, orig) = modes
        .get(o["current_mode"].as_u64()? as usize)
        .and_then(mode_spec)?;
    if cw * 9 == ch * 16 {
        return None;
    }
    let want = want_w as u64;
    let best = modes
        .iter()
        .filter_map(|m| {
            mode_spec(m).map(|(w, h, spec)| (w, h, m["refresh_rate"].as_u64().unwrap_or(0), spec))
        })
        .filter(|(w, h, _, _)| w * 9 == h * 16)
        .max_by_key(|(w, _, r, _)| (*w >= want, if *w >= want { u64::MAX - w } else { *w }, *r))?;
    let st = tokio::process::Command::new("niri")
        .args(["msg", "output", name, "mode", &best.3])
        .status()
        .await
        .ok()?;
    if !st.success() {
        return None;
    }
    eprintln!(
        "[luchd] monitor {name}: {orig} -> {} for the mirror",
        best.3
    );
    if let Ok(d) = crate::config_dir() {
        let _ = std::fs::write(d.join("restore-mode"), format!("{name} {orig}"));
    }
    tokio::time::sleep(Duration::from_millis(1200)).await; // modeset
    Some(ModeGuard {
        output: name.clone(),
        orig,
    })
}

/// A headless Niri output created for one session, and the Mutter-compatible ScreenCast session on it.
struct NiriVirtual {
    _output: VirtualOutput,
    session: zbus::Proxy<'static>,
    node: u32,
}

struct VirtualOutput(String);

impl Drop for VirtualOutput {
    fn drop(&mut self) {
        let _ = std::process::Command::new("niri-virtual")
            .args(["msg", "remove-virtual-output", &self.0])
            .output();
    }
}

/// Capture sources this machine can actually use, best first. Niri-only ones appear only under Niri.
static LOCAL_AUDIO: AtomicBool = AtomicBool::new(true);

/// Whether the sender keeps playing the sound itself while mirroring (default yes).
pub fn local_audio() -> bool {
    LOCAL_AUDIO.load(Ordering::Relaxed)
}

pub fn set_local_audio(on: bool) {
    LOCAL_AUDIO.store(on, Ordering::Relaxed);
    if let Ok(d) = crate::config_dir() {
        let _ = std::fs::write(d.join("local-audio"), if on { "1" } else { "0" });
    }
}

fn pactl(args: &[&str]) -> Option<String> {
    let o = std::process::Command::new("pactl")
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Index of the sink called `name` (first column of `pactl list short sinks`).
fn sink_index(name: &str) -> Option<String> {
    pactl(&["list", "short", "sinks"])?.lines().find_map(|l| {
        let mut c = l.split('\t');
        let (idx, n) = (c.next()?, c.next()?);
        (n == name).then(|| idx.to_string())
    })
}

/// Ids of the playback streams currently on sink `idx`.
fn inputs_on(idx: &str) -> Vec<String> {
    pactl(&["list", "short", "sink-inputs"])
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut c = l.split('\t');
            let (id, sink) = (c.next()?, c.next()?);
            (sink == idx).then(|| id.to_string())
        })
        .collect()
}

const CAST_SINK: &str = "luch_cast";

/// Undo `LocalMute`: default sink and streams back where they were, the null sink unloaded.
fn restore_audio_state(prev: &str, module: &str) {
    if let Some(cast) = sink_index(CAST_SINK) {
        if sink_index(prev).is_some() {
            for id in inputs_on(&cast) {
                let _ = pactl(&["move-sink-input", &id, prev]);
            }
        }
    }
    if sink_index(prev).is_some() {
        let _ = pactl(&["set-default-sink", prev]);
    }
    let _ = pactl(&["unload-module", module]);
}

/// While a session runs the sender's own sound goes to a null sink, which `luchd` records, so the
/// receiver plays it and the sender stays silent (no echo with the receiver, no Bluetooth delay).
struct LocalMute {
    prev: String,
    module: String,
}

impl LocalMute {
    fn new() -> Option<Self> {
        let prev = pactl(&["get-default-sink"])?;
        let module = pactl(&[
            "load-module",
            "module-null-sink",
            &format!("sink_name={CAST_SINK}"),
            "sink_properties=device.description=Luch",
        ])?;
        let mute = LocalMute {
            prev: prev.clone(),
            module: module.clone(),
        };
        if let Some(d) = crate::config_dir().ok() {
            let _ = std::fs::write(d.join("restore-audio"), format!("{prev} {module}"));
        }
        let (Some(old), Some(_)) = (sink_index(&prev), sink_index(CAST_SINK)) else {
            return Some(mute);
        };
        let moving = inputs_on(&old);
        let _ = pactl(&["set-default-sink", CAST_SINK]);
        for id in moving {
            let _ = pactl(&["move-sink-input", &id, CAST_SINK]);
        }
        Some(mute)
    }
}

impl Drop for LocalMute {
    fn drop(&mut self) {
        restore_audio_state(&self.prev, &self.module);
        if let Ok(d) = crate::config_dir() {
            let _ = std::fs::remove_file(d.join("restore-audio"));
        }
    }
}

static MUTE: std::sync::Mutex<Option<LocalMute>> = std::sync::Mutex::new(None);

/// Make the sender's sound match the setting: silent while a session runs and LocalAudio is off.
/// Called when a session starts or ends and when the setting changes. One shared mute, so with
/// several sessions at once the sender is silent as long as any of them runs.
pub fn sync_mute(session_active: bool) {
    let mut m = MUTE.lock().unwrap();
    if session_active && !local_audio() {
        if m.is_none() {
            *m = LocalMute::new();
        }
    } else {
        *m = None;
    }
}

/// Held by a running session.
static MUTE_SESSIONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
struct MuteSession;

impl MuteSession {
    fn start() -> Self {
        MUTE_SESSIONS.fetch_add(1, Ordering::SeqCst);
        sync_mute(true);
        MuteSession
    }
}

impl Drop for MuteSession {
    fn drop(&mut self) {
        sync_mute(MUTE_SESSIONS.fetch_sub(1, Ordering::SeqCst) > 1);
    }
}

/// At start: read the saved setting, and undo a mute a crash left behind.
pub fn restore_audio() {
    let Ok(d) = crate::config_dir() else { return };
    LOCAL_AUDIO.store(
        std::fs::read_to_string(d.join("local-audio"))
            .map(|t| t.trim() != "0")
            .unwrap_or(true),
        Ordering::Relaxed,
    );
    let f = d.join("restore-audio");
    if let Some((prev, module)) = std::fs::read_to_string(&f).ok().and_then(|t| {
        t.trim()
            .split_once(' ')
            .map(|(a, b)| (a.to_string(), b.to_string()))
    }) {
        restore_audio_state(&prev, &module);
    }
    let _ = std::fs::remove_file(f);
}

/// Capture sources this machine can actually use, best first.
pub fn available_sources() -> Vec<&'static str> {
    let on_path = |name: &str| {
        std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
            || std::env::current_exe()
                .ok()
                .and_then(|e| e.parent().map(|d| d.join(name).is_file()))
                .unwrap_or(false)
    };
    let niri = std::env::var_os("NIRI_SOCKET").is_some() && on_path("niri");
    let mut v = Vec::new();
    if on_path("luch-kms") || kms_socket().is_some() {
        v.push("fast");
        if niri {
            v.push("fast169");
        }
    }
    if !niri || niri_virtual_available() {
        v.push("virtual");
    }
    v.extend(["monitor", "window", "audio"]);
    v
}

pub fn niri_virtual_available() -> bool {
    std::env::var_os("NIRI_SOCKET").is_some()
        && std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("niri-virtual").is_file()))
}

async fn niri_virtual(name: &str, (w, h): (u32, u32)) -> Result<NiriVirtual, String> {
    use std::collections::HashMap;
    use zbus::zvariant::{OwnedObjectPath, Value};
    let out = tokio::process::Command::new("niri-virtual")
        .args([
            "msg",
            "create-virtual-output",
            "--name",
            name,
            "--width",
            &w.to_string(),
            "--height",
            &h.to_string(),
            "--refresh-rate",
            &FPS.to_string(),
        ])
        .output()
        .await
        .map_err(|e| format!("niri-virtual: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "niri-virtual: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let output = VirtualOutput(name.to_string());
    // The guard removes the output on every failure below.
    let e = |e: zbus::Error| format!("niri screencast: {e}");
    let conn = zbus::Connection::session().await.map_err(e)?;
    const DEST: &str = "org.gnome.Mutter.ScreenCast";
    let sc = zbus::Proxy::new(&conn, DEST, "/org/gnome/Mutter/ScreenCast", DEST)
        .await
        .map_err(e)?;
    let opts: HashMap<&str, Value> = HashMap::new();
    let spath: OwnedObjectPath = sc.call("CreateSession", &(opts,)).await.map_err(e)?;
    let session = zbus::Proxy::new(&conn, DEST, spath, "org.gnome.Mutter.ScreenCast.Session")
        .await
        .map_err(e)?;
    let mut ropts: HashMap<&str, Value> = HashMap::new();
    ropts.insert("cursor-mode", Value::U32(1)); // embedded
    let stream_path: OwnedObjectPath = session
        .call("RecordMonitor", &(name, ropts))
        .await
        .map_err(e)?;
    let stream = zbus::Proxy::new(
        &conn,
        DEST,
        stream_path,
        "org.gnome.Mutter.ScreenCast.Stream",
    )
    .await
    .map_err(e)?;
    let mut added = stream
        .receive_signal("PipeWireStreamAdded")
        .await
        .map_err(e)?;
    session.call::<_, _, ()>("Start", &()).await.map_err(e)?;
    let msg = tokio::time::timeout(Duration::from_secs(5), added.next())
        .await
        .map_err(|_| "niri screencast: no stream".to_string())?
        .ok_or("niri screencast: closed")?;
    let node: u32 = msg
        .body()
        .deserialize()
        .map_err(|e| format!("niri screencast: {e}"))?;
    Ok(NiriVirtual {
        _output: output,
        session,
        node,
    })
}

/// Holds the screen awake (org.freedesktop.ScreenSaver, as KDE, GNOME and Steam answer it) while a session
/// runs: a screen that goes to sleep has nothing to capture.
struct ScreenAwake(Option<(zbus::Connection, String, u32)>);

impl ScreenAwake {
    async fn start() -> Self {
        let Ok(conn) = zbus::Connection::session().await else {
            return ScreenAwake(None);
        };
        for path in ["/org/freedesktop/ScreenSaver", "/ScreenSaver"] {
            let Ok(p) = zbus::Proxy::new(
                &conn,
                "org.freedesktop.ScreenSaver",
                path,
                "org.freedesktop.ScreenSaver",
            )
            .await
            else {
                continue;
            };
            if let Ok(cookie) = p
                .call::<_, _, u32>("Inhibit", &("Luch", "Mirroring to a receiver"))
                .await
            {
                return ScreenAwake(Some((conn, path.to_string(), cookie)));
            }
        }
        ScreenAwake(None)
    }
}

impl Drop for ScreenAwake {
    fn drop(&mut self) {
        if let Some((conn, path, cookie)) = self.0.take() {
            tokio::spawn(async move {
                if let Ok(p) = zbus::Proxy::new(
                    &conn,
                    "org.freedesktop.ScreenSaver",
                    path,
                    "org.freedesktop.ScreenSaver",
                )
                .await
                {
                    let _ = p.call::<_, _, ()>("UnInhibit", &(cookie,)).await;
                }
            });
        }
    }
}

/// Messages to the control-channel task.
#[derive(Debug)]
pub enum Cmd {
    Bye,
    Send(Value),
}

/// Luch v1 datagram header (24 bytes, big-endian).
pub fn header(
    kind: u8,
    flags: u8,
    idx: u16,
    shards: u16,
    seq: u32,
    pts_us: u32,
    len: u32,
    host_lat: u16,
) -> [u8; HEADER] {
    let mut h = [0u8; HEADER];
    h[0] = kind;
    h[1] = flags;
    h[2..4].copy_from_slice(&idx.to_be_bytes());
    h[4..6].copy_from_slice(&shards.to_be_bytes());
    // 6..8 parity_shards = 0 (no FEC yet)
    h[8..12].copy_from_slice(&seq.to_be_bytes());
    h[12..16].copy_from_slice(&pts_us.to_be_bytes());
    h[16..20].copy_from_slice(&len.to_be_bytes());
    h[20..22].copy_from_slice(&host_lat.to_be_bytes());
    h
}

/// Shards per burst before a short pause, so a big IDR doesn't overflow the Wi-Fi queue.
/// The pause itself lives in the per-receiver sender task (transport.rs), never here.
pub(crate) const BURST: usize = 16;
pub(crate) const BURST_GAP: Duration = Duration::from_micros(250);

/// 19-byte OpusHead (RFC 7845) for 48 kHz stereo: MediaCodec CSD-0.
pub fn opus_head() -> [u8; 19] {
    let mut h = [0u8; 19];
    h[..8].copy_from_slice(b"OpusHead");
    h[8] = 1; // version
    h[9] = 2; // channels
    h[10..12].copy_from_slice(&312u16.to_le_bytes()); // pre-skip (opusenc lookahead)
    h[12..16].copy_from_slice(&48_000u32.to_le_bytes());
    h
}

async fn portal(
    source: SourceType,
) -> Result<
    (
        Screencast,
        ashpd::desktop::Session<Screencast>,
        OwnedFd,
        u32,
    ),
    String,
> {
    let e = |e: ashpd::Error| format!("portal: {e}");
    let token_name = if source == SourceType::VIRTUAL {
        "restore-token-virtual"
    } else if source == SourceType::WINDOW {
        "restore-token-window"
    } else {
        "restore-token"
    };
    let token_path = crate::config_dir()
        .map(|d| d.join(token_name))
        .map_err(|e| e.to_string())?;
    let token = std::fs::read_to_string(&token_path)
        .ok()
        .or_else(|| {
            token_path
                .parent()
                .and_then(|d| std::fs::read_to_string(d.join("restore-token")).ok())
        })
        .filter(|t| !t.trim().is_empty());
    let sc = Screencast::new().await.map_err(e)?;
    // Old installations shared one token across source types. If it restores another type,
    // close it and ask for the requested source instead of streaming the wrong desktop.
    for restore in [token.as_deref().map(str::trim), None] {
        let session = sc.create_session(Default::default()).await.map_err(e)?;
        let opts = SelectSourcesOptions::default()
            .set_cursor_mode(CursorMode::EMBEDDED)
            .set_sources(source)
            .set_multiple(false)
            .set_persist_mode(PersistMode::ExplicitlyRevoked)
            .set_restore_token(restore);
        sc.select_sources(&session, opts).await.map_err(e)?;
        let streams = sc
            .start(&session, None, StartCastOptions::default())
            .await
            .map_err(e)?
            .response()
            .map_err(e)?;
        let stream = streams
            .streams()
            .first()
            .ok_or("portal: no stream selected")?;
        if stream.source_type().is_some_and(|actual| actual != source) {
            let _ = session.close().await;
            continue;
        }
        if let Some(t) = streams.restore_token() {
            let _ = std::fs::write(&token_path, t);
        }
        let node = stream.pipe_wire_node_id();
        let fd = sc
            .open_pipe_wire_remote(&session, Default::default())
            .await
            .map_err(e)?;
        return Ok((sc, session, fd, node));
    }
    Err("portal returned a different source type".into())
}

/// The `format=DMA_DRM` values `vapostproc`'s src pad offers right now, formatted as the
/// `drm-format={...}` list the pipeline needs. `None` when the element (or its caps)
/// is unavailable — then the caller keeps its fallback list.
fn offered_drm_formats() -> Option<String> {
    use gstreamer::prelude::*;
    let e = gst::ElementFactory::make("vapostproc").build().ok()?;
    let tpl = e.pad_template("src")?;
    let caps = tpl.caps();
    let mut formats: Vec<String> = Vec::new();
    for s in caps.iter() {
        let is_dma = s.get::<&str>("format").is_ok_and(|f| f == "DMA_DRM");
        if !is_dma {
            continue;
        }
        if let Ok(list) = s.get::<gstreamer::List>("drm-format") {
            for v in list.iter() {
                // The VA-API encoder takes NV12 zero-copy only; keep the driver's modifiers.
                if let Ok(f) = v.get::<&str>() {
                    if f.starts_with("NV12") {
                        formats.push(format!("\"{f}\""));
                    }
                }
            }
        }
    }
    (!formats.is_empty()).then(|| format!("{{{}}}", formats.join(",")))
}

/// Video from the portal (`Some((fd, node))`) plus audio, or audio only when luch-kms does video.
fn pipeline(
    portal: Option<(Option<i32>, u32)>,
    size: Size,
    virtual_source: bool,
) -> Result<gst::Pipeline, String> {
    let keepalive = 2000 / fps().max(1); // resend the last frame at fps/2 when the screen is static
                                         // ponytail: the Intel tiled NV12 modifier is what vapostproc offers here; make it a negotiated
                                         // list when other GPUs matter.
                                         // Newer gst-plugins-bad dropped vapostproc's `scale-method`; only pass it where it exists.
    let has_scale = gst::ElementFactory::make("vapostproc")
        .build()
        .is_ok_and(|e| e.find_property("scale-method").is_some());
    let scale = if has_scale {
        format!(
            "scale-method={}",
            std::env::var("LUCH_SCALE").unwrap_or_else(|_| "fast".into())
        )
    } else {
        String::new()
    };
    // DRM modifiers vapostproc actually offers on this GPU (it differs per driver: the
    // Intel tiled list does not negotiate on AMD and vice versa). LUCH_DRM_FORMAT overrides.
    let drm = std::env::var("LUCH_DRM_FORMAT")
        .map(|d| format!("\"{d}\""))
        .unwrap_or_else(|_| {
            offered_drm_formats().unwrap_or_else(|| {
                "{\"NV12:0x0100000000000009\",\"NV12:0x0200000010401b04\",\"NV12:0x0200000000082305\",\"NV12:0x0\"}".into()
            })
        });
    // Zero copy: compositor DMA-BUF -> GPU conversion -> DMA-BUF -> VA-API encoder.
    let (width, height) = (size.width, size.height);
    let capture_size = if virtual_source {
        format!(",width={width},height={height}")
    } else {
        String::new()
    };
    let video = portal.map(|(fd, node)| (fd.map(|f| format!("fd={f}")).unwrap_or_default(), node)).map(|(fd, node)| format!(
        "pipewiresrc {fd} path={node} do-timestamp=true always-copy=false keepalive-time={keepalive} \
           ! video/x-raw(memory:DMABuf){capture_size} ! queue max-size-buffers=1 max-size-bytes=0 max-size-time=0 leaky=downstream \
           ! vapostproc {scale} \
           ! video/x-raw(memory:DMABuf),format=DMA_DRM,drm-format={drm},width={width},height={height} \
           ! appsink name=v sync=false max-buffers=1 drop=true "
    )).unwrap_or_default();
    let desc = format!(
        "{video} pulsesrc device=@DEFAULT_MONITOR@ do-timestamp=true buffer-time=10000 latency-time={lat} \
           ! audioconvert ! audioresample ! audio/x-raw,rate=48000,channels=2 \
           ! opusenc audio-type=restricted-lowdelay frame-size={AUDIO_FRAME_MS} bitrate={AUDIO_BPS} bitrate-type=cbr \
           ! appsink name=a sync=false",
        lat = AUDIO_FRAME_MS * 1000,
    );
    gst::parse::launch(&desc)
        .map_err(|e| format!("pipeline: {e}"))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| "pipeline: not a pipeline".to_string())
}

/// Microseconds of pipeline running time for `pts`, and capture->now latency in 0.1 ms.
fn timing(sink: &gst_app::AppSink, pts: Option<gst::ClockTime>) -> (u64, u16) {
    // GstVideoEncoder shifts output PTS by 1000 h so DTS can't go negative; undo it so video and
    // audio share the pipeline running-time base again.
    const ENC_OFFSET: gst::ClockTime = gst::ClockTime::from_seconds(1000 * 3600);
    let pts = pts.unwrap_or(gst::ClockTime::ZERO);
    let pts = if pts >= ENC_OFFSET {
        pts - ENC_OFFSET
    } else {
        pts
    };
    let now = sink.current_running_time().unwrap_or(pts);
    let lat = now.saturating_sub(pts).useconds() / 100;
    (pts.useconds(), lat.min(u16::MAX as u64) as u16)
}

/// fd, modifier, plane offsets and pitches of an NV12 DMA-BUF sample.
fn dmabuf_of(sample: &gst::Sample) -> Result<crate::enc::Nv12Dmabuf, String> {
    let buf = sample.buffer().ok_or("no buffer")?;
    let caps = sample.caps().ok_or("no caps")?;
    let drm = caps
        .structure(0)
        .and_then(|s| s.get::<String>("drm-format").ok())
        .ok_or("no drm-format")?;
    let (fourcc, modifier) = drm.split_once(':').unwrap_or((&drm, "0x0"));
    if fourcc != "NV12" {
        return Err(format!("expected NV12, got {drm}"));
    }
    let modifier =
        u64::from_str_radix(modifier.trim_start_matches("0x"), 16).map_err(|e| e.to_string())?;
    if buf.n_memory() != 1 {
        return Err(format!("{} memories, expected 1", buf.n_memory()));
    }
    let mem = buf.peek_memory(0);
    let dmabuf = mem
        .downcast_memory_ref::<gstreamer_allocators::DmaBufMemory>()
        .ok_or("not a DMA-BUF")?;
    let meta = buf
        .meta::<gstreamer_video::VideoMeta>()
        .ok_or("no video meta")?;
    let (off, stride) = (meta.offset(), meta.stride());
    Ok(crate::enc::Nv12Dmabuf {
        fd: dmabuf.fd(),
        modifier,
        offsets: [off[0] as u32, off[1] as u32],
        pitches: [stride[0] as u32, stride[1] as u32],
    })
}

/// Hooks the appsinks to the group's EncoderSession: capture -> encode -> FrameBus,
/// right on the capture thread. IDR requests and OpusHead resends live inside the
/// EncoderSession. Without a "v" sink only audio is wired.
pub(crate) fn wire(p: &gst::Pipeline, enc: encoder::EncSession, fps: u32) {
    let warned = AtomicBool::new(false);
    let dbg = Mutex::new((0f64, 0f64, 0u32));
    // Screen refresh can be 120/144 Hz while the stream targets the group rate: forwarding
    // every captured frame multiplies encoder and decoder load and browser decoders fall
    // behind indefinitely.
    let gate = Mutex::new(FpsGate::new(fps));
    let asink = p
        .by_name("a")
        .unwrap()
        .downcast::<gst_app::AppSink>()
        .unwrap();

    let enc_v = enc.clone();
    if let Some(vsink) = p
        .by_name("v")
        .and_then(|v| v.downcast::<gst_app::AppSink>().ok())
    {
        vsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                // vapostproc only hands out DMA-BUFs to a peer that understands VideoMeta (plane offsets/strides).
                .propose_allocation(|_, query| {
                    query.add_allocation_meta::<gstreamer_video::VideoMeta>(None);
                    true
                })
                .new_sample(move |s| {
                    // Runs on the capture thread: encode and publish right here, no queue in between.
                    let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let report = |e: String| {
                        if !warned.swap(true, Ordering::Relaxed) {
                            eprintln!("[luchd] video: {e}");
                        }
                    };
                    let frame = match dmabuf_of(&sample) {
                        Ok(f) => f,
                        Err(e) => {
                            report(e);
                            return Ok(gst::FlowSuccess::Ok);
                        }
                    };
                    let pts_rt = sample.buffer().and_then(|b| b.pts());
                    let probe = std::env::var_os("LUCH_PROBE").is_some();
                    let at_sink = if probe { timing(s, pts_rt).1 } else { 0 };
                    let (pts, _) = timing(s, pts_rt);
                    if !gate.lock().unwrap().accept(pts, enc_v.idr_pending()) {
                        return Ok(gst::FlowSuccess::Ok);
                    }
                    if let Err(e) = enc_v.encode_video(&frame, pts as i64, || timing(s, pts_rt).1) {
                        // Encoder open/encode failure: stop the pipeline like the old
                        // FlowError::Error path, so the run loop reports and cleans up.
                        report(e);
                        return Err(gst::FlowError::Error);
                    }
                    if probe {
                        let mut a = dbg.lock().unwrap();
                        a.0 += at_sink as f64 / 10.0;
                        a.1 += timing(s, pts_rt).1 as f64 / 10.0;
                        a.2 += 1;
                        if a.2 == 60 {
                            eprintln!(
                                "[probe] appsink entry {:.1} ms, after encode {:.1} ms",
                                a.0 / 60.0,
                                a.1 / 60.0
                            );
                            *a = (0.0, 0.0, 0);
                        }
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
    }
    let enc_a = enc.clone();
    asink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |s| {
                let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buf = sample.buffer().ok_or(gst::FlowError::Error)?;
                let map = buf.map_readable().map_err(|_| gst::FlowError::Error)?;
                let (pts, lat) = timing(s, buf.pts());
                enc_a.publish_audio(&map, pts as u32, lat);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
}

/// Where a privileged `luch-kms --serve` listens, when there is one (env `LUCH_KMS_SOCKET`, else
/// `$XDG_RUNTIME_DIR/luch/kms.sock`): used by a luchd that cannot run luch-kms itself (a Flatpak sandbox).
pub fn kms_socket() -> Option<std::path::PathBuf> {
    let p = std::env::var_os("LUCH_KMS_SOCKET")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_RUNTIME_DIR")
                .map(|d| std::path::PathBuf::from(d).join("luch/kms.sock"))
        })?;
    p.exists().then_some(p)
}

/// The KMS grabber: a child process (or a `luch-kms --serve` connection) and the thread
/// that reads its encoded stream. The thread is owned here, so a group stop kills the
/// helper, wakes the reader and joins it — no frame can be published after teardown.
pub(crate) struct Grabber {
    kind: GrabberKind,
    reader: Option<std::thread::JoinHandle<()>>,
    /// Set before an intentional stop, so an EOF is not reported as a dead helper.
    stopping: Arc<std::sync::atomic::AtomicBool>,
}

enum GrabberKind {
    Child(std::process::Child),
    Socket(std::os::unix::net::UnixStream),
}

impl Grabber {
    /// Ask for an IDR.
    fn idr(&mut self) {
        use std::io::Write;
        let _ = match &mut self.kind {
            GrabberKind::Child(c) => c
                .stdin
                .as_mut()
                .map(|s| s.write_all(b"k"))
                .unwrap_or(Ok(())),
            GrabberKind::Socket(s) => s.write_all(b"k"),
        };
    }

    fn stop(mut self) {
        self.stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        match &mut self.kind {
            GrabberKind::Child(c) => {
                let _ = c.kill();
                let _ = c.wait();
            }
            GrabberKind::Socket(s) => {
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Lowest-latency video: luch-kms (KMS scanout -> EGL NV12 -> h264_vaapi) as a child, or through its
/// socket, publishing its packets into the group's EncoderSession as they come. `k` on its
/// input forces an IDR.
pub(crate) fn spawn_kms(
    enc: encoder::EncSession,
    hevc: bool,
    size: Size,
    fps: u32,
    on_dead: impl Fn() + Send + 'static,
) -> Result<Grabber, String> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    let kbps = kbps_for(size.width, size.height);
    // `--fps` is the helper's keepalive pace (it republishes a static screen at fps/2) and
    // must be nonzero; the grab loop itself sends every scanout flip. The group's rate is
    // applied below by `FpsGate`, so fps=0 really forwards every captured frame here.
    let kms_fps = if fps == 0 { FPS } else { fps };
    let opts = format!(
        "--size {}x{} --fps {kms_fps} --kbps {kbps} --codec {}",
        size.width,
        size.height,
        if hevc { "hevc" } else { "h264" }
    );
    let (kind, mut out): (GrabberKind, Box<dyn Read + Send>) =
        match kms_socket().and_then(|p| std::os::unix::net::UnixStream::connect(p).ok()) {
            Some(mut s) => {
                writeln!(s, "{opts}").map_err(|e| format!("luch-kms socket: {e}"))?;
                let out = s.try_clone().map_err(|e| format!("luch-kms socket: {e}"))?;
                (GrabberKind::Socket(s), Box::new(out))
            }
            None => {
                // LUCH_KMS points at a helper outside the checkout: file capabilities do not
                // work on a nosuid mount (many /home setups), so it is often installed to
                // /usr/local/bin with setcap instead of living next to luchd.
                let exe = std::env::var_os("LUCH_KMS")
                    .map(std::path::PathBuf::from)
                    .or_else(|| {
                        std::env::current_exe()
                            .ok()
                            .and_then(|p| p.parent().map(|d| d.join("luch-kms")))
                    });
                let exe = exe
                    .filter(|p| p.exists())
                    .unwrap_or_else(|| "luch-kms".into());
                let mut child = Command::new(&exe)
                    .args(opts.split(' '))
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .spawn()
                    .map_err(|e| format!("{}: {e}", exe.display()))?;
                let out = child.stdout.take().unwrap();
                (GrabberKind::Child(child), Box::new(out))
            }
        };
    let stopping = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_flag = stopping.clone();
    let reader = std::thread::spawn(move || {
        let mut hdr = [0u8; 13];
        let mut buf = Vec::new();
        let mut base: Option<u64> = None;
        // Same gate as the portal path: a 144 Hz scanout must not send 140 fps when the
        // group asked for 60. IDRs always pass (they carry SPS/PPS).
        let mut gate = FpsGate::new(fps);
        while out.read_exact(&mut hdr).is_ok() {
            let len = u32::from_be_bytes(hdr[0..4].try_into().unwrap()) as usize;
            let idr = hdr[4] & 1 != 0;
            let cap = u64::from_be_bytes(hdr[5..13].try_into().unwrap());
            buf.resize(len, 0);
            if out.read_exact(&mut buf).is_err() {
                break;
            }
            if !gate.accept(cap, idr) {
                continue;
            }
            let now = mono_us();
            let base = *base.get_or_insert(cap);
            let lat = (now.saturating_sub(cap) / 100).min(u16::MAX as u64) as u16; // scanout flip -> encoded
                                                                                   // luch-kms output is the group's encoded stream: the EncoderSession assigns
                                                                                   // the group-wide seq and sets the OpusHead flag on IDR via publish().
            enc.publish(
                KIND_VIDEO,
                if idr { F_IDR } else { 0 },
                (cap - base) as u32,
                lat,
                &buf,
            );
        }
        // EOF or a broken pipe. An intentional stop is flagged before the helper is
        // killed, so only a helper that died on its own reaches the group.
        if !stop_flag.load(std::sync::atomic::Ordering::SeqCst) {
            on_dead();
        }
    });
    Ok(Grabber {
        kind,
        reader: Some(reader),
        stopping,
    })
}

fn mono_us() -> u64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid out-pointer for clock_gettime.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    t.tv_sec as u64 * 1_000_000 + t.tv_nsec as u64 / 1000
}

/// Debug: average (running time - capture pts) at the src pad of each video element, printed every second.
pub(crate) fn probe_stages(p: &gst::Pipeline) {
    const ENC_OFFSET: gst::ClockTime = gst::ClockTime::from_seconds(1000 * 3600);
    let mut it = p.iterate_recurse();
    while let Ok(Some(el)) = it.next() {
        let name = el.name().to_string();
        if ![
            "pipewiresrc",
            "queue",
            "capsfilter",
            "vapostproc",
            "vah264enc",
            "h264parse",
        ]
        .iter()
        .any(|k| name.starts_with(k))
        {
            continue;
        }
        let Some(pad) = el.static_pad("src") else {
            continue;
        };
        let acc = std::sync::Mutex::new((0f64, 0u32, std::time::Instant::now()));
        let el2 = el.clone();
        pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            if let Some(buf) = info.buffer() {
                if let (Some(pts), Some(now)) = (buf.pts(), el2.current_running_time()) {
                    let pts = if pts >= ENC_OFFSET {
                        pts - ENC_OFFSET
                    } else {
                        pts
                    };
                    let mut a = acc.lock().unwrap();
                    a.0 += now.saturating_sub(pts).nseconds() as f64 / 1e6;
                    a.1 += 1;
                    if a.2.elapsed().as_secs() >= 1 {
                        eprintln!(
                            "[probe] {:<14} {:6.1} ms  ({} bufs)",
                            el2.name(),
                            a.0 / a.1 as f64,
                            a.1
                        );
                        *a = (0.0, 0, std::time::Instant::now());
                    }
                }
            }
            gst::PadProbeReturn::Ok
        });
    }
}

pub(crate) fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A helper that publishes one key frame and exits: what a crash or a lost KMS looks
    /// like from luchd. Its socket path is pointed at a nonexistent file so the child
    /// branch is taken even on a machine that runs `luch-kms --serve`.
    #[test]
    fn an_unexpected_kms_eof_reaches_the_group_and_the_reader_is_joined() {
        let dir = std::env::temp_dir().join(format!("luch-kms-eof-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut frame = Vec::new();
        frame.extend_from_slice(&1u32.to_be_bytes()); // payload length
        frame.push(1); // key frame
        frame.extend_from_slice(&0u64.to_be_bytes()); // capture timestamp
        frame.push(7); // payload
        std::fs::write(dir.join("frame.bin"), frame).unwrap();
        let helper = dir.join("luch-kms");
        std::fs::write(&helper, "#!/bin/sh\ncat \"$(dirname \"$0\")/frame.bin\"\n").unwrap();
        std::fs::set_permissions(&helper, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        std::env::set_var("LUCH_KMS", &helper);
        std::env::set_var("LUCH_KMS_SOCKET", dir.join("no-socket"));
        let dead = Arc::new(AtomicBool::new(false));
        let flag = dead.clone();
        let bus = frame_bus::FrameBus::new();
        let enc = encoder::create_with_fps(bus, crate::display::Size::default(), 60);
        let g = spawn_kms(enc, false, crate::display::Size::default(), 60, move || {
            flag.store(true, Ordering::SeqCst)
        })
        .unwrap();
        for _ in 0..200 {
            if dead.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        std::env::remove_var("LUCH_KMS");
        std::env::remove_var("LUCH_KMS_SOCKET");
        assert!(dead.load(Ordering::SeqCst), "EOF must be reported as death");
        // `stop` kills the (already gone) helper and joins the reader: it must not block.
        g.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_size_defaults_to_full_hd() {
        assert_eq!(session_size(None, Some("3840x2160")), (1920, 1080));
        assert_eq!(session_size(Some(""), None), (1920, 1080));
        assert_eq!(
            session_size(Some("nonsense"), Some("3840x2160")),
            (1920, 1080)
        );
    }

    #[test]
    fn session_size_auto_follows_the_receiver() {
        assert_eq!(session_size(Some("auto"), Some("3840x2160")), (3840, 2160));
        assert_eq!(session_size(Some("auto"), Some("4096x2304")), (3840, 2160)); // announced max above UHD
        assert_eq!(session_size(Some("auto"), Some("1280x720")), (1280, 720));
        assert_eq!(session_size(Some("auto"), None), (1920, 1080));
        assert_eq!(session_size(Some("auto"), Some("0x0")), (1920, 1080));
    }

    #[test]
    fn session_size_explicit_is_capped_by_the_receiver_and_even() {
        assert_eq!(
            session_size(Some("2560x1440"), Some("3840x2160")),
            (2560, 1440)
        );
        assert_eq!(
            session_size(Some("3840x2160"), Some("1920x1080")),
            (1920, 1080)
        );
        assert_eq!(session_size(Some("2561x1441"), None), (2560, 1440));
        assert_eq!(session_size(Some("99999x1080"), None), (1920, 1080));
    }

    #[test]
    fn offered_drm_formats_are_nv12_and_driver_specific() {
        // vapostproc may be absent (headless CI): then the pipeline falls back to its list.
        if gst::init().is_ok() && gst::ElementFactory::find("vapostproc").is_some() {
            if let Some(list) = offered_drm_formats() {
                assert!(list.starts_with("{\"NV12:"), "{list}");
                assert!(list.ends_with("\"}"), "{list}");
            }
        }
    }

    #[test]
    fn bitrate_scales_with_pixels_but_not_below_1080p() {
        assert_eq!(kbps_for(1920, 1080), VIDEO_KBPS);
        assert_eq!(kbps_for(1280, 720), VIDEO_KBPS);
        assert_eq!(kbps_for(3840, 2160), VIDEO_KBPS * 2);
    }

    #[test]
    fn header_layout() {
        let h = header(KIND_VIDEO, F_IDR, 2, 5, 0x01020304, 0x0a0b0c0d, 7000, 33);
        assert_eq!(&h[..8], &[1, 1, 0, 2, 0, 5, 0, 0]);
        assert_eq!(&h[8..12], &[1, 2, 3, 4]);
        assert_eq!(&h[12..16], &[0x0a, 0x0b, 0x0c, 0x0d]);
        assert_eq!(u32::from_be_bytes(h[16..20].try_into().unwrap()), 7000);
        assert_eq!(u16::from_be_bytes(h[20..22].try_into().unwrap()), 33);
    }

    #[test]
    fn opus_head_is_rfc7845() {
        let h = opus_head();
        assert_eq!(&h[..8], b"OpusHead");
        assert_eq!((h[8], h[9]), (1, 2));
        assert_eq!(u32::from_le_bytes(h[12..16].try_into().unwrap()), 48_000);
    }

    #[tokio::test]
    async fn wired_audio_prepends_opus_head_and_publishes_to_the_bus() {
        gst::init().unwrap();
        let pipeline =
            gst::parse::launch("appsrc name=src format=time ! appsink name=a sync=false")
                .unwrap()
                .downcast::<gst::Pipeline>()
                .unwrap();
        let src = pipeline
            .by_name("src")
            .unwrap()
            .downcast::<gst_app::AppSrc>()
            .unwrap();
        let bus = frame_bus::FrameBus::new();
        let enc = encoder::create(bus.clone(), Size::default());
        let mut rx = bus.subscribe_audio();
        wire(&pipeline, enc, FPS);
        pipeline.set_state(gst::State::Playing).unwrap();
        src.push_buffer(gst::Buffer::from_mut_slice(vec![0xf8, 0xff, 0xfe]))
            .unwrap();
        // First audio frame carries the OpusHead config (the old need_head behaviour)...
        let config = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(config.kind, KIND_AUDIO);
        assert!(config.flags & F_CONFIG != 0);
        assert_eq!(&config.data[..], &opus_head());
        // ...followed by the Opus payload itself.
        let data = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(data.flags, 0);
        assert_eq!(&*data.data, &[0xf8, 0xff, 0xfe]);
        pipeline.set_state(gst::State::Null).unwrap();
    }

    #[test]
    fn fps_gate_phase_locks_to_the_source_grid() {
        // 144 Hz source at 60: every third frame (48 fps), not "16.7 ms after the last".
        let mut g = FpsGate::new(60);
        let mut n = 0;
        for i in 0..144 {
            if g.accept(i * 1_000_000 / 144, false) {
                n += 1;
            }
        }
        assert!((47..=49).contains(&n), "{n}");

        let mut g = FpsGate::new(30);
        let mut n = 0;
        for i in 0..144 {
            if g.accept(i * 1_000_000 / 144, false) {
                n += 1;
            }
        }
        assert!((28..=30).contains(&n), "{n}");

        let mut g = FpsGate::new(0);
        for i in 0..144 {
            assert!(g.accept(i, false));
        }
    }

    #[test]
    fn fps_gate_lets_keyframes_through_and_rephases() {
        let mut g = FpsGate::new(60);
        assert!(g.accept(0, false));
        assert!(!g.accept(6_944, false)); // inside the slice
        assert!(g.accept(6_944, true)); // forced IDR passes early and moves the grid
        assert!(!g.accept(13_000, false));
        // A capture restart (clock jumps far back) re-phases instead of stalling.
        assert!(g.accept(10_000_000, false));
        assert!(g.accept(0, false));
    }
}
