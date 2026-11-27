//! Screen mirroring: xdg-desktop-portal ScreenCast -> PipeWire -> VA-API H.264 + Opus -> Luch v1 UDP.
//!
//! Latency rules borrowed from Sunshine/Moonlight: newest frame only, no B-frames, one reference,
//! one-frame CPB, IDR only on request, packets sent straight from the encoder thread.

use std::{
    net::{Ipv4Addr, SocketAddrV4, UdpSocket},
    os::fd::{AsRawFd, OwnedFd},
    sync::{
        atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use ashpd::desktop::{
    screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType, StartCastOptions},
    PersistMode,
};
use futures_util::StreamExt;
use gstreamer::{self as gst, prelude::*};
use gstreamer_app as gst_app;
use serde_json::{json, Value};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{broadcast, mpsc};

use crate::display::{Layout, Size};

static VIRTUAL_SETUP: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const UHD: (u32, u32) = (3840, 2160);
pub const WIDTH: u32 = 1920;
pub const HEIGHT: u32 = 1080;
pub const FPS: u32 = 60;
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

/// Latest numbers for one mirroring session, shared with D-Bus `Stats`.
pub type Stats = Arc<Mutex<Value>>;

/// Where and what to mirror.
pub struct Target {
    pub name: String,
    pub address: Ipv4Addr,
    pub ifname: String,
    pub source: SourceType,
    pub size: Size,
    /// Full screen straight from KMS scanout via the privileged luch-kms helper (lowest latency).
    pub kms: bool,
    /// Receiver's preferred decoder from its welcome bench ("video/hevc" or "video/avc").
    pub prefer: String,
    /// Web receiver: browsers take no UDP, media goes over its websocket instead.
    pub niri_virtual: bool,
    pub mirror169: bool,
    pub ws: Option<mpsc::Sender<Vec<u8>>>,
    /// The `audio` part of `start` (limits::start_audio), or None for the picture only.
    pub audio: Option<Value>,
    /// Sound only, like a speaker (PROTOCOL.md 4d): no capture of the screen, no video stream.
    pub audio_only: bool,
    /// Played in step with other receivers (PROTOCOL.md 4h), None for a session of its own as before.
    pub group: Option<Group>,
}

/// A group session (PROTOCOL.md 4h): this target and its other members present every frame at pts + D.
pub struct Group {
    pub id: String,
    pub timeline: u32,
    pub mode: crate::sync::Mode,
    pub tolerance_ms: f64,
    pub floor_ms: f64,
    /// The target's own part: its id (for its correction), role and correction.
    pub primary: MemberId,
    pub members: Vec<Member>,
}

/// Who a member is and what it presents.
#[derive(Clone)]
pub struct MemberId {
    pub id: String,
    pub name: String,
    /// "video", "audio" or "both".
    pub role: String,
    pub offset_ms: f64,
}

/// Another receiver of a group: its control channel and where its streams go.
pub struct Member {
    pub who: MemberId,
    pub address: Ipv4Addr,
    pub ifname: String,
    pub audio: Option<Value>,
    pub cmd: mpsc::Sender<Cmd>,
    pub events: broadcast::Receiver<Value>,
}

impl MemberId {
    fn video(&self) -> bool {
        self.role != "audio"
    }

    fn audio(&self) -> bool {
        self.role != "video"
    }
}

/// Waits for a receiver's `ready` (or its refusal) after a start.
async fn await_ready(events: &mut broadcast::Receiver<Value>) -> Result<Value, String> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Ok(v) if v["t"] == "ready" => return Ok(v),
                Ok(v) if v["t"] == "error" || v["t"] == "stop" => {
                    return Err(format!("receiver: {v}"))
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => return Err("control channel closed".to_string()),
            }
        }
    })
    .await
    .map_err(|_| "receiver did not answer start".to_string())?
}

fn port_of(ready: &Value, k: &str) -> Option<u16> {
    ready[k]
        .as_u64()
        .filter(|p| *p > 0 && *p < 65536)
        .map(|p| p as u16)
}

/// A member's start: the streams of its role, and its part of the group.
fn member_start(
    video: &Value,
    audio: &Option<Value>,
    (id, timeline, mode, tolerance_ms): (&str, u32, crate::sync::Mode, f64),
    who: &MemberId,
) -> Value {
    let mut start = if who.video() {
        json!({"t": "start", "mode": "mirror", "video": video})
    } else {
        json!({"t": "start", "mode": "audio"})
    };
    if let (true, Some(a)) = (who.audio(), audio) {
        start["audio"] = a.clone();
    }
    start["group"] =
        crate::sync::group_block(id, timeline, &who.role, mode, tolerance_ms, who.offset_ms);
    start
}

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
    v.extend(["monitor", "window"]);
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
pub enum Cmd {
    Bye,
    Send(Value),
}

/// Luch v1 datagram header (24 bytes, big-endian). The last field, the playout delay D (PROTOCOL.md 4h), is
/// written per receiver by `Tx` and left 0 here.
#[allow(clippy::too_many_arguments)]
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

/// One receiver of a kind's stream: its socket, pinned to the LAN interface, and the playout delay D (0.1 ms) its
/// packets carry. A receiver that left the group is skipped.
pub struct Dest {
    sock: UdpSocket,
    delay: AtomicU16,
    alive: AtomicBool,
}

impl Dest {
    fn open(ifname: &str, to: SocketAddrV4, dscp: u32) -> std::io::Result<Self> {
        use socket2::{Domain, Protocol, Socket, Type};
        let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        s.bind_device(Some(ifname.as_bytes()))?;
        s.set_tos_v4(dscp << 2)?; // CS5 video / CS6 audio -> Wi-Fi WMM video/voice queues
        s.set_send_buffer_size(1 << 20)?;
        s.connect(&to.into())?;
        Ok(Dest::of(s.into()))
    }

