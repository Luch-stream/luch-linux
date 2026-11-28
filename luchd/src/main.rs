//! luchd — Luch v0 sender daemon: announces `_luchsrc._tcp`, browses `_luch._tcp`
//! receivers via Avahi's D-Bus API, keeps control channels, mirrors the screen, exposes dev.luch.Luch1.
//! TVs without Luch (DLNA renderers, Chromecasts) are found too and play cast media (tv.rs).

mod cast;
mod direct;
mod discovery;
mod display;
mod enc;
mod find;
mod input;
mod latency;
mod limits;
mod mirror;
mod mpris;
mod notes;
mod pair;
mod present;
mod problem;
mod quiet;
mod report_send;
mod reports;
mod sync;
mod tv;
mod volume;
mod wake;
mod web_auth;
mod web_tls;

use std::{collections::HashMap, error::Error, io, net::SocketAddr, sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use luch_proto::{self as p, ReceiverRow};
use mirror::Cmd;
use serde_json::{json, Value};
use tokio::{
    io::{
        AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
        BufReader,
    },
    net::TcpSocket,
    sync::{broadcast, mpsc, oneshot, Mutex},
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
    /// The TLS port from the TXT "tls" (protocol v1, pairing), 0 for an older receiver.
    tls_port: u16,
    service: String, // Avahi instance name, for ItemRemove
}

/// A live control channel: commands to it, receiver messages (ready, keyframe, stop, ...) from it.
#[derive(Clone)]
struct Conn {
    cmd: mpsc::Sender<Cmd>,
    events: broadcast::Sender<Value>,
    /// Web receiver only: media frames (header + data) to its websocket writer task.
    media: Option<mirror::WebMedia>,
    /// This machine's and the receiver's address on the control connection: what a receiver fetches cast files from,
    /// and whom they are served to. None for the web receiver.
    addrs: Option<(std::net::IpAddr, std::net::IpAddr)>,
}

#[derive(Default)]
struct State {
    found: HashMap<String, Receiver>,
    conns: HashMap<String, Conn>,
    mirrors: HashMap<String, mpsc::Sender<mirror::Ctl>>, // id -> its session (every member of a group: the group's)
    stats: HashMap<String, mirror::Stats>,               // id -> live mirroring stats
    last_stats: HashMap<String, Value>, // id -> the numbers its last session ended with, for Report
    welcomes: HashMap<String, Value>,   // id -> last welcome (hw passport, decoder bench)
    codec: HashMap<String, String>, // id -> "auto" | "avc" | "hevc": a person's choice of the video codec
    audio: HashMap<String, String>, // id -> "auto" | "on" | "off" | "sync": a person's choice of the sound
    playing: HashMap<String, Value>, // id -> last media_state of what it plays for us (PROTOCOL.md 4b)
    volumes: HashMap<String, Value>, // id -> the receiver's last {"level","muted"} (PROTOCOL.md 4e)
    docs: HashMap<String, (u64, u64)>, // id -> (page, pages) of the document it shows for us (4g)
    /// Receivers we stopped: a report still on its way is no longer news, until the next media_ready.
    stopped: std::collections::HashSet<String>,
    /// Receivers asking for pairing: where the code typed here goes, and the state for Pairing (JSON).
    codes: HashMap<String, mpsc::Sender<CodeTyped>>,
    pairing: HashMap<String, Value>,
    /// Receivers connected over TLS (pinned), for Info.
    secure: std::collections::HashSet<String>,
    /// Browsers waiting for this computer's confirmation (web receiver): request id -> what it is (for WebClients)
    /// and the way back for ApproveWeb.
    web_waiting: HashMap<String, (Value, oneshot::Sender<bool>)>,
    web_limits: web_auth::Limits,
    /// id -> how it was found: "mdns", "scan", "ble", "sound", "qr", "manual" (PROTOCOL.md 1).
    via: HashMap<String, std::collections::BTreeSet<&'static str>>,
    /// id -> its Bluetooth signal strength in dBm, when it was heard: the nearest is listed first.
    rssi: HashMap<String, i16>,
    /// id -> the one-time pairing token its QR code or sound gave, for the next hello.
    tokens: HashMap<String, String>,
    /// id -> how its running session captures ("fast", "monitor", ...), for the latency lab's QR code.
    sources: HashMap<String, String>,
    /// The latency lab, while it is on (docs/latency-lab.md).
    lab: Option<latency::Lab>,
    /// id -> the id of the group it plays in (PROTOCOL.md 4k), for every member.
    groups: HashMap<String, String>,
}

async fn m_name(st: &Shared, id: &str) -> String {
    receiver_name(&*st.lock().await, id)
}

/// A receiver's name for a message, its id when it is not on the list.
fn receiver_name(s: &State, id: &str) -> String {
    s.found
        .get(id)
        .map_or_else(|| id.to_string(), |r| r.name.clone())
}

/// A member left its group (removed, or by itself) while the group plays on: it is free again. Only when it still
/// belongs to this session, which it may have left long ago for another.
async fn forget_member(st: &Shared, id: &str, session: &mpsc::Sender<mirror::Ctl>) {
    let mut s = st.lock().await;
    if s.mirrors.get(id).is_some_and(|c| c.same_channel(session)) {
        s.mirrors.remove(id);
        s.groups.remove(id);
        s.stats.remove(id);
        s.sources.remove(id);
    }
}

/// The code a person typed for a receiver, and where Pair's answer goes (the welcome, or why not).
type CodeTyped = (Vec<u8>, PairReply);
type PairReply = oneshot::Sender<Result<String, String>>;
type Shared = Arc<Mutex<State>>;

/// One fake BlueZ on the test bus at a time: the tests that make one share its well-known name.
#[cfg(test)]
pub(crate) static FAKE_BLUEZ: Mutex<()> = Mutex::const_new(());

/// Lists a receiver found another way than mDNS (find.rs). One already listed keeps its entry; what is new is
/// how it was found and how strong its signal is.
async fn found(
    st: &Shared,
    conn: &Connection,
    r: Receiver,
    via: &'static str,
    rssi: Option<i16>,
    token: Option<String>,
) {
    let mut s = st.lock().await;
    let fresh = !s.found.contains_key(&r.id);
    let id = r.id.clone();
    if fresh {
        eprintln!(
            "[luchd] found ({via}): {} id={} {}:{} via {}",
            r.name, r.id, r.address, r.port, r.ifname
        );
        s.found.insert(id.clone(), r);
    }
    let new_way = s.via.entry(id.clone()).or_default().insert(via);
    let stronger = rssi.is_some_and(|x| s.rssi.insert(id.clone(), x) != Some(x));
    if let Some(t) = token {
        s.tokens.insert(id, t);
    }
    drop(s);
    if fresh || new_way || stronger {
        changed(conn).await;
    }
}

async fn find_progress(conn: &Connection, json: &str) {
    if let Ok(e) = SignalEmitter::new(conn, p::DBUS_PATH) {
        let _ = Luch::find_progress(&e, json).await;
    }
}

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
    /// This computer's certificate for TLS receivers; None if it could not be made (then older receivers only).
    ident: Option<Arc<pair::Identity>>,
    finder: find::Finder,
    tvs: Arc<tv::Tvs>,
    http: reqwest::Client,
}

#[interface(name = "dev.luch.Luch1")]
impl Luch {
    /// One row per TV: a Luch receiver, or a TV without Luch (its kind is in `Devices`). `connected` is a control
    /// channel to a Luch receiver, or something a TV plays for us.
    async fn receivers(&self) -> Vec<ReceiverRow> {
        let groups = self.groups().await;
        let st = self.st.lock().await;
        let mut rows = Vec::new();
        for g in groups {
            let w = &g.ways[0];
            let playing = self.tvs.with(&w.id, |_| ()).await.is_some();
            rows.push((
                w.id.clone(),
                w.name.clone(),
                w.model.clone(),
                w.address.clone(),
                w.ifname.clone(),
                w.port,
                st.conns.contains_key(&w.id) || playing,
                st.mirrors.contains_key(&w.id),
            ));
        }
        // Heard over Bluetooth: the strongest signal, the nearest TV, first; then by name.
        rows.sort_by_cached_key(|r| {
            let rssi = st.rssi.get(&r.0).copied().unwrap_or(i16::MIN);
            (std::cmp::Reverse(rssi), r.1.to_lowercase(), r.0.clone())
        });
        rows
    }

    /// Every TV as JSON, one per row of `Receivers`: `id`, `name`, `model`, `address`, `kind` ("luch", "cast",
    /// "dlna": the way luchd uses), `can` (what that way does: "mirror", "media", "photo", "doc", "apps",
    /// "volume", "subs", "rate"), `control_ms` (about how long a command takes to show) and `transports`, every
    /// way to that TV, best first, as {kind, id}.
    async fn devices(&self) -> String {
        Value::Array(self.groups().await.iter().map(tv::describe).collect()).to_string()
    }

    /// Starts an immediate Avahi rescan, a search for other TVs and, at most every 30 s, the automatic ladder (a
    /// sweep of the LAN, then Bluetooth); results arrive via ReceiversChanged, what is tried via FindProgress.
    async fn refresh(&self, #[zbus(connection)] conn: &Connection) {
        self.discovery.refresh();
        self.tvs.refresh.notify_one();
        let _ = self.finder.start(conn, "auto").await;
    }

    /// Climbs the discovery ladder (PROTOCOL.md 1): "auto" (mDNS and a sweep of the LAN, then Bluetooth), "all"
    /// (then also the TV's sound through the microphone, its QR code through the camera, and which TVs offer a direct
    /// connection: only when the person asked), or some of "mdns,scan,ble,sound,qr,direct" at once. Progress comes as
    /// FindProgress.
    async fn find(&self, mode: String, #[zbus(connection)] conn: &Connection) -> fdo::Result<()> {
        self.finder
            .start(conn, &mode)
            .await
            .map_err(fdo::Error::InvalidArgs)
    }

    /// The last FindProgress (JSON).
    async fn find_status(&self) -> String {
        self.finder.status().await
    }

