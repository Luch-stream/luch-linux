//! TVs without the Luch app (docs/other-tvs.md): DLNA/UPnP renderers and Chromecasts play what `Cast` is given,
//! fetching it over HTTP from `cast::Server` like a Luch receiver does. They are found next to the Luch receivers
//! (SSDP, mDNS `_googlecast._tcp`), shown in the same list, one row per TV, and driven through the same D-Bus calls:
//! `Cast`, `MediaCmd`, `MediaState`, `StopCast`, `SetVolume`. What they report is told as Luch's `media_state`.
//!
//! When one TV speaks several of these, the best is used (`Kind` is ordered by rank); a caller may name another
//! with the `transport` option.

pub mod castv2;
pub mod dlna;
pub mod mdns;
pub mod ssdp;
mod xml;

#[cfg(test)]
pub(crate) mod fake_cast;
#[cfg(test)]
pub(crate) mod fake_dlna;
#[cfg(test)]
mod tests;

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, watch, Notify};

/// How luchd talks to a TV, best first: Luch (its own receiver: mirroring, the lowest latency, everything), then
/// Google Cast (state pushed to us, subtitles, speed, about 2 s to start), then DLNA (state polled once a second,
/// subtitles only on some TVs, one speed).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Luch,
    Cast,
    Dlna,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Luch => "luch",
            Kind::Cast => "cast",
            Kind::Dlna => "dlna",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        [Kind::Luch, Kind::Cast, Kind::Dlna]
            .into_iter()
            .find(|k| k.as_str() == s)
    }

    /// What can be done over it, as `Devices` lists it.
    pub fn can(self) -> &'static [&'static str] {
        match self {
            Kind::Luch => &[
                "mirror", "media", "photo", "doc", "apps", "volume", "subs", "rate",
            ],
            Kind::Cast => &["media", "photo", "volume", "subs", "rate"],
            Kind::Dlna => &["media", "photo", "volume", "subs"],
        }
    }

    /// About how long a press takes to show on the TV (ms): what the ranking weighs, measured on real devices of
    /// each kind (docs/other-tvs.md).
    pub fn control_ms(self) -> u32 {
        match self {
            Kind::Luch => 30,
            Kind::Cast => 150,
            Kind::Dlna => 600,
        }
    }
}

/// A TV found on the network that is not (only) a Luch receiver.
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    /// "dlna:<UDN>" or "cast:<id>": stable across restarts, never a Luch UUID.
    pub id: String,
    pub kind: Kind,
    pub name: String,
    pub model: String,
    pub address: IpAddr,
    pub port: u16,
    pub control: Control,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Control {
    Dlna {
        av_transport: String,
        /// The service type as announced (AVTransport:1 or :2): SOAP actions are named with it.
        av_type: String,
        rendering: Option<(String, String)>,
    },
    Cast,
}

// ---------- one list ----------

/// One way to reach a TV, as the list is built from: Luch receivers and devices alike.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub kind: Kind,
    pub id: String,
    pub name: String,
    pub model: String,
    pub address: String,
    pub ifname: String,
    pub port: u16,
}

impl From<&Device> for Entry {
    fn from(d: &Device) -> Entry {
        Entry {
            kind: d.kind,
            id: d.id.clone(),
            name: d.name.clone(),
            model: d.model.clone(),
            address: d.address.to_string(),
            ifname: String::new(),
            port: d.port,
        }
    }
}

/// A TV with every way to reach it, best first; `ways[0]` names the row.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub ways: Vec<Entry>,
}

/// Puts the ways that lead to one TV together: the same address, each kind once (two Chromecasts at one address are
/// a TV and a speaker group, two rows). Groups come in a stable order, by name.
pub fn group(entries: impl IntoIterator<Item = Entry>) -> Vec<Group> {
    let mut all: Vec<Entry> = entries.into_iter().collect();
    all.sort_by(|a, b| (a.kind, &a.id).cmp(&(b.kind, &b.id)));
    let mut groups: Vec<Group> = Vec::new();
    for e in all {
        match groups
            .iter_mut()
            .find(|g| g.ways[0].address == e.address && g.ways.iter().all(|w| w.kind != e.kind))
        {
            Some(g) => g.ways.push(e),
            None => groups.push(Group { ways: vec![e] }),
        }
    }
    groups.sort_by(|a, b| (&a.ways[0].name, &a.ways[0].id).cmp(&(&b.ways[0].name, &b.ways[0].id)));
    groups
}