    fn of(sock: UdpSocket) -> Self {
        Dest {
            sock,
            delay: AtomicU16::new(0),
            alive: AtomicBool::new(true),
        }
    }
}

/// One media kind's stream to its receivers over UDP — or a websocket for the web receiver.
struct Tx {
    out: Out,
    kind: u8,
    seq: AtomicU32,
    warned: AtomicBool,
    bytes: AtomicU64,
    lat_sum: AtomicU64, // host latency, 0.1 ms units, summed over frames
    /// A group's stream: its first frame carries the new-timeline flag.
    timeline: AtomicBool,
}

enum Out {
    /// Every receiver of this kind: one, or the members of a group.
    Udp(Vec<Dest>),
    /// Whole frame per message (websocket framing is reliable and ordered); on a full channel
    /// drop the frame and ask for an IDR — the browser lags, the stream must not.
    Ws(mpsc::Sender<Vec<u8>>, Arc<AtomicBool>),
}

/// Shards per burst before a short pause, so a big IDR doesn't overflow the Wi-Fi queue.
const BURST: usize = 16;
const BURST_GAP: Duration = Duration::from_micros(250);

impl Tx {
    /// The same stream to several receivers (a group), each on its own socket.
    fn open_many(kind: u8, to: &[(String, SocketAddrV4)], dscp: u32) -> std::io::Result<Self> {
        let dests = to
            .iter()
            .map(|(ifname, addr)| Dest::open(ifname, *addr, dscp))
            .collect::<std::io::Result<Vec<_>>>()?;
        Ok(Self::udp(kind, dests))
    }

    fn udp(kind: u8, dests: Vec<Dest>) -> Self {
        Tx {
            out: Out::Udp(dests),
            kind,
            seq: AtomicU32::new(0),
            warned: AtomicBool::new(false),
            bytes: AtomicU64::new(0),
            lat_sum: AtomicU64::new(0),
            timeline: AtomicBool::new(false),
        }
    }

    /// This stream plays to a group: its first frame says its pts start anew.
    fn start_timeline(&self) {
        self.timeline.store(true, Ordering::Relaxed);
    }

    fn dests(&self) -> &[Dest] {
        match &self.out {
            Out::Udp(d) => d,
            Out::Ws(..) => &[],
        }
    }

    /// Sets the playout delay D (0.1 ms) of receiver `i`'s packets, from the next packet on.
    fn set_delay(&self, i: usize, units: u16) {
        if let Some(d) = self.dests().get(i) {
            d.delay.store(units, Ordering::Relaxed);
        }
    }

    /// Stops sending to receiver `i` (it left the group).
    fn drop_dest(&self, i: usize) {
        if let Some(d) = self.dests().get(i) {
            d.alive.store(false, Ordering::Relaxed);
        }
    }

    /// Answers the receivers' clock requests (PROTOCOL.md 4h) on every socket, with t2 and t3 from `now_us` (the pts
    /// clock), until this Tx is dropped. A thread per receiver: a request is answered the moment it is read.
    fn answer_clock(self: &Arc<Self>, now_us: Arc<dyn Fn() -> Option<u64> + Send + Sync>) {
        for (i, d) in self.dests().iter().enumerate() {
            let Ok(sock) = d.sock.try_clone() else {
                continue;
            };
            let _ = sock.set_read_timeout(Some(Duration::from_millis(200)));
            let (weak, now_us) = (Arc::downgrade(self), now_us.clone());
            std::thread::Builder::new()
                .name(format!("luchd-clock-{i}"))
                .spawn(move || {
                    let mut buf = [0u8; 128];
                    while weak.strong_count() > 0 {
                        let n = match sock.recv(&mut buf) {
                            Ok(n) => n,
                            // Timeouts, and the ICMP "unreachable" of a receiver not listening yet: go on.
                            Err(_) => continue,
                        };
                        let Some(t2) = now_us() else { continue };
                        if crate::sync::parse_request(&buf[..n]).is_some() {
                            let t3 = now_us().unwrap_or(t2);
                            let _ = sock.send(&crate::sync::reply(&buf[..n], t2, t3));
                        }
                    }
                })
                .ok();
        }
    }

    fn ws(kind: u8, tx: mpsc::Sender<Vec<u8>>, want_idr: Arc<AtomicBool>) -> Self {
        Tx {
            out: Out::Ws(tx, want_idr),
            kind,
            seq: AtomicU32::new(0),
            warned: AtomicBool::new(false),
            bytes: AtomicU64::new(0),
            lat_sum: AtomicU64::new(0),
            timeline: AtomicBool::new(false),
        }
    }

