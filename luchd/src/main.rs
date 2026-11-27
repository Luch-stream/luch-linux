//! luchd — Luch v0 sender daemon: announces `_luchsrc._tcp`, browses `_luch._tcp`
//! receivers via Avahi's D-Bus API, keeps control channels, mirrors the screen, exposes dev.luch.Luch1.

mod cast;
mod discovery;
mod display;
mod enc;
mod limits;
mod mirror;
mod mpris;
mod reports;
mod volume;
mod wake;
mod web_tls;

use std::{
    collections::HashMap,
    error::Error,
    io,
    net::SocketAddr,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use luch_proto::{self as p, ReceiverRow};
use mirror::{
    group::{GroupParams, MirrorGroup, NewReceiver},
    Cmd,
};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::{tcp::OwnedWriteHalf, TcpSocket},
    sync::{broadcast, mpsc, Mutex, Notify},
    time::{timeout, Instant},
};
use tokio_tungstenite::{accept_async, tungstenite::Message, WebSocketStream};
use zbus::{
    fdo, interface, message::Type as MsgType, object_server::SignalEmitter,
    zvariant::OwnedObjectPath, Connection, MatchRule, MessageStream,
};

const AVAHI: &str = "org.freedesktop.Avahi";
const AVAHI_SERVER: &str = "org.freedesktop.Avahi.Server";
const AVAHI_IF_UNSPEC: i32 = -1;
const AVAHI_PROTO_INET: i32 = 0;
const PING_EVERY: Duration = Duration::from_secs(1); // also the RTT sample rate
const DEAD_AFTER: Duration = Duration::from_secs(30);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Receiver {
    id: String,
    name: String,
    model: String,
    address: String,
    ifname: String,
    ifindex: i32,
    port: u16,
    service: String, // Avahi instance name, for ItemRemove
}

/// A live control channel: commands to it, receiver messages (ready, keyframe, stop, ...) from it.
#[derive(Clone)]
struct Conn {
    cmd: mpsc::Sender<Cmd>,
    events: broadcast::Sender<Value>,
    /// Web receiver only: media frames (header + data) to its websocket writer task,
    /// in two bounded queues so sound cannot crowd video out of the shared socket.
    media: Option<mirror::group::WsMedia>,
    /// This machine's and the receiver's address on the control connection: what a receiver fetches cast files from,
    /// and whom they are served to. None for the web receiver.
    addrs: Option<(std::net::IpAddr, std::net::IpAddr)>,
}

/// A group owned by the manager. The slot lives in `State.groups` for the group's whole
/// life; the Option inside is taken out only by whoever stops the group (a D-Bus method
/// or the group watcher), so a group can never be stopped twice and a stop never races
/// an in-flight add/remove (they hold the same per-group lock — the global state lock is
/// never held across a network operation).
type GroupSlot = Arc<Mutex<Option<MirrorGroup>>>;

#[derive(Default)]
struct State {
    found: HashMap<String, Receiver>,
    conns: HashMap<String, Conn>,
    groups: HashMap<String, GroupSlot>, // group id -> running group
    mirrors: HashMap<String, String>,   // receiver id -> group id
    /// Last encoder counters per receiver, for the per-second deltas of D-Bus `Stats`.
    stats_snap: HashMap<String, (u64, u64, u64, tokio::time::Instant)>, // receiver id -> (frames, bytes, lat_sum, at)
    welcomes: HashMap<String, Value>, // id -> last welcome (hw passport, decoder bench)
    codec: HashMap<String, String>, // id -> "auto" | "avc" | "hevc": a person's choice of the video codec
    audio: HashMap<String, String>, // id -> "auto" | "on" | "off" | "sync": a person's choice of the sound
    playing: HashMap<String, Value>, // id -> last media_state of what it plays for us (PROTOCOL.md 4b)
    volumes: HashMap<String, Value>, // id -> the receiver's last {"level","muted"} (PROTOCOL.md 4e)
    docs: HashMap<String, (u64, u64)>, // id -> (page, pages) of the document it shows for us (4g)
    /// Receivers we stopped: a report still on its way is no longer news, until the next media_ready.
    stopped: std::collections::HashSet<String>,
    /// Receiver ids reserved for a group that is being created: the check and the take
    /// happen under one lock, so two D-Bus calls cannot both start mirroring to them.
    starting: std::collections::HashSet<String>,
}
type Shared = Arc<Mutex<State>>;

/// Whether something accepts TCP at `address:port` within a second: tells a receiver that is on from one Avahi
/// still lists after it went to standby.
async fn answers(address: &str, port: u16) -> bool {
    tokio::time::timeout(
        Duration::from_secs(1),
        tokio::net::TcpStream::connect((address, port)),
    )
    .await
    .is_ok_and(|c| c.is_ok())
}

struct Luch {
    st: Shared,
    sender_id: String,
    host: String,
    discovery: discovery::Discovery,
}

#[interface(name = "dev.luch.Luch1")]
impl Luch {
    async fn receivers(&self) -> Vec<ReceiverRow> {
        let st = self.st.lock().await;
        let mut rows: Vec<ReceiverRow> = st
            .found
            .values()
            .map(|r| {
                let (c, m) = (st.conns.contains_key(&r.id), st.mirrors.contains_key(&r.id));
                (
                    r.id.clone(),
                    r.name.clone(),
                    r.model.clone(),
                    r.address.clone(),
                    r.ifname.clone(),
                    r.port,
                    c,
                    m,
                )
            })
            .collect();
        rows.sort();
        rows
    }

    /// Starts an immediate Avahi rescan; results arrive via ReceiversChanged.
    async fn refresh(&self) {
        self.discovery.refresh();
    }