/// The way to use for a row id: the best, or the one of kind `forced`. None when the id names no row, or the TV
/// has no such way.
pub fn pick<'a>(groups: &'a [Group], id: &str, forced: Option<Kind>) -> Option<&'a Entry> {
    let g = groups.iter().find(|g| g.ways.iter().any(|w| w.id == id))?;
    match forced {
        Some(k) => g.ways.iter().find(|w| w.kind == k),
        None => g.ways.first(),
    }
}

/// A row of `Devices`: what a front-end needs to show the TV and offer what it can do.
pub fn describe(g: &Group) -> Value {
    let best = &g.ways[0];
    json!({
        "id": best.id,
        "name": best.name,
        "model": best.model,
        "address": best.address,
        "kind": best.kind.as_str(),
        "can": best.kind.can(),
        "control_ms": best.kind.control_ms(),
        "transports": g.ways.iter().map(|w| json!({"kind": w.kind.as_str(), "id": w.id})).collect::<Vec<_>>(),
    })
}

/// What `Capabilities` says of a TV that has no Luch welcome: its kind and modes, as a welcome would.
pub fn capabilities(d: &Device) -> Value {
    let modes: Vec<&str> = d
        .kind
        .can()
        .iter()
        .copied()
        .filter(|c| ["media", "photo"].contains(c))
        .collect();
    json!({"kind": d.kind.as_str(), "modes": modes, "name": d.name, "model": d.model})
}

// ---------- what to play ----------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sub {
    pub url: String,
    pub mime: String,
    pub lang: String,
}

/// One thing for a TV to play or show, from the message `cast::build` makes for a Luch receiver.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Media {
    pub url: String,
    pub mime: String,
    pub title: String,
    pub start: f64,
    pub paused: bool,
    pub poster: Option<String>,
    pub subs: Vec<Sub>,
    /// A picture rather than something that plays.
    pub photo: bool,
    /// Served by luchd (a local file or relayed), so byte ranges work; otherwise the TV fetches a site itself.
    pub ours: bool,
}

impl Media {
    /// The first (best) source of a `media` message, or the first picture of a `photo` one. Documents and the TV's
    /// apps are Luch's alone.
    pub fn from_message(m: &Value) -> Result<Media, String> {
        let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
        match m["t"].as_str() {
            Some("media") => Ok(Media {
                url: s(&m["src"][0]["url"]),
                mime: s(&m["src"][0]["mime"]),
                title: s(&m["title"]),
                start: m["start"].as_f64().unwrap_or(0.0),
                paused: m["paused"].as_bool().unwrap_or(false),
                poster: m["poster"].as_str().map(String::from),
                subs: m["subs"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|x| Sub {
                        url: s(&x["url"]),
                        mime: s(&x["mime"]),
                        lang: s(&x["lang"]),
                    })
                    .collect(),
                photo: false,
                ours: m["via"] == "sender",
            }),
            Some("photo") => {
                let p = &m["items"][0];
                Ok(Media {
                    url: s(&p["url"]),
                    mime: s(&p["mime"]),
                    title: s(&p["name"]),
                    photo: true,
                    ours: true,
                    ..Default::default()
                })
            }
            Some("doc") => Err("this TV cannot show documents: only a Luch receiver can".into()),
            Some("open_app") => {
                Err("this TV cannot open YouTube from here: only a Luch receiver can".into())
            }
            _ => Err("nothing this TV can play".into()),
        }
    }
}

// ---------- a session ----------

/// What a session is asked: a `media_cmd` (without "t"), a Luch `volume` message, to end (the TV stops), or to
/// leave (something newer plays there now: the TV is left alone).
#[derive(Debug)]
pub enum Ask {
    Cmd(Value),
    Volume(Value),
    Stop,
    Leave,
}

type Reply = oneshot::Sender<Result<Value, String>>;

/// A TV playing something for us. Its task reports `media_state`s and ends with a `stopped` one.
#[derive(Clone)]
pub struct Session {
    asks: mpsc::Sender<(Ask, Reply)>,
    state: watch::Receiver<Value>,
}

impl Session {
    pub async fn ask(&self, a: Ask) -> Result<Value, String> {
        let (tx, rx) = oneshot::channel();
        self.asks
            .send((a, tx))
            .await
            .map_err(|_| "the TV is no longer playing".to_string())?;
        tokio::time::timeout(Duration::from_secs(10), rx)
            .await
            .map_err(|_| "the TV did not answer".to_string())?
            .map_err(|_| "the TV is no longer playing".to_string())?
    }

    pub fn state(&self) -> Value {
        self.state.borrow().clone()
    }

    pub fn ended(&self) -> bool {
        self.asks.is_closed()
    }

    /// Whether both handles are the one session.
    pub fn same(&self, other: &Session) -> bool {
        self.asks.same_channel(&other.asks)
    }
}