    // ponytail: one send() per shard; sendmmsg/UDP_SEGMENT if the syscall count ever shows up in profiles.
    fn send_frame(&self, data: &[u8], flags: u8, pts_us: u64, host_lat: u16) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        // In a group, the first frame of a pipeline marks its pts as starting anew (PROTOCOL.md 4h).
        let flags = if seq == 0 && self.timeline.load(Ordering::Relaxed) {
            flags | crate::sync::F_TIMELINE
        } else {
            flags
        };
        self.bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
        self.lat_sum.fetch_add(host_lat as u64, Ordering::Relaxed);
        if seq == 0 {
            eprintln!(
                "[luchd] first {} frame: {} bytes",
                if self.kind == KIND_VIDEO {
                    "video"
                } else {
                    "audio"
                },
                data.len()
            );
        }
        let shards = data.len().div_ceil(SHARD).max(1);
        match &self.out {
            Out::Udp(dests) => {
                let mut pkt = Vec::with_capacity(HEADER + SHARD);
                for (i, chunk) in data.chunks(SHARD).enumerate() {
                    if i > 0 && i % BURST == 0 {
                        std::thread::sleep(BURST_GAP); // ponytail: fixed pacing, ~3 ms for a 200 KB IDR
                    }
                    pkt.clear();
                    pkt.extend_from_slice(&header(
                        self.kind,
                        flags,
                        i as u16,
                        shards as u16,
                        seq,
                        pts_us as u32,
                        data.len() as u32,
                        host_lat,
                    ));
                    pkt.extend_from_slice(chunk); // last shard is not padded; receiver uses frame_len
                    for d in dests.iter().filter(|d| d.alive.load(Ordering::Relaxed)) {
                        pkt[22..24].copy_from_slice(&d.delay.load(Ordering::Relaxed).to_be_bytes());
                        // A lost datagram is recovered by the receiver's keyframe request; just report the first failure.
                        if let Err(e) = d.sock.send(&pkt) {
                            if !self.warned.swap(true, Ordering::Relaxed) {
                                eprintln!("[luchd] udp send (kind {}): {e}", self.kind);
                            }
                        }
                    }
                }
            }
            Out::Ws(tx, want_idr) => {
                let mut msg = Vec::with_capacity(HEADER + data.len());
                msg.extend_from_slice(&header(
                    self.kind,
                    flags,
                    0,
                    1,
                    seq,
                    pts_us as u32,
                    data.len() as u32,
                    host_lat,
                ));
                msg.extend_from_slice(data);
                if let Err(TrySendError::Full(_) | TrySendError::Closed(_)) = tx.try_send(msg) {
                    want_idr.store(true, Ordering::Relaxed);
                }
            }
        }
    }
}

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