    /// Opens and keeps a control channel; returns the receiver's welcome as JSON.
    async fn connect(
        &self,
        id: String,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<String> {
        {
            let st = self.st.lock().await;
            if st.conns.contains_key(&id) {
                return Err(fdo::Error::Failed(format!("already connected: {id}")));
            }
            if !st.found.contains_key(&id) {
                return Err(fdo::Error::InvalidArgs(format!("unknown receiver: {id}")));
            }
        }
        let r = self
            .discovery
            .receiver(id.clone())
            .await
            .map_err(fdo::Error::Failed)?;
        let (welcome, rd, wr, addrs) =
            handshake(&r, &self.sender_id, &self.host)
                .await
                .map_err(|e| {
                    fdo::Error::Failed(format!("{} ({}%{}): {e}", r.name, r.address, r.ifname))
                })?;
        let (tx, rx) = mpsc::channel(8);
        let (events, _) = broadcast::channel(16);
        {
            let mut st = self.st.lock().await;
            st.conns.insert(
                id.clone(),
                Conn {
                    cmd: tx,
                    events: events.clone(),
                    media: None,
                    addrs: Some(addrs),
                },
            );
            st.welcomes.insert(id.clone(), welcome.clone());
        }
        // Remembered with its MAC (the handshake has just filled the ARP table), to wake it later.
        wake::remember(&id, &r.name, &r.model, &r.address);
        eprintln!(
            "[luchd] connected to {} ({}:{} via {})",
            r.name, r.address, r.port, r.ifname
        );
        changed(conn).await;
        tokio::spawn(run_channel(
            r.name,
            id,
            rd,
            wr,
            rx,
            events,
            self.st.clone(),
            conn.clone(),
        ));
        Ok(welcome.to_string())
    }

    async fn disconnect(&self, id: String) -> fdo::Result<()> {
        let c = self.st.lock().await.conns.get(&id).cloned();
        let c = c.ok_or_else(|| fdo::Error::InvalidArgs(format!("not connected: {id}")))?;
        let _ = c.cmd.send(Cmd::Bye).await;
        Ok(())
    }

    /// Mirror to a receiver (connecting first if needed). `source`:
    /// "fast" (full screen straight from KMS scanout via luch-kms, lowest latency),
    /// "monitor", "window" or "virtual" (a new GNOME virtual monitor, i.e. extend) via the
    /// screencast portal: its picker is shown the first time, later runs reuse a saved token.
    /// Returns once the session is set up; it then runs until StopMirror or the receiver stops it.
    async fn mirror(
        &self,
        id: String,
        source: String,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        self.mirror_sized(id, source, 0, 0, conn).await
    }

    async fn mirror_sized(
        &self,
        id: String,
        source: String,
        width: u32,
        height: u32,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        if (width, height) != (0, 0)
            && (!(320..=7680).contains(&width)
                || !(240..=4320).contains(&height)
                || width % 2 != 0
                || height % 2 != 0)
        {
            return Err(fdo::Error::InvalidArgs(format!(
                "bad size {width}x{height}"
            )));
        }
        ensure_connected(self, &id, conn).await?;
        start_mirror(&self.st, conn, &id, &source, (width, height), mirror::fps()).await
    }

    /// Capture sources usable on this machine, best first (`fast`, `fast169`, `virtual`, `monitor`, `window`).
    async fn sources(&self) -> Vec<String> {
        mirror::available_sources()
            .into_iter()
            .map(String::from)
            .collect()
    }

    /// The receiver's last `welcome` as JSON (decoders, `hw.display.modes`, `bench`), "{}" until
    /// the control channel has been opened once. Does not connect by itself.
    async fn capabilities(&self, id: String) -> String {
        self.st
            .lock()
            .await
            .welcomes
            .get(&id)
            .map(|w| w.to_string())
            .unwrap_or_else(|| "{}".into())
    }

    /// The video codec for the next show to a receiver: "auto" (HEVC when the receiver decodes it about as fast as
    /// H.264, else the receiver's own pick), "hevc" or "avc". Kept until luchd restarts; front-ends save the choice.
    async fn set_codec(&self, id: String, codec: String) -> fdo::Result<()> {
        if !["auto", "avc", "hevc"].contains(&codec.as_str()) {
            return Err(fdo::Error::InvalidArgs(format!(
                "codec must be auto|avc|hevc, not {codec}"
            )));
        }
        save_codec(&id, &codec);
        self.st.lock().await.codec.insert(id, codec);
        Ok(())
    }

    /// The saved choice for a receiver ("auto" when none).
    async fn get_codec(&self, id: String) -> String {
        match self.st.lock().await.codec.get(&id).cloned() {
            Some(c) => c,
            None => saved_codecs().remove(&id).unwrap_or_else(|| "auto".into()),
        }
    }

    /// The sound for the next show to a receiver: "auto" (with the picture held back to match where the receiver
    /// cannot make it short, Android 5 and 6; else as it comes), "on" (as it comes), "off" (picture only, the lowest
    /// latency) or "sync" (always held to match). Saved across restarts.
    async fn set_audio(&self, id: String, mode: String) -> fdo::Result<()> {
        if !limits::AUDIO_MODES.contains(&mode.as_str()) {
            return Err(fdo::Error::InvalidArgs(format!(
                "audio must be auto|on|off|sync, not {mode}"
            )));
        }
        save_choice("audio", &id, &mode);
        self.st.lock().await.audio.insert(id, mode);
        Ok(())
    }

    /// The saved sound choice for a receiver ("auto" when none).
    async fn get_audio(&self, id: String) -> String {
        audio_choice(&self.st, &id).await
    }

    /// What a person should know about a receiver, as JSON: `android` ("Android 5.1", empty until connected once),
    /// `limits` (what works worse there, in plain words), `audio_latency_ms`, `audio` (the sound mode in use).
    async fn info(&self, id: String) -> String {
        let welcome = self
            .st
            .lock()
            .await
            .welcomes
            .get(&id)
            .cloned()
            .unwrap_or(Value::Null);
        json!({
            "android": welcome["sdk"].as_u64().map(limits::android).unwrap_or_default(),
            "limits": limits::describe(&welcome),
            "audio_latency_ms": limits::audio_latency_ms(&welcome),
            "audio": audio_choice(&self.st, &id).await,
        })
        .to_string()
    }

    /// Crash and error reports the receivers handed over, as a JSON array (oldest first).
    async fn reports(&self) -> String {
        Value::Array(reports::list()).to_string()
    }

    /// The link that files a report as an issue (title and body filled in), "" if there is no such report.
    async fn report_url(&self, id: String) -> String {
        reports::list()
            .iter()
            .find(|r| r["id"] == id.as_str())
            .map(reports::issue_url)
            .unwrap_or_default()
    }

    /// Forgets a report (filed or not worth it).
    async fn dismiss_report(&self, id: String) -> fdo::Result<()> {
        if reports::remove(&id) {
            Ok(())
        } else {
            Err(fdo::Error::InvalidArgs(format!("no report {id}")))
        }
    }

    /// Every receiver luchd has talked to, as a JSON array: `id`, `name`, `model`, `address`, `mac`, `ble`, `seen`
    /// (unix time), `online` (on the LAN now), `wake` (the ways it can be turned on: "wol", "ble") and `hint` (what
    /// to do when there is none yet).
    async fn known(&self) -> String {
        // Avahi keeps a receiver for the record's TTL after it went to standby, so being found is not enough:
        // one not connected must also answer on its port now.
        let (found, connected) = {
            let st = self.st.lock().await;
            (
                st.found.clone(),
                st.conns.keys().cloned().collect::<Vec<_>>(),
            )
        };
        let known = wake::load();
        let probes = known.keys().map(|id| {
            let target = found.get(id).filter(|_| !connected.contains(id)).cloned();
            let up = found.contains_key(id);
            async move {
                match target {
                    Some(r) => answers(&r.address, r.port).await,
                    None => up,
                }
            }
        });
        let online = futures_util::future::join_all(probes).await;
        let rows: Vec<Value> = known
            .into_iter()
            .zip(online)
            .map(|((id, mut r), online)| {
                r["id"] = json!(id);
                r["online"] = json!(online);
                r["wake"] = json!(wake::methods(&r));
                if let Some(h) = wake::hint(&r) {
                    r["hint"] = json!(h);
                }
                r
            })
            .collect();
        Value::Array(rows).to_string()
    }

    /// Turns a known receiver on from standby, every way it can be (Wake-on-LAN, the remote's Bluetooth
    /// advertisement); returns the ways tried, e.g. "wol,ble". Takes about 2 s with Bluetooth. The receiver
    /// shows up in Receivers once it is up.
    async fn wake(&self, id: String) -> fdo::Result<String> {
        let r = wake::load()
            .remove(&id)
            .ok_or_else(|| fdo::Error::InvalidArgs(format!("unknown receiver: {id}")))?;
        let tried = wake::wake(&r).await.map_err(fdo::Error::Failed)?;
        eprintln!(
            "[luchd] waking {} ({})",
            r["name"].as_str().unwrap_or(&id),
            tried.join(", ")
        );
        self.discovery.refresh();
        Ok(tried.join(","))
    }

    /// Listens up to `seconds` for the remote's power key over Bluetooth and keeps that advertisement for waking
    /// the receiver; returns it as hex. The receiver should be off, so that the remote advertises.
    async fn learn_wake(&self, id: String, seconds: u32) -> fdo::Result<String> {
        if !wake::load().contains_key(&id) {
            return Err(fdo::Error::InvalidArgs(format!(
                "unknown receiver {id}: connect to it once first"
            )));
        }
        let adv = wake::learn(u64::from(seconds.clamp(5, 120)))
            .await
            .map_err(fdo::Error::Failed)?;
        let hex = wake::hex(&adv);
        wake::set(&id, "ble", &hex).map_err(fdo::Error::Failed)?;
        Ok(hex)
    }

    /// Sets how to wake a known receiver by hand: `kind` "mac" (for Wake-on-LAN) or "ble" (the remote's MediaTek
    /// manufacturer data, hex); an empty `value` forgets it.
    async fn set_wake(&self, id: String, kind: String, value: String) -> fdo::Result<()> {
        wake::set(&id, &kind, &value).map_err(fdo::Error::InvalidArgs)
    }

    /// Live mirroring numbers as JSON (sender side + the receiver's last stats), "{}" if idle.
    /// Same shape the pre-group daemon produced: {"sender": ..., "receiver": ..., "est_ms": ...};
    /// sender fps/mbps/host_ms are per-second deltas between calls (the client polls once a second).
    async fn stats(&self, id: String) -> String {
        let slot = {
            let s = self.st.lock().await;
            s.mirrors.get(&id).and_then(|g| s.groups.get(g).cloned())
        };
        let Some(slot) = slot else {
            return "{}".into();
        };
        let mut guard = slot.lock().await;
        let Some(g) = guard.as_mut() else {
            return "{}".into();
        };
        g.reap();
        let (frames, bytes, lat) = {
            let c = g.enc_counters();
            (
                c.frames.load(Ordering::Relaxed),
                c.bytes.load(Ordering::Relaxed),
                c.lat_sum.load(Ordering::Relaxed),
            )
        };
        let Some(r) = g.receivers().find(|r| r.id == id) else {
            return "{}".into();
        };
        // Per-receiver baselines: two receivers of one group polled in the same second
        // must each see their own delta, not steal each other's. Rates are divided by the
        // real time between polls, so a late or first poll cannot report nonsense like
        // "96011 fps".
        let now = tokio::time::Instant::now();
        let last = self
            .st
            .lock()
            .await
            .stats_snap
            .insert(id.clone(), (frames, bytes, lat, now));
        let (df, host_ms, mbps) = match last {
            Some((pf, pb, pl, at)) => {
                let secs = now.duration_since(at).as_secs_f64().max(1e-3);
                let df = frames.saturating_sub(pf);
                let host_ms = if df > 0 {
                    lat.saturating_sub(pl) as f64 / df as f64 / 10.0
                } else {
                    0.0
                };
                (
                    df as f64 / secs,
                    host_ms,
                    bytes.saturating_sub(pb) as f64 * 8.0 / 1e6 / secs,
                )
            }
            None => (0.0, 0.0, 0.0), // first poll: nothing to compare against yet
        };
        let rx = r.rx_stats.lock().unwrap().clone();
        let rtt = r.rtt_ms();
        // Rough glass-to-glass estimate without the display: capture+encode + one-way network + decode.
        let est = host_ms + rtt / 2.0 + rx["decode_ms"].as_f64().unwrap_or(0.0);
        let sender = mirror::stats::sender_stats_json(
            g.codec() == "video/hevc",
            g.size(),
            df,
            mbps,
            host_ms,
            rtt,
            g.virtual_monitor(),
        );
        json!({"sender": sender, "receiver": rx, "est_ms": mirror::round1(est)}).to_string()
    }

    /// Group numbers as JSON (source, codec, resolution, receivers, encoder counters),
    /// "{}" for an unknown group.
    async fn group_stats(&self, group_id: String) -> String {
        let slot = self.st.lock().await.groups.get(&group_id).cloned();
        if let Some(slot) = slot {
            if let Some(g) = slot.lock().await.as_ref() {
                let mut v = g.stats_json();
                v["receiver_stats"] = json!(g
                    .receivers()
                    .map(mirror::stats::receiver_stats_json)
                    .collect::<Vec<_>>());
                return v.to_string();
            }
        }
        "{}".into()
    }

    /// Stop the whole group the receiver is mirroring from (a plain Mirror is a group of one).
    async fn stop_mirror(
        &self,
        id: String,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        let gid = self.st.lock().await.mirrors.get(&id).cloned();
        let gid = gid.ok_or_else(|| fdo::Error::InvalidArgs(format!("not mirroring: {id}")))?;
        self.stop_group(gid, conn).await
    }

    async fn stop_group(
        &self,
        group_id: String,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        let slot = self.st.lock().await.groups.get(&group_id).cloned();
        let slot =
            slot.ok_or_else(|| fdo::Error::InvalidArgs(format!("unknown group: {group_id}")))?;
        let group = slot
            .lock()
            .await
            .take()
            .ok_or_else(|| fdo::Error::Failed(format!("group already stopping: {group_id}")))?;
        {
            let mut s = self.st.lock().await;
            s.groups.remove(&group_id);
            s.mirrors.retain(|_, g| *g != group_id);
            let State {
                mirrors,
                stats_snap,
                ..
            } = &mut *s;
            stats_snap.retain(|rid, _| mirrors.contains_key(rid));
        }
        group.stop().await;
        changed(conn).await;
        Ok(())
    }

    /// Mirror one source to several receivers at once (connecting them first if needed):
    /// one capture, one encoder, an independent stream per receiver. Returns the group id.
    /// Stream rate: `LUCH_FPS` (default 60); use `MirrorGroupFps` to choose per group.
    async fn mirror_group(
        &self,
        receiver_ids: Vec<String>,
        source: String,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<String> {
        start_group_shared(self, receiver_ids, source, mirror::fps(), conn).await
    }

    /// Like `MirrorGroup`, with the stream rate chosen for the group: 0 keeps the
    /// screen's own rate (game streaming to one capable device), otherwise 30 or 60.
    /// The profile is fixed at creation (spec §14).
    async fn mirror_group_fps(
        &self,
        receiver_ids: Vec<String>,
        source: String,
        fps: u32,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<String> {
        let fps = check_fps(fps)?;
        start_group_shared(self, receiver_ids, source, fps, conn).await
    }

    /// `Mirror` for a single receiver with an explicit stream rate (0 = the screen's rate).
    async fn mirror_at(
        &self,
        id: String,
        source: String,
        fps: u32,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        let fps = check_fps(fps)?;
        ensure_connected(self, &id, conn).await?;
        start_mirror(&self.st, conn, &id, &source, (0, 0), fps).await
    }

    /// Add a receiver to a running group (connecting it first if needed). The group's codec
    /// profile is fixed at creation: a HEVC group rejects a receiver that prefers H.264.
    async fn add_to_group(
        &self,
        group_id: String,
        receiver_id: String,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        attach_to_group(&self.st, conn, &group_id, &receiver_id).await
    }

    /// Detach a receiver from its group; the rest keep streaming. A group that loses its
    /// last receiver stops.
    async fn remove_from_group(
        &self,
        group_id: String,
        receiver_id: String,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        let slot = self.st.lock().await.groups.get(&group_id).cloned();
        let slot =
            slot.ok_or_else(|| fdo::Error::InvalidArgs(format!("unknown group: {group_id}")))?;
        let mut guard = slot.lock().await;
        let g = guard
            .as_mut()
            .ok_or_else(|| fdo::Error::Failed(format!("group stopped: {group_id}")))?;
        g.reap();
        if !g.receivers().any(|r| r.id == receiver_id) {
            return Err(fdo::Error::InvalidArgs(format!(
                "not in group {group_id}: {receiver_id}"
            )));
        }
        g.remove(&receiver_id).await.map_err(fdo::Error::Failed)?;
        let group = if g.receiver_count() == 0 {
            guard.take()
        } else {
            None
        };
        drop(guard);
        {
            let mut s = self.st.lock().await;
            s.mirrors.remove(&receiver_id);
            s.stats_snap.remove(&receiver_id);
        }
        if let Some(group) = group {
            {
                let mut s = self.st.lock().await;
                s.groups.remove(&group_id);
                s.mirrors.retain(|_, gid| *gid != group_id);
                let State {
                    mirrors,
                    stats_snap,
                    ..
                } = &mut *s;
                stats_snap.retain(|rid, _| mirrors.contains_key(rid));
            }
            group.stop().await;
        }
        changed(conn).await;
        Ok(())
    }

    /// Whether the sender keeps playing sound itself while mirroring. Off: its sound goes to the
    /// receiver only (a null sink is the default output for the session). Saved across restarts.
    #[zbus(property)]
    async fn local_audio(&self) -> bool {
        mirror::local_audio()
    }

    #[zbus(property)]
    async fn set_local_audio(&self, on: bool) {
        mirror::set_local_audio(on);
        let active = !self.st.lock().await.mirrors.is_empty();
        mirror::sync_mute(active);
    }

    #[zbus(property)]
    async fn sender_id(&self) -> String {
        self.sender_id.clone()
    }

    /// The built-in web receiver's page (`https://<lan-ip>:47801/`): open it in any browser with
    /// WebCodecs — a TV will do — and it becomes a receiver. Empty with no default route.
    #[zbus(property)]
    async fn web_url(&self) -> String {
        lan_ip()
            .map(|ip| format!("https://{ip}:{}/", p::WEB_PORT))
            .unwrap_or_default()
    }

    /// Shows on a receiver (connecting first if needed) local files or links: photos (all items pictures) or one
    /// video or sound, played by the receiver itself in its own quality (PROTOCOL.md 4b), with any subtitle files
    /// among the items. `options`: JSON, may be "": `title`, `start` (s), `paused`, `rate`, `subs` (paths or
    /// links), `via` ("direct": the receiver fetches a link itself, the default; "sender": luchd relays it, with
    /// `headers`), `headers` (Referer, User-Agent, Origin, Cookie, Authorization), `mime`, `poster`.
    /// Returns "media" or "photo" once the receiver has taken it.
    async fn cast(
        &self,
        id: String,
        items: Vec<String>,
        options: String,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<String> {
        let o = cast::Options::parse(&options).map_err(fdo::Error::InvalidArgs)?;
        if !self.st.lock().await.conns.contains_key(&id) {
            self.connect(id.clone(), conn).await?;
        }
        let c = self.st.lock().await.conns.get(&id).cloned();
        let c = c.ok_or_else(|| fdo::Error::Failed("receiver went away".into()))?;
        let (me, tv) = c
            .addrs
            .ok_or_else(|| fdo::Error::NotSupported("the web receiver cannot play media".into()))?;
        let srv = files()
            .await
            .map_err(|e| fdo::Error::Failed(format!("cannot serve files: {e}")))?;
        // Documents that are not PDF yet are turned into PDF first (LibreOffice; takes a few seconds).
        let mut items = items;
        for i in items.iter_mut() {
            if cast::kind_of(i) == cast::Kind::Document && !i.to_ascii_lowercase().ends_with(".pdf")
            {
                let path = std::path::PathBuf::from(i.strip_prefix("file://").unwrap_or(i));
                let out = std::env::temp_dir().join(format!("luch-doc-{}", std::process::id()));
                let pdf = tokio::task::spawn_blocking(move || cast::to_pdf(&path, &out))
                    .await
                    .map_err(|e| fdo::Error::Failed(e.to_string()))?
                    .map_err(fdo::Error::Failed)?;
                *i = pdf.to_string_lossy().into_owned();
            }
        }
        let msg = cast::build(&items, &o, srv, tv, me).map_err(fdo::Error::InvalidArgs)?;
        let wire_kind = msg["t"].as_str().unwrap_or("").to_string();
        let kind = if wire_kind == "open_app" {
            "app".to_string()
        } else {
            wire_kind.clone()
        };
        let mut events = c.events.subscribe();
        c.cmd
            .send(Cmd::Send(msg))
            .await
            .map_err(|_| fdo::Error::Failed("channel closed".into()))?;
        let answer = timeout(Duration::from_secs(10), async {
            loop {
                match events.recv().await {
                    Ok(v)
                        if v["t"] == "media_ready"
                            || v["t"] == "photo_ready"
                            || v["t"] == "doc_ready"
                            || v["t"] == "app_opened" =>
                    {
                        return Ok(())
                    }
                    Ok(v)
                        if v["t"] == "error" && v["for"] == "open_app" && v["code"] == "no_app" =>
                    {
                        return Err("the TV has no YouTube app".to_string())
                    }
                    Ok(v)
                        if v["t"] == "error"
                            && v["for"] == wire_kind.as_str()
                            && v["code"] == "unknown" =>
                    {
                        return Err(format!(
                            "this receiver version cannot show {kind} yet: update it"
                        ))
                    }
                    Ok(v)
                        if v["t"] == "error"
                            && (v["for"] == wire_kind.as_str() || v["for"].is_null()) =>
                    {
                        return Err(format!(
                            "{}{}",
                            v["code"].as_str().unwrap_or("error"),
                            v["detail"]
                                .as_str()
                                .map(|d| format!(": {d}"))
                                .unwrap_or_default()
                        ))
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => return Err("channel closed".to_string()),
                }
            }
        })
        .await
        .map_err(|_| fdo::Error::Failed("the receiver did not answer".into()))?;
        answer.map_err(fdo::Error::Failed)?;
        Ok(kind)
    }

    /// Asks the receiver about what it plays: `cmd` is a `media_cmd` (PROTOCOL.md 4b) without "t", as JSON:
    /// {"do":"pause"}, {"do":"seek","pos":90}, {"do":"rate","rate":1.5}, {"do":"sub","id":null}...
    async fn media_cmd(&self, id: String, cmd: String) -> fdo::Result<()> {
        let mut v: Value =
            serde_json::from_str(&cmd).map_err(|e| fdo::Error::InvalidArgs(e.to_string()))?;
        if !v["do"].is_string() {
            return Err(fdo::Error::InvalidArgs("no \"do\"".into()));
        }
        v["t"] = json!("media_cmd");
        self.send_to(&id, v).await
    }

    /// The last `media_state` from the receiver as JSON, "{}" when it plays nothing for us.
    async fn media_state(&self, id: String) -> String {
        self.st
            .lock()
            .await
            .playing
            .get(&id)
            .map(|v| v.to_string())
            .unwrap_or_else(|| "{}".into())
    }

    /// Ends what the receiver plays or shows for us (media or photos).
    async fn stop_cast(
        &self,
        id: String,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        self.send_to(&id, json!({"t": "stop"})).await?;
        // The receiver does not answer a sender's stop: the end is told here.
        let was = {
            let mut s = self.st.lock().await;
            s.stopped.insert(id.clone());
            s.playing.remove(&id).is_some()
        };
        if was {
            media_changed(
                conn,
                &id,
                &json!({"t": "media_state", "state": "stopped", "reason": "sender"}),
            )
            .await;
        }
        let addrs = self.st.lock().await.conns.get(&id).and_then(|c| c.addrs);
        if let (Some(srv), Some((_, tv))) = (FILES.get(), addrs) {
            srv.forget(tv);
        }
        Ok(())
    }

    /// Puts the receiver to standby (PROTOCOL.md 4c), connecting first if needed. Fails with the reason when the
    /// receiver cannot: "off" (not turned on in its accessibility settings) or "no" (Android older than 9).
    async fn sleep(&self, id: String, #[zbus(connection)] conn: &Connection) -> fdo::Result<()> {
        if !self.st.lock().await.conns.contains_key(&id) {
            self.connect(id.clone(), conn).await?;
        }
        let c = self.st.lock().await.conns.get(&id).cloned();
        let c = c.ok_or_else(|| fdo::Error::Failed("receiver went away".into()))?;
        let mut events = c.events.subscribe();
        let _ = c.cmd.send(Cmd::Send(json!({"t": "sleep"}))).await;
        let r = timeout(Duration::from_secs(5), async {
            loop {
                match events.recv().await {
                    Ok(v) if v["t"] == "sleeping" => return Ok(()),
                    Ok(v) if v["t"] == "error" && v["for"] == "sleep" => {
                        return Err(match v["sleep"].as_str() {
                            Some("off") => "sleep is off on the receiver: turn on \"Luch\" in its accessibility settings".to_string(),
                            Some("no") => "the receiver's Android is too old to be put to sleep (9 or newer)".to_string(),
                            _ if v["code"] == "unknown" => "this receiver version cannot sleep yet: update it".to_string(),
                            _ => v["code"].as_str().unwrap_or("error").to_string(),
                        })
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => return Ok(()), // the channel went with the TV: it is asleep
                }
            }
        })
        .await
        .map_err(|_| fdo::Error::Failed("the receiver did not answer".into()))?;
        r.map_err(fdo::Error::Failed)
    }

    /// Sets the receiver's volume (connecting first if needed): "0".."100", "+", "-", "mute", "unmute". Returns its
    /// volume after the change as JSON {"level","muted"}.
    async fn set_volume(
        &self,
        id: String,
        what: String,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<String> {
        let msg = volume::message(&what).map_err(fdo::Error::InvalidArgs)?;
        if !self.st.lock().await.conns.contains_key(&id) {
            self.connect(id.clone(), conn).await?;
        }
        let c = self.st.lock().await.conns.get(&id).cloned();
        let c = c.ok_or_else(|| fdo::Error::Failed("receiver went away".into()))?;
        let mut events = c.events.subscribe();
        let _ = c.cmd.send(Cmd::Send(msg)).await;
        let r = timeout(Duration::from_secs(3), async {
            loop {
                match events.recv().await {
                    Ok(v) if v["t"] == "volume" => return Ok(v),
                    Ok(v) if v["t"] == "error" && v["for"] == "volume" => {
                        return Err(if v["code"] == "unknown" {
                            "this receiver version cannot set its volume yet: update it".to_string()
                        } else {
                            "the TV keeps its volume to itself".to_string()
                        })
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => return Err("channel closed".to_string()),
                }
            }
        })
        .await
        .map_err(|_| fdo::Error::Failed("the receiver did not answer".into()))?;
        let mut v = r.map_err(fdo::Error::Failed)?;
        if let Some(o) = v.as_object_mut() {
            o.remove("t");
        }
        Ok(v.to_string())
    }

    /// Turns the page of the document the receiver shows: "next", "prev" or a page number from 1. Returns
    /// "page/pages".
    async fn doc_page(&self, id: String, what: String) -> fdo::Result<String> {
        let (page, pages) = self
            .st
            .lock()
            .await
            .docs
            .get(&id)
            .copied()
            .unwrap_or((0, 0));
        if pages == 0 {
            return Err(fdo::Error::Failed("no document on that receiver".into()));
        }
        let to = match what.as_str() {
            "next" | "+" => (page + 1).min(pages - 1),
            "prev" | "-" => page.saturating_sub(1),
            n => n
                .parse::<u64>()
                .ok()
                .filter(|n| (1..=pages).contains(n))
                .map(|n| n - 1)
                .ok_or_else(|| fdo::Error::InvalidArgs(format!("page 1..{pages}, next or prev")))?,
        };
        self.send_to(&id, json!({"t": "doc_page", "page": to}))
            .await?;
        Ok(format!("{}/{pages}", to + 1))
    }

    /// The receiver's last known volume as JSON, "{}" until it told one.
    async fn get_volume(&self, id: String) -> String {
        let s = self.st.lock().await;
        s.volumes
            .get(&id)
            .cloned()
            .or_else(|| {
                s.welcomes
                    .get(&id)
                    .map(|w| w["volume"].clone())
                    .filter(|v| v.is_object())
            })
            .map(|mut v| {
                if let Some(o) = v.as_object_mut() {
                    o.remove("t");
                }
                v.to_string()
            })
            .unwrap_or_else(|| "{}".into())
    }

    #[zbus(signal)]
    async fn receivers_changed(e: &SignalEmitter<'_>) -> zbus::Result<()>;

    /// A receiver's `media_state` changed (JSON); {"state":"stopped"} when it ended.
    #[zbus(signal)]
    async fn media_state_changed(e: &SignalEmitter<'_>, id: &str, state: &str) -> zbus::Result<()>;
}

impl Luch {
    async fn send_to(&self, id: &str, v: Value) -> fdo::Result<()> {
        let c = self.st.lock().await.conns.get(id).cloned();
        let c = c.ok_or_else(|| fdo::Error::InvalidArgs(format!("not connected: {id}")))?;
        c.cmd
            .send(Cmd::Send(v))
            .await
            .map_err(|_| fdo::Error::Failed("channel closed".into()))
    }
}

async fn changed(conn: &Connection) {
    if let Ok(e) = SignalEmitter::new(conn, p::DBUS_PATH) {
        let _ = Luch::receivers_changed(&e).await;
    }
}

// ---------- control channel ----------

type Reader = BufReader<tokio::net::tcp::OwnedReadHalf>;

/// One JSON line, at most MAX_LINE bytes. Ok(None) on EOF.
async fn read_msg(rd: &mut Reader) -> io::Result<Option<Value>> {
    let mut buf = Vec::new();
    let n = (&mut *rd)
        .take(p::MAX_LINE as u64 + 1)
        .read_until(b'\n', &mut buf)
        .await?;
    if n == 0 {
        return Ok(None);
    }
    if buf.last() != Some(&b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "line too long or truncated",
        ));
    }
    serde_json::from_slice(&buf)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

async fn send(wr: &mut OwnedWriteHalf, v: &Value) -> io::Result<()> {
    let mut line = v.to_string();
    line.push('\n');
    wr.write_all(line.as_bytes()).await
}

/// Socket pinned to the interface the receiver was discovered on: the default
/// route may be a full-tunnel VPN, so it must never decide where this goes.
async fn handshake(
    r: &Receiver,
    id: &str,
    host: &str,
) -> io::Result<(
    Value,
    Reader,
    OwnedWriteHalf,
    (std::net::IpAddr, std::net::IpAddr),
)> {
    let addr: SocketAddr = format!("{}:{}", r.address, r.port)
        .parse()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let sock = TcpSocket::new_v4()?;
    sock.bind_device(Some(r.ifname.as_bytes()))?;
    let stream = timeout(HANDSHAKE_TIMEOUT, sock.connect(addr))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connect timed out"))??;
    let addrs = (stream.local_addr()?.ip(), stream.peer_addr()?.ip());
    let (rd, mut wr) = stream.into_split();
    let mut rd = BufReader::new(rd);
    let hello = json!({"t": "hello", "v": p::VERSION, "id": id, "name": host, "app": p::APP,
                       "want": ["mirror", "media", "photo", "reports"]});
    send(&mut wr, &hello).await?;
    let welcome = timeout(HANDSHAKE_TIMEOUT, read_msg(&mut rd))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no welcome"))??
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "closed before welcome"))?;
    if welcome["t"] != "welcome" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected welcome, got {welcome}"),
        ));
    }
    Ok((welcome, rd, wr, addrs))
}

async fn run_channel(
    name: String,
    id: String,
    mut rd: Reader,
    mut wr: OwnedWriteHalf,
    mut cmds: mpsc::Receiver<Cmd>,
    events: broadcast::Sender<Value>,
    st: Shared,
    conn: Connection,
) {
    // Reading lives in its own task: read_until is not cancel-safe inside select!.
    let (in_tx, mut inbox) = mpsc::channel::<Value>(16);
    let reader = tokio::spawn(async move {
        loop {
            match read_msg(&mut rd).await {
                Ok(Some(v)) => {
                    if in_tx.send(v).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                    eprintln!("[luchd] bad line: {e}")
                }
                Err(_) => break,
            }
        }
    });

    let mut last_rx = Instant::now();
    let mut n: u64 = 0;
    let mut ping_sent = Instant::now();
    let mut tick = tokio::time::interval(PING_EVERY);
    tick.tick().await;
    let why = loop {
        tokio::select! {
            c = cmds.recv() => match c {
                Some(Cmd::Send(v)) => {
                    if send(&mut wr, &v).await.is_err() {
                        break "write failed";
                    }
                }
                Some(Cmd::Bye) | None => {
                    let _ = send(&mut wr, &json!({"t": "bye"})).await;
                    break "disconnect requested";
                }
            },
            _ = tick.tick() => {
                if last_rx.elapsed() >= DEAD_AFTER {
                    break "silent for 30 s";
                }
                n += 1;
                ping_sent = Instant::now();
                if send(&mut wr, &json!({"t": "ping", "n": n})).await.is_err() {
                    break "write failed";
                }
            }
            m = inbox.recv() => {
                let Some(v) = m else { break "closed by receiver" };
                last_rx = Instant::now();
                match v["t"].as_str().unwrap_or("") {
                    "pong" if v["n"] == n => {
                        // Internal event for the mirror task; never sent on the wire.
                        let rtt = ping_sent.elapsed().as_secs_f64() * 1000.0;
                        let _ = events.send(json!({"t": "_rtt", "ms": rtt}));
                    }
                    "pong" | "welcome" => {}
                    "ping" => {
                        if send(&mut wr, &json!({"t": "pong", "n": v["n"]})).await.is_err() {
                            break "write failed";
                        }
                    }
                    "bye" => break "bye from receiver",
                    "error" => {
                        eprintln!("[luchd] {name}: error {v}");
                        let _ = events.send(v);
                    }
                    "media_ready" => {
                        st.lock().await.stopped.remove(&id);
                        let _ = events.send(v);
                    }
                    "doc_ready" | "doc_page" => {
                        {
                            let mut s = st.lock().await;
                            let d = s.docs.entry(id.clone()).or_insert((0, 0));
                            if let Some(n) = v["pages"].as_u64() {
                                *d = (0, n);
                            }
                            if let Some(p) = v["page"].as_u64() {
                                d.0 = p;
                            }
                        }
                        let _ = events.send(v);
                    }
                    "ready" | "keyframe" | "stats" | "photo_ready" | "sleeping" | "app_opened" => {
                        let _ = events.send(v);
                    }
                    "stop" => {
                        // What it played for us is over: its files are no longer served.
                        let was_playing = st.lock().await.playing.remove(&id).is_some();
                        if was_playing {
                            media_changed(&conn, &id, &json!({"t": "media_state", "state": "stopped",
                                                               "reason": v["reason"]})).await;
                        }
                        let _ = events.send(v);
                    }
                    "volume" => {
                        st.lock().await.volumes.insert(id.clone(), v.clone());
                        let _ = events.send(v);
                    }
                    "media_key" => {
                        // The TV's remote, for the player on this computer (PROTOCOL.md 4d).
                        let key = v["key"].as_str().unwrap_or("").to_string();
                        let (st, conn, id) = (st.clone(), conn.clone(), id.clone());
                        tokio::spawn(async move {
                            if mpris::key(&conn, &key).await {
                                tokio::time::sleep(Duration::from_millis(300)).await;
                                push_now_playing(&st, &conn, Some(&id)).await;
                            }
                        });
                    }
                    "media_state" => {
                        {
                            let mut s = st.lock().await;
                            if s.stopped.contains(&id) {
                                continue;
                            }
                            s.playing.insert(id.clone(), v.clone());
                        }
                        media_changed(&conn, &id, &v).await;
                        let _ = events.send(v);
                    }
                    "reports" => {
                        let model = st.lock().await.found.get(&id).map(|r| r.model.clone()).unwrap_or_default();
                        let items = v["items"].as_array().cloned().unwrap_or_default();
                        let (ids, new) = reports::store(&model, &items);
                        if !ids.is_empty()
                            && send(&mut wr, &json!({"t": "reports_taken", "ids": ids})).await.is_err()
                        {
                            break "write failed";
                        }
                        if new > 0 {
                            eprintln!("[luchd] {name}: {new} new report(s), see `luchctl reports`");
                            let last = reports::list().into_iter().rev().find(|r| ids.iter().any(|i| r["id"] == i.as_str()));
                            if let Some(r) = last {
                                let body = format!("{}\nClick to file it as an issue; `luchctl reports` lists them all.", reports::summary(&r));
                                reports::notify(&conn, &format!("{name} ran into a problem"), &body, Some(reports::issue_url(&r))).await;
                            }
                        }
                    }
                    t => {
                        let _ = send(&mut wr, &json!({"t": "error", "code": "unknown", "for": t})).await;
                    }
                }
            }
        }
    };
    reader.abort();
    let _ = wr.shutdown().await;
    eprintln!("[luchd] {name}: channel closed ({why})");
    {
        let mut s = st.lock().await;
        s.conns.remove(&id);
        s.playing.remove(&id);
    }
    changed(&conn).await;
}

/// Serves cast files, relayed links and covers to receivers; started when first needed.
static FILES: tokio::sync::OnceCell<cast::Server> = tokio::sync::OnceCell::const_new();

async fn files() -> io::Result<&'static cast::Server> {
    FILES.get_or_try_init(cast::Server::start).await
}

/// What the player on this computer plays, to every receiver that has a session with us (or only `only`):
/// `now_playing` (PROTOCOL.md 4d), its cover shared with each receiver.
async fn push_now_playing(st: &Shared, conn: &Connection, only: Option<&str>) {
    let targets: Vec<(String, Conn)> = {
        let s = st.lock().await;
        s.mirrors
            .keys()
            .filter(|id| only.is_none_or(|o| o == id.as_str()))
            .filter_map(|id| s.conns.get(id).map(|c| (id.clone(), c.clone())))
            .collect()
    };
    if targets.is_empty() {
        return;
    }
    let Some((np, art)) = mpris::now(conn).await else {
        return;
    };
    for (_, c) in targets {
        let mut msg = np.clone();
        msg["t"] = json!("now_playing");
        if let (Some(a), Some((me, tv))) = (&art, c.addrs) {
            if let Ok(srv) = files().await {
                let item = if let Some(path) = a.strip_prefix("file://") {
                    let path = path.replace("%20", " ");
                    std::path::Path::new(&path)
                        .is_file()
                        .then(|| cast::Item::File(path.into()))
                } else if a.starts_with("http://") || a.starts_with("https://") {
                    Some(cast::Item::Relay {
                        url: a.clone(),
                        headers: vec![],
                    })
                } else {
                    None
                };
                if let Some(item) = item {
                    let t = srv.share(item, tv, "image/jpeg");
                    msg["art"] = json!(srv.url(me, &t, "cover.jpg"));
                }
            }
        }
        let _ = c.cmd.send(Cmd::Send(msg)).await;
    }
}

/// Follows the players' changes and tells the receivers that have a session (at most every 300 ms).
async fn follow_players(st: Shared, conn: Connection) {
    let rule = match MatchRule::builder()
        .msg_type(MsgType::Signal)
        .interface("org.freedesktop.DBus.Properties")
        .and_then(|b| b.member("PropertiesChanged"))
        .and_then(|b| b.path("/org/mpris/MediaPlayer2"))
    {
        Ok(b) => b.build(),
        Err(_) => return,
    };
    let Ok(mut stream) = MessageStream::for_match_rule(rule, &conn, None).await else {
        return;
    };
    while stream.next().await.is_some() {
        tokio::time::sleep(Duration::from_millis(300)).await;
        // Whatever came meanwhile is folded into this one push.
        while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(1), stream.next()).await
        {
        }
        push_now_playing(&st, &conn, None).await;
    }
}

async fn media_changed(conn: &Connection, id: &str, state: &Value) {
    if let Ok(e) = SignalEmitter::new(conn, p::DBUS_PATH) {
        let _ = Luch::media_state_changed(&e, id, &state.to_string()).await;
    }
}

// ---------- mirroring ----------

/// Stream rate argument: 0 = the screen's own rate (no cap), 60 or 30.
fn check_fps(fps: u32) -> fdo::Result<u32> {
    if mirror::allowed_fps(fps) {
        Ok(fps)
    } else {
        Err(fdo::Error::InvalidArgs(format!(
            "fps must be 0 (the screen's own rate), 60 or 30, not {fps}"
        )))
    }
}

/// Open the control channel first when the receiver is not connected yet.
async fn ensure_connected(luch: &Luch, id: &str, conn: &Connection) -> fdo::Result<()> {
    if !luch.st.lock().await.conns.contains_key(id) {
        luch.connect(id.to_string(), conn).await?;
    }
    Ok(())
}

/// Capture parameters of a group from a `source` name ("fast", "fast169", "monitor",
/// "window", "virtual"), an explicitly requested size ((0, 0) = automatic) and a welcome
/// to take sizing hints from (`max` caps `LUCH_SIZE`, display modes seed "virtual").
fn group_params(source: &str, requested: (u32, u32), welcome: &Value) -> fdo::Result<GroupParams> {
    let mirror169 = source == "fast169";
    let audio_only = source == "audio";
    let kms = source == "fast" || mirror169;
    let niri_virtual = source == "virtual" && mirror::niri_virtual_available();
    // Say what is actually wrong when the host cannot do the requested source (the KMS
    // helper is a separate binary), instead of failing later with "No such file".
    if kms && !mirror::available_sources().contains(&source) {
        return Err(fdo::Error::Failed(format!(
            "source '{source}' is not available here: luch-kms was not found; available: {}",
            mirror::available_sources().join(", ")
        )));
    }
    let source_ty = match source {
        "" | "monitor" | "fast" | "fast169" | "audio" => {
            ashpd::desktop::screencast::SourceType::MONITOR
        }
        "window" => ashpd::desktop::screencast::SourceType::WINDOW,
        "virtual" => ashpd::desktop::screencast::SourceType::VIRTUAL,
        s => {
            return Err(fdo::Error::InvalidArgs(format!(
                "source must be fast|monitor|window|virtual|audio, not {s}"
            )))
        }
    };
    let size = if requested != (0, 0) {
        display::Size {
            width: requested.0,
            height: requested.1,
        }
    } else if std::env::var("LUCH_SIZE")
        .ok()
        .is_some_and(|v| !v.trim().is_empty())
    {
        let (width, height) = mirror::session_size(
            std::env::var("LUCH_SIZE").ok().as_deref(),
            welcome["max"].as_str(),
        );
        display::Size { width, height }
    } else if source_ty == ashpd::desktop::screencast::SourceType::VIRTUAL {
        display::native_size(welcome).unwrap_or_default()
    } else {
        display::Size::default()
    };
    Ok(GroupParams {
        source: source_ty,
        size,
        kms,
        mirror169,
        niri_virtual,
        fps: mirror::fps(), // per-group override lands in start_group
        audio_only,
        audio: None, // start_group fills it from the initiating receiver's sound mode
    })
}

/// A receiver ready to join a group, built from the connection layer's state. The
/// `events` sender clone is dropped when the group consumes it, so the group's task
/// still sees the channel close when the receiver goes away.
fn build_receiver(s: &State, id: &str) -> fdo::Result<NewReceiver> {
    let (c, r) = s
        .conns
        .get(id)
        .cloned()
        .zip(s.found.get(id).cloned())
        .ok_or_else(|| fdo::Error::Failed("receiver went away".into()))?;
    let welcome = s.welcomes.get(id).cloned().unwrap_or(Value::Null);
    // HEVC carries the same picture in less data. The receiver's own pick favours H.264 unless HEVC is clearly faster
    // to decode; here HEVC also wins when it decodes about as fast (within 1 ms), as long as the receiver has a decoder
    // for it and its bench ran. A person's choice (SetCodec) beats both.
    let bench_ms = |mime: &str| welcome["bench"][mime]["ms"].as_f64();
    let has_hevc = welcome["decoders"]
        .as_array()
        .is_some_and(|d| d.iter().any(|m| m.as_str() == Some("video/hevc")));
    let chosen = match s.codec.get(id).cloned() {
        Some(c) => c,
        None => saved_codecs().get(id).cloned().unwrap_or_default(),
    };
    let prefer = match (
        chosen.as_str(),
        bench_ms("video/avc"),
        bench_ms("video/hevc"),
    ) {
        ("avc", _, _) => "video/avc".to_string(),
        ("hevc", _, _) if has_hevc => "video/hevc".to_string(),
        (_, Some(avc), Some(hevc)) if has_hevc && hevc <= avc + 1.0 => "video/hevc".to_string(),
        _ => welcome["bench"]["prefer"]
            .as_str()
            .unwrap_or("video/avc")
            .to_string(),
    };
    let address = r
        .address
        .parse()
        .map_err(|_| fdo::Error::Failed(format!("not IPv4: {}", r.address)))?;
    Ok(NewReceiver {
        id: id.to_string(),
        name: r.name,
        address,
        ifname: r.ifname,
        prefer,
        ws: c.media,
        cmd: c.cmd,
        events: c.events,
    })
}

/// Body of D-Bus `AddToGroup` and of the web receiver's auto-join: put an already
/// connected receiver into a running group (shared capture/encoder, spec §12).
async fn attach_to_group(
    st: &Shared,
    conn: &Connection,
    group_id: &str,
    receiver_id: &str,
) -> fdo::Result<()> {
    let slot = st.lock().await.groups.get(group_id).cloned();
    let slot = slot.ok_or_else(|| fdo::Error::InvalidArgs(format!("unknown group: {group_id}")))?;
    reserve_attach(st, receiver_id).await?;
    let built = {
        let s = st.lock().await;
        build_receiver(&s, receiver_id)
    };
    let recv = match built {
        Ok(r) => r,
        Err(e) => {
            st.lock().await.starting.remove(receiver_id);
            return Err(e);
        }
    };
    // Under the slot guard (slot→state order, the same one the watcher takes): a stop that
    // took the group while `add` was handshaking can no longer slip in before the mirror
    // entry and leave it pointing at a group that no longer exists.
    let res = {
        let mut guard = slot.lock().await;
        match guard.as_mut() {
            Some(g) => {
                let res = g.add(recv).await;
                if res.is_ok() {
                    let mut s = st.lock().await;
                    s.mirrors
                        .insert(receiver_id.to_string(), group_id.to_string());
                    s.starting.remove(receiver_id);
                }
                res
            }
            None => Err(format!("group stopped: {group_id}")),
        }
    };
    if let Err(e) = res {
        st.lock().await.starting.remove(receiver_id);
        return Err(fdo::Error::Failed(e));
    }
    changed(conn).await;
    Ok(())
}

/// Reserve a connected, idle receiver for one attach. The check and the take share one
/// state lock, so two AddToGroup calls for the same receiver — or an AddToGroup racing a
/// MirrorGroup start — cannot both pass and end up in two groups.
async fn reserve_attach(st: &Shared, receiver_id: &str) -> fdo::Result<()> {
    let mut s = st.lock().await;
    if !s.conns.contains_key(receiver_id) {
        return Err(fdo::Error::Failed(format!("not connected: {receiver_id}")));
    }
    if s.mirrors.contains_key(receiver_id) {
        return Err(fdo::Error::Failed(format!(
            "already mirroring to {receiver_id}"
        )));
    }
    if s.starting.contains(receiver_id) {
        return Err(fdo::Error::Failed(format!(
            "a group with {receiver_id} is starting"
        )));
    }
    s.starting.insert(receiver_id.to_string());
    Ok(())
}

/// Shared body of D-Bus `MirrorGroup`/`MirrorGroupFps`: connect missing receivers, create the
/// group with the chosen stream rate.
async fn start_group_shared(
    luch: &Luch,
    receiver_ids: Vec<String>,
    source: String,
    fps: u32,
    conn: &Connection,
) -> fdo::Result<String> {
    if receiver_ids.is_empty() {
        return Err(fdo::Error::InvalidArgs("no receivers".into()));
    }
    for id in &receiver_ids {
        ensure_connected(luch, id, conn).await?;
    }
    let gid = format!("g-{}", uuid::Uuid::new_v4());
    start_group(&luch.st, conn, &gid, &receiver_ids, &source, (0, 0), fps).await?;
    Ok(gid)
}

/// Create a group over `receiver_ids` (all connected, none mirroring yet) and register
/// it: `groups` + `mirrors` entries, a ReceiversChanged signal, and the watcher that
/// reaps it when it ends on its own. The capture/encoder/handshake setup runs outside
/// the state lock; a failure leaves no trace in the maps.
async fn start_group(
    st: &Shared,
    conn: &Connection,
    gid: &str,
    receiver_ids: &[String],
    source: &str,
    requested: (u32, u32),
    fps: u32,
) -> fdo::Result<()> {
    reserve_receivers(st, receiver_ids).await?;
    let started = start_reserved_group(st, conn, gid, receiver_ids, source, requested, fps).await;
    if started.is_err() {
        release_receivers(st, receiver_ids).await;
    }
    started
}

/// Check and take every receiver id in one lock: a duplicate inside the call, an id that
/// is already mirroring, and an id another call is starting are all rejected before any
/// capture is created. `release_receivers` undoes it on failure.
async fn reserve_receivers(st: &Shared, receiver_ids: &[String]) -> fdo::Result<()> {
    let mut s = st.lock().await;
    let mut seen = std::collections::HashSet::new();
    for id in receiver_ids {
        if !seen.insert(id.as_str()) {
            return Err(fdo::Error::InvalidArgs(format!("duplicate receiver: {id}")));
        }
        if s.mirrors.contains_key(id) {
            return Err(fdo::Error::Failed(format!("already mirroring to {id}")));
        }
        if s.starting.contains(id) {
            return Err(fdo::Error::Failed(format!("a group with {id} is starting")));
        }
    }
    for id in receiver_ids {
        s.starting.insert(id.clone());
    }
    Ok(())
}

async fn release_receivers(st: &Shared, receiver_ids: &[String]) {
    let mut s = st.lock().await;
    for id in receiver_ids {
        s.starting.remove(id);
    }
}

/// Body of `start_group` under a held reservation.
async fn start_reserved_group(
    st: &Shared,
    conn: &Connection,
    gid: &str,
    receiver_ids: &[String],
    source: &str,
    requested: (u32, u32),
    fps: u32,
) -> fdo::Result<()> {
    let (recvs, welcome) = {
        let s = st.lock().await;
        let recvs = receiver_ids
            .iter()
            .map(|id| build_receiver(&s, id))
            .collect::<fdo::Result<Vec<_>>>()?;
        let welcome = s
            .welcomes
            .get(&receiver_ids[0])
            .cloned()
            .unwrap_or(Value::Null);
        (recvs, welcome)
    };
    // One audio profile per group (spec §14): the sound comes from the shared encoder, so it
    // is decided once, by the receiver that starts the show. Receivers joining later stream
    // with it; their own SetAudio applies to shows they start.
    let mut params = group_params(source, requested, &welcome)?;
    params.fps = fps;
    // Sound only always carries sound, whatever the person chose for mirroring.
    if params.audio_only
        && !welcome["modes"]
            .as_array()
            .is_some_and(|m| m.iter().any(|x| x == "audio"))
    {
        return Err(fdo::Error::NotSupported(
            "this receiver version cannot play sound only yet: update it".into(),
        ));
    }
    let chosen_audio = if params.audio_only {
        "on".to_string()
    } else {
        audio_choice(st, &receiver_ids[0]).await
    };
    params.audio = limits::start_audio(&chosen_audio, &welcome, mirror::AUDIO_FRAME_MS);
    let group = MirrorGroup::start(gid.to_string(), params, recvs)
        .await
        .map_err(fdo::Error::Failed)?;
    let notify = group.stopped_notify();
    let slot: GroupSlot = Arc::new(Mutex::new(Some(group)));
    {
        let mut s = st.lock().await;
        s.groups.insert(gid.to_string(), slot.clone());
        for id in receiver_ids {
            s.mirrors.insert(id.clone(), gid.to_string());
            s.starting.remove(id);
        }
    }
    changed(conn).await;
    for rid in receiver_ids {
        let (st, conn, rid) = (st.clone(), conn.clone(), rid.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await; // once the receiver is ready
            push_now_playing(&st, &conn, Some(&rid)).await;
        });
    }
    spawn_group_watcher(st.clone(), conn.clone(), gid.to_string(), notify, slot);
    Ok(())
}

/// Body of the D-Bus `Mirror`/`MirrorSized` methods and of the web receiver's auto-start:
/// one session to `id`, run as a group of one ("g-<receiver id>").
async fn start_mirror(
    st: &Shared,
    conn: &Connection,
    id: &str,
    source: &str,
    requested: (u32, u32),
    fps: u32,
) -> fdo::Result<()> {
    let gid = format!("g-{id}");
    start_group(st, conn, &gid, &[id.to_string()], source, requested, fps).await
}

/// Reaps receivers that left on their own (stop event, dead control channel) and stops
/// the group when its pipeline dies or its last receiver leaves; then unregisters it and
/// signals ReceiversChanged. The group's notify fires on every receiver end and on a
/// fatal pipeline error, so a receiver leaving mid-session is promptly gone from D-Bus.
fn spawn_group_watcher(
    st: Shared,
    conn: Connection,
    gid: String,
    notify: Arc<Notify>,
    slot: GroupSlot,
) {
    tokio::spawn(async move {
        // Register for the next notification *before* inspecting the group: notify_waiters
        // stores no permit, so a receiver ending between the check and the await would be lost.
        let notified = notify.notified();
        tokio::pin!(notified);
        loop {
            notified.as_mut().enable();
            let (stop, dirty, ended) = {
                let mut guard = slot.lock().await;
                let Some(g) = guard.as_mut() else {
                    break; // a D-Bus method stopped the group and took it
                };
                let ended = g.reap();
                let live = g.receiver_ids();
                let dirty = {
                    let mut s = st.lock().await;
                    let before = s.mirrors.len();
                    s.mirrors.retain(|rid, g2| *g2 != gid || live.contains(rid));
                    let dirty = s.mirrors.len() != before;
                    // A receiver that left on its own takes its stats baseline with it.
                    let State {
                        mirrors,
                        stats_snap,
                        ..
                    } = &mut *s;
                    stats_snap.retain(|rid, _| mirrors.contains_key(rid));
                    dirty
                };
                let stop = if g.pipeline_dead() || g.receiver_count() == 0 {
                    guard.take()
                } else {
                    None
                };
                (stop, dirty, ended)
            };
            // The receiver told why it stopped: the one reason worth a desktop notification is
            // "the receiver could not show the picture" (draw-over-other-apps not allowed).
            for e in ended {
                if let Some(reason) = e
                    .reason
                    .filter(|r| r.starts_with("the receiver could not show"))
                {
                    reports::notify(
                        &conn,
                        &format!("{} did not show the picture", e.name),
                        reason,
                        None,
                    )
                    .await;
                }
            }
            let Some(group) = stop else {
                if dirty {
                    changed(&conn).await;
                }
                notified.as_mut().await;
                // A completed `Notified` future is terminal: without a fresh one every
                // later `await` returns Ready at once and this loop spins hot.
                notified.set(notify.notified());
                continue;
            };
            {
                let mut s = st.lock().await;
                s.groups.remove(&gid);
                s.mirrors.retain(|_, g2| *g2 != gid);
                let State {
                    mirrors,
                    stats_snap,
                    ..
                } = &mut *s;
                stats_snap.retain(|rid, _| mirrors.contains_key(rid));
            }
            group.stop().await;
            changed(&conn).await;
            break;
        }
    });
}

// ---------- web receiver ----------

/// The whole receiver in one file, served at `/`.
const PAGE: &str = include_str!("web.html");

/// Source IPv4 of the default route; a UDP connect() only picks it, no packet leaves.
fn lan_ip() -> Option<std::net::IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:53").ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_loopback()).then_some(ip)
}