    /// Connects to a receiver straight over its own Wi-Fi Direct network (PROTOCOL.md, "direct"), for when no network
    /// joins this computer and the TV: over Bluetooth the TV is asked (its person allows it, or pairs), this computer
    /// leaves its Wi-Fi for the TV's network while the session lasts and comes back after. Only on a person's request.
    /// `key`: "" for the nearest TV offering it, else the start of its id or its name (DirectOffers). Returns at once
    /// with {"leaves": the Wi-Fi network it leaves, or null}; the steps come as DirectChanged.
    async fn direct(
        &self,
        key: String,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<String> {
        let ctx = direct::Ctx {
            st: self.st.clone(),
            sender_id: self.sender_id.clone(),
            host: self.host.clone(),
            ident: self.ident.clone(),
        };
        direct::start(ctx, conn, &key)
            .await
            .map_err(fdo::Error::Failed)
    }

    /// Ends the session over a TV's network and goes back to the Wi-Fi network before; says what it did.
    async fn direct_off(&self, #[zbus(connection)] conn: &Connection) -> fdo::Result<String> {
        direct::off(&self.st, conn)
            .await
            .map_err(fdo::Error::Failed)
    }

    /// Where a direct connection stands (JSON, as DirectChanged).
    async fn direct_status(&self) -> String {
        direct::status().to_string()
    }

    /// The TVs heard over Bluetooth lately that offer a direct connection, nearest first, and the Wi-Fi network this
    /// computer would leave: {"offers":[{"key","name","rssi"}],"leaves":"<name>"|null}.
    async fn direct_offers(&self) -> String {
        direct::offered().await.to_string()
    }

    /// How each listed receiver was found, and its Bluetooth signal: {"<id>":{"via":["mdns","ble"],"rssi":-48}}.
    async fn discovered(&self) -> String {
        let st = self.st.lock().await;
        let all: serde_json::Map<String, Value> = st
            .found
            .keys()
            .map(|id| {
                let via: Vec<&str> = st
                    .via
                    .get(id)
                    .map(|v| v.iter().copied().collect())
                    .unwrap_or_default();
                (id.clone(), json!({"via": via, "rssi": st.rssi.get(id)}))
            })
            .collect();
        Value::Object(all).to_string()
    }

    /// Lists a receiver from what a person has: its QR link (luch://connect?...) or its address (host[:port]),
    /// checked with a hello. Returns its id.
    async fn add_receiver(
        &self,
        link: String,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<String> {
        let card = find::card::parse_link(&link).ok_or_else(|| {
            fdo::Error::InvalidArgs(format!("not a receiver's link or address: {link}"))
        })?;
        let via = if link.trim().starts_with("luch://") {
            "qr"
        } else {
            "manual"
        };
        let ctx = find::Ctx {
            st: self.st.clone(),
            sender_id: self.sender_id.clone(),
            host: self.host.clone(),
            discovery: self.discovery.clone(),
        };
        find::reach(&ctx, conn, &card, via, None)
            .await
            .map_err(fdo::Error::Failed)?;
        let (ip, _) = card.addr.unwrap_or((std::net::Ipv4Addr::UNSPECIFIED, 0));
        let st = self.st.lock().await;
        st.found
            .values()
            .find(|r| {
                r.address == ip.to_string()
                    && (card.id.is_empty() || r.id.replace('-', "").starts_with(&card.id))
            })
            .map(|r| r.id.clone())
            .ok_or_else(|| fdo::Error::Failed("receiver went away".into()))
    }

    /// Opens and keeps a control channel; returns the receiver's welcome as JSON. A receiver not paired yet answers
    /// with a code on its screen: this then fails with AccessDenied ("... asks for pairing ...") and the connection
    /// waits for the code (Pair), the state in Pairing and PairChanged.
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
            if st.codes.contains_key(&id) {
                return Err(pairing_needed(&id, "the receiver", &st.pairing[&id]));
            }
            if let Some(d) = self.tvs.device(&id) {
                return Err(fdo::Error::NotSupported(format!(
                    "{} has no Luch app: it plays what is cast to it, but cannot show this screen",
                    d.name
                )));
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
        let dir = config_dir().map_err(|e| fdo::Error::Failed(e.to_string()))?;
        let pinned = pair::pinned_fp(&dir, &id);
        let ident = self.ident.as_deref().filter(|_| r.tls_port > 0);
        if ident.is_none() && pinned.is_some() {
            // Paired once, so it spoke TLS: a receiver at its name that does not is not let pass.
            return Err(fdo::Error::AccessDenied(format!(
                "{} was paired but now offers no encryption: refused (another device under its name, or an older \
                 Luch on it). `luchctl forget {id}` to use it unencrypted",
                r.name
            )));
        }
        // A token from the QR code or the sound is good for one hello only (PROTOCOL.md 1e).
        let token = self.st.lock().await.tokens.remove(&id);
        let o = handshake(&r, &self.sender_id, &self.host, ident, pinned, token.as_deref())
            .await
            .map_err(|e| {
                if e.kind() == io::ErrorKind::PermissionDenied {
                    fdo::Error::AccessDenied(format!("{}: {e}. If it was reset or Luch was reinstalled on it, `luchctl forget {id}` and pair again", r.name))
                } else {
                    fdo::Error::Failed(format!("{} ({}%{}): {e}", r.name, r.address, r.ifname))
                }
            })?;
        match o.first["t"].as_str().unwrap_or("") {
            "welcome" if ident.is_some() && pinned.is_none() => {
                Err(fdo::Error::AccessDenied(format!(
                    "{} let this computer in without pairing: not trusted, nothing sent",
                    r.name
                )))
            }
            "welcome" => {
                install(
                    &self.st,
                    conn,
                    r,
                    id,
                    o.first.clone(),
                    o.rd,
                    o.wr,
                    o.addrs,
                    ident.is_some(),
                )
                .await;
                Ok(o.first.to_string())
            }
            "pair" => {
                let (Some(theirs), Some(me)) = (o.receiver_fp, ident) else {
                    return Err(fdo::Error::Failed("pairing asked for without TLS".into()));
                };
                let (tx, rx) = mpsc::channel(1);
                let state = pair_state(&o.first, &r.name);
                {
                    let mut st = self.st.lock().await;
                    st.codes.insert(id.clone(), tx);
                    st.pairing.insert(id.clone(), state.clone());
                }
                pair_changed(conn, &id, &state).await;
                let err = pairing_needed(&id, &r.name, &state);
                tokio::spawn(pairing(
                    self.st.clone(),
                    conn.clone(),
                    r,
                    id,
                    o,
                    me.fp,
                    theirs,
                    dir,
                    rx,
                ));
                Err(err)
            }
            "pair_refused" => Err(fdo::Error::AccessDenied(pair::explain(
                &pair::refused(&o.first),
                &r.name,
            ))),
            _ => Err(fdo::Error::Failed(format!(
                "{}: expected welcome, got {}",
                r.name, o.first
            ))),
        }
    }

    /// The code shown on a receiver that asked for pairing (see Connect), as typed: six digits, spaces allowed.
    /// Both sides then pin each other's certificate; returns the welcome. A wrong code ends this pairing: connect
    /// again for a new code.
    async fn pair(&self, id: String, code: String) -> fdo::Result<String> {
        let code = pair::clean_code(&code).map_err(fdo::Error::InvalidArgs)?;
        let (tx, step) = {
            let st = self.st.lock().await;
            (
                st.codes.get(&id).cloned(),
                st.pairing.get(&id).map(|v| v["step"].clone()),
            )
        };
        let tx = tx.ok_or_else(|| {
            fdo::Error::Failed(format!(
                "no pairing with {id} going on: connect to it first"
            ))
        })?;
        if step.as_ref().and_then(Value::as_str) != Some("code") {
            return Err(fdo::Error::Failed(
                "no code on the TV yet: allow the connection there first".into(),
            ));
        }
        let (rtx, rrx) = oneshot::channel();
        tx.send((code, rtx))
            .await
            .map_err(|_| fdo::Error::Failed("the pairing ended meanwhile".into()))?;
        timeout(Duration::from_secs(20), rrx)
            .await
            .map_err(|_| fdo::Error::Failed("the receiver did not finish pairing".into()))?
            .map_err(|_| fdo::Error::Failed("the pairing ended meanwhile".into()))?
            .map_err(fdo::Error::AccessDenied)
    }

    /// Where pairing with a receiver stands, as JSON: `step` "none", "allow" (waiting for the TV's allow), "code"
    /// (a code is on the TV: Pair), "paired" or "failed" (with `reason` in words), and `name`, `digits`.
    async fn pairing(&self, id: String) -> String {
        self.st
            .lock()
            .await
            .pairing
            .get(&id)
            .cloned()
            .unwrap_or_else(|| json!({"step": "none"}))
            .to_string()
    }

    /// The receivers this computer is paired with, as a JSON array of {id, name, fp, paired (unix time)}.
    async fn paired(&self) -> String {
        let dir = config_dir().ok();
        let all = dir.as_deref().map(pair::pinned).unwrap_or_default();
        let mut rows: Vec<Value> = all
            .into_iter()
            .map(|(id, mut v)| {
                v["id"] = json!(id);
                v
            })
            .collect();
        rows.sort_by_key(|v| v["name"].as_str().unwrap_or("").to_string());
        Value::Array(rows).to_string()
    }

    /// Forgets a receiver's pinned certificate: the next connection pairs again with a new code.
    async fn forget(&self, id: String) -> fdo::Result<()> {
        let dir = config_dir().map_err(|e| fdo::Error::Failed(e.to_string()))?;
        match pair::forget(&dir, &id) {
            Ok(true) => Ok(()),
            Ok(false) => Err(fdo::Error::InvalidArgs(format!("not paired with {id}"))),
            Err(e) => Err(fdo::Error::Failed(e.to_string())),
        }
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
        if !self.st.lock().await.conns.contains_key(&id) {
            self.connect(id.clone(), conn).await?;
        }
        start_mirror(&self.st, conn, &id, &source, (width, height), None).await
    }

    /// Mirrors to several receivers in step (PROTOCOL.md 4k): `ids[0]` shows the picture, the others present what
    /// `roles` says ("video", "audio" or "both"; `roles[0]` is the first one's). `mode` "interactive" keeps the
    /// picture as fast as alone and lets the sound wait for it; "watch" gives everyone the same delay of at least 150
    /// ms, for rooms of speakers and walls of TVs. Every receiver must be connected and announce sync.
    async fn mirror_group(
        &self,
        ids: Vec<String>,
        roles: Vec<String>,
        source: String,
        mode: String,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        let mode = sync::Mode::parse(&mode).ok_or_else(|| {
            fdo::Error::InvalidArgs(format!("mode must be interactive|watch, not {mode}"))
        })?;
        if ids.len() < 2 || ids.len() != roles.len() {
            return Err(fdo::Error::InvalidArgs(
                "two or more receivers, one role each".into(),
            ));
        }
        for id in &ids {
            if !self.st.lock().await.conns.contains_key(id) {
                self.connect(id.clone(), conn).await?;
            }
        }
        let group = GroupRequest {
            role: roles[0].clone(),
            members: ids[1..]
                .iter()
                .cloned()
                .zip(roles[1..].iter().cloned())
                .collect(),
            mode,
        };
        start_mirror(&self.st, conn, &ids[0], &source, (0, 0), Some(group)).await
    }

    /// Puts a receiver into a running group (PROTOCOL.md 4k): `member` names the group by any receiver in it. The
    /// receiver is connected when it is not (pairing as for any connection), gets the group's timeline and D, and
    /// starts from the group's next keyframe; D rises for everyone, once, if it needs more. `role` "" is the picture
    /// and its sound, or a speaker in a group of sound only.
    async fn add_to_group(
        &self,
        member: String,
        id: String,
        role: String,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        if !matches!(role.as_str(), "" | "video" | "audio" | "both") {
            return Err(fdo::Error::InvalidArgs(format!(
                "role video|audio|both, not {role}"
            )));
        }
        let ctl = {
            let s = self.st.lock().await;
            if !s.groups.contains_key(&member) {
                return Err(fdo::Error::InvalidArgs(format!(
                    "{member} is not playing in a group"
                )));
            }
            if s.mirrors.contains_key(&id) {
                return Err(fdo::Error::Failed(format!(
                    "{} is playing already: stop it first",
                    receiver_name(&s, &id)
                )));
            }
            s.mirrors[&member].clone()
        };
        if !self.st.lock().await.conns.contains_key(&id) {
            self.connect(id.clone(), conn).await?;
        }
        let (stats, source) = {
            let mut s = self.st.lock().await;
            let m = group_member(&s, &id, role)?;
            // Busy from now on: a second join of it, or a session of its own, is refused meanwhile.
            if s.mirrors.contains_key(&id) {
                return Err(fdo::Error::Failed(format!(
                    "{} is playing already: stop it first",
                    m.who.name
                )));
            }
            s.mirrors.insert(id.clone(), ctl.clone());
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = ctl.try_send(mirror::Ctl::Join(m, tx));
            let stats = s.stats.get(&member).cloned();
            let source = s.sources.get(&member).cloned();
            drop(s);
            let failed = match timeout(Duration::from_secs(10), rx).await {
                Ok(Ok(Ok(()))) => None,
                Ok(Ok(Err(e))) => Some(e),
                Ok(Err(_)) => Some("the group ended".to_string()),
                Err(_) => Some(format!("{} did not answer", m_name(&self.st, &id).await)),
            };
            if let Some(e) = failed {
                self.st.lock().await.mirrors.remove(&id);
                return Err(fdo::Error::Failed(e));
            }
            (stats, source)
        };
        {
            let mut s = self.st.lock().await;
            let gid = s.groups[&member].clone();
            s.groups.insert(id.clone(), gid);
            if let Some(stats) = stats {
                s.stats.insert(id.clone(), stats);
            }
            if let Some(source) = source {
                s.sources.insert(id.clone(), source);
            }
        }
        changed(conn).await;
        Ok(())
    }

    /// Takes a receiver out of the group it plays in: it stops, the others play on untouched; the last one out ends
    /// the group.
    async fn remove_from_group(
        &self,
        id: String,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        let ctl = {
            let s = self.st.lock().await;
            match (s.groups.contains_key(&id), s.mirrors.get(&id)) {
                (true, Some(c)) => c.clone(),
                _ => {
                    return Err(fdo::Error::InvalidArgs(format!(
                        "{} is not playing in a group",
                        receiver_name(&s, &id)
                    )))
                }
            }
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        ctl.send(mirror::Ctl::Leave(id.clone(), tx))
            .await
            .map_err(|_| fdo::Error::Failed("the group ended".into()))?;
        match rx.await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(fdo::Error::Failed(e)),
            Err(_) => return Err(fdo::Error::Failed("the group ended".into())),
        }
        // At once, so that it can be added again right away (the session tells it too).
        forget_member(&self.st, &id, &ctl).await;
        changed(conn).await;
        Ok(())
    }

    /// A receiver's own correction in ms (positive = later), for the output route it uses now: the slider of a group.
    /// Saved, and applied at once when it is playing in a group.
    async fn set_offset(&self, id: String, ms: f64) -> fdo::Result<()> {
        if !(-1000.0..=1000.0).contains(&ms) {
            return Err(fdo::Error::InvalidArgs(format!(
                "offset {ms} ms is beyond ±1 s"
            )));
        }
        save_offset(&sync::offset_key(&id, &last_route(&id)), ms);
        if self.st.lock().await.mirrors.contains_key(&id) {
            self.send_to(&id, json!({"t": "offset", "ms": ms})).await?;
        }
        Ok(())
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
        if let Some(d) = self.tvs.device(&id) {
            return tv::capabilities(&d).to_string();
        }
        self.st
            .lock()
            .await
            .welcomes
            .get(&id)
            .map(|w| w.to_string())
            .unwrap_or_else(|| "{}".into())
    }

    /// Live mirroring numbers as JSON (sender side + the receiver's last stats), "{}" if idle.
    async fn stats(&self, id: String) -> String {
        let s = self.st.lock().await.stats.get(&id).cloned();
        s.map(|s| s.lock().unwrap().to_string())
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

    /// What a person should know about a receiver, as JSON: `paired` (its certificate is pinned), `encrypted` (the
    /// channel now is TLS), `android` ("Android 5.1", empty until connected once),
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
        let paired = config_dir()
            .ok()
            .and_then(|d| pair::pinned_fp(&d, &id))
            .is_some();
        let encrypted = self.st.lock().await.secure.contains(&id);
        json!({
            "paired": paired,
            "encrypted": encrypted,
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

    /// "Report a problem": one archive in Downloads with this computer's log, the logs of the receivers in use now or
    /// lately (each asked, 10 s at most), what they are, their crash reports and `text`; addresses and names replaced
    /// unless `keep_names`. Nothing is sent anywhere. Returns JSON: `path`, `description` (a few lines for a chat),
    /// `issue` (a new-issue link, filled in) and `notes` (receivers that gave no log, and why).
    async fn report(&self, text: String, keep_names: bool) -> fdo::Result<String> {
        problem::report(self, &text, !keep_names)
            .await
            .map(|v| v.to_string())
            .map_err(|e| fdo::Error::Failed(format!("cannot write the report: {e}")))
    }

    /// "Send to the developers": the report at `path` (one `Report` made since luchd started) to the DSN this build
    /// carries (`LUCH_REPORT_DSN`), as an event and then the zip. Returns JSON: `number` (`LUCH-` and 8 hex digits,
    /// for the person to tell), `event_id`, `attached` and `attach_error` when the zip did not go.
    async fn send_report(&self, path: String) -> fdo::Result<String> {
        let dsn = report_send::dsn().ok_or_else(|| {
            fdo::Error::NotSupported("this build sends reports nowhere (LUCH_REPORT_DSN)".into())
        })?;
        report_send::send(&self.http, &dsn, std::path::Path::new(&path))
            .await
            .map(|v| v.to_string())
            .map_err(fdo::Error::Failed)
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

    /// The latency lab (docs/latency-lab.md). On: returns the address of a page to open full screen on this computer,
    /// and the receiver draws its own pattern over the mirrored picture. Off: both go, and "" is returned. `id` may be
    /// "" while exactly one receiver is mirroring.
    async fn latency_lab(&self, id: String, on: bool) -> fdo::Result<String> {
        let id = if id.is_empty() {
            let s = self.st.lock().await;
            match (s.mirrors.keys().next(), s.mirrors.len()) {
                (Some(one), 1) => one.clone(),
                _ => s.lab.as_ref().map(|l| l.id.clone()).ok_or_else(|| {
                    fdo::Error::InvalidArgs(
                        "name the receiver: none or several are mirroring".into(),
                    )
                })?,
            }
        } else {
            id
        };
        if on && !self.st.lock().await.found.contains_key(&id) {
            return Err(fdo::Error::InvalidArgs(format!("unknown receiver {id}")));
        }
        let url = if on {
            let lab = latency::open(self.st.clone(), id.clone())
                .await
                .map_err(|e| fdo::Error::Failed(format!("latency lab: {e}")))?;
            let url = lab.url.clone();
            self.st.lock().await.lab = Some(lab);
            url
        } else {
            self.st.lock().await.lab = None;
            String::new()
        };
        // Until a control channel is open, `start_mirror` tells the receiver.
        let _ = self.send_to(&id, json!({"t": "latency", "on": on})).await;
        Ok(url)
    }

    async fn stop_mirror(&self, id: String) -> fdo::Result<()> {
        let tx = self.st.lock().await.mirrors.get(&id).cloned();
        let tx = tx.ok_or_else(|| fdo::Error::InvalidArgs(format!("not mirroring: {id}")))?;
        let _ = tx.send(mirror::Ctl::Stop).await;
        Ok(())
    }

    /// Whether a receiver's touch, keyboard, mouse and gamepads may control this computer without asking:
    /// "always" (the person said so once) or "ask" (the default).
    async fn input_choice(&self, id: String) -> String {
        saved_choices(input::ask::CHOICES_FILE)
            .remove(&id)
            .unwrap_or_else(|| "ask".into())
    }

    /// Sets the choice above; "ask" takes an "always" back. Takes effect from the next session.
    async fn set_input_choice(&self, id: String, choice: String) -> fdo::Result<()> {
        if choice != "ask" && choice != "always" {
            return Err(fdo::Error::InvalidArgs(format!(
                "choice must be ask|always, not {choice}"
            )));
        }
        save_choice(input::ask::CHOICES_FILE, &id, &choice);
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
        let forced = match o.transport.as_deref() {
            None | Some("") | Some("auto") => None,
            Some(t) => Some(tv::Kind::parse(t).ok_or_else(|| {
                fdo::Error::InvalidArgs(format!("transport must be luch, cast or dlna, not {t}"))
            })?),
        };
        let groups = self.groups().await;
        let way = tv::pick(&groups, &id, forced).cloned();
        if forced.is_some() && way.is_none() {
            return Err(fdo::Error::InvalidArgs(format!(
                "{id} cannot be reached that way: see Devices for its transports"
            )));
        }
        // Several things for one TV: the row's id stays the handle, whichever way it plays.
        let id = match way {
            Some(w) if w.kind != tv::Kind::Luch => {
                let d = self
                    .tvs
                    .device(&w.id)
                    .ok_or_else(|| fdo::Error::Failed("the TV went away".into()))?;
                return self.cast_to_tv(&id, d, items, o, conn).await;
            }
            Some(w) => w.id,
            None => id,
        };
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
        if let Some(r) = self.tvs.ask(&id, tv::Ask::Cmd(v.clone())).await {
            return r.map(|_| ()).map_err(fdo::Error::Failed);
        }
        v["t"] = json!("media_cmd");
        self.send_to(&id, v).await
    }

    /// The last `media_state` from the receiver as JSON, "{}" when it plays nothing for us.
    async fn media_state(&self, id: String) -> String {
        if let Some(s) = self.tvs.with(&id, |s| s.state()).await {
            return s.to_string();
        }
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
        if let Some(r) = self.tvs.stop(&id).await {
            if let (Some(srv), Some(d)) = (FILES.get(), self.tvs.device(&id)) {
                srv.forget(d.address);
            }
            return r.map(|_| ()).map_err(fdo::Error::Failed);
        }
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
        if let Some(p) = PRESENTER.get() {
            p.end(&id);
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
        if let Some(r) = self.tvs.ask(&id, tv::Ask::Volume(msg.clone())).await {
            return r.map(|v| v.to_string()).map_err(fdo::Error::Failed);
        }
        if self.tvs.device(&id).is_some() {
            return Err(fdo::Error::Failed(
                "this TV's volume can be set while it plays something cast to it".into(),
            ));
        }
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

    /// Shows a PDF or a presentation (.odp, .pptx and the other documents `Cast` takes) on a receiver, with a
    /// presenter view on this computer: current and next slide, speaker notes, timer, laser pointer and ink.
    /// Returns the view's address (http://127.0.0.1:<port>/<token>/), to open in a browser here.
    async fn present(
        &self,
        id: String,
        file: String,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<String> {
        if cast::kind_of(&file) != cast::Kind::Document {
            return Err(fdo::Error::InvalidArgs(
                "present takes a PDF or a presentation (.odp, .pptx...)".into(),
            ));
        }
        let original = std::path::PathBuf::from(file.strip_prefix("file://").unwrap_or(&file))
            .canonicalize()
            .map_err(|e| fdo::Error::InvalidArgs(format!("{file}: {e}")))?;
        let name = original
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // The notes come from the original: a PDF made by LibreOffice has none (notes.rs).
        let out = std::env::temp_dir().join(format!("luch-doc-{}", std::process::id()));
        let src = original.clone();
        let (pdf, notes) = tokio::task::spawn_blocking(move || {
            cast::to_pdf(&src, &out).map(|pdf| {
                let notes = notes::speaker_notes(&src, &pdf);
                (pdf, notes)
            })
        })
        .await
        .map_err(|e| fdo::Error::Failed(e.to_string()))?
        .map_err(fdo::Error::Failed)?;
        let options = json!({ "title": name }).to_string();
        self.cast(
            id.clone(),
            vec![pdf.to_string_lossy().into_owned()],
            options,
            conn,
        )
        .await?;
        let (c, welcome, doc, receiver_name) = {
            let s = self.st.lock().await;
            (
                s.conns.get(&id).cloned(),
                s.welcomes.get(&id).cloned().unwrap_or(Value::Null),
                s.docs.get(&id).copied().unwrap_or((0, 0)),
                s.found.get(&id).map(|r| r.name.clone()).unwrap_or_default(),
            )
        };
        let c = c.ok_or_else(|| fdo::Error::Failed("receiver went away".into()))?;
        let ink = welcome["modes"]
            .as_array()
            .is_some_and(|m| m.iter().any(|x| x == "ink"));
        let srv = PRESENTER
            .get_or_try_init(present::Server::start)
            .await
            .map_err(|e| fdo::Error::Failed(format!("presenter view: {e}")))?;
        let show = present::Show::new(
            id,
            receiver_name,
            name,
            pdf,
            notes,
            ink,
            c.cmd,
            c.events,
            doc.0,
            doc.1,
            Some(quiet_session(conn).await),
        );
        Ok(srv.add(show))
    }

    /// Points or draws on what the receiver shows for us (PROTOCOL.md 4h): `msg` is a `pointer`, `ink` or `blank`
    /// message as JSON. Passed on as it comes: a caller sending pointer moves keeps them to one a frame itself.
    async fn annotate(&self, id: String, msg: String) -> fdo::Result<()> {
        let v: Value =
            serde_json::from_str(&msg).map_err(|e| fdo::Error::InvalidArgs(e.to_string()))?;
        let v = present::sanitize(&v)
            .filter(|v| v["t"] != "doc_page")
            .ok_or_else(|| fdo::Error::InvalidArgs("not a pointer, ink or blank message".into()))?;
        self.send_to(&id, v).await
    }

    /// Do not disturb while presenting or mirroring: "ask" (the GNOME extension asks once a login, see QuietAsk),
    /// "on" (notification banners hidden) or "off".
    #[zbus(property)]
    async fn quiet(&self) -> String {
        quiet::with(|q| q.mode().as_str().to_string())
    }

    #[zbus(property)]
    async fn set_quiet(&self, mode: String) -> fdo::Result<()> {
        let m = quiet::Mode::parse(&mode)
            .ok_or_else(|| fdo::Error::InvalidArgs("ask, on or off".into()))?;
        quiet::save_mode(m);
        quiet::with(|q| q.set_mode(m));
        Ok(())
    }

    /// The person's answer to QuietAsk: hide notifications while presenting (true) or not; kept until logout.
    async fn answer_quiet(&self, hide: bool) {
        quiet::with(|q| q.answer(hide));
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

    /// The browsers allowed to watch this screen through the web receiver, and the one waiting to be (with
    /// `waiting` and its `code`), as a JSON array of {id, name, address, allowed, used}.
    async fn web_clients(&self) -> String {
        let mut rows: Vec<Value> = self
            .st
            .lock()
            .await
            .web_waiting
            .values()
            .map(|(v, _)| v.clone())
            .collect();
        if let Ok(d) = config_dir() {
            rows.extend(web_auth::list(&d));
        }
        Value::Array(rows).to_string()
    }

    /// Lets a waiting browser in (or not): `request` from WebApproval or WebClients. The notification's buttons do
    /// the same.
    async fn approve_web(&self, request: String, allow: bool) -> fdo::Result<()> {
        let w = self.st.lock().await.web_waiting.remove(&request);
        let (_, tx) =
            w.ok_or_else(|| fdo::Error::InvalidArgs(format!("no browser waiting as {request}")))?;
        let _ = tx.send(allow);
        Ok(())
    }

    /// Forgets an allowed browser: it is asked about again next time.
    async fn forget_web(&self, id: String) -> fdo::Result<()> {
        let d = config_dir().map_err(|e| fdo::Error::Failed(e.to_string()))?;
        match web_auth::forget(&d, &id) {
            Ok(true) => Ok(()),
            Ok(false) => Err(fdo::Error::InvalidArgs(format!("no browser {id}"))),
            Err(e) => Err(fdo::Error::Failed(e.to_string())),
        }
    }

    /// A browser opened the web receiver and waits to be allowed: its page shows `code` too.
    #[zbus(signal)]
    async fn web_approval(
        e: &SignalEmitter<'_>,
        request: &str,
        name: &str,
        address: &str,
        code: &str,
    ) -> zbus::Result<()>;

    /// Pairing with a receiver moved on (JSON as in Pairing): "allow", "code", "paired" or "failed".
    #[zbus(signal)]
    async fn pair_changed(e: &SignalEmitter<'_>, id: &str, state: &str) -> zbus::Result<()>;
    /// What the discovery ladder is trying (JSON, as FindStatus).
    #[zbus(signal)]
    async fn find_progress(e: &SignalEmitter<'_>, status: &str) -> zbus::Result<()>;
    /// A direct connection's step (JSON, as DirectStatus).
    #[zbus(signal)]
    async fn direct_changed(e: &SignalEmitter<'_>, status: &str) -> zbus::Result<()>;

    /// A receiver's `media_state` changed (JSON); {"state":"stopped"} when it ended.
    #[zbus(signal)]
    async fn media_state_changed(e: &SignalEmitter<'_>, id: &str, state: &str) -> zbus::Result<()>;

    /// A presentation or mirroring began and Quiet is "ask": ask the person, answer with AnswerQuiet.
    #[zbus(signal)]
    async fn quiet_ask(e: &SignalEmitter<'_>) -> zbus::Result<()>;
}

impl Luch {
    /// Every TV with the ways to it, from the Luch receivers and the TVs found.
    async fn groups(&self) -> Vec<tv::Group> {
        let luch: Vec<tv::Entry> = self
            .st
            .lock()
            .await
            .found
            .values()
            .map(|r| tv::Entry {
                kind: tv::Kind::Luch,
                id: r.id.clone(),
                name: r.name.clone(),
                model: r.model.clone(),
                address: r.address.clone(),
                ifname: r.ifname.clone(),
                port: r.port,
            })
            .collect();
        let others = self.tvs.devices();
        tv::group(luch.into_iter().chain(others.iter().map(tv::Entry::from)))
    }

    /// `Cast` to a TV without Luch: the same message a Luch receiver would get, played through the TV's own way.
    async fn cast_to_tv(
        &self,
        row: &str,
        d: tv::Device,
        items: Vec<String>,
        mut o: cast::Options,
        conn: &Connection,
    ) -> fdo::Result<String> {
        // Such a TV cannot send a page's headers: a link that needs them goes through luchd.
        if !o.headers.is_empty() && o.via.is_none() {
            o.via = Some("sender".into());
        }
        let me = tv::local_for(d.address)
            .map_err(|e| fdo::Error::Failed(format!("no way to {}: {e}", d.address)))?;
        let srv = files()
            .await
            .map_err(|e| fdo::Error::Failed(format!("cannot serve files: {e}")))?;
        let msg = cast::build(&items, &o, srv, d.address, me).map_err(fdo::Error::InvalidArgs)?;
        let kind = msg["t"].as_str().unwrap_or("").to_string();
        let mut m = tv::Media::from_message(&msg).map_err(fdo::Error::NotSupported)?;
        match d.kind {
            tv::Kind::Dlna => tv::dlna::prepare(srv, &m),
            tv::Kind::Cast => tv::castv2::prepare(srv, &mut m),
            tv::Kind::Luch => {}
        }
        let (session, mut states) = tv::play(&d, m, &self.http)
            .await
            .map_err(fdo::Error::Failed)?;
        eprintln!("[luchd] {} plays {kind} over {}", d.name, d.kind.as_str());
        self.tvs.start(row, session.clone()).await;
        changed(conn).await;
        let (conn, row, tvs, address) =
            (conn.clone(), row.to_string(), self.tvs.clone(), d.address);
        tokio::spawn(async move {
            while let Some(v) = states.recv().await {
                // A session something newer replaced on this TV is no longer news, and its files stay for the new one.
                if tvs.with(&row, |s| !s.same(&session)).await == Some(true) {
                    continue;
                }
                media_changed(&conn, &row, &v).await;
                if v["state"] == "stopped" {
                    srv.forget(address);
                    changed(&conn).await;
                }
            }
        });
        Ok(kind)
    }

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

/// A control channel's two ends, over TCP or TLS alike.
type Reader = BufReader<Box<dyn AsyncRead + Send + Unpin>>;
type Writer = Box<dyn AsyncWrite + Send + Unpin>;

/// One JSON line, at most MAX_LINE bytes. Ok(None) on EOF.
pub(crate) async fn read_msg<R: AsyncBufRead + Unpin>(rd: &mut R) -> io::Result<Option<Value>> {
    let mut buf = Vec::new();
    let n = (&mut *rd)
        .take(p::MAX_LINE as u64 + 1)
        .read_until(b'\n', &mut buf)
        .await?;
    if n == 0 {
        return Ok(None);
    }
    p::control::parse_line(&buf)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub(crate) async fn send<W: AsyncWrite + Unpin>(wr: &mut W, v: &Value) -> io::Result<()> {
    wr.write_all(p::control::line(v).as_bytes()).await?;
    wr.flush().await
}

/// A control connection just opened: the receiver's first answer to hello (welcome, pair or pair_refused) and the
/// two ends, with the receiver's certificate when it is TLS.
struct Opened {
    first: Value,
    rd: Reader,
    wr: Writer,
    addrs: (std::net::IpAddr, std::net::IpAddr),
    receiver_fp: Option<pair::Fingerprint>,
}

/// What a sender says first (section 3). `token`: the one-time pairing token from the receiver's QR code or sound
/// (section 1e). `probe`: discovery only asks who is there, and says bye after the welcome. This is the plain hello;
/// open_channel makes it the TLS one.
fn hello(id: &str, host: &str, token: Option<&str>, probe: bool) -> Value {
    let mut h = p::control::hello(id, host, &["mirror", "media", "photo", "reports", "input"]);
    if let Some(t) = token {
        h["token"] = json!(t);
    }
    if probe {
        h["probe"] = json!(true);
        h["want"] = json!([]);
    }
    h
}

/// Connects to `addr` from `ifname` (section 2: the default route may be a full-tunnel VPN, so it must never decide
/// where this goes; None only for loopback).
async fn connect_from(
    addr: SocketAddr,
    ifname: Option<&str>,
    connect_in: Duration,
) -> io::Result<tokio::net::TcpStream> {
    let sock = TcpSocket::new_v4()?;
    if let Some(i) = ifname {
        sock.bind_device(Some(i.as_bytes()))?;
    }
    timeout(connect_in, sock.connect(addr))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connect timed out"))?
}

/// Socket pinned to the interface the receiver was discovered on. With `ident`, TLS to the receiver's TLS port, its
/// certificate taken only if it is `pinned` (when there is one); without a pin the hello asks the receiver to pair
/// even if it knows this computer. `token` (section 1e) goes either way: on the plain port it lets this computer in
/// without the Allow on the TV; over TLS the receiver uses it up but still asks for its code.
async fn handshake(
    r: &Receiver,
    id: &str,
    host: &str,
    ident: Option<&pair::Identity>,
    pinned: Option<pair::Fingerprint>,
    token: Option<&str>,
) -> io::Result<Opened> {
    let port = if ident.is_some() { r.tls_port } else { r.port };
    let addr: SocketAddr = format!("{}:{}", r.address, port)
        .parse()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let stream = connect_from(addr, Some(&r.ifname), HANDSHAKE_TIMEOUT).await?;
    open_channel(stream, &hello(id, host, token, false), ident, pinned).await
}

/// Who answers at `addr`: the welcome of a probe hello on the plain port, then bye (discovery, find). The welcome
/// must come within the 2 s a receiver has (section 3).
async fn probe(
    addr: SocketAddr,
    ifname: Option<&str>,
    id: &str,
    host: &str,
    token: Option<&str>,
    connect_in: Duration,
) -> io::Result<Value> {
    let h = hello(id, host, token, true);
    let mut o = timeout(connect_in + find::scan::WELCOME_TIMEOUT, async {
        let stream = connect_from(addr, ifname, connect_in).await?;
        open_channel(stream, &h, None, None).await
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no welcome"))??;
    let _ = send(&mut o.wr, &json!({"t": "bye"})).await;
    Ok(o.first)
}

/// TLS (or not) and hello on a connected stream; the half of handshake that tests drive over localhost.
async fn open_channel(
    stream: tokio::net::TcpStream,
    hello: &Value,
    ident: Option<&pair::Identity>,
    pinned: Option<pair::Fingerprint>,
) -> io::Result<Opened> {
    stream.set_nodelay(true)?;
    let addrs = (stream.local_addr()?.ip(), stream.peer_addr()?.ip());
    let (rd, wr, receiver_fp) = match ident {
        Some(me) => tls_halves(stream, addrs.1, me, pinned).await?,
        None => {
            let (rd, wr) = stream.into_split();
            (
                Box::new(rd) as Box<dyn AsyncRead + Send + Unpin>,
                Box::new(wr) as Writer,
                None,
            )
        }
    };
    greet(rd, wr, addrs, receiver_fp, hello, ident.is_some(), pinned).await
}

/// TLS to a receiver over any stream (TCP, or the Bluetooth channel of a direct connection, direct/mod.rs): its two
/// ends and the receiver's certificate, taken only if it is `pinned` (when there is one).
async fn tls_halves<S>(
    stream: S,
    peer: std::net::IpAddr,
    me: &pair::Identity,
    pinned: Option<pair::Fingerprint>,
) -> io::Result<(
    Box<dyn AsyncRead + Send + Unpin>,
    Writer,
    Option<pair::Fingerprint>,
)>
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let (connector, pin) = me.connector(pinned)?;
    let name = tokio_rustls::rustls::pki_types::ServerName::IpAddress(peer.into());
    let tls = timeout(HANDSHAKE_TIMEOUT, connector.connect(name, stream))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS handshake timed out"))?
        .map_err(|e| {
            if e.to_string().contains(pair::PIN_MISMATCH) {
                io::Error::new(io::ErrorKind::PermissionDenied, pair::PIN_MISMATCH)
            } else {
                e
            }
        })?;
    let fp = *pin.seen.lock().unwrap();
    let (rd, wr) = tokio::io::split(tls);
    Ok((Box::new(rd), Box::new(wr), fp))
}

/// The hello (made the TLS one over `tls`) and the receiver's first answer.
async fn greet(
    rd: Box<dyn AsyncRead + Send + Unpin>,
    mut wr: Writer,
    addrs: (std::net::IpAddr, std::net::IpAddr),
    receiver_fp: Option<pair::Fingerprint>,
    hello: &Value,
    tls: bool,
    pinned: Option<pair::Fingerprint>,
) -> io::Result<Opened> {
    let mut rd = BufReader::new(rd);
    let mut hello = hello.clone();
    if tls {
        hello["v"] = json!(p::VERSION_TLS);
        if pinned.is_none() {
            hello["pair"] = json!(true);
        }
    }
    send(&mut wr, &hello).await?;
    let first = timeout(HANDSHAKE_TIMEOUT, read_msg(&mut rd))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no welcome"))??
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "closed before welcome"))?;
    Ok(Opened {
        first,
        rd,
        wr,
        addrs,
        receiver_fp,
    })
}

/// A channel with its welcome becomes a receiver luchd is connected to.
#[allow(clippy::too_many_arguments)]
async fn install(
    st: &Shared,
    conn: &Connection,
    r: Receiver,
    id: String,
    welcome: Value,
    rd: Reader,
    wr: Writer,
    addrs: (std::net::IpAddr, std::net::IpAddr),
    secure: bool,
) {
    let (tx, rx) = mpsc::channel(8);
    // Room for a receiver's whole log (problem.rs: some 30 slices) besides the session's own messages.
    let (events, _) = broadcast::channel(64);
    {
        let mut st = st.lock().await;
        st.conns.insert(
            id.clone(),
            Conn {
                cmd: tx,
                events: events.clone(),
                media: None,
                addrs: Some(addrs),
            },
        );
        st.welcomes.insert(id.clone(), welcome);
        if secure {
            st.secure.insert(id.clone());
        } else {
            st.secure.remove(&id);
        }
    }
    // Remembered with its MAC (the handshake has just filled the ARP table), to wake it later; not on the TV's own
    // network of a direct connection, where its address and MAC wake nothing later.
    if !direct::carries(&r.address) {
        wake::remember(&id, &r.name, &r.model, &r.address);
    }
    eprintln!(
        "[luchd] connected to {} ({}:{} via {}{})",
        r.name,
        r.address,
        r.port,
        r.ifname,
        if secure { ", TLS" } else { ", not encrypted" }
    );
    changed(conn).await;
    tokio::spawn(run_channel(
        r.name,
        id,
        rd,
        wr,
        rx,
        events,
        st.clone(),
        conn.clone(),
    ));
}

/// What Pairing says about a receiver's `pair` message.
fn pair_state(msg: &Value, name: &str) -> Value {
    json!({"step": msg["step"], "digits": msg["digits"].as_u64().unwrap_or(pair::DIGITS as u64), "name": name})
}

/// The error Connect (and everything that connects) gives while a receiver waits for its code.
fn pairing_needed(id: &str, name: &str, state: &Value) -> fdo::Error {
    let name = state["name"].as_str().unwrap_or(name);
    fdo::Error::AccessDenied(if state["step"] == "allow" {
        format!("{name} asks for pairing: allow it on the TV, then enter the code it shows (`luchctl pair {id}`)")
    } else {
        format!("{name} asks for pairing: enter the code it shows (`luchctl pair {id}`)")
    })
}

async fn pair_changed(conn: &Connection, id: &str, state: &Value) {
    if let Ok(e) = SignalEmitter::new(conn, p::DBUS_PATH) {
        let _ = Luch::pair_changed(&e, id, &state.to_string()).await;
    }
}

/// A receiver asking for pairing: waits for its allow (when it asks that first) and for the code typed here (Pair),
/// runs the code check, pins its certificate and goes on as a connected receiver. Gives up after two minutes.
#[allow(clippy::too_many_arguments)]
async fn pairing(
    st: Shared,
    conn: Connection,
    r: Receiver,
    id: String,
    o: Opened,
    me: pair::Fingerprint,
    theirs: pair::Fingerprint,
    dir: std::path::PathBuf,
    mut codes: mpsc::Receiver<CodeTyped>,
) {
    let Opened {
        first,
        mut rd,
        mut wr,
        addrs,
        ..
    } = o;
    let name = r.name.clone();
    let result = pair_on(
        &st, &conn, &id, &name, first, &mut rd, &mut wr, &me, &theirs, &mut codes,
    )
    .await;
    st.lock().await.codes.remove(&id);
    match result {
        Ok((welcome, reply)) => {
            if let Err(e) = pair::pin(&dir, &id, &theirs, &name) {
                eprintln!("[luchd] cannot keep the pairing with {name}: {e}");
            }
            eprintln!("[luchd] paired with {name}");
            let state = json!({"step": "paired", "name": name});
            st.lock().await.pairing.insert(id.clone(), state.clone());
            install(
                &st,
                &conn,
                r,
                id.clone(),
                welcome.clone(),
                rd,
                wr,
                addrs,
                true,
            )
            .await;
            pair_changed(&conn, &id, &state).await;
            if let Some(reply) = reply {
                let _ = reply.send(Ok(welcome.to_string()));
            }
        }
        Err((why, reply)) => {
            eprintln!("[luchd] pairing with {name}: {why}");
            let state = json!({"step": "failed", "name": name, "reason": why});
            st.lock().await.pairing.insert(id.clone(), state.clone());
            pair_changed(&conn, &id, &state).await;
            if let Some(reply) = reply {
                let _ = reply.send(Err(why));
            }
        }
    }
}

/// A receiver asking for pairing on an open channel (`first` is its `pair`): waits for its allow when it asks that
/// first and for the code typed here (Pair under `key`, the name Pairing and PairChanged use too), and runs the code
/// check. The welcome after it, or why not; and Pair's caller when a code was typed. Gives up after two minutes.
#[allow(clippy::too_many_arguments)]
async fn pair_on(
    st: &Shared,
    conn: &Connection,
    key: &str,
    name: &str,
    mut first: Value,
    rd: &mut Reader,
    wr: &mut Writer,
    me: &pair::Fingerprint,
    theirs: &pair::Fingerprint,
    codes: &mut mpsc::Receiver<CodeTyped>,
) -> Result<(Value, Option<PairReply>), (String, Option<PairReply>)> {
    let deadline = Instant::now() + Duration::from_secs(130);
    loop {
        if first["t"] == "pair_refused" {
            return Err((pair::explain(&pair::refused(&first), name), None));
        }
        if first["t"] != "pair" {
            return Err((format!("{name}: unexpected {first}"), None));
        }
        let state = pair_state(&first, name);
        st.lock()
            .await
            .pairing
            .insert(key.to_string(), state.clone());
        pair_changed(conn, key, &state).await;
        if first["step"] == "allow" {
            // The TV asks its person first; its next word is the code step or a refusal.
            first = match tokio::time::timeout_at(deadline, read_msg(&mut *rd)).await {
                Ok(Ok(Some(v))) => v,
                Ok(_) => return Err((format!("{name} closed the connection"), None)),
                Err(_) => return Err((format!("nobody answered on {name} in time"), None)),
            };
            continue;
        }
        // The code is on the TV: wait for it from here, or for the TV to give up (fill_buf is cancel-safe).
        let (code, reply) = tokio::select! {
            c = codes.recv() => match c {
                Some(c) => c,
                None => return Err(("luchd is stopping".into(), None)),
            },
            _ = rd.fill_buf() => {
                let v = read_msg(&mut *rd).await.ok().flatten().unwrap_or(Value::Null);
                return Err((pair::explain(&pair::refused(&v), name), None));
            }
            _ = tokio::time::sleep_until(deadline) => {
                let _ = send(&mut *wr, &json!({"t": "bye"})).await;
                return Err((format!("no code was entered for {name} in time"), None));
            }
        };
        return match pair::code_check(&mut *rd, &mut *wr, &code, me, theirs).await {
            Ok(()) => match timeout(HANDSHAKE_TIMEOUT, read_msg(&mut *rd)).await {
                Ok(Ok(Some(w))) if w["t"] == "welcome" => Ok((w, Some(reply))),
                _ => Err((format!("{name} paired but sent no welcome"), Some(reply))),
            },
            Err(f) => Err((pair::explain(&f, name), Some(reply))),
        };
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_channel(
    name: String,
    id: String,
    mut rd: Reader,
    mut wr: Writer,
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
                    "ready" | "keyframe" | "stats" | "photo_ready" | "sleeping" | "app_opened" | "blank"
                    | "input_request" | "logs" => {
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
        s.secure.remove(&id);
    }
    changed(&conn).await;
}

/// The presenter views (present.rs); started when first needed.
static PRESENTER: tokio::sync::OnceCell<present::Server> = tokio::sync::OnceCell::const_new();

/// Counts a presentation or a mirroring session in for do-not-disturb, and asks the person when it is time to.
async fn quiet_session(conn: &Connection) -> quiet::Session {
    let (session, ask) = tokio::task::spawn_blocking(quiet::Session::start)
        .await
        .unwrap_or_else(|_| quiet::Session::start());
    if ask {
        if let Ok(e) = SignalEmitter::new(conn, p::DBUS_PATH) {
            let _ = Luch::quiet_ask(&e).await;
        }
    }
    session
}

/// Serves cast files, relayed links and covers to receivers; started when first needed.
static FILES: tokio::sync::OnceCell<cast::Server> = tokio::sync::OnceCell::const_new();

async fn files() -> io::Result<&'static cast::Server> {
    FILES.get_or_try_init(cast::Server::start).await
}

/// The cover's token per (receiver, cover): the same link while the cover stays.
static ART_LINKS: std::sync::LazyLock<std::sync::Mutex<HashMap<(String, String), String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

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
                    // One link per cover and TV while it stays: a new token at every update (position, pause) was a
                    // new address, which the receiver loaded again, and the cover blinked.
                    let key = (tv.to_string(), a.clone());
                    let known = ART_LINKS
                        .lock()
                        .ok()
                        .and_then(|m| m.get(&key).cloned())
                        .filter(|t| srv.has(t));
                    let t = match known {
                        Some(t) => t,
                        None => {
                            let t = srv.share(item, tv, "image/jpeg");
                            if let Ok(mut m) = ART_LINKS.lock() {
                                m.retain(|(who, _), _| who != &key.0);
                                m.insert(key, t.clone());
                            }
                            t
                        }
                    };
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

/// Body of the D-Bus `Mirror` method and of the web receiver's auto-start: one session to `id`.
/// A group to mirror to (PROTOCOL.md 4k): the other receivers and what each presents, and the mode.
struct GroupRequest {
    /// The target's own role, then (id, role) of every other member.
    role: String,
    members: Vec<(String, String)>,
    mode: sync::Mode,
}

async fn start_mirror(
    st: &Shared,
    conn: &Connection,
    id: &str,
    source: &str,
    requested: (u32, u32),
    group: Option<GroupRequest>,
) -> fdo::Result<()> {
    let source_name = source;
    let mirror169 = source == "fast169";
    let audio_only = source == "audio";
    let kms = source == "fast" || mirror169;
    let niri_virtual = source == "virtual" && mirror::niri_virtual_available();
    let source = match source {
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
    if st.lock().await.mirrors.contains_key(id) {
        return Err(fdo::Error::Failed(format!("already mirroring to {id}")));
    }
    let (c, r, welcome) = {
        let s = st.lock().await;
        (
            s.conns.get(id).cloned(),
            s.found.get(id).cloned(),
            s.welcomes.get(id).cloned().unwrap_or(Value::Null),
        )
    };
    let (c, r) = c
        .zip(r)
        .ok_or_else(|| fdo::Error::Failed("receiver went away".into()))?;
    let chosen = match st.lock().await.codec.get(id).cloned() {
        Some(c) => c,
        None => saved_codecs().get(id).cloned().unwrap_or_default(),
    };
    let prefer = p::session::choose_codec(&welcome, &chosen).to_string();
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
    } else if source == ashpd::desktop::screencast::SourceType::VIRTUAL {
        display::native_size(&welcome).unwrap_or_default()
    } else {
        display::Size::default()
    };
    let ws = c.media.clone();
    // Sound only always carries sound, whatever the person chose for mirroring.
    let chosen_audio = if audio_only {
        "on".to_string()
    } else {
        audio_choice(st, id).await
    };
    let audio = limits::start_audio(&chosen_audio, &welcome, mirror::AUDIO_FRAME_MS);
    if audio_only
        && !welcome["modes"]
            .as_array()
            .is_some_and(|m| m.iter().any(|x| x == "audio"))
    {
        return Err(fdo::Error::NotSupported(
            "this receiver version cannot play sound only yet: update it".into(),
        ));
    }
    let address = r
        .address
        .parse()
        .map_err(|_| fdo::Error::Failed(format!("not IPv4: {}", r.address)))?;
    let (group, member_ids) = match group {
        Some(g) => {
            let ids = g.members.iter().map(|(i, _)| i.clone()).collect::<Vec<_>>();
            (Some(build_group(st, id, &r.name, &welcome, g).await?), ids)
        }
        None => (None, Vec::new()),
    };
    // A group's picture is one stream: H.264 when a member that shows it cannot decode HEVC.
    let prefer = match &group {
        Some(g) if g.members.iter().any(|m| m.who.video() && !m.can_hevc) => "video/avc".into(),
        _ => prefer,
    };
    let (left_tx, mut left_rx) = mpsc::unbounded_channel::<String>();
    let gid = group.as_ref().map(|g| g.id.clone());
    let target = mirror::Target {
        name: r.name.clone(),
        address,
        ifname: r.ifname.clone(),
        source,
        kms,
        prefer,
        ws,
        size,
        niri_virtual,
        mirror169,
        audio,
        audio_only,
        input: (input::mode() != "off").then(|| input::Config {
            dbus: conn.clone(),
            receiver_can: welcome["input"].as_array().is_some_and(|a| !a.is_empty()),
            always: input::ask::remembered(
                saved_choices(input::ask::CHOICES_FILE)
                    .get(id)
                    .map(String::as_str),
            )
            .is_some(),
        }),
        id: id.to_string(),
        group,
        left: left_tx,
    };
    let (stop_tx, stop_rx) = mpsc::channel(4);
    let stats = mirror::Stats::default();
    {
        let mut s = st.lock().await;
        // Every member of a group is busy with it, with the group's numbers: StopMirror on any of them ends the group.
        for m in member_ids.iter().map(String::as_str).chain([id]) {
            s.mirrors.insert(m.to_string(), stop_tx.clone());
            s.stats.insert(m.to_string(), stats.clone());
            s.sources.insert(m.to_string(), source_name.to_string());
            if let Some(gid) = &gid {
                s.groups.insert(m.to_string(), gid.clone());
            }
        }
        if s.lab.as_ref().is_some_and(|l| l.id == id) {
            let _ = c
                .cmd
                .try_send(Cmd::Send(json!({"t": "latency", "on": true})));
        }
    }
    changed(conn).await;
    {
        // A member that left the group playing on is free again.
        let (st, conn, session) = (st.clone(), conn.clone(), stop_tx.clone());
        tokio::spawn(async move {
            while let Some(id) = left_rx.recv().await {
                forget_member(&st, &id, &session).await;
                changed(&conn).await;
            }
        });
    }
    let (st, conn, id) = (st.clone(), conn.clone(), id.to_string());
    // Subscribe here and drop our copy of the sender: the mirror must see the channel close
    // (receiver gone or restarted) instead of streaming into the void.
    let Conn { cmd, events, .. } = c;
    let events_rx = events.subscribe();
    drop(events);
    let name = target.name.clone();
    {
        let (st, conn, id) = (st.clone(), conn.clone(), id.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await; // once the receiver is ready
            push_now_playing(&st, &conn, Some(&id)).await;
        });
    }
    tokio::spawn(async move {
        // The screen is on the TV: notifications are not for everyone in the room (sound only is not a screen).
        let _quiet = match audio_only {
            false => Some(quiet_session(&conn).await),
            true => None,
        };
        let res = mirror::run(target, cmd, events_rx, stop_rx, stats.clone()).await;
        match res {
            Ok(why) => {
                eprintln!("[luchd] {name}: mirroring ended ({why})");
                if why.starts_with("the receiver could not show") {
                    reports::notify(
                        &conn,
                        &format!("{name} did not show the picture"),
                        why,
                        None,
                    )
                    .await;
                }
            }
            Err(e) => eprintln!("[luchd] {name}: mirroring failed: {e}"),
        }
        {
            let mut s = st.lock().await;
            let last = stats.lock().unwrap().clone();
            s.last_stats.insert(id.clone(), last);
            // Everyone still in this session (a group's members come and go, and the first may have left it).
            let ours: Vec<String> = s
                .mirrors
                .iter()
                .filter(|(_, c)| c.same_channel(&stop_tx))
                .map(|(m, _)| m.clone())
                .collect();
            for m in ours {
                s.mirrors.remove(&m);
                s.stats.remove(&m);
                s.sources.remove(&m);
                s.groups.remove(&m);
            }
        }
        changed(&conn).await;
    });
    Ok(())
}

/// The members of a group, each connected, able to sync (its welcome says so) and not busy; with their saved
/// corrections.
async fn build_group(
    st: &Shared,
    id: &str,
    name: &str,
    welcome: &Value,
    g: GroupRequest,
) -> fdo::Result<mirror::Group> {
    let role_ok = |r: &str| matches!(r, "video" | "audio" | "both");
    if !role_ok(&g.role) || g.role == "audio" {
        return Err(fdo::Error::InvalidArgs(
            "the first receiver shows the picture: role video or both".into(),
        ));
    }
    if !sync::capable(welcome) {
        return Err(fdo::Error::NotSupported(format!(
            "{name} cannot play in step yet: update it"
        )));
    }
    let mut members = Vec::new();
    for (mid, role) in g.members {
        if !role_ok(&role) || mid == id {
            return Err(fdo::Error::InvalidArgs(format!(
                "bad member {mid} ({role})"
            )));
        }
        let s = st.lock().await;
        let m = group_member(&s, &mid, role)?;
        if s.mirrors.contains_key(&mid) {
            return Err(fdo::Error::Failed(format!(
                "{} is playing already: stop it first",
                m.who.name
            )));
        }
        members.push(m);
    }
    Ok(mirror::Group {
        id: uuid::Uuid::new_v4().to_string(),
        timeline: u32::from_be_bytes(uuid::Uuid::new_v4().as_bytes()[..4].try_into().unwrap()),
        mode: g.mode,
        tolerance_ms: 20.0,
        floor_ms: 150.0,
        primary: mirror::MemberId {
            id: id.to_string(),
            name: name.to_string(),
            role: g.role,
            offset_ms: offset(id),
        },
        members,
    })
}

/// A receiver's correction as it plays now: for the output route it last reported.
fn offset(id: &str) -> f64 {
    saved_offset(&sync::offset_key(id, &last_route(id))).unwrap_or(0.0)
}

/// A connected receiver as a member of a group: able to play in step (its welcome says so), with its saved
/// correction and its own control channel.
fn group_member(s: &State, mid: &str, role: String) -> fdo::Result<mirror::Member> {
    let (c, r, w) = (s.conns.get(mid), s.found.get(mid), s.welcomes.get(mid));
    let (Some(c), Some(r)) = (c, r) else {
        return Err(fdo::Error::Failed(format!(
            "{} is not connected",
            receiver_name(s, mid)
        )));
    };
    let w = w.unwrap_or(&Value::Null);
    if !sync::capable(w) {
        return Err(fdo::Error::NotSupported(format!(
            "{} cannot play in step yet: update it",
            r.name
        )));
    }
    Ok(mirror::Member {
        who: mirror::MemberId {
            id: mid.to_string(),
            name: r.name.clone(),
            role,
            offset_ms: offset(mid),
        },
        address: r
            .address
            .parse()
            .map_err(|_| fdo::Error::Failed(format!("not IPv4: {}", r.address)))?,
        ifname: r.ifname.clone(),
        audio: limits::start_audio("on", w, mirror::AUDIO_FRAME_MS),
        cmd: c.cmd.clone(),
        events: c.events.subscribe(),
        can_hevc: w["decoders"]
            .as_array()
            .is_some_and(|d| d.iter().any(|m| m == "video/hevc")),
    })
}

/// A receiver's correction for one output route (sync::offset_key), saved across restarts.
pub(crate) fn saved_offset(key: &str) -> Option<f64> {
    saved_choices("sync-offsets")
        .get(key)
        .and_then(|v| v.parse().ok())
}

fn save_offset(key: &str, ms: f64) {
    save_choice("sync-offsets", key, &format!("{ms}"));
}

/// Keeps the output route a receiver reported, for the correction of its next group.
pub(crate) fn remember_route(id: &str, route: &str) {
    if last_route(id) != route {
        save_choice("sync-routes", id, route);
    }
}

/// The output route a receiver last reported ("unknown" before it ever did).
fn last_route(id: &str) -> String {
    saved_choices("sync-routes")
        .get(id)
        .cloned()
        .unwrap_or_else(|| "unknown".into())
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
    let Ok(config) = config_dir() else {
        return eprintln!("[luchd] web receiver off: no config directory");
    };
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
        let (st, dbus, sid, host, dir) = (
            st.clone(),
            dbus.clone(),
            sender_id.clone(),
            host.clone(),
            config.clone(),
        );
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
                web_session(stream, peer, st, dbus, sid, host, dir).await
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
    dir: std::path::PathBuf,
) -> io::Result<()> {
    let head = timeout(HANDSHAKE_TIMEOUT, read_http_head(&mut stream))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "HTTP header timeout"))??;
    let text = String::from_utf8_lossy(&head).into_owned();
    let first = text.split("\r\n").next().unwrap_or_default().to_string();
    let path = first.split(' ').nth(1).unwrap_or_default().to_string();
    // A browser allowed before carries its cookie; any other is asked about on this computer first.
    let allowed = web_auth::cookie(&text).is_some_and(|t| web_auth::known(&dir, t));
    if first.starts_with("POST /auth ") {
        return set_cookie(stream, &text, &dir).await;
    }
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
        return run_web(ws, peer, st, dbus, sender_id, host, allowed, &dir).await;
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

/// `POST /auth` with the token the page was given over its websocket: answered with that token as an HttpOnly
/// cookie, which the page's scripts cannot read and the browser sends with every later visit.
async fn set_cookie<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    head: &str,
    dir: &std::path::Path,
) -> io::Result<()> {
    let len = head
        .split("\r\n")
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    let mut body = vec![0u8; len.min(256)];
    timeout(HANDSHAKE_TIMEOUT, stream.read_exact(&mut body))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "auth body timeout"))??;
    let token = String::from_utf8_lossy(&body).trim().to_string();
    let resp = if web_auth::known(dir, &token) {
        format!("HTTP/1.1 204 No Content\r\nSet-Cookie: {}={token}; Max-Age=31536000; Path=/; Secure; HttpOnly; SameSite=Strict\r\nConnection: close\r\n\r\n", web_auth::COOKIE)
    } else {
        "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
    };
    stream.write_all(resp.as_bytes()).await?;
    stream.shutdown().await
}

/// Before a browser that is not allowed yet becomes a receiver: the code on its page and in a question here. Ok
/// with the browser let in (and given its token), Err with what its page says otherwise.
async fn approve_browser<W>(
    sink: &mut W,
    st: &Shared,
    dbus: &Connection,
    dir: &std::path::Path,
    name: &str,
    address: &str,
) -> Result<(), String>
where
    W: futures_util::Sink<Message> + Unpin,
{
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let verdict = st.lock().await.web_limits.check(address, now);
    match verdict {
        web_auth::Verdict::Banned(s) => {
            return Err(format!(
                "refused on the computer; try again in {} min",
                s.div_ceil(60)
            ))
        }
        web_auth::Verdict::Busy => {
            return Err("another browser is waiting to be allowed; try again in a minute".into())
        }
        web_auth::Verdict::Ask => {}
    }
    let code = web_auth::code();
    let request = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = oneshot::channel();
    st.lock().await.web_waiting.insert(
        request.clone(),
        (
            json!({"id": request, "name": name, "address": address, "code": code, "waiting": true}),
            tx,
        ),
    );
    let _ = sink
        .send(Message::Text(
            json!({"t": "confirm", "code": code}).to_string().into(),
        ))
        .await;
    if let Ok(e) = SignalEmitter::new(dbus, p::DBUS_PATH) {
        let _ = Luch::web_approval(&e, &request, name, address, &code).await;
    }
    eprintln!("[luchd] web receiver: asking about {name} ({address}), code {code}");
    let answer = web_auth::ask(dbus, name, address, &code, rx).await;
    {
        let mut s = st.lock().await;
        s.web_waiting.remove(&request);
        s.web_limits.done();
        if answer == Some(false) {
            s.web_limits.strike(address, now);
        }
    }
    match answer {
        Some(true) => {
            let token = web_auth::allow(dir, name, address).map_err(|e| e.to_string())?;
            let _ = sink
                .send(Message::Text(
                    json!({"t": "authorized", "token": token})
                        .to_string()
                        .into(),
                ))
                .await;
            eprintln!("[luchd] web receiver: {name} ({address}) allowed");
            Ok(())
        }
        Some(false) => Err("refused on the computer".into()),
        None => Err("nobody answered on the computer".into()),
    }
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
#[allow(clippy::too_many_arguments)]
async fn run_web<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    ws: WebSocketStream<S>,
    peer: SocketAddr,
    st: Shared,
    dbus: Connection,
    sender_id: String,
    host: String,
    // A browser allowed before (its cookie); else it is asked about here first, and remembered in `dir`.
    allowed: bool,
    dir: &std::path::Path,
) -> io::Result<()> {
    let (mut sink, mut stream) = ws.split();
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
    if !allowed {
        let who = hello["model"].as_str().unwrap_or("a browser").to_string();
        if let Err(why) =
            approve_browser(&mut sink, &st, &dbus, dir, &who, &peer.ip().to_string()).await
        {
            eprintln!("[luchd] web {peer}: not let in ({why})");
            let _ = sink
                .send(Message::Text(
                    json!({"t": "denied", "reason": why}).to_string().into(),
                ))
                .await;
            let _ = sink.close().await;
            return Ok(());
        }
    }
    let id = format!("web-{}", uuid::Uuid::new_v4());
    let name = hello["name"].as_str().unwrap_or("Web").to_string();
    let mode = hello["mode"].as_str().unwrap_or("fast").to_string();
    let (cmd_tx, cmd_rx) = mpsc::channel(8);
    let (events, _) = broadcast::channel(16);
    let (media_tx, mut video_rx, mut audio_rx) = mirror::WebMedia::new();
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
                tls_port: 0,
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
                media: Some(media_tx),
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
        let (mut cmd_rx, mut sink) = (cmd_rx, sink);
        let why = loop {
            tokio::select! {
                biased;
                c = cmd_rx.recv() => match c {
                    Some(Cmd::Send(v)) => { if sink.send(Message::Text(v.to_string().into())).await.is_err() { break "write failed"; } }
                    Some(Cmd::Bye) => { let _ = sink.send(Message::Text(json!({"t": "bye"}).to_string().into())).await; break "disconnect requested"; }
                    None => break "control closed",
                },
                m = audio_rx.recv() => match m {
                    Some(b) => { if sink.send(Message::Binary(b.into())).await.is_err() { break "write failed"; } }
                    None => break "media closed",
                },
                m = video_rx.recv() => match m {
                    Some(b) => { if sink.send(Message::Binary(b.into())).await.is_err() { break "write failed"; } }
                    None => break "media closed",
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
    if mode != "none" {
        if let Err(e) = start_mirror(&st, &dbus, &id, &mode, (0, 0), None).await {
            let _ = cmd_tx
                .send(Cmd::Send(json!({"t": "error", "msg": e.to_string()})))
                .await;
        }
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
        tls_port: t.get("tls").and_then(|p| p.parse().ok()).unwrap_or(0),
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
    // Back to this computer's own Wi-Fi if luchd stopped while on a TV's network.
    tokio::spawn(direct::go_back_after_restart());
    // Notification banners back on if a crash left them off (quiet.rs).
    quiet::with(|_| ());
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
    let ident = match config_dir()
        .map_err(Box::<dyn Error>::from)
        .and_then(|d| pair::Identity::load(&d))
    {
        Ok(i) => Some(Arc::new(i)),
        Err(e) => {
            eprintln!("[luchd] no sender certificate ({e}): older, unencrypted receivers only");
            None
        }
    };
    let (discovery, worker) = discovery::Discovery::new();
    let finder = find::Finder::new(find::Ctx {
        st: st.clone(),
        sender_id: id.clone(),
        host: host.clone(),
        discovery: discovery.clone(),
    });
    let tvs = tv::Tvs::new();
    let conn = zbus::connection::Builder::session()?
        .name(p::DBUS_NAME)?
        .serve_at(
            p::DBUS_PATH,
            Luch {
                st: st.clone(),
                sender_id: id.clone(),
                host: host.clone(),
                discovery,
                ident,
                finder: finder.clone(),
                tvs: tvs.clone(),
                http: reqwest::Client::new(),
            },
        )?
        .build()
        .await?;
    {
        // The first climb right away: a network without mDNS shows its receivers without anyone asking.
        let conn = conn.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let _ = finder.start(&conn, "auto").await;
        });
    }
    let on_change = {
        let conn = conn.clone();
        move || {
            let conn = conn.clone();
            tokio::spawn(async move { changed(&conn).await });
        }
    };
    tokio::spawn(tv::search_renderers(
        tvs.clone(),
        tv::ssdp::Target::default(),
        on_change.clone(),
    ));
    {
        let (sys, tvs) = (sys.clone(), tvs.clone());
        tokio::spawn(async move {
            if let Err(e) = tv::mdns::browse(sys, tvs, on_change).await {
                eprintln!("[luchd] cannot browse {}: {e}", tv::mdns::CAST_TYPE);
            }
        });
    }
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

    /// Tests that point XDG_CONFIG_HOME somewhere of their own hold this: the variable is one for the process.
    pub(crate) static CONFIG_HOME: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A receiver that has reports gets them taken over the control channel: answered with `reports_taken`, kept on
    /// disk. Needs a session bus (the channel signals changes on it); skipped without one.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // held on purpose for the whole test; nothing else in it takes the lock
    async fn reports_from_a_receiver_are_taken_and_kept() {
        let Ok(conn) = Connection::session().await else {
            eprintln!("no session bus: skipped");
            return;
        };
        let _env = CONFIG_HOME.lock().unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!("luch-chan-{}", uuid::Uuid::new_v4()));
        std::env::set_var("XDG_CONFIG_HOME", &home);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (client, (server, _)) = tokio::join!(tokio::net::TcpStream::connect(addr), async {
            listener.accept().await.unwrap()
        });
        let (rd, wr) = client.unwrap().into_split();
        let rd: Reader = BufReader::new(Box::new(rd));
        let wr: Writer = Box::new(wr);
        let (_cmd_tx, cmd_rx) = mpsc::channel(8);
        let (events, _) = broadcast::channel(16);
        let st = Shared::default();
        tokio::spawn(run_channel(
            "TV".into(),
            "rx".into(),
            rd,
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

    /// A TV made of Rust for the pairing tests: its own certificate, any client certificate let through and
    /// remembered, and a script for what it says after the hello.
    pub(crate) mod tv {
        use super::*;
        use tokio_rustls::rustls::{
            self,
            client::danger::HandshakeSignatureValid,
            pki_types::{CertificateDer, PrivateKeyDer, UnixTime},
            server::danger::{ClientCertVerified, ClientCertVerifier},
            DigitallySignedStruct, DistinguishedName, SignatureScheme,
        };

        #[derive(Debug)]
        pub struct AnyClient(std::sync::Mutex<Option<pair::Fingerprint>>);

        fn algs() -> rustls::crypto::WebPkiSupportedAlgorithms {
            rustls::crypto::ring::default_provider().signature_verification_algorithms
        }

        impl ClientCertVerifier for AnyClient {
            fn root_hint_subjects(&self) -> &[DistinguishedName] {
                &[]
            }
            fn verify_client_cert(
                &self,
                end_entity: &CertificateDer<'_>,
                _: &[CertificateDer<'_>],
                _: UnixTime,
            ) -> Result<ClientCertVerified, rustls::Error> {
                *self.0.lock().unwrap() = Some(pair::fingerprint(end_entity));
                Ok(ClientCertVerified::assertion())
            }
            fn verify_tls12_signature(
                &self,
                m: &[u8],
                c: &CertificateDer<'_>,
                d: &DigitallySignedStruct,
            ) -> Result<HandshakeSignatureValid, rustls::Error> {
                rustls::crypto::verify_tls12_signature(m, c, d, &algs())
            }
            fn verify_tls13_signature(
                &self,
                m: &[u8],
                c: &CertificateDer<'_>,
                d: &DigitallySignedStruct,
            ) -> Result<HandshakeSignatureValid, rustls::Error> {
                rustls::crypto::verify_tls13_signature(m, c, d, &algs())
            }
            fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
                algs().supported_schemes()
            }
        }

        pub struct Tv {
            pub fp: pair::Fingerprint,
            config: Arc<rustls::ServerConfig>,
            client: Arc<AnyClient>,
        }

        impl Tv {
            pub fn new() -> Tv {
                let key = rcgen::KeyPair::generate().unwrap();
                let cert = rcgen::CertificateParams::new(Vec::<String>::new())
                    .unwrap()
                    .self_signed(&key)
                    .unwrap();
                let client = Arc::new(AnyClient(std::sync::Mutex::new(None)));
                let config = rustls::ServerConfig::builder_with_provider(
                    rustls::crypto::ring::default_provider().into(),
                )
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_client_cert_verifier(client.clone())
                .with_single_cert(
                    vec![cert.der().clone()],
                    PrivateKeyDer::try_from(key.serialize_der()).unwrap(),
                )
                .unwrap();
                Tv {
                    fp: pair::fingerprint(cert.der()),
                    config: Arc::new(config),
                    client,
                }
            }

            /// Accepts one sender; after its hello runs `script` with the hello, the two ends and the sender's
            /// fingerprint.
            pub async fn serve<F, Fut>(&self, script: F) -> std::net::SocketAddr
            where
                F: FnOnce(Value, Reader, Writer, pair::Fingerprint) -> Fut + Send + 'static,
                Fut: std::future::Future<Output = ()> + Send,
            {
                let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = l.local_addr().unwrap();
                let acceptor = tokio_rustls::TlsAcceptor::from(self.config.clone());
                let client = self.client.clone();
                tokio::spawn(async move {
                    let (s, _) = l.accept().await.unwrap();
                    let Ok(tls) = acceptor.accept(s).await else {
                        return; // the sender refused our certificate
                    };
                    let (rd, wr) = tokio::io::split(tls);
                    let mut rd: Reader = BufReader::new(Box::new(rd));
                    let wr: Writer = Box::new(wr);
                    let hello = read_msg(&mut rd).await.unwrap().unwrap();
                    let fp = client.0.lock().unwrap().unwrap();
                    script(hello, rd, wr, fp).await;
                });
                addr
            }
        }
    }

    fn temp_dir(what: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("luch-{what}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    async fn dial(
        addr: std::net::SocketAddr,
        me: &pair::Identity,
        pinned: Option<pair::Fingerprint>,
    ) -> io::Result<Opened> {
        let s = tokio::net::TcpStream::connect(addr).await.unwrap();
        open_channel(
            s,
            &hello("sender-id", "Linux Laptop", None, false),
            Some(me),
            pinned,
        )
        .await
    }

    fn test_receiver(addr: std::net::SocketAddr) -> Receiver {
        Receiver {
            id: "tv".into(),
            name: "Living room TV".into(),
            model: String::new(),
            address: addr.ip().to_string(),
            ifname: "lo".into(),
            ifindex: 1,
            port: addr.port(),
            tls_port: addr.port(),
            service: String::new(),
        }
    }

    /// The whole of it against a TV that asks for the code: luchd waits for the code (as Pair hands it over), both
    /// sides check it, luchd pins the TV and is connected; next time TLS alone lets it in; and a TV with another
    /// certificate under the same id is refused in the handshake, before a single line is sent.
    #[tokio::test]
    async fn pairing_pins_the_receiver_and_another_certificate_is_refused() {
        let Ok(conn) = Connection::session().await else {
            eprintln!("no session bus: skipped");
            return;
        };
        let dir = temp_dir("pairing");
        let me = pair::Identity::load(&dir).unwrap();
        let tv = tv::Tv::new();
        let tv_fp = tv.fp;
        let addr = tv
            .serve(move |hello, mut rd, mut wr, sender| async move {
                assert_eq!(hello["v"], 1);
                assert_eq!(hello["pair"], true, "nothing pinned yet: it asks to pair");
                send(&mut wr, &json!({"t": "pair", "step": "code", "digits": 6}))
                    .await
                    .unwrap();
                assert!(
                    pair::tests::receiver_side(&mut rd, &mut wr, b"123456", &sender, &tv_fp).await
                );
                send(&mut wr, &json!({"t": "welcome", "v": 1}))
                    .await
                    .unwrap();
                while let Ok(Some(_)) = read_msg(&mut rd).await {}
            })
            .await;
        let o = dial(addr, &me, None).await.unwrap();
        assert_eq!(o.first["t"], "pair");
        assert_eq!(o.receiver_fp, Some(tv.fp));

        let st = Shared::default();
        let (tx, rx) = mpsc::channel(1);
        st.lock().await.codes.insert("tv".into(), tx.clone());
        tokio::spawn(pairing(
            st.clone(),
            conn.clone(),
            test_receiver(addr),
            "tv".into(),
            o,
            me.fp,
            tv.fp,
            dir.clone(),
            rx,
        ));
        let (reply, answer) = oneshot::channel();
        tx.send((pair::clean_code("123 456").unwrap(), reply))
            .await
            .unwrap();
        let welcome = timeout(Duration::from_secs(5), answer)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(welcome.contains("welcome"));
        assert_eq!(pair::pinned_fp(&dir, "tv"), Some(tv.fp));
        {
            let s = st.lock().await;
            assert!(s.conns.contains_key("tv") && s.secure.contains("tv"));
            assert_eq!(s.pairing["tv"]["step"], "paired");
            assert!(!s.codes.contains_key("tv"));
        }

        // Paired: the same TV takes us at once, and the hello no longer asks to pair.
        let again = tv
            .serve(|hello, _, mut wr, _| async move {
                assert!(hello.get("pair").is_none());
                send(&mut wr, &json!({"t": "welcome", "v": 1}))
                    .await
                    .unwrap();
            })
            .await;
        assert_eq!(
            dial(again, &me, Some(tv.fp)).await.unwrap().first["t"],
            "welcome"
        );

        // Another TV (or one reset) under the paired id: refused by its certificate.
        let other = tv::Tv::new();
        let impostor =
            other
                .serve(|_, _, _, _| async move {
                    panic!("nothing may reach a TV with another certificate")
                })
                .await;
        let e = dial(impostor, &me, Some(tv.fp))
            .await
            .err()
            .expect("refused");
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A token from the QR code or the sound goes in the TLS hello too (PROTOCOL.md 1e): the receiver uses it up
    /// there, but the hello still asks to pair, since the token does not replace the code.
    #[tokio::test]
    async fn a_tls_hello_carries_the_token_and_still_pairs() {
        let dir = temp_dir("token");
        let me = pair::Identity::load(&dir).unwrap();
        let tv = tv::Tv::new();
        let addr = tv
            .serve(|hello, _, mut wr, _| async move {
                assert_eq!(hello["v"], 1);
                assert_eq!(hello["token"], "0badf00d");
                assert_eq!(hello["pair"], true);
                assert!(hello.get("probe").is_none());
                send(&mut wr, &json!({"t": "pair", "step": "code", "digits": 6}))
                    .await
                    .unwrap();
            })
            .await;
        let s = tokio::net::TcpStream::connect(addr).await.unwrap();
        let h = hello("sender-id", "Linux Laptop", Some("0badf00d"), false);
        let o = open_channel(s, &h, Some(&me), None).await.unwrap();
        assert_eq!(o.first["t"], "pair");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A mistyped code: the TV refuses, nothing is pinned, and Pair hears why in words.
    #[tokio::test]
    async fn a_wrong_code_pins_nothing() {
        let Ok(conn) = Connection::session().await else {
            eprintln!("no session bus: skipped");
            return;
        };
        let dir = temp_dir("wrong-code");
        let me = pair::Identity::load(&dir).unwrap();
        let tv = tv::Tv::new();
        let tv_fp = tv.fp;
        let addr = tv
            .serve(move |_, mut rd, mut wr, sender| async move {
                send(&mut wr, &json!({"t": "pair", "step": "allow"}))
                    .await
                    .unwrap();
                send(&mut wr, &json!({"t": "pair", "step": "code", "digits": 6}))
                    .await
                    .unwrap();
                assert!(
                    !pair::tests::receiver_side(&mut rd, &mut wr, b"123456", &sender, &tv_fp).await
                );
            })
            .await;
        let o = dial(addr, &me, None).await.unwrap();
        assert_eq!(o.first["step"], "allow");
        let st = Shared::default();
        let (tx, rx) = mpsc::channel(1);
        st.lock().await.codes.insert("tv".into(), tx.clone());
        tokio::spawn(pairing(
            st.clone(),
            conn,
            test_receiver(addr),
            "tv".into(),
            o,
            me.fp,
            tv.fp,
            dir.clone(),
            rx,
        ));
        // The code is asked for only once the TV has moved on from "allow".
        timeout(Duration::from_secs(5), async {
            while st
                .lock()
                .await
                .pairing
                .get("tv")
                .map(|v| v["step"] == "code")
                != Some(true)
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let (reply, answer) = oneshot::channel();
        tx.send((b"123457".to_vec(), reply)).await.unwrap();
        let why = timeout(Duration::from_secs(5), answer)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(why, "wrong code: try again, Living room TV shows a new one");
        assert_eq!(pair::pinned_fp(&dir, "tv"), None);
        assert_eq!(st.lock().await.pairing["tv"]["step"], "failed");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A TV that says no on its screen while the code is up: told at once, in words, without waiting for a code.
    #[tokio::test]
    async fn a_denial_on_the_tv_ends_the_wait_for_the_code() {
        let Ok(conn) = Connection::session().await else {
            eprintln!("no session bus: skipped");
            return;
        };
        let dir = temp_dir("denied");
        let me = pair::Identity::load(&dir).unwrap();
        let tv = tv::Tv::new();
        let addr = tv
            .serve(|_, _, mut wr, _| async move {
                send(&mut wr, &json!({"t": "pair", "step": "code", "digits": 6}))
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(100)).await;
                send(&mut wr, &json!({"t": "pair_refused", "reason": "denied"}))
                    .await
                    .unwrap();
            })
            .await;
        let o = dial(addr, &me, None).await.unwrap();
        let st = Shared::default();
        let (_tx, rx) = mpsc::channel(1);
        pairing(
            st.clone(),
            conn,
            test_receiver(addr),
            "tv".into(),
            o,
            me.fp,
            tv.fp,
            dir.clone(),
            rx,
        )
        .await;
        let s = st.lock().await;
        assert_eq!(
            s.pairing["tv"]["reason"],
            "Living room TV refused the connection"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The web receiver: a new browser sees a code and waits for this computer; allowed, it gets a token that comes
    /// back as an HttpOnly cookie and lets it straight in the next time; refused, its page says so.
    #[tokio::test]
    async fn a_browser_is_let_in_once_confirmed_and_keeps_its_cookie() {
        let Ok(dbus) = Connection::session().await else {
            eprintln!("no session bus: skipped");
            return;
        };
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let dir = temp_dir("web");
        let st = Shared::default();
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        {
            let (st, dbus, dir) = (st.clone(), dbus.clone(), dir.clone());
            tokio::spawn(async move {
                loop {
                    let (s, peer) = l.accept().await.unwrap();
                    let (st, dbus, dir) = (st.clone(), dbus.clone(), dir.clone());
                    tokio::spawn(web_session(
                        s,
                        peer,
                        st,
                        dbus,
                        "sid".into(),
                        "Linux Laptop".into(),
                        dir,
                    ));
                }
            });
        }
        let browser = |cookie: Option<String>| async move {
            let mut req = format!("ws://{addr}/ws?mode=none")
                .into_client_request()
                .unwrap();
            if let Some(c) = cookie {
                req.headers_mut().insert("Cookie", c.parse().unwrap());
            }
            let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
            let (mut ws, _) = tokio_tungstenite::client_async(req, tcp).await.unwrap();
            ws.send(Message::Text(
                json!({"t": "hello", "model": "Chrome/120", "mode": "none"})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            ws
        };
        async fn next_json<S: AsyncRead + AsyncWrite + Unpin>(
            ws: &mut WebSocketStream<S>,
        ) -> Value {
            loop {
                match timeout(Duration::from_secs(5), ws.next()).await.unwrap() {
                    Some(Ok(Message::Text(t))) => return serde_json::from_str(t.as_str()).unwrap(),
                    Some(Ok(_)) => continue,
                    other => panic!("closed: {other:?}"),
                }
            }
        }
        let answer = |st: Shared, allow: bool| async move {
            let (req, code) = timeout(Duration::from_secs(5), async {
                loop {
                    if let Some((id, (v, _))) = st.lock().await.web_waiting.iter().next() {
                        return (id.clone(), v["code"].as_str().unwrap().to_string());
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
            let (_, tx) = st.lock().await.web_waiting.remove(&req).unwrap();
            tx.send(allow).unwrap();
            code
        };

        // A new browser: its page shows the same code as the question here; allowed, it gets a token.
        let mut ws = browser(None).await;
        let confirm = next_json(&mut ws).await;
        assert_eq!(confirm["t"], "confirm");
        assert_eq!(
            answer(st.clone(), true).await,
            confirm["code"].as_str().unwrap()
        );
        let ok = next_json(&mut ws).await;
        assert_eq!(ok["t"], "authorized");
        let token = ok["token"].as_str().unwrap().to_string();
        assert_eq!(next_json(&mut ws).await["t"], "welcome");
        drop(ws);

        // The page trades the token for an HttpOnly cookie.
        let mut post = tokio::net::TcpStream::connect(addr).await.unwrap();
        post.write_all(
            format!(
                "POST /auth HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{token}",
                token.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        let mut resp = String::new();
        post.read_to_string(&mut resp).await.unwrap();
        assert!(resp.starts_with("HTTP/1.1 204"), "{resp}");
        assert!(
            resp.contains(&format!("Set-Cookie: luch_web={token};")) && resp.contains("HttpOnly")
        );

        // With the cookie: straight in, nothing asked.
        let mut ws = browser(Some(format!("luch_web={token}"))).await;
        assert_eq!(next_json(&mut ws).await["t"], "welcome");
        assert!(st.lock().await.web_waiting.is_empty());
        drop(ws);

        // A made-up cookie is asked about like a new browser; refused, its page says so.
        let mut ws = browser(Some("luch_web=0000".into())).await;
        assert_eq!(next_json(&mut ws).await["t"], "confirm");
        answer(st.clone(), false).await;
        let no = next_json(&mut ws).await;
        assert_eq!(no["t"], "denied");
        assert_eq!(no["reason"], "refused on the computer");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Against a real receiver (the Android app on an emulator): `LUCH_TV=host:port` of its TLS port, and the code
    /// it shows written to the file `LUCH_TV_CODE` by whoever reads the screen. Pairs, connects again with the pin,
    /// and starts a sealed session. Run by hand: `cargo test -p luchd -- --ignored real_receiver`.
    #[tokio::test]
    #[ignore]
    async fn real_receiver_pairs_and_takes_sealed_media() {
        let tv: SocketAddr = std::env::var("LUCH_TV").unwrap().parse().unwrap();
        let code_file = std::env::var("LUCH_TV_CODE").unwrap();
        let dir = temp_dir("real");
        let me = pair::Identity::load(&dir).unwrap();
        let mut o = dial(tv, &me, None).await.unwrap();
        assert_eq!(o.first["t"], "pair", "{}", o.first);
        let code = timeout(Duration::from_secs(60), async {
            loop {
                if let Ok(c) = std::fs::read_to_string(&code_file) {
                    if let Ok(c) = pair::clean_code(c.trim()) {
                        return c;
                    }
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
        .await
        .unwrap();
        let theirs = o.receiver_fp.unwrap();
        pair::code_check(&mut o.rd, &mut o.wr, &code, &me.fp, &theirs)
            .await
            .unwrap();
        assert_eq!(read_msg(&mut o.rd).await.unwrap().unwrap()["t"], "welcome");
        drop(o);

        let mut o = dial(tv, &me, Some(theirs)).await.unwrap();
        assert_eq!(o.first["t"], "welcome", "paired: straight in");
        send(
            &mut o.wr,
            &json!({"t": "start", "mode": "mirror",
            "video": {"codec": "video/avc", "width": 1280, "height": 720, "fps": 30}}),
        )
        .await
        .unwrap();
        let ready = loop {
            let v = read_msg(&mut o.rd).await.unwrap().unwrap();
            if v["t"] == "ready" {
                break v;
            }
        };
        assert_eq!(ready["crypt"], "aes-128-gcm");
        assert_eq!(mirror::media_key(&ready).unwrap().unwrap().len(), 16);
        eprintln!("real receiver: paired, pinned, ready with a media key");
        std::fs::remove_dir_all(dir).unwrap();
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
}