/// The ends a transport's task works with: asks to answer, states to tell.
pub struct Ends {
    pub asks: mpsc::Receiver<(Ask, Reply)>,
    pub tell: Teller,
}

pub fn session() -> (Session, Ends, mpsc::UnboundedReceiver<Value>) {
    let (atx, arx) = mpsc::channel(8);
    let (wtx, wrx) = watch::channel(json!({}));
    let (stx, srx) = mpsc::unbounded_channel();
    (
        Session {
            asks: atx,
            state: wrx,
        },
        Ends {
            asks: arx,
            tell: Teller {
                watch: wtx,
                out: stx,
                last: json!({}),
                last_sent: None,
                asked: None,
                force: false,
            },
        },
        srx,
    )
}

/// Tells states as Luch receivers do (PROTOCOL.md 4b): on every change of state or rate, on a jump of the position
/// (a seek), and every 5 s while playing; `by` says whether our ask or the TV's own remote caused it.
pub struct Teller {
    watch: watch::Sender<Value>,
    out: mpsc::UnboundedSender<Value>,
    last: Value,
    last_sent: Option<Instant>,
    asked: Option<Instant>,
    force: bool,
}

impl Teller {
    /// We just asked the TV something: the next state is told even if it looks the same (a short seek, "do":
    /// "state"), and what follows is "by" the sender.
    pub fn asked(&mut self) {
        self.asked = Some(Instant::now());
        self.force = true;
    }

    /// The TV's state now: "loading", "playing", "paused", "ended" or "error", where it is and how long it is.
    pub fn tell(&mut self, state: &str, pos: f64, dur: Option<f64>, rate: f64) {
        let pos = (pos * 10.0).round() / 10.0;
        let since = self.last_sent.map(|t| t.elapsed().as_secs_f64());
        let was = self.last["state"].as_str().unwrap_or("");
        let expected = match (was, since) {
            ("playing", Some(s)) => {
                self.last["pos"].as_f64().unwrap_or(0.0)
                    + s * self.last["rate"].as_f64().unwrap_or(1.0)
            }
            _ => self.last["pos"].as_f64().unwrap_or(0.0),
        };
        let changed = was != state
            || self.last["rate"].as_f64() != Some(rate)
            || (pos - expected).abs() > 2.5
            || self.last["dur"].as_f64() != dur;
        let due = self.force || (state == "playing" && since.is_none_or(|s| s >= 5.0));
        if !changed && !due {
            return;
        }
        let by = if self
            .asked
            .is_some_and(|t| t.elapsed() < Duration::from_secs(3))
        {
            "sender"
        } else {
            "receiver"
        };
        let v = json!({"t": "media_state", "state": state, "pos": pos, "dur": dur, "rate": rate,
                       "live": dur.is_none() && !matches!(state, "loading" | "error"), "by": by});
        self.send(v);
    }

    /// The end: `reason` "ended" (played to the end), "sender" (StopCast), "receiver" (stopped on the TV),
    /// "replaced" (something else plays there now) or "gone" (the TV went away).
    pub fn stopped(&mut self, reason: &str) {
        self.send(json!({"t": "media_state", "state": "stopped", "reason": reason}));
    }

    pub fn error(&mut self, detail: &str) {
        self.send(json!({"t": "media_state", "state": "error", "detail": detail}));
    }

    fn send(&mut self, v: Value) {
        self.force = false;
        self.last = v.clone();
        self.last_sent = Some(Instant::now());
        let _ = self.watch.send(v.clone());
        let _ = self.out.send(v);
    }
}

/// Starts playing `m` on `d`; returns once the TV has taken it, with the session and its states.
pub async fn play(
    d: &Device,
    m: Media,
    http: &reqwest::Client,
) -> Result<(Session, mpsc::UnboundedReceiver<Value>), String> {
    match &d.control {
        Control::Dlna { .. } => dlna::play(d, m, http).await,
        Control::Cast => castv2::play(d, m).await,
    }
}