async fn serve_web(st: Shared, dbus: Connection, sender_id: String, host: String) {
    let mut identity = match config_dir()
        .map_err(Box::<dyn Error>::from)
        .and_then(|dir| web_tls::Identity::load(&dir, lan_ip()))
    {
        Ok(identity) => identity,
        Err(e) => return eprintln!("[luchd] web receiver off: TLS identity: {e}"),
    };
    let l = match tokio::net::TcpListener::bind(("0.0.0.0", p::WEB_PORT)).await {
        Ok(l) => l,
        Err(e) => return eprintln!("[luchd] web receiver off: cannot bind {}: {e}", p::WEB_PORT),
    };
    eprintln!(
        "[luchd] web receiver at https://<this host>:{}/ (local self-signed certificate)",
        p::WEB_PORT
    );
    loop {
        let Ok((mut stream, peer)) = l.accept().await else {
            continue;
        };
        let Ok(local) = stream.local_addr() else {
            continue;
        };
        let acceptor = match identity.for_ip(local.ip()) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("[luchd] web TLS: {e}");
                continue;
            }
        };
        let (st, dbus, sid, host) = (st.clone(), dbus.clone(), sender_id.clone(), host.clone());
        tokio::spawn(async move {
            let result: io::Result<()> = async {
                let mut first = [0u8; 1];
                timeout(HANDSHAKE_TIMEOUT, stream.peek(&mut first)).await
                    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "HTTP/TLS timeout"))??;
                if first[0] != 22 { // TLS handshake record; old HTTP links redirect on the same port.
                    let head = timeout(HANDSHAKE_TIMEOUT, read_http_head(&mut stream)).await
                        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "HTTP header timeout"))??;
                    let text = String::from_utf8_lossy(&head);
                    let path = text.split_whitespace().nth(1).filter(|p| p.starts_with('/')).unwrap_or("/");
                    let response = format!("HTTP/1.1 302 Found\r\nLocation: https://{local}{path}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    stream.write_all(response.as_bytes()).await?;
                    return stream.shutdown().await;
                }
                let stream = timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await
                    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS handshake timeout"))??;
                web_session(stream, peer, st, dbus, sid, host).await
            }.await;
            if let Err(e) = result {
                eprintln!("[luchd] web {peer}: {e}");
            }
        });
    }
}