/// Video from the portal (`Some((fd, node))`) plus audio, or audio only when luch-kms does video.
fn pipeline(
    portal: Option<(Option<i32>, u32)>,
    size: Size,
    virtual_source: bool,
) -> Result<gst::Pipeline, String> {
    let keepalive = 2000 / FPS; // resend the last frame at fps/2 when the screen is static
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
    // NV12 modifiers vapostproc may offer: Intel Y-tiled, AMD DCC, AMD 64KB tiled, linear.
    // LUCH_DRM_FORMAT overrides.
    let drm = std::env::var("LUCH_DRM_FORMAT")
        .map(|d| format!("\"{d}\""))
        .unwrap_or_else(|_| {
            "{\"NV12:0x0100000000000009\",\"NV12:0x0200000010401b04\",\"NV12:0x0200000000082305\",\"NV12:0x0\"}".into()
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

/// Hooks the appsinks to the senders; returns the "resend OpusHead" flag (set on every video IDR).
/// `want_idr` is shared with a websocket sink, which sets it when it drops a frame.
/// Without a "v" sink only audio is wired.
fn wire(
    p: &gst::Pipeline,
    video: Arc<Tx>,
    audio: Arc<Tx>,
    want_idr: Arc<AtomicBool>,
    size: Size,
) -> Arc<AtomicBool> {
    let need_head = Arc::new(AtomicBool::new(true));
    let encoder: Arc<Mutex<Option<crate::enc::Encoder>>> = Arc::default();
    let warned = AtomicBool::new(false);
    let dbg = Mutex::new((0f64, 0f64, 0u32));
    let asink = p
        .by_name("a")
        .unwrap()
        .downcast::<gst_app::AppSink>()
        .unwrap();
    let nh_audio = need_head.clone();

    let nh = need_head.clone();
    let wi = want_idr.clone();
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
                    // Runs on the capture thread: encode and send right here, no queue in between.
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
                    let at_sink = timing(s, pts_rt).1;
                    let mut enc = encoder.lock().unwrap();
                    if enc.is_none() {
                        match crate::enc::Encoder::open(
                            &render_node(),
                            size.width,
                            size.height,
                            FPS,
                            kbps_for(size.width, size.height),
                        ) {
                            Ok(e) => *enc = Some(e),
                            Err(e) => {
                                report(e);
                                return Err(gst::FlowError::Error);
                            }
                        }
                    }
                    let force = wi.swap(false, Ordering::Relaxed);
                    let (pts, _) = timing(s, pts_rt);
                    let r =
                        enc.as_mut()
                            .unwrap()
                            .encode(&frame, pts as i64, force, |bytes, idr| {
                                if idr {
                                    nh.store(true, Ordering::Relaxed); // receiver may have (re)started its audio decoder
                                }
                                let (_, lat) = timing(s, pts_rt); // capture -> encoded, measured after encode
                                video.send_frame(bytes, if idr { F_IDR } else { 0 }, pts, lat);
                            });
                    if let Err(e) = r {
                        report(e);
                    }
                    if std::env::var_os("LUCH_PROBE").is_some() {
                        let mut a = dbg.lock().unwrap();
                        a.0 += at_sink as f64 / 10.0;
                        a.1 += timing(s, pts_rt).1 as f64 / 10.0;
                        a.2 += 1;
                        if a.2 == 60 {
                            eprintln!(
                                "[probe] appsink entry {:.1} ms, after encode+send {:.1} ms",
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
    asink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |s| {
                let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buf = sample.buffer().ok_or(gst::FlowError::Error)?;
                let map = buf.map_readable().map_err(|_| gst::FlowError::Error)?;
                let (pts, lat) = timing(s, buf.pts());
                if nh_audio.swap(false, Ordering::Relaxed) {
                    audio.send_frame(&opus_head(), F_CONFIG, pts, 0);
                }
                audio.send_frame(&map, 0, pts, lat);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    need_head
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

/// The KMS grabber: a child process, or a connection to a `luch-kms --serve`.
enum Grabber {
    Child(std::process::Child),
    Socket(std::os::unix::net::UnixStream),
}

impl Grabber {
    /// Ask for an IDR.
    fn idr(&mut self) {
        use std::io::Write;
        let _ = match self {
            Grabber::Child(c) => c
                .stdin
                .as_mut()
                .map(|s| s.write_all(b"k"))
                .unwrap_or(Ok(())),
            Grabber::Socket(s) => s.write_all(b"k"),
        };
    }

    fn stop(self) {
        match self {
            Grabber::Child(mut c) => {
                let _ = c.kill();
                let _ = c.wait();
            }
            Grabber::Socket(s) => {
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
        }
    }
}

/// Lowest-latency video: luch-kms (KMS scanout -> EGL NV12 -> h264_vaapi) as a child, or through its
/// socket, forwarding its packets as they come. `k` on its input forces an IDR.
fn spawn_kms(
    video: Arc<Tx>,
    need_head: Arc<AtomicBool>,
    hevc: bool,
    size: Size,
    pipeline: gst::Pipeline,
) -> Result<Grabber, String> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    let kbps = kbps_for(size.width, size.height);
    let opts = format!(
        "--size {}x{} --fps {FPS} --kbps {kbps} --codec {}",
        size.width,
        size.height,
        if hevc { "hevc" } else { "h264" }
    );
    let (grabber, mut out): (Grabber, Box<dyn Read + Send>) =
        match kms_socket().and_then(|p| std::os::unix::net::UnixStream::connect(p).ok()) {
            Some(mut s) => {
                writeln!(s, "{opts}").map_err(|e| format!("luch-kms socket: {e}"))?;
                let out = s.try_clone().map_err(|e| format!("luch-kms socket: {e}"))?;
                (Grabber::Socket(s), Box::new(out))
            }
            None => {
                let exe = std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.join("luch-kms")));
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
                (Grabber::Child(child), Box::new(out))
            }
        };
    std::thread::spawn(move || {
        let mut hdr = [0u8; 13];
        let mut buf = Vec::new();
        let mut base: Option<u64> = None;
        while out.read_exact(&mut hdr).is_ok() {
            let len = u32::from_be_bytes(hdr[0..4].try_into().unwrap()) as usize;
            let idr = hdr[4] & 1 != 0;
            let cap = u64::from_be_bytes(hdr[5..13].try_into().unwrap());
            buf.resize(len, 0);
            if out.read_exact(&mut buf).is_err() {
                break;
            }
            let now = mono_us();
            // The pts are the pipeline's running time, as the sound's are (and the clock a group's receivers follow):
            // scanout time minus where the running time started on the monotonic clock. Before the pipeline runs,
            // the first frame's time, as before.
            let base = *base.get_or_insert_with(|| {
                pipeline
                    .current_running_time()
                    .map_or(cap, |rt| now.saturating_sub(rt.useconds()))
            });
            let lat = (now.saturating_sub(cap) / 100).min(u16::MAX as u64) as u16; // scanout flip -> encoded
            if idr {
                need_head.store(true, Ordering::Relaxed);
            }
            video.send_frame(&buf, if idr { F_IDR } else { 0 }, cap - base, lat);
        }
    });
    Ok(grabber)
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
fn probe_stages(p: &gst::Pipeline) {
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

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// Runs one mirroring session until stopped (by us, the receiver, or an error).
/// What a running group knows of its members, and the policy choosing its D (PROTOCOL.md 4h).
struct GroupState {
    who: Vec<MemberId>,
    reports: Vec<crate::sync::Report>,
    present: Vec<bool>,
    /// Each member's correction in use, and the route it was chosen for.
    offsets: Vec<(f64, Option<String>)>,
    pending: Vec<(usize, f64)>,
    policy: crate::sync::Policy,
    started: std::time::Instant,
    d: (f64, f64),
}

impl GroupState {
    fn new(g: Group, tx: &mpsc::Sender<(usize, Value)>, cmds: &mut Vec<mpsc::Sender<Cmd>>) -> Self {
        let mut who = vec![g.primary.clone()];
        for (k, m) in g.members.into_iter().enumerate() {
            let (tx, mut events) = (tx.clone(), m.events);
            tokio::spawn(async move {
                loop {
                    match events.recv().await {
                        Ok(v) => {
                            if tx.send((k + 1, v)).await.is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(_) => {
                            let _ = tx.send((k + 1, json!({"t": "_closed"}))).await;
                            break;
                        }
                    }
                }
            });
            cmds.push(m.cmd);
            who.push(m.who);
        }
        let n = who.len();
        GroupState {
            offsets: who.iter().map(|w| (w.offset_ms, None)).collect(),
            who,
            reports: vec![Default::default(); n],
            present: vec![true; n],
            pending: Vec::new(),
            policy: crate::sync::Policy::new(g.mode, g.tolerance_ms, g.floor_ms),
            started: std::time::Instant::now(),
            d: (0.0, 0.0),
        }
    }

    fn report(&mut self, k: usize, stats: &Value) {
        let Some(r) = self.reports.get_mut(k) else {
            return;
        };
        *r = crate::sync::Report::from_stats(stats);
        // A new output route (a soundbar turned on, a speaker swapped): its own saved correction, or the preset.
        if let Some(route) = r.route.clone() {
            if self.offsets[k].1.as_deref() != Some(&route) {
                crate::remember_route(&self.who[k].id, &route);
                let key = crate::sync::offset_key(&self.who[k].id, &route);
                let ms = crate::saved_offset(&key)
                    .unwrap_or_else(|| crate::sync::route_preset_ms(&route, r.audio_ms));
                self.offsets[k] = (ms, Some(route));
                self.pending.push((k, ms));
            }
        }
    }

    fn left(&mut self, k: usize) {
        if let Some(p) = self.present.get_mut(k) {
            *p = false;
        }
    }

    fn offsets_to_send(&mut self) -> Vec<(usize, f64)> {
        std::mem::take(&mut self.pending)
    }

    /// Once a second: D from what the members reported, into every packet from now on; the group's stats.
    fn tick(&mut self, video: &Tx, video_of: &[usize], audio: &Tx, audio_of: &[usize]) -> Value {
        let live = |k: &usize| self.present[*k];
        let video_pts: Vec<f64> = (0..self.who.len())
            .filter(|k| live(k) && self.who[*k].video())
            .filter_map(|k| self.reports[k].video_pts_ms)
            .collect();
        let need: Vec<f64> = (0..self.who.len())
            .filter(|k| live(k) && (self.policy_needs_all() || self.who[*k].audio()))
            .filter_map(|k| self.reports[k].need_ms)
            .collect();
        self.d = self
            .policy
            .update(self.started.elapsed().as_secs_f64(), &video_pts, &need);
        let (dv, da) = (
            crate::sync::header_units(self.d.0),
            crate::sync::header_units(self.d.1),
        );
        (0..video_of.len()).for_each(|i| video.set_delay(i, dv));
        (0..audio_of.len()).for_each(|i| audio.set_delay(i, da));
        json!({"d_video_ms": self.d.0, "d_audio_ms": self.d.1,
               "members": self.who.iter().zip(&self.reports).zip(&self.offsets).zip(&self.present)
                   .map(|(((w, r), o), p)| json!({"id": w.id, "name": w.name, "role": w.role, "present": p,
                        "need_ms": r.need_ms, "video_pts_ms": r.video_pts_ms, "route": r.route, "offset_ms": o.0}))
                   .collect::<Vec<_>>()})
    }

    fn policy_needs_all(&self) -> bool {
        matches!(self.policy_mode(), crate::sync::Mode::Watching)
    }

    fn policy_mode(&self) -> crate::sync::Mode {
        self.policy.mode()
    }
}

pub async fn run(
    t: Target,
    cmd: mpsc::Sender<Cmd>,
    mut events: broadcast::Receiver<Value>,
    mut stop: mpsc::Receiver<()>,
    stats: Stats,
) -> Result<&'static str, String> {
    let _awake = ScreenAwake::start().await;
    let _mute = MuteSession::start();
    let size = t.size;
    let virtual_source = t.source == SourceType::VIRTUAL && !t.niri_virtual;
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
    let portal = if t.kms || t.niri_virtual || t.audio_only {
        None
    } else {
        Some(portal(t.source).await?)
    };
    let _mode = if t.kms && t.mirror169 {
        switch_to_16x9(size.width).await
    } else {
        None
    };
    let nv = if t.niri_virtual {
        Some(
            niri_virtual(
                &format!("LUCH-{}", t.address.to_string().replace('.', "-")),
                (size.width, size.height),
            )
            .await?,
        )
    } else {
        None
    };

    // HEVC only on the KMS path (luch-kms); the portal path's encoder is H.264.
    let hevc = t.kms && t.prefer == "video/hevc";
    let mut start = if t.audio_only {
        json!({"t": "start", "mode": "audio"})
    } else {
        json!({"t": "start", "mode": "mirror",
            "video": {"codec": if hevc { "video/hevc" } else { "video/avc" }, "width": size.width, "height": size.height, "fps": FPS}})
    };
    let mut group = t.group;
    let primary_audio = group.as_ref().is_none_or(|g| g.primary.audio());
    if let (true, Some(a)) = (primary_audio, &t.audio) {
        start["audio"] = a.clone();
    }
    if let Some(g) = &group {
        start["group"] = crate::sync::group_block(
            &g.id,
            g.timeline,
            &g.primary.role,
            g.mode,
            g.tolerance_ms,
            g.primary.offset_ms,
        );
    }
    cmd.send(Cmd::Send(start.clone()))
        .await
        .map_err(|_| "control channel closed")?;
    let ready = await_ready(&mut events).await?;
    let port = |k: &str| port_of(&ready, k);
    // Web receivers take media over their websocket; everything else gets UDP sockets.
    let want_idr = Arc::new(AtomicBool::new(false));
    // Which member (0 = the target) each video and audio destination belongs to.
    let (mut video_of, mut audio_of): (Vec<usize>, Vec<usize>) = (vec![0], vec![0]);
    let (video, audio) = if let Some(ws) = t.ws.clone() {
        (
            Arc::new(Tx::ws(KIND_VIDEO, ws.clone(), want_idr.clone())),
            Arc::new(Tx::ws(KIND_AUDIO, ws, want_idr.clone())),
        )
    } else {
        let (vp, ap) = port("video_port")
            .zip(port("audio_port"))
            .ok_or(format!("bad ready: {ready}"))?;
        let io = |e: std::io::Error| format!("udp: {e}");
        // A group: every member gets the streams of its role, each on its own sockets.
        let (mut vto, mut ato) = (Vec::new(), Vec::new());
        let primary = group.as_ref().map(|g| g.primary.clone());
        if primary.as_ref().is_none_or(|p| p.video()) {
            vto.push((t.ifname.clone(), SocketAddrV4::new(t.address, vp), 0));
        }
        if primary.as_ref().is_none_or(|p| p.audio()) {
            ato.push((t.ifname.clone(), SocketAddrV4::new(t.address, ap), 0));
        }
        if let Some(g) = group.as_mut() {
            let video_block = start["video"].clone();
            let gid = g.id.clone();
            let info = (gid.as_str(), g.timeline, g.mode, g.tolerance_ms);
            for (k, m) in g.members.iter_mut().enumerate() {
                let st = member_start(&video_block, &m.audio, info, &m.who);
                m.cmd
                    .send(Cmd::Send(st))
                    .await
                    .map_err(|_| format!("{}: control channel closed", m.who.name))?;
                let r = await_ready(&mut m.events)
                    .await
                    .map_err(|e| format!("{}: {e}", m.who.name))?;
                if let (true, Some(vp)) = (m.who.video(), port_of(&r, "video_port")) {
                    vto.push((m.ifname.clone(), SocketAddrV4::new(m.address, vp), k + 1));
                }
                if let (true, Some(ap)) = (m.who.audio(), port_of(&r, "audio_port")) {
                    ato.push((m.ifname.clone(), SocketAddrV4::new(m.address, ap), k + 1));
                }
            }
        }
        let addrs = |v: &[(String, SocketAddrV4, usize)]| {
            v.iter()
                .map(|(i, a, _)| (i.clone(), *a))
                .collect::<Vec<_>>()
        };
        video_of = vto.iter().map(|x| x.2).collect();
        audio_of = ato.iter().map(|x| x.2).collect();
        (
            Arc::new(Tx::open_many(KIND_VIDEO, &addrs(&vto), 40).map_err(io)?),
            Arc::new(Tx::open_many(KIND_AUDIO, &addrs(&ato), 48).map_err(io)?),
        )
    };

    let p = pipeline(
        portal
            .as_ref()
            .map(|(_, _, fd, node)| (Some(fd.as_raw_fd()), *node))
            .or(nv.as_ref().map(|n| (None, n.node))),
        size,
        virtual_source,
    )?;
    if group.is_some() {
        // The pts clock is the pipeline's running time, the clock every pts here is on.
        let pc = p.clone();
        let now: Arc<dyn Fn() -> Option<u64> + Send + Sync> =
            Arc::new(move || pc.current_running_time().map(|t| t.useconds()));
        video.answer_clock(now.clone());
        audio.answer_clock(now);
        video.start_timeline();
        audio.start_timeline();
    }
    let need_head = wire(&p, video.clone(), audio.clone(), want_idr.clone(), size);
    let mut kms = if t.kms {
        Some(spawn_kms(video.clone(), need_head, hevc, size, p.clone())?)
    } else {
        None
    };
    // A group's members: their events in one stream, tagged with the member's number (1..; 0 is the target).
    let (member_tx, mut member_rx) = mpsc::channel::<(usize, Value)>(64);
    let mut member_cmds: Vec<mpsc::Sender<Cmd>> = Vec::new();
    let mut gs = group.map(|g| GroupState::new(g, &member_tx, &mut member_cmds));
    drop(member_tx);
    if std::env::var_os("LUCH_PROBE").is_some() {
        probe_stages(&p);
    }
    if let Err(e) = p.set_state(gst::State::Playing) {
        let detail = p.bus().and_then(|b| {
            b.timed_pop_filtered(
                gst::ClockTime::from_mseconds(500),
                &[gst::MessageType::Error],
            )
        });
        let _ = p.set_state(gst::State::Null);
        let _ = cmd.send(Cmd::Send(json!({"t": "stop"}))).await;
        return Err(match detail.as_ref().map(|m| m.view()) {
            Some(gst::MessageView::Error(err)) => {
                format!("pipeline: {} ({:?})", err.error(), err.debug())
            }
            _ => format!("pipeline: {e}"),
        });
    }
    let virtual_monitor = if let Some(layout) = &layout {
        match layout.place_new_right(size).await {
            Ok(connector) => Some(connector),
            Err(e) => {
                let _ = p.set_state(gst::State::Null);
                let _ = cmd.send(Cmd::Send(json!({"t": "stop"}))).await;
                if let Some((_, session, _, _)) = &portal {
                    let _ = session.close().await;
                }
                return Err(e);
            }
        }
    } else {
        None
    };
    drop(virtual_setup);
    eprintln!("[luchd] mirroring to {} ({})", t.name, t.address);

    let mut bus = p.bus().unwrap().stream();
    let mut second = tokio::time::interval(Duration::from_secs(1));
    let (mut rtt, mut rx_stats) = (Value::Null, Value::Null);
    let (mut last_frames, mut last_bytes, mut last_lat) = (0u64, 0u64, 0u64);
    let why = loop {
        tokio::select! {
            _ = second.tick() => {
                // Sender-side numbers, once a second: to the receiver (for its on-screen overlay) and to D-Bus.
                let frames = video.seq.load(Ordering::Relaxed) as u64;
                let bytes = video.bytes.load(Ordering::Relaxed);
                let lat = video.lat_sum.load(Ordering::Relaxed);
                let df = frames - last_frames;
                let host_ms = if df > 0 { (lat - last_lat) as f64 / df as f64 / 10.0 } else { 0.0 };
                let mbps = (bytes - last_bytes) as f64 * 8.0 / 1e6;
                (last_frames, last_bytes, last_lat) = (frames, bytes, lat);
                let sender = json!({"t": "sender_stats", "rtt_ms": rtt, "host_ms": round1(host_ms),
                                    "fps": df, "mbps": round1(mbps), "virtual_monitor": virtual_monitor, "encoder": if hevc { "hevc_vaapi" } else { "h264_vaapi" }, "res": format!("{}x{}", size.width, size.height)});
                let _ = cmd.send(Cmd::Send(sender.clone())).await;
                for c in &member_cmds {
                    let _ = c.send(Cmd::Send(sender.clone())).await;
                }
                // Rough glass-to-glass estimate without the display: capture+encode + one-way network + decode.
                let decode = rx_stats["decode_ms"].as_f64().unwrap_or(0.0);
                let est = host_ms + rtt.as_f64().unwrap_or(0.0) / 2.0 + decode;
                let mut all = json!({"sender": sender, "receiver": rx_stats, "est_ms": round1(est)});
                if let Some(g) = gs.as_mut() {
                    all["group"] = g.tick(&video, &video_of, &audio, &audio_of);
                    for (k, offset) in g.offsets_to_send() {
                        let msg = Cmd::Send(json!({"t": "offset", "ms": offset}));
                        let _ = if k == 0 { cmd.send(msg).await } else { member_cmds[k - 1].send(msg).await };
                    }
                }
                *stats.lock().unwrap() = all;
            }
            _ = stop.recv() => {
                let _ = cmd.send(Cmd::Send(json!({"t": "stop"}))).await;
                break Ok("stopped");
            }
            ev = events.recv() => match ev {
                Ok(v) if v["t"] == "keyframe" => {
                    want_idr.store(true, Ordering::Relaxed);
                    if let Some(k) = kms.as_mut() {
                        k.idr();
                    }
                }
                Ok(v) if v["t"] == "stats" => {
                    if let Some(g) = gs.as_mut() {
                        g.report(0, &v);
                    }
                    rx_stats = v;
                }
                Ok(v) if v["t"] == "_rtt" => rtt = json!(round1(v["ms"].as_f64().unwrap_or(0.0))),
                Ok(v) if v["t"] == "stop" => {
                    eprintln!("[luchd] {}: receiver sent {v}", t.name);
                    break Ok(crate::limits::stop_reason(&v));
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break Ok("control channel closed"),
            },
            Some((k, v)) = member_rx.recv() => match v["t"].as_str() {
                Some("keyframe") => {
                    want_idr.store(true, Ordering::Relaxed);
                    if let Some(kms) = kms.as_mut() {
                        kms.idr();
                    }
                }
                Some("stats") => {
                    if let Some(g) = gs.as_mut() {
                        g.report(k, &v);
                    }
                }
                Some("stop") | Some("_closed") => {
                    // A member left: the others go on without it.
                    eprintln!("[luchd] group member {k} left: {v}");
                    for (tx, of) in [(&video, &video_of), (&audio, &audio_of)] {
                        of.iter().enumerate().filter(|(_, m)| **m == k).for_each(|(i, _)| tx.drop_dest(i));
                    }
                    if let Some(g) = gs.as_mut() {
                        g.left(k);
                    }
                }
                _ => {}
            },
            msg = bus.next() => match msg.as_ref().map(|m| m.view()) {
                Some(gst::MessageView::Error(err)) => {
                    let _ = cmd.send(Cmd::Send(json!({"t": "stop"}))).await;
                    break Err(format!("pipeline: {} ({:?})", err.error(), err.debug()));
                }
                Some(gst::MessageView::Eos(_)) | None => break Ok("capture ended"),
                _ => {}
            },
        }
    };
    for c in &member_cmds {
        let _ = c.send(Cmd::Send(json!({"t": "stop"}))).await;
    }
    let _ = p.set_state(gst::State::Null);
    if let Some(k) = kms {
        k.stop();
    }
    if let Some(n) = nv {
        let _ = n.session.call::<_, _, ()>("Stop", &()).await;
    }
    if let Some((_sc, session, fd, _)) = portal {
        let _ = session.close().await;
        drop(fd);
    }
    // Removing the virtual monitor must not disturb the physical ones: restore the layout that
    // was active before extend, once Mutter has dropped the virtual connector.
    if let (Some(layout), Some(connector)) = (&layout, &virtual_monitor) {
        if let Err(e) = layout.restore(connector).await {
            eprintln!("[luchd] restore display layout: {e}");
        }
    }
    why
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn frame_is_sharded_and_reassembles() {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        let to = match rx.local_addr().unwrap() {
            std::net::SocketAddr::V4(a) => a,
            _ => unreachable!(),
        };
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.connect(to).unwrap();
        let tx = Tx::udp(KIND_VIDEO, vec![Dest::of(sock)]);
        tx.seq.store(7, Ordering::Relaxed);
        let frame: Vec<u8> = (0..SHARD * 2 + 100).map(|i| i as u8).collect();
        tx.send_frame(&frame, F_IDR, 1234, 5);
        let mut got = Vec::new();
        let mut buf = [0u8; 2000];
        for i in 0..3u16 {
            let n = rx.recv(&mut buf).unwrap();
            assert_eq!(u16::from_be_bytes([buf[2], buf[3]]), i);
            assert_eq!(u16::from_be_bytes([buf[4], buf[5]]), 3);
            assert_eq!(u32::from_be_bytes(buf[8..12].try_into().unwrap()), 7);
            got.extend_from_slice(&buf[HEADER..n]);
        }
        assert_eq!(got, frame);
    }

    fn pair() -> (UdpSocket, Dest) {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        rx.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.connect(rx.local_addr().unwrap()).unwrap();
        (rx, Dest::of(sock))
    }

    #[test]
    fn a_group_gets_the_same_frame_each_with_its_own_delay() {
        let (a, da) = pair();
        let (b, db) = pair();
        let (c, dc) = pair();
        let tx = Tx::udp(KIND_AUDIO, vec![da, db, dc]);
        tx.start_timeline();
        tx.set_delay(0, 1500);
        tx.set_delay(1, 0); // a receiver that plays at once
        tx.drop_dest(2); // left the group
        tx.send_frame(&[1, 2, 3], 0, 0x1_2345_6789, 4);
        tx.send_frame(&[4], 0, 0x1_2345_6790, 4);
        let mut buf = [0u8; 64];
        for (rx, delay) in [(&a, 1500u16), (&b, 0)] {
            let n = rx.recv(&mut buf).unwrap();
            assert_eq!(&buf[HEADER..n], &[1, 2, 3]);
            assert_eq!(u16::from_be_bytes([buf[22], buf[23]]), delay);
            assert_eq!(
                buf[1] & crate::sync::F_TIMELINE,
                crate::sync::F_TIMELINE,
                "the first frame"
            );
            assert_eq!(
                u32::from_be_bytes(buf[12..16].try_into().unwrap()),
                0x2345_6789
            );
            rx.recv(&mut buf).unwrap();
            assert_eq!(buf[1] & crate::sync::F_TIMELINE, 0);
        }
        c.set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        assert!(c.recv(&mut buf).is_err(), "nothing to a member that left");
    }

    #[test]
    fn clock_requests_are_answered_on_the_media_socket() {
        let (rx, d) = pair();
        let tx = Arc::new(Tx::udp(KIND_AUDIO, vec![d]));
        let sender = match &tx.out {
            Out::Udp(d) => d[0].sock.local_addr().unwrap(),
            _ => unreachable!(),
        };
        rx.connect(sender).unwrap();
        let ticks = Arc::new(AtomicU64::new(1_000_000));
        let t = ticks.clone();
        tx.answer_clock(Arc::new(move || Some(t.fetch_add(150, Ordering::Relaxed))));
        let mut req = vec![0u8; 32];
        req[0] = crate::sync::KIND_CLOCK;
        req[8..12].copy_from_slice(&9u32.to_be_bytes());
        req[24..32].copy_from_slice(&0xabcdefu64.to_be_bytes());
        rx.send(&req).unwrap();
        let mut buf = [0u8; 64];
        let n = rx.recv(&mut buf).unwrap();
        assert_eq!(n, 48);
        assert_eq!(buf[1], crate::sync::F_REPLY);
        assert_eq!(u32::from_be_bytes(buf[8..12].try_into().unwrap()), 9);
        let at = |o: usize| u64::from_be_bytes(buf[o..o + 8].try_into().unwrap());
        assert_eq!((at(24), at(32), at(40)), (0xabcdef, 1_000_000, 1_000_150));
        // Media packets that come back by mistake are not answered.
        rx.send(&buf[..n]).unwrap();
        rx.set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        assert!(rx.recv(&mut buf).is_err());
        drop(tx); // the answering thread ends with the stream
    }

    #[test]
    fn a_members_start_carries_its_role_and_the_group() {
        let who = MemberId {
            id: "spk".into(),
            name: "Kitchen speaker".into(),
            role: "audio".into(),
            offset_ms: 12.5,
        };
        let video = json!({"codec": "video/avc", "width": 1920, "height": 1080, "fps": 60});
        let audio =
            Some(json!({"codec": "audio/opus", "rate": 48000, "channels": 2, "frame_ms": 5}));
        let info = ("g", 3, crate::sync::Mode::Interactive, 20.0);
        let s = member_start(&video, &audio, info, &who);
        assert_eq!(s["mode"], "audio");
        assert!(s.get("video").is_none());
        assert_eq!(s["audio"]["rate"], 48000);
        assert_eq!(s["group"]["role"], "audio");
        assert_eq!(s["group"]["offset_ms"], 12.5);
        let tv = MemberId {
            role: "video".into(),
            ..who
        };
        let s = member_start(&video, &audio, info, &tv);
        assert_eq!(
            (s["mode"].as_str(), s["video"]["fps"].as_u64()),
            (Some("mirror"), Some(60))
        );
        assert!(s.get("audio").is_none());
    }

    #[tokio::test]
    async fn kms_idr_resends_opus_head_without_requesting_video_idr() {
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
        let (tx, mut rx) = mpsc::channel(8);
        let want_idr = Arc::new(AtomicBool::new(false));
        let video = Arc::new(Tx::ws(KIND_VIDEO, tx.clone(), want_idr.clone()));
        let audio = Arc::new(Tx::ws(KIND_AUDIO, tx, want_idr.clone()));
        let need_head = wire(&pipeline, video, audio, want_idr.clone(), Size::default());
        pipeline.set_state(gst::State::Playing).unwrap();
        for _ in 0..2 {
            // The KMS forwarding thread sets this on every IDR.
            need_head.store(true, Ordering::Relaxed);
            src.push_buffer(gst::Buffer::from_mut_slice(vec![0xf8, 0xff, 0xfe]))
                .unwrap();
            let config = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap();
            let data = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(config[1], F_CONFIG);
            assert_eq!(&config[HEADER..], &opus_head());
            assert_eq!(data[1], 0);
            assert!(!want_idr.load(Ordering::Relaxed));
        }
        pipeline.set_state(gst::State::Null).unwrap();
    }

    #[tokio::test]
    async fn ws_frame_is_one_whole_message() {
        let (tx, mut rx) = mpsc::channel(1);
        let want_idr = Arc::new(AtomicBool::new(false));
        let t = Tx::ws(KIND_AUDIO, tx, want_idr.clone());
        let frame: Vec<u8> = (0..SHARD * 2 + 100).map(|i| i as u8).collect();
        t.send_frame(&frame, F_CONFIG, 42, 3); // fills the channel
        t.send_frame(&frame, 0, 43, 3); // full: dropped, an IDR is requested instead of stalling
        let msg = rx.recv().await.unwrap();
        assert!(rx.try_recv().is_err());
        assert_eq!(
            &msg[..HEADER],
            &header(KIND_AUDIO, F_CONFIG, 0, 1, 0, 42, frame.len() as u32, 3)
        );
        assert_eq!(&msg[HEADER..], &frame[..]);
        assert!(want_idr.load(Ordering::Relaxed));
    }
}