/// The address of this machine the TV reaches: the source the routing table picks for it.
pub fn local_for(tv: IpAddr) -> std::io::Result<IpAddr> {
    let s = std::net::UdpSocket::bind(if tv.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" })?;
    s.connect((tv, 9))?;
    Ok(s.local_addr()?.ip())
}

/// The TVs found so far and what they play for us; shared by the D-Bus object and the discovery tasks.
#[derive(Default)]
pub struct Tvs {
    devices: Mutex<HashMap<String, (Device, Instant)>>,
    sessions: tokio::sync::Mutex<HashMap<String, Session>>,
    pub refresh: Notify,
}

/// A device not seen again for this long is gone (SSDP has no reliable bye; Avahi tells removals itself).
const FORGET_AFTER: Duration = Duration::from_secs(200);

impl Tvs {
    pub fn new() -> Arc<Tvs> {
        Arc::new(Tvs::default())
    }

    /// Notes a device; true when it is new or changed.
    pub fn saw(&self, d: Device) -> bool {
        let mut all = self.devices.lock().unwrap();
        let new = all.get(&d.id).is_none_or(|(old, _)| old != &d);
        if new {
            eprintln!(
                "[luchd] found {}: {} ({}) at {}",
                d.kind.as_str(),
                d.name,
                d.model,
                d.address
            );
        }
        all.insert(d.id.clone(), (d, Instant::now()));
        new
    }

    pub fn remove(&self, id: &str) -> bool {
        self.devices.lock().unwrap().remove(id).is_some()
    }

    /// Forgets devices of `kind` not seen for a while; true when any went.
    pub fn prune(&self, kind: Kind) -> bool {
        let mut all = self.devices.lock().unwrap();
        let before = all.len();
        all.retain(|_, (d, seen)| d.kind != kind || seen.elapsed() < FORGET_AFTER);
        all.len() != before
    }

    pub fn device(&self, id: &str) -> Option<Device> {
        self.devices.lock().unwrap().get(id).map(|(d, _)| d.clone())
    }

    pub fn devices(&self) -> Vec<Device> {
        self.devices
            .lock()
            .unwrap()
            .values()
            .map(|(d, _)| d.clone())
            .collect()
    }

    /// `s` plays for `row` now; one that played there before leaves the TV to it.
    pub async fn start(&self, row: &str, s: Session) {
        let old = self.sessions.lock().await.insert(row.to_string(), s);
        if let Some(old) = old {
            let _ = old.ask(Ask::Leave).await;
        }
    }

    /// The session playing for row `row`, if one still runs.
    pub async fn with<R>(&self, row: &str, f: impl FnOnce(&Session) -> R) -> Option<R> {
        let mut all = self.sessions.lock().await;
        if all.get(row).is_some_and(Session::ended) {
            all.remove(row);
        }
        all.get(row).map(f)
    }

    /// Asks the session playing for `row`; None when nothing plays there for us.
    pub async fn ask(&self, row: &str, a: Ask) -> Option<Result<Value, String>> {
        let s = self
            .sessions
            .lock()
            .await
            .get(row)
            .filter(|s| !s.ended())
            .cloned()?;
        Some(s.ask(a).await)
    }

    pub async fn stop(&self, row: &str) -> Option<Result<Value, String>> {
        let r = self.ask(row, Ask::Stop).await;
        self.sessions.lock().await.remove(row);
        r
    }
}

/// Keeps finding DLNA renderers: a search at start, every minute and on `refresh`.
pub async fn search_renderers(
    tvs: Arc<Tvs>,
    target: ssdp::Target,
    on_change: impl Fn() + Send + Sync,
) {
    let http = reqwest::Client::new();
    loop {
        let mut changed = false;
        match ssdp::search(target, Duration::from_secs(2)).await {
            Ok(answers) => {
                for a in answers {
                    match ssdp::describe(&http, &a.location).await {
                        Ok(Some(d)) => changed |= tvs.saw(d),
                        Ok(None) => {}
                        Err(e) => eprintln!("[luchd] renderer {}: {e}", a.location),
                    }
                }
            }
            Err(e) => eprintln!("[luchd] SSDP search failed: {e}"),
        }
        changed |= tvs.prune(Kind::Dlna);
        if changed {
            on_change();
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(60)) => {}
            _ = tvs.refresh.notified() => {}
        }
    }
}

/// "H:MM:SS", the time format of AVTransport.
pub fn hms(t: f64) -> String {
    let t = t.max(0.0) as u64;
    format!("{}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60)
}

/// "H+:MM:SS[.F+]" (also "MM:SS", fractions as decimals or "F0/F1") in seconds; None for "NOT_IMPLEMENTED" or "".
pub fn parse_hms(s: &str) -> Option<f64> {
    let s = s.trim();
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    let mut t = 0.0;
    for part in whole.split(':') {
        t = t * 60.0 + part.parse::<u64>().ok()? as f64;
    }
    let f = match frac.split_once('/') {
        Some((a, b)) => a.parse::<f64>().ok()? / b.parse::<f64>().ok().filter(|b| *b > 0.0)?,
        None if frac.is_empty() => 0.0,
        None => format!("0.{frac}").parse().ok()?,
    };
    Some(t + f)
}