/// One browser connection: any GET serves the page, `/ws` upgrades and becomes a receiver.
async fn web_session<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    mut stream: S,
    peer: SocketAddr,
    st: Shared,
    dbus: Connection,
    sender_id: String,
    host: String,
) -> io::Result<()> {
    let head = timeout(HANDSHAKE_TIMEOUT, read_http_head(&mut stream))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "HTTP header timeout"))??;
    let text = String::from_utf8_lossy(&head);
    let first = text.split("\r\n").next().unwrap_or_default().to_string();
    let path = first.split(' ').nth(1).unwrap_or_default().to_string();
    if text.to_ascii_lowercase().contains("upgrade: websocket")
        && path.split('?').next() == Some("/ws")
    {
        // Replay the consumed header so tungstenite validates the entire upgrade request.
        let stream = Prefixed {
            inner: std::io::Cursor::new(head).chain(stream),
        };
        let ws = accept_async(stream)
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        return run_web(ws, peer, st, dbus, sender_id, host).await;
    }
    // Any GET (not just /) serves the page: TV browsers append or mangle paths.
    let (code, body, ctype) = if first.starts_with("GET ") {
        ("200 OK", PAGE, "text/html; charset=utf-8")
    } else {
        ("404 Not Found", "not found", "text/plain")
    };
    let resp = format!("HTTP/1.1 {code}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", body.len());
    stream.write_all(resp.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.shutdown().await
}

async fn read_http_head<S: AsyncRead + Unpin>(stream: &mut S) -> io::Result<Vec<u8>> {
    let mut head = Vec::with_capacity(512);
    loop {
        if head.len() == 8192 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP header too large",
            ));
        }
        head.push(stream.read_u8().await?);
        if head.ends_with(b"\r\n\r\n") {
            return Ok(head);
        }
    }
}

/// Read the already consumed HTTP header before continuing with the underlying TCP/TLS stream.
struct Prefixed<S> {
    inner: tokio::io::Chain<std::io::Cursor<Vec<u8>>, S>,
}

impl<S: AsyncRead + Unpin> AsyncRead for Prefixed<S> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for Prefixed<S> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        std::pin::Pin::new(self.inner.get_mut().1).poll_write(cx, buf)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(self.inner.get_mut().1).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(self.inner.get_mut().1).poll_shutdown(cx)
    }
}

/// A web receiver: hello/welcome over the websocket, then it behaves like any other receiver
/// (start/ready, keyframe, pings); media goes back over the same socket as binary frames.
async fn run_web<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    ws: WebSocketStream<S>,
    peer: SocketAddr,
    st: Shared,
    dbus: Connection,
    sender_id: String,
    host: String,
) -> io::Result<()> {
    let (sink, mut stream) = ws.split();
    let hello = match timeout(HANDSHAKE_TIMEOUT, stream.next()).await {
        Ok(Some(Ok(Message::Text(s)))) => {
            serde_json::from_str::<Value>(s.as_str()).unwrap_or(Value::Null)
        }
        Err(_) => {
            eprintln!(
                "[luchd] web {peer}: no hello in {} s",
                HANDSHAKE_TIMEOUT.as_secs()
            );
            return Ok(());
        }
        _ => return Ok(()), // a non-text first message: just drop it
    };
    if hello["t"] != "hello" {
        eprintln!("[luchd] web {peer}: expected hello, got {hello}");
        return Ok(());
    }
    let id = format!("web-{}", uuid::Uuid::new_v4());
    let name = hello["name"].as_str().unwrap_or("Web").to_string();
    let mode = hello["mode"].as_str().unwrap_or("fast").to_string();
    let (cmd_tx, cmd_rx) = mpsc::channel(8);
    let (events, _) = broadcast::channel(16);
    // A deep media queue is latency: 16 frames at 48 fps is a third of a second before a
    // slow browser even starts. Four frames bound the lead-in; loss handling is AwaitingIdr.
    // Video: four frames (a third of a second of lead-in at 48 fps is already too much).
    // Audio: separate and slightly deeper, so 200 small messages a second never take the
    // slot a video frame needs; both stay shallow because the socket writer is the limit.
    let (media_tx, media_rx) = mpsc::channel::<bytes::Bytes>(4);
    let (audio_tx, audio_rx) = mpsc::channel::<bytes::Bytes>(8);
    // Browsers: H.264 everywhere (WebCodecs HEVC support is not universal).
    let welcome = json!({"t": "welcome", "v": p::VERSION, "id": sender_id, "name": host, "app": p::APP,
                         "bench": {"prefer": "video/avc"}, "hw": {"display": hello["display"]}});
    {
        let mut s = st.lock().await;
        s.found.insert(
            id.clone(),
            Receiver {
                name: name.clone(),
                model: hello["model"].as_str().unwrap_or_default().to_string(),
                address: peer.ip().to_string(),
                ifname: String::new(),
                ifindex: 0,
                port: p::WEB_PORT,
                service: String::new(),
                id: id.clone(),
            },
        );
        s.welcomes.insert(id.clone(), welcome.clone());
        s.conns.insert(
            id.clone(),
            Conn {
                cmd: cmd_tx.clone(),
                events: events.clone(),
                media: Some(mirror::group::WsMedia {
                    video: media_tx,
                    audio: audio_tx,
                }),
                addrs: None,
            },
        );
    }
    eprintln!("[luchd] web receiver: {name} ({peer})");
    changed(&dbus).await;
    let _ = cmd_tx.send(Cmd::Send(welcome)).await; // completes the hello, like any other receiver

    // Writer: control JSON and media binary share the socket.
    let wname = name.clone();
    tokio::spawn(async move {
        let (mut cmd_rx, mut media_rx, mut audio_rx, mut sink) = (cmd_rx, media_rx, audio_rx, sink);
        let why = loop {
            tokio::select! {
                c = cmd_rx.recv() => match c {
                    Some(Cmd::Send(v)) => { if sink.send(Message::Text(v.to_string().into())).await.is_err() { break "write failed"; } }
                    Some(Cmd::Bye) => { let _ = sink.send(Message::Text(json!({"t": "bye"}).to_string().into())).await; break "disconnect requested"; }
                    None => break "control closed",
                },
                m = media_rx.recv() => match m {
                    Some(b) => { if sink.send(Message::Binary(b)).await.is_err() { break "write failed"; } }
                    None => break "media closed",
                },
                m = audio_rx.recv() => match m {
                    Some(b) => { if sink.send(Message::Binary(b)).await.is_err() { break "write failed"; } }
                    None => break "audio closed",
                },
            }
        };
        let _ = sink.close().await;
        eprintln!("[luchd] web {wname}: writer done ({why})");
    });

    // Reading lives in its own task: next() is not cancel-safe inside select!.
    let (in_tx, mut inbox) = mpsc::channel::<Value>(16);
    let reader = tokio::spawn(async move {
        loop {
            match stream.next().await {
                Some(Ok(Message::Text(s))) => {
                    if let Ok(v) = serde_json::from_str::<Value>(s.as_str()) {
                        if in_tx.send(v).await.is_err() {
                            break;
                        }
                    }
                }
                // Media flows one way; protocol-level pings are answered by tungstenite itself.
                Some(Ok(
                    Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_),
                )) => {}
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
            }
        }
    });

    // One tap on the TV starts the whole thing; `?mode=none` keeps it manual.
    // This runs in its own task: the `ready` answer comes through the control loop below,
    // so awaiting the handshake here would deadlock it ("did not answer start"). When a
    // single group already runs, the browser joins it and shares its capture/encoder
    // (spec §12) instead of starting a second session.
    if mode != "none" {
        let (st2, dbus2, id2, mode2) = (st.clone(), dbus.clone(), id.clone(), mode.clone());
        let cmd2 = cmd_tx.clone();
        let name2 = name.clone();
        tokio::spawn(async move {
            // Concurrent taps must not each see "no group" and start their own capture:
            // decide start-vs-join under one lock, and hold it through the start so the
            // next tap finds exactly one group.
            static START_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
            let _serial = START_LOCK.lock().await;
            let single = {
                let s = st2.lock().await;
                let mut ids: Vec<String> = s.groups.keys().cloned().collect();
                (ids.len() == 1).then(|| ids.remove(0))
            };
            if let Some(gid) = single {
                match attach_to_group(&st2, &dbus2, &gid, &id2).await {
                    Ok(()) => {
                        eprintln!("[luchd] web {name2}: joined group {gid}");
                        return;
                    }
                    Err(e) => eprintln!(
                        "[luchd] web {name2}: cannot join {gid} ({e}); starting its own session"
                    ),
                }
            }
            let fps = mirror::fps();
            if let Err(e) = start_mirror(&st2, &dbus2, &id2, &mode2, (0, 0), fps).await {
                let _ = cmd2
                    .send(Cmd::Send(json!({"t": "error", "msg": e.to_string()})))
                    .await;
            }
        });
    }

    let mut last_rx = Instant::now();
    let (mut n, mut ping_sent) = (0u64, Instant::now());
    let mut tick = tokio::time::interval(PING_EVERY);
    tick.tick().await;
    let why = loop {
        tokio::select! {
            _ = tick.tick() => {
                if last_rx.elapsed() >= DEAD_AFTER {
                    break "silent for 30 s";
                }
                n += 1;
                ping_sent = Instant::now();
                if cmd_tx.send(Cmd::Send(json!({"t": "ping", "n": n}))).await.is_err() {
                    break "write failed";
                }
            }
            m = inbox.recv() => {
                let Some(v) = m else { break "closed by receiver" };
                last_rx = Instant::now();
                match v["t"].as_str().unwrap_or("") {
                    // Internal event for the mirror task; never sent on the wire.
                    "pong" if v["n"].as_u64() == Some(n) => {
                        let rtt = ping_sent.elapsed().as_secs_f64() * 1000.0;
                        let _ = events.send(json!({"t": "_rtt", "ms": rtt}));
                    }
                    "pong" | "welcome" | "hello" => {}
                    "ping" => {
                        if cmd_tx.send(Cmd::Send(json!({"t": "pong", "n": v["n"]}))).await.is_err() {
                            break "write failed";
                        }
                    }
                    "bye" => break "bye from receiver",
                    "error" => {
                        eprintln!("[luchd] web {name}: error {v}");
                        let _ = events.send(v);
                    }
                    "ready" | "keyframe" | "stop" | "stats" => {
                        let _ = events.send(v);
                    }
                    t => {
                        if cmd_tx.send(Cmd::Send(json!({"t": "error", "code": "unknown", "for": t}))).await.is_err() {
                            break "write failed";
                        }
                    }
                }
            }
        }
    };
    reader.abort();
    let _ = cmd_tx.send(Cmd::Bye).await; // tells the browser why the socket closes
    {
        let mut s = st.lock().await;
        s.conns.remove(&id);
        s.found.remove(&id); // unlike Avahi receivers, nothing re-announces a web one
        s.welcomes.remove(&id);
    }
    changed(&dbus).await;
    eprintln!("[luchd] web {name}: receiver gone ({why})");
    Ok(())
}

// ---------- Avahi ----------

fn ifname(index: i32) -> String {
    let mut buf = [0 as libc::c_char; libc::IF_NAMESIZE];
    // SAFETY: buf is IF_NAMESIZE bytes, as if_indextoname requires.
    let p = unsafe { libc::if_indextoname(index as u32, buf.as_mut_ptr()) };
    if p.is_null() {
        return String::new();
    }
    // SAFETY: on success the buffer holds a NUL-terminated name.
    unsafe { std::ffi::CStr::from_ptr(p) }
        .to_string_lossy()
        .into_owned()
}

/// tun/wg/ppp: point-to-point links (IFF_POINTOPOINT = 0x10).
fn is_tunnel(ifname: &str) -> bool {
    let flags =
        std::fs::read_to_string(format!("/sys/class/net/{ifname}/flags")).unwrap_or_default();
    u32::from_str_radix(flags.trim().trim_start_matches("0x"), 16).is_ok_and(|f| f & 0x10 != 0)
}

/// Interface of the main-table default route (the physical LAN, even under a full-tunnel VPN).
fn lan_iface() -> String {
    std::fs::read_to_string("/proc/net/route")
        .unwrap_or_default()
        .lines()
        .skip(1)
        .find_map(|l| {
            let p: Vec<_> = l.split_whitespace().collect();
            (p.get(1) == Some(&"00000000")).then(|| p[0].to_string())
        })
        .unwrap_or_default()
}

fn txt_map(txt: &[Vec<u8>]) -> HashMap<String, String> {
    txt.iter()
        .filter_map(|e| {
            let s = String::from_utf8_lossy(e);
            let (k, v) = s.split_once('=')?;
            Some((k.to_ascii_lowercase(), v.to_string()))
        })
        .collect()
}

async fn publish(sys: &Connection, id: &str, host: &str) -> zbus::Result<OwnedObjectPath> {
    let group: OwnedObjectPath = sys
        .call_method(Some(AVAHI), "/", Some(AVAHI_SERVER), "EntryGroupNew", &())
        .await?
        .body()
        .deserialize()?;
    let txt: Vec<Vec<u8>> = [
        format!("v={}", p::VERSION),
        format!("id={id}"),
        format!("name={host}"),
    ]
    .into_iter()
    .map(String::into_bytes)
    .collect();
    let eg = Some("org.freedesktop.Avahi.EntryGroup");
    // The sender listens on nothing yet; port 9 (discard) is a placeholder, the TXT is what matters.
    let args = (
        AVAHI_IF_UNSPEC,
        AVAHI_PROTO_INET,
        0u32,
        host,
        p::SENDER_TYPE,
        "",
        "",
        9u16,
        txt,
    );
    sys.call_method(Some(AVAHI), &group, eg, "AddService", &args)
        .await?;
    sys.call_method(Some(AVAHI), &group, eg, "Commit", &())
        .await?;
    Ok(group)
}

async fn resolve(
    sys: &Connection,
    iface: i32,
    proto: i32,
    name: &str,
    ty: &str,
    domain: &str,
) -> zbus::Result<Option<Receiver>> {
    let args = (iface, proto, name, ty, domain, AVAHI_PROTO_INET, 0u32);
    let reply = sys
        .call_method(
            Some(AVAHI),
            "/",
            Some(AVAHI_SERVER),
            "ResolveService",
            &args,
        )
        .await?;
    let (ifindex, _, service, _, _, _host, _, address, port, txt, _): (
        i32,
        i32,
        String,
        String,
        String,
        String,
        i32,
        String,
        u16,
        Vec<Vec<u8>>,
        u32,
    ) = reply.body().deserialize()?;
    let t = txt_map(&txt);
    if t.get("v").is_some_and(|v| v != "0") {
        return Ok(None);
    }
    let Some(id) = t.get("id").cloned() else {
        return Ok(None);
    };
    Ok(Some(Receiver {
        name: t.get("name").cloned().unwrap_or_else(|| service.clone()),
        model: t.get("model").cloned().unwrap_or_default(),
        id,
        address,
        ifname: ifname(ifindex),
        ifindex,
        port,
        service,
    }))
}

// ---------- identity ----------

/// ~/.config/luch (created on demand).
pub(crate) fn config_dir() -> io::Result<std::path::PathBuf> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config")))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no HOME"))?
        .join("luch");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// The video codec chosen per receiver, kept in ~/.config/luch/codec (one `id=codec` line each), so that the choice
/// survives restarts and holds whichever front-end starts the show.
fn saved_codecs() -> HashMap<String, String> {
    saved_choices("codec")
}

fn save_codec(id: &str, codec: &str) {
    save_choice("codec", id, codec)
}

/// A per-receiver choice kept in ~/.config/luch/<file>, one `id=value` line each (codec, audio).
fn saved_choices(file: &str) -> HashMap<String, String> {
    let text = config_dir()
        .and_then(|d| std::fs::read_to_string(d.join(file)))
        .unwrap_or_default();
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.trim().to_string()))
        .collect()
}

fn save_choice(file: &str, id: &str, value: &str) {
    let mut all = saved_choices(file);
    all.insert(id.to_string(), value.to_string());
    if let Ok(d) = config_dir() {
        let text: String = all.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
        let _ = std::fs::write(d.join(file), text);
    }
}

/// The sound mode for a receiver: set in this run, else saved, else "auto".
async fn audio_choice(st: &Shared, id: &str) -> String {
    match st.lock().await.audio.get(id).cloned() {
        Some(m) => m,
        None => saved_choices("audio")
            .remove(id)
            .unwrap_or_else(|| "auto".into()),
    }
}

fn sender_id() -> io::Result<String> {
    let dir = config_dir()?;
    let path = dir.join("id");
    if let Ok(s) = std::fs::read_to_string(&path) {
        if !s.trim().is_empty() {
            return Ok(s.trim().to_string());
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    std::fs::write(&path, format!("{id}\n"))?;
    Ok(id)
}

fn short_host() -> String {
    let h = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
    h.trim()
        .split('.')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("linux")
        .to_string()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    gstreamer::init()?;
    mirror::restore_leftovers();
    mirror::restore_audio();
    let id = sender_id()?;
    let host = short_host();
    let st = Shared::default();
    let sys = Connection::system().await?;
    // ponytail: name collisions on the LAN are not handled (EntryGroup Collision state).
    let _group = match publish(&sys, &id, &host).await {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("[luchd] cannot publish {}: {e}", p::SENDER_TYPE);
            None
        }
    };
    let (discovery, worker) = discovery::Discovery::new();
    let conn = zbus::connection::Builder::session()?
        .name(p::DBUS_NAME)?
        .serve_at(
            p::DBUS_PATH,
            Luch {
                st: st.clone(),
                sender_id: id.clone(),
                host: host.clone(),
                discovery,
            },
        )?
        .build()
        .await?;
    eprintln!(
        "[luchd] {} id={id} name={host}; browsing {}",
        p::APP,
        p::RECEIVER_TYPE
    );
    tokio::spawn(follow_players(st.clone(), conn.clone()));
    {
        // The volume keys while a session sends this computer's sound: to the receivers in a session.
        let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
        tokio::spawn(volume::follow(move |v| {
            let _ = tx.send(v);
        }));
        let st = st.clone();
        tokio::spawn(async move {
            while let Some(v) = rx.recv().await {
                let s = st.lock().await;
                for id in s.mirrors.keys() {
                    if let Some(c) = s.conns.get(id) {
                        let _ = c.cmd.try_send(Cmd::Send(v.clone()));
                    }
                }
            }
        });
    }
    tokio::spawn(serve_web(
        st.clone(),
        conn.clone(),
        id.clone(),
        host.clone(),
    ));
    worker.run(sys, st, conn).await?;
    Err("avahi browse stream ended".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn receiver_ids_are_reserved_atomically_and_duplicates_are_rejected() {
        let st: Shared = Arc::new(Mutex::new(State::default()));
        let twice = vec!["a".to_string(), "a".to_string()];
        assert!(reserve_receivers(&st, &twice).await.is_err());
        assert!(!st.lock().await.starting.contains("a"));
        let one = vec!["a".to_string()];
        reserve_receivers(&st, &one).await.unwrap();
        assert!(
            reserve_receivers(&st, &one).await.is_err(),
            "a second start must not pass the check while the first is creating its group"
        );
        release_receivers(&st, &one).await;
        reserve_receivers(&st, &one).await.unwrap(); // released on failure, reusable
    }

    #[tokio::test]
    async fn attach_and_group_start_reserve_a_receiver_atomically() {
        let st: Shared = Arc::new(Mutex::new(State::default()));
        let id = "r1".to_string();
        {
            let (cmd, _cmd_rx) = mpsc::channel(1);
            let (events, _events_rx) = broadcast::channel(1);
            st.lock().await.conns.insert(
                id.clone(),
                Conn {
                    cmd,
                    events,
                    media: None,
                    addrs: None,
                },
            );
        }
        // Two simultaneous AddToGroup calls: exactly one may hold the receiver, otherwise
        // both groups would add it and leave one of them with a member the maps deny.
        let (a, b) = tokio::join!(reserve_attach(&st, &id), reserve_attach(&st, &id));
        assert_ne!(
            a.is_ok(),
            b.is_ok(),
            "only one attach may reserve the receiver"
        );
        // While an attach handshakes, a MirrorGroup start must not take the same receiver...
        assert!(reserve_receivers(&st, &[id.clone()]).await.is_err());
        release_receivers(&st, &[id.clone()]).await;
        // ...and a running start blocks the attach the same way.
        reserve_receivers(&st, &[id.clone()]).await.unwrap();
        assert!(reserve_attach(&st, &id).await.is_err());
        release_receivers(&st, &[id]).await;
        reserve_attach(&st, &"r1".to_string()).await.unwrap(); // released, reusable
    }

    /// A receiver that has reports gets them taken over the control channel: answered with `reports_taken`, kept on
    /// disk. Needs a session bus (the channel signals changes on it); skipped without one.
    #[tokio::test]
    async fn reports_from_a_receiver_are_taken_and_kept() {
        let Ok(conn) = Connection::session().await else {
            eprintln!("no session bus: skipped");
            return;
        };
        let home = std::env::temp_dir().join(format!("luch-chan-{}", uuid::Uuid::new_v4()));
        std::env::set_var("XDG_CONFIG_HOME", &home);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (client, (server, _)) = tokio::join!(tokio::net::TcpStream::connect(addr), async {
            listener.accept().await.unwrap()
        });
        let (rd, wr) = client.unwrap().into_split();
        let (_cmd_tx, cmd_rx) = mpsc::channel(8);
        let (events, _) = broadcast::channel(16);
        let st = Shared::default();
        tokio::spawn(run_channel(
            "TV".into(),
            "rx".into(),
            BufReader::new(rd),
            wr,
            cmd_rx,
            events,
            st,
            conn,
        ));

        let (srd, mut swr) = server.into_split();
        let mut srd = BufReader::new(srd);
        let id = "3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47";
        let line = json!({"t": "reports", "items": [{"id": id, "kind": "crash", "time": 1, "count": 1,
                          "what": "java.lang.IllegalStateException", "stack": "", "app": "0.4", "android": "5.1", "sdk": 22}]});
        swr.write_all(format!("{line}\n").as_bytes()).await.unwrap();
        let taken = timeout(Duration::from_secs(5), async {
            loop {
                let mut l = String::new();
                if srd.read_line(&mut l).await.unwrap() == 0 {
                    return None;
                }
                let v: Value = serde_json::from_str(&l).unwrap();
                if v["t"] == "reports_taken" {
                    return Some(v);
                }
            }
        })
        .await
        .unwrap()
        .expect("channel closed");
        assert_eq!(taken["ids"], json!([id]));
        assert_eq!(reports::list().len(), 1);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn txt_parses_and_lowercases_keys() {
        let t = txt_map(&[
            b"v=0".to_vec(),
            b"ID=abc".to_vec(),
            b"dec=avc,hevc".to_vec(),
            b"junk".to_vec(),
        ]);
        assert_eq!(t["v"], "0");
        assert_eq!(t["id"], "abc");
        assert_eq!(t["dec"], "avc,hevc");
        assert!(!t.contains_key("junk"));
    }

    #[tokio::test]
    async fn local_https_identity_persists_and_carries_websocket_media() {
        use std::os::unix::fs::PermissionsExt;
        use tokio_rustls::rustls::{
            self,
            pki_types::{pem::PemObject, CertificateDer, ServerName},
        };
        let dir = std::env::temp_dir().join(format!("luch-tls-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let ip = "127.0.0.1".parse().unwrap();
        drop(web_tls::Identity::load(&dir, Some(ip)).unwrap());
        let original = std::fs::read(dir.join("web-cert.pem")).unwrap();
        let key = std::fs::read(dir.join("web-key.pem")).unwrap();
        assert_eq!(
            std::fs::metadata(dir.join("web-key.pem"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let mut identity = web_tls::Identity::load(&dir, Some(ip)).unwrap();
        assert_eq!(original, std::fs::read(dir.join("web-cert.pem")).unwrap());
        identity.for_ip("192.0.2.1".parse().unwrap()).unwrap();
        assert_eq!(key, std::fs::read(dir.join("web-key.pem")).unwrap());
        let renewed = std::fs::read(dir.join("web-cert.pem")).unwrap();
        identity = web_tls::Identity::load(&dir, Some(ip)).unwrap();
        assert_eq!(renewed, std::fs::read(dir.join("web-cert.pem")).unwrap());
        let cert = CertificateDer::from_pem_file(dir.join("web-cert.pem")).unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert).unwrap();
        let config = rustls::ClientConfig::builder_with_provider(
            rustls::crypto::ring::default_provider().into(),
        )
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut tls = identity.acceptor.accept(stream).await.unwrap();
            let head = read_http_head(&mut tls).await.unwrap();
            let mut ws = accept_async(Prefixed {
                inner: std::io::Cursor::new(head).chain(tls),
            })
            .await
            .unwrap();
            let ready = ws.next().await.unwrap().unwrap();
            assert_eq!(ready, Message::Text("{\"t\":\"ready\"}".into()));
            ws.send(Message::Binary(vec![1, 2, 3].into()))
                .await
                .unwrap();
        });
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        // Verify a new LAN IP SAN as well as trust, without disabling certificate validation.
        let tls = connector
            .connect(
                ServerName::IpAddress("192.0.2.1".parse::<std::net::IpAddr>().unwrap().into()),
                stream,
            )
            .await
            .unwrap();
        let (mut ws, response) = tokio_tungstenite::client_async(format!("wss://{addr}/ws"), tls)
            .await
            .unwrap();
        assert_eq!(response.status(), 101);
        ws.send(Message::Text("{\"t\":\"ready\"}".into()))
            .await
            .unwrap();
        assert_eq!(
            ws.next().await.unwrap().unwrap(),
            Message::Binary(vec![1, 2, 3].into())
        );
        task.await.unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn fragmented_http_header_preserves_websocket_handshake() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (mut server, _) = listener.accept().await.unwrap();
        let request = b"GET /ws HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n";
        let task = tokio::spawn(async move {
            let head = timeout(Duration::from_secs(2), read_http_head(&mut server))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(head, request);
            accept_async(Prefixed {
                inner: std::io::Cursor::new(head).chain(server),
            })
            .await
            .unwrap()
        });
        // Keep the initial prefix pending long enough to expose repeated-peek duplication.
        client.write_all(&request[..20]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!task.is_finished());
        for chunk in request[20..].chunks(7) {
            client.write_all(chunk).await.unwrap();
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let mut server = task.await.unwrap();
        let response = read_http_head(&mut client).await.unwrap();
        assert!(response.starts_with(b"HTTP/1.1 101 "));
        let mut client = WebSocketStream::from_raw_socket(
            client,
            tokio_tungstenite::tungstenite::protocol::Role::Client,
            None,
        )
        .await;
        server
            .send(Message::Binary(vec![1, 2, 3].into()))
            .await
            .unwrap();
        assert_eq!(
            client.next().await.unwrap().unwrap(),
            Message::Binary(vec![1, 2, 3].into())
        );
        client
            .send(Message::Text("{\"t\":\"ready\"}".into()))
            .await
            .unwrap();
        assert_eq!(
            server.next().await.unwrap().unwrap(),
            Message::Text("{\"t\":\"ready\"}".into())
        );
    }

    #[tokio::test]
    async fn oversized_http_header_is_rejected() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (mut server, _) = listener.accept().await.unwrap();
        client.write_all(&vec![b'x'; 8193]).await.unwrap();
        let err = timeout(Duration::from_secs(2), read_http_head(&mut server))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn read_msg_limits_and_eof() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            s.write_all(b"{\"t\":\"pong\",\"n\":1}\n").await.unwrap();
            s.write_all(&vec![b'x'; p::MAX_LINE + 10]).await.unwrap();
        });
        let s = tokio::net::TcpStream::connect(a).await.unwrap();
        let (rd, _wr) = s.into_split();
        let mut rd = BufReader::new(rd);
        assert_eq!(read_msg(&mut rd).await.unwrap().unwrap()["t"], "pong");
        assert!(read_msg(&mut rd).await.is_err());
    }

    #[test]
    fn stream_rate_is_limited_to_native_60_or_30() {
        for ok in [0u32, 30, 60] {
            assert_eq!(check_fps(ok).unwrap(), ok);
        }
        for bad in [1u32, 25, 72, 120, 240] {
            assert!(check_fps(bad).is_err(), "{bad}");
        }
    }
}
