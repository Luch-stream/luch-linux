//! Finding a receiver when mDNS does not get through (PROTOCOL.md 1): a ladder that climbs by itself.
//!
//! 1. mDNS (discovery.rs) and, at the same time, a sweep of the LAN for the control port (scan.rs);
//! 2. after a few seconds with nothing: Bluetooth LE, the receivers' cards over the air (ble.rs);
//! 3. only when the person asked for it (`Find("all")`), as it needs them and opens the microphone and the camera:
//!    the TV saying its card by sound (modem.rs) and its QR code held up to the camera (qr.rs); and which TVs nearby
//!    offer a direct connection over their own Wi-Fi network (direct/mod.rs), never made without the person.
//!
//! Every receiver found any way is checked with the real hello before it is listed, and joins the same list
//! (Receivers, ReceiversChanged). `FindProgress` says what is being tried.

pub mod ble;
pub mod card;
pub mod modem;
pub mod qr;
mod rs;
pub mod scan;

use std::{
    future::Future,
    net::{Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use serde_json::{json, Value};
use tokio::{
    sync::Mutex,
    task::JoinSet,
    time::{timeout, Instant},
};
use zbus::Connection;

use crate::{Receiver, Shared};
use card::Card;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Rung {
    Mdns,
    Scan,
    Ble,
    Sound,
    Qr,
    Direct,
}

impl Rung {
    pub const ALL: [Rung; 6] = [
        Rung::Mdns,
        Rung::Scan,
        Rung::Ble,
        Rung::Sound,
        Rung::Qr,
        Rung::Direct,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Rung::Mdns => "mdns",
            Rung::Scan => "scan",
            Rung::Ble => "ble",
            Rung::Sound => "sound",
            Rung::Qr => "qr",
            Rung::Direct => "direct",
        }
    }

    fn parse(s: &str) -> Option<Rung> {
        Rung::ALL.into_iter().find(|r| r.name() == s)
    }

    /// What the person is told while it runs.
    fn trying(self) -> &'static str {
        match self {
            Rung::Mdns => "asking the network (mDNS)",
            Rung::Scan => "asking every address of this network",
            Rung::Ble => "listening for receivers over Bluetooth",
            Rung::Sound => {
                "listening on the microphone: on the TV, open Luch and choose \"Say the address by sound\""
            }
            Rung::Qr => "looking through the camera: hold it up to the QR code on the TV",
            Rung::Direct => {
                "asking the TVs nearby over Bluetooth whether they connect directly (Wi-Fi Direct)"
            }
        }
    }
}

/// Rungs that start together, and how long they are given before the next tier starts (sooner when all of them
/// have finished).
#[derive(Clone, Debug)]
pub struct Tier {
    pub rungs: Vec<Rung>,
    pub patience: Duration,
}

/// The order of the climb and where it is.
#[derive(Clone, Debug)]
pub struct Ladder {
    tiers: Vec<Tier>,
    next: usize,
    tier_at: Duration,
}

/// mDNS answers within a second or two on a network that carries it; the sweep of a /24 takes about as long.
pub const FIRST_PATIENCE: Duration = Duration::from_secs(4);
pub const BLE_LISTEN: Duration = Duration::from_secs(12);
pub const PERSON_LISTEN: Duration = Duration::from_secs(30);

impl Ladder {
    /// "auto" (what Refresh climbs), "all" (also sound and the camera), or a comma-separated list of rungs to try
    /// right away.
    pub fn new(mode: &str) -> Result<Ladder, String> {
        let first = Tier {
            rungs: vec![Rung::Mdns, Rung::Scan],
            patience: FIRST_PATIENCE,
        };
        let ble = Tier {
            rungs: vec![Rung::Ble],
            patience: BLE_LISTEN,
        };
        let person = Tier {
            rungs: vec![Rung::Sound, Rung::Qr, Rung::Direct],
            patience: PERSON_LISTEN,
        };
        let tiers = match mode {
            "" | "auto" => vec![first, ble],
            "all" => vec![first, ble, person],
            list => {
                let rungs: Option<Vec<Rung>> =
                    list.split(',').map(|r| Rung::parse(r.trim())).collect();
                let rungs = rungs.ok_or_else(|| {
                    format!("unknown way to look: {list} (auto, all, or some of mdns,scan,ble,sound,qr,direct)")
                })?;
                vec![Tier {
                    rungs,
                    patience: PERSON_LISTEN,
                }]
            }
        };
        Ok(Ladder {
            tiers,
            next: 0,
            tier_at: Duration::ZERO,
        })
    }

    pub fn rungs(&self) -> impl Iterator<Item = Rung> + '_ {
        self.tiers.iter().flat_map(|t| t.rungs.iter().copied())
    }

    /// The rungs to start now: the first tier at once, the next one when nothing has been found and the last tier
    /// has had its time or has finished.
    pub fn due(&mut self, now: Duration, found_any: bool, running: usize) -> Vec<Rung> {
        if self.next >= self.tiers.len() {
            return Vec::new();
        }
        if self.next > 0 {
            let waited = now.saturating_sub(self.tier_at);
            if found_any || (running > 0 && waited < self.tiers[self.next - 1].patience) {
                return Vec::new();
            }
        }
        self.tier_at = now;
        self.next += 1;
        self.tiers[self.next - 1].rungs.clone()
    }

    pub fn exhausted(&self) -> bool {
        self.next >= self.tiers.len()
    }
}

/// What each rung is doing, for FindProgress.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub running: bool,
    pub mode: String,
    pub rungs: Vec<(Rung, &'static str, String)>, // rung, state, note
    pub found: usize,
    pub elapsed: Duration,
}

impl Progress {
    fn set(&mut self, rung: Rung, state: &'static str, note: String) {
        if let Some(r) = self.rungs.iter_mut().find(|r| r.0 == rung) {
            r.1 = state;
            r.2 = note;
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "running": self.running,
            "mode": self.mode,
            "found": self.found,
            "elapsed": (self.elapsed.as_secs_f64() * 10.0).round() / 10.0,
            "rungs": self.rungs.iter().map(|(r, state, note)| json!({"rung": r.name(), "state": state, "note": note}))
                .collect::<Vec<_>>(),
        })
    }
}

pub type RungFuture = Pin<Box<dyn Future<Output = Result<String, String>> + Send>>;

/// Climbs `ladder`: starts the rungs as they fall due, follows them, and reports every change to `report`.
/// `found` says how many receivers are listed now; `run` starts one rung and resolves to its note (or failure).
pub async fn climb(
    mut ladder: Ladder,
    mode: &str,
    found: impl Fn() -> Pin<Box<dyn Future<Output = usize> + Send>>,
    run: impl Fn(Rung) -> RungFuture,
    mut report: impl FnMut(&Progress),
) -> Progress {
    let t0 = Instant::now();
    let mut p = Progress {
        running: true,
        mode: mode.to_string(),
        rungs: ladder
            .rungs()
            .map(|r| (r, "waiting", String::new()))
            .collect(),
        ..Default::default()
    };
    let mut jobs: JoinSet<(Rung, Result<String, String>)> = JoinSet::new();
    loop {
        p.found = found().await;
        p.elapsed = t0.elapsed();
        let due = ladder.due(t0.elapsed(), p.found > 0, jobs.len());
        for r in &due {
            p.set(*r, "running", r.trying().to_string());
            let job = run(*r);
            let r = *r;
            jobs.spawn(async move { (r, job.await) });
        }
        if !due.is_empty() {
            report(&p);
        }
        if jobs.is_empty() && (p.found > 0 || ladder.exhausted()) {
            break;
        }
        tokio::select! {
            Some(done) = jobs.join_next(), if !jobs.is_empty() => {
                if let Ok((r, result)) = done {
                    match result {
                        Ok(note) => p.set(r, "done", note),
                        Err(e) => p.set(r, "failed", e),
                    }
                    p.found = found().await;
                    p.elapsed = t0.elapsed();
                    report(&p);
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
    }
    for r in &mut p.rungs {
        if r.1 == "waiting" {
            r.1 = "skipped";
            r.2 = "a receiver was found before".into();
        }
    }
    p.running = false;
    p.found = found().await;
    p.elapsed = t0.elapsed();
    report(&p);
    p
}

/// What a rung needs from luchd.
#[derive(Clone)]
pub struct Ctx {
    pub st: Shared,
    pub sender_id: String,
    pub host: String,
    pub discovery: crate::discovery::Discovery,
}

/// The climb in luchd: one at a time, its last progress kept for FindStatus.
#[derive(Clone)]
pub struct Finder {
    ctx: Ctx,
    last: Arc<Mutex<(Progress, Option<Instant>)>>,
}

/// Refresh climbs the automatic ladder at most this often; mDNS is refreshed every time anyway.
const AUTO_EVERY: Duration = Duration::from_secs(30);

impl Finder {
    pub fn new(ctx: Ctx) -> Self {
        Finder {
            ctx,
            last: Arc::default(),
        }
    }

    pub async fn status(&self) -> String {
        self.last.lock().await.0.to_json().to_string()
    }

    /// Starts a climb unless one is running (or, for "auto", one ended a moment ago).
    pub async fn start(&self, conn: &Connection, mode: &str) -> Result<(), String> {
        let ladder = Ladder::new(mode)?;
        {
            let mut last = self.last.lock().await;
            if last.0.running {
                return Ok(());
            }
            if matches!(mode, "" | "auto") && last.1.is_some_and(|t| t.elapsed() < AUTO_EVERY) {
                return Ok(());
            }
            last.0 = Progress {
                running: true,
                mode: mode.into(),
                ..Default::default()
            };
        }
        let (me, conn, mode) = (self.clone(), conn.clone(), mode.to_string());
        tokio::spawn(async move {
            me.prune().await;
            let st = me.ctx.st.clone();
            let found = move || -> Pin<Box<dyn Future<Output = usize> + Send>> {
                let st = st.clone();
                Box::pin(async move { st.lock().await.found.len() })
            };
            let (ctx, c) = (me.ctx.clone(), conn.clone());
            let run =
                move |r: Rung| -> RungFuture { Box::pin(run_rung(r, ctx.clone(), c.clone())) };
            let last = me.last.clone();
            let c = conn.clone();
            let report = move |p: &Progress| {
                let (json, last, c, p) =
                    (p.to_json().to_string(), last.clone(), c.clone(), p.clone());
                eprintln!("[luchd] find: {json}");
                tokio::spawn(async move {
                    last.lock().await.0 = p;
                    crate::find_progress(&c, &json).await;
                });
            };
            let p = climb(ladder, &mode, found, run, report).await;
            *me.last.lock().await = (p, Some(Instant::now()));
        });
        Ok(())
    }

    /// Receivers found other ways than mDNS go when they no longer answer (mDNS has its own ItemRemove).
    async fn prune(&self) {
        let quiet: Vec<(String, String, u16)> = {
            let s = self.ctx.st.lock().await;
            s.found
                .values()
                .filter(|r| r.service.is_empty() && !s.conns.contains_key(&r.id))
                .map(|r| (r.id.clone(), r.address.clone(), r.port))
                .collect()
        };
        for (id, address, port) in quiet {
            if !crate::answers(&address, port).await {
                let mut s = self.ctx.st.lock().await;
                s.found.remove(&id);
                s.via.remove(&id);
                s.rssi.remove(&id);
            }
        }
    }
}

/// Connects to `ip:port` from the right interface, checks it is a receiver (and the one the card names), and
/// lists it. Returns its name.
pub async fn reach(
    ctx: &Ctx,
    conn: &Connection,
    card: &Card,
    via: &'static str,
    rssi: Option<i16>,
) -> Result<String, String> {
    let (ip, port) = card.addr.ok_or("no address")?;
    let iface = scan::iface_for(ip, &scan::interfaces(), &crate::lan_iface())
        .ok_or_else(|| format!("no network interface reaches {ip}"))?;
    let bind = (!ip.is_loopback()).then_some(iface.name.as_str());
    let welcome = crate::probe(
        SocketAddr::from((ip, port)),
        bind,
        &ctx.sender_id,
        &ctx.host,
        card.token.as_deref(),
        crate::HANDSHAKE_TIMEOUT,
    )
    .await
    .map_err(|e| {
        format!("{ip}:{port} does not answer here ({e}): the network may keep devices apart")
    })?;
    let r = receiver_from(&welcome, ip, port, &iface.name, iface.index)
        .ok_or_else(|| format!("{ip}:{port} is not a Luch receiver"))?;
    if !r.id.replace('-', "").starts_with(&card.id) {
        return Err(format!(
            "{ip}:{port} is another receiver ({}) than the one heard",
            r.name
        ));
    }
    // A token the receiver no longer takes (it was used, or the TV restarted): listed all the same, unpaired.
    let mut name = r.name.clone();
    if card.token.is_some() && welcome["token"] != "ok" {
        name += " (its code has changed since: pairing needs a fresh one)";
    }
    let token = card.token.clone().filter(|_| welcome["token"] == "ok");
    crate::found(&ctx.st, conn, r, via, rssi, token).await;
    Ok(name)
}

/// A receiver from its welcome (only a v0 welcome with an id counts).
pub fn receiver_from(
    w: &Value,
    ip: Ipv4Addr,
    port: u16,
    ifname: &str,
    ifindex: u32,
) -> Option<Receiver> {
    if w["t"] != "welcome" || w["v"].as_u64() != Some(luch_proto::VERSION as u64) {
        return None;
    }
    let id = w["id"].as_str().filter(|s| !s.is_empty())?.to_string();
    Some(Receiver {
        name: w["name"].as_str().unwrap_or(&id).to_string(),
        model: w["model"].as_str().unwrap_or_default().to_string(),
        id,
        address: ip.to_string(),
        ifname: ifname.to_string(),
        ifindex: ifindex as i32,
        port,
        // The plain welcome says where the receiver speaks TLS (section 3): pairing goes there.
        tls_port: w["tls_port"]
            .as_u64()
            .and_then(|p| u16::try_from(p).ok())
            .unwrap_or(0),
        service: String::new(),
    })
}

async fn run_rung(rung: Rung, ctx: Ctx, conn: Connection) -> Result<String, String> {
    let count = |via: &'static str| {
        let st = ctx.st.clone();
        async move {
            st.lock()
                .await
                .via
                .values()
                .filter(|v| v.contains(via))
                .count()
        }
    };
    match rung {
        Rung::Mdns => {
            ctx.discovery.refresh();
            tokio::time::sleep(FIRST_PATIENCE - Duration::from_millis(500)).await;
            Ok(format!("{} found", count("mdns").await))
        }
        Rung::Scan => {
            let arp = std::fs::read_to_string("/proc/net/arp").unwrap_or_default();
            let ifaces: Vec<scan::Iface> = scan::interfaces()
                .into_iter()
                .filter(scan::scannable)
                .collect();
            if ifaces.is_empty() {
                return Err("no LAN interface to scan (a VPN or tunnel is never scanned)".into());
            }
            let mut targets = Vec::new();
            let mut nets = Vec::new();
            for i in &ifaces {
                let t = scan::plan(i, &arp);
                nets.push(format!("{} addresses on {}", t.len(), i.name));
                targets.extend(t.into_iter().map(|a| (a, i.clone())));
            }
            let (c2, ctx2) = (conn.clone(), ctx.clone());
            let found = scan::sweep(targets, scan::IN_FLIGHT, move |(ip, iface)| {
                let (ctx, conn) = (ctx2.clone(), c2.clone());
                async move {
                    let welcome = crate::probe(
                        SocketAddr::from((ip, luch_proto::CONTROL_PORT)),
                        Some(&iface.name),
                        &ctx.sender_id,
                        &ctx.host,
                        None,
                        scan::CONNECT_TIMEOUT,
                    )
                    .await
                    .ok()?;
                    let r = receiver_from(
                        &welcome,
                        ip,
                        luch_proto::CONTROL_PORT,
                        &iface.name,
                        iface.index,
                    )?;
                    let name = r.name.clone();
                    crate::found(&ctx.st, &conn, r, "scan", None, None).await;
                    Some(name)
                }
            });
            use futures_util::StreamExt;
            let names: Vec<String> = timeout(Duration::from_secs(20), found.collect::<Vec<_>>())
                .await
                .unwrap_or_default();
            Ok(format!("asked {}: {}", nets.join(", "), say_found(&names)))
        }
        Rung::Ble => {
            let sys = Connection::system().await.map_err(|e| e.to_string())?;
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let looking = Card::looking(&ctx.sender_id);
            let (c2, ctx2) = (conn.clone(), ctx.clone());
            // Each receiver is tried as soon as it is heard, while the scan goes on.
            let tries = tokio::spawn(async move {
                let mut notes = Vec::new();
                while let Some(h) = rx.recv().await {
                    let h: ble::Heard = h;
                    notes.push(match reach(&ctx2, &c2, &h.card, "ble", h.rssi).await {
                        Ok(name) => name,
                        Err(_) if h.card.addr.is_none() => format!(
                            "heard {0} with no address on this network: it can connect directly (`luchctl direct {0}`)",
                            h.card.id
                        ),
                        Err(e) => format!("heard {} but {e}", h.card.id),
                    });
                }
                notes
            });
            let heard = ble::search(&sys, &looking, BLE_LISTEN, tx).await?;
            let notes = tries.await.unwrap_or_default();
            crate::direct::note_offers(&ctx.st, &heard).await;
            // The signal strength of the whole scan, for the order of the list.
            for h in &heard {
                let mut s = ctx.st.lock().await;
                let id = s
                    .found
                    .keys()
                    .find(|id| id.replace('-', "").starts_with(&h.card.id))
                    .cloned();
                if let (Some(id), Some(r)) = (id, h.rssi) {
                    s.rssi.insert(id, r);
                }
            }
            crate::changed(&conn).await;
            Ok(if heard.is_empty() {
                "no receiver heard".into()
            } else {
                notes.join("; ")
            })
        }
        Rung::Sound => {
            let heard = modem::listen("autoaudiosrc", PERSON_LISTEN.as_secs()).await?;
            let mut notes = Vec::new();
            for card in heard.iter().filter_map(|b| Card::decode(b)) {
                notes.push(
                    reach(&ctx, &conn, &card, "sound", None)
                        .await
                        .unwrap_or_else(|e| e),
                );
            }
            Ok(if notes.is_empty() {
                "heard nothing".into()
            } else {
                notes.join("; ")
            })
        }
        Rung::Qr => {
            let seen = qr::watch("v4l2src", PERSON_LISTEN.as_secs()).await?;
            let mut notes = Vec::new();
            for card in &seen {
                notes.push(
                    reach(&ctx, &conn, card, "qr", None)
                        .await
                        .unwrap_or_else(|e| e),
                );
            }
            Ok(if notes.is_empty() {
                "saw no code".into()
            } else {
                notes.join("; ")
            })
        }
        Rung::Direct => {
            // Only which TVs offer it: joining one takes this computer off its Wi-Fi, so the person chooses.
            let mut offers = crate::direct::offers::fresh();
            if offers.is_empty() {
                let sys = Connection::system().await.map_err(|e| e.to_string())?;
                let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
                let heard =
                    ble::search(&sys, &Card::looking(&ctx.sender_id), BLE_LISTEN, tx).await?;
                offers = crate::direct::note_offers(&ctx.st, &heard).await;
            }
            Ok(crate::direct::say_offers(&offers))
        }
    }
}

fn say_found(names: &[String]) -> String {
    match names {
        [] => "no receiver".into(),
        names => format!("found {}", names.join(", ")),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A binary PGM (P5) picture: width, height, pixels.
    pub(crate) fn read_pgm(b: &[u8]) -> (usize, usize, &[u8]) {
        let mut fields = Vec::new();
        let mut i = 0;
        while fields.len() < 4 {
            while b[i].is_ascii_whitespace() {
                i += 1;
            }
            let start = i;
            while !b[i].is_ascii_whitespace() {
                i += 1;
            }
            fields.push(std::str::from_utf8(&b[start..i]).unwrap().to_string());
        }
        assert_eq!(fields[0], "P5");
        let (w, h) = (fields[1].parse().unwrap(), fields[2].parse().unwrap());
        (w, h, &b[i + 1..i + 1 + w * h])
    }

    #[test]
    fn the_ladder_climbs_only_while_nothing_is_found() {
        let s = Duration::from_secs;
        let mut l = Ladder::new("auto").unwrap();
        assert_eq!(l.due(s(0), false, 0), vec![Rung::Mdns, Rung::Scan]);
        assert!(l.due(s(1), false, 2).is_empty());
        assert!(l.due(s(3), false, 1).is_empty(), "mDNS still has time");
        assert_eq!(l.due(s(4), false, 1), vec![Rung::Ble]);
        assert!(l.exhausted());

        // Found by the sweep in the first second: Bluetooth is never tried.
        let mut l = Ladder::new("auto").unwrap();
        l.due(s(0), false, 0);
        assert!(l.due(s(10), true, 0).is_empty());

        // Both first rungs gave up early: the next tier starts at once, it does not wait for its time.
        let mut l = Ladder::new("all").unwrap();
        l.due(s(0), false, 0);
        assert_eq!(l.due(s(2), false, 0), vec![Rung::Ble]);
        assert_eq!(
            l.due(Duration::from_millis(2100), false, 0),
            vec![Rung::Sound, Rung::Qr, Rung::Direct]
        );
        // Never in the automatic climb: a direct connection takes the computer off its network.
        assert!(!Ladder::new("auto")
            .unwrap()
            .rungs()
            .any(|r| r == Rung::Direct));

        assert_eq!(
            Ladder::new("qr, ble").unwrap().due(s(0), false, 0),
            vec![Rung::Qr, Rung::Ble]
        );
        assert!(Ladder::new("smoke signals").is_err());
    }

    /// The whole climb with fake rungs on a paused clock: nothing on the LAN, Bluetooth hears the TV after 3 s.
    #[tokio::test(start_paused = true)]
    async fn a_climb_says_what_it_tries_and_stops_when_found() {
        let found = Arc::new(AtomicUsize::new(0));
        let f = found.clone();
        let started = Arc::new(std::sync::Mutex::new(Vec::new()));
        let st = started.clone();
        let t0 = Instant::now();
        let run = move |r: Rung| -> RungFuture {
            st.lock().unwrap().push((r, t0.elapsed().as_secs()));
            let f = f.clone();
            Box::pin(async move {
                match r {
                    Rung::Mdns => {
                        // Longer than its tier's patience: Bluetooth starts at 4 s without waiting for it.
                        tokio::time::sleep(Duration::from_secs(10)).await;
                        Ok("0 found".into())
                    }
                    Rung::Scan => {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        Ok("asked 253 addresses on wlan0: no receiver".into())
                    }
                    Rung::Ble => {
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        f.store(1, Ordering::SeqCst);
                        Ok("Living room TV".into())
                    }
                    _ => panic!("sound and camera are not climbed to once a receiver is found"),
                }
            })
        };
        let f = found.clone();
        let count = move || -> Pin<Box<dyn Future<Output = usize> + Send>> {
            let n = f.load(Ordering::SeqCst);
            Box::pin(async move { n })
        };
        let mut reports = Vec::new();
        let p = climb(Ladder::new("all").unwrap(), "all", count, run, |p| {
            reports.push(p.to_json())
        })
        .await;
        assert_eq!(
            *started.lock().unwrap(),
            vec![(Rung::Mdns, 0), (Rung::Scan, 0), (Rung::Ble, 4)]
        );
        assert_eq!(p.found, 1);
        assert!(!p.running);
        let j = p.to_json();
        assert_eq!(
            j["rungs"][2],
            json!({"rung": "ble", "state": "done", "note": "Living room TV"})
        );
        assert_eq!(j["rungs"][3]["state"], "skipped");
        assert_eq!(j["rungs"][4]["state"], "skipped");
        assert_eq!(j["rungs"][5]["state"], "skipped");
        // Found at 7 s; the climb ends when mDNS, still running, has finished.
        assert!(
            (10.0..10.6).contains(&j["elapsed"].as_f64().unwrap()),
            "{j}"
        );
        // The person was told what was tried, in order.
        let tried: Vec<String> = reports
            .iter()
            .flat_map(|r| r["rungs"].as_array().unwrap().clone())
            .filter(|r| r["state"] == "running")
            .map(|r| r["rung"].as_str().unwrap().to_string())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        assert_eq!(tried, vec!["ble", "mdns", "scan"]);
        assert_eq!(
            reports.first().unwrap()["rungs"][1]["note"],
            Rung::Scan.trying()
        );
    }

    /// A failed rung is said as such, and the next one starts at once.
    #[tokio::test(start_paused = true)]
    async fn a_rung_that_cannot_run_hands_over_at_once() {
        let t0 = Instant::now();
        let starts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let s = starts.clone();
        let run = move |r: Rung| -> RungFuture {
            s.lock().unwrap().push((r, t0.elapsed().as_millis() / 100));
            Box::pin(async move {
                match r {
                    Rung::Ble => Err("Bluetooth is off".into()),
                    Rung::Sound | Rung::Qr | Rung::Direct => {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        Ok("heard nothing".into())
                    }
                    _ => Ok("nothing".into()),
                }
            })
        };
        let count = || -> Pin<Box<dyn Future<Output = usize> + Send>> { Box::pin(async { 0 }) };
        let p = climb(Ladder::new("all").unwrap(), "all", count, run, |_| {}).await;
        let starts = starts.lock().unwrap().clone();
        assert_eq!(
            starts.iter().map(|s| s.0).collect::<Vec<_>>(),
            Rung::ALL.to_vec()
        );
        // Within a tick of each other, not 4 and 16 s apart.
        assert!(starts.iter().all(|s| s.1 <= 5), "{starts:?}");
        assert_eq!(p.to_json()["rungs"][2]["state"], "failed");
        assert_eq!(p.to_json()["rungs"][2]["note"], "Bluetooth is off");
        assert_eq!(p.found, 0);
    }

    struct FakeReceiver {
        port: u16,
    }

    /// A receiver on loopback that speaks just hello/welcome (and checks a token), or `junk` for something else on
    /// the port.
    async fn fake_receiver(ip: &str, id: &str, name: &str, junk: bool) -> FakeReceiver {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind((ip, 0)).await.unwrap();
        let port = l.local_addr().unwrap().port();
        let (id, name) = (id.to_string(), name.to_string());
        tokio::spawn(async move {
            while let Ok((s, _)) = l.accept().await {
                let (id, name) = (id.clone(), name.clone());
                tokio::spawn(async move {
                    let (rd, mut wr) = s.into_split();
                    let mut lines = tokio::io::BufReader::new(rd).lines();
                    if junk {
                        let _ = wr.write_all(b"SSH-2.0-OpenSSH_9.6\r\n").await;
                        return;
                    }
                    let Ok(Some(hello)) = lines.next_line().await else {
                        return;
                    };
                    let hello: Value = serde_json::from_str(&hello).unwrap();
                    assert_eq!(hello["t"], "hello");
                    let token = match hello["token"].as_str() {
                        Some("0badf00d") => json!("ok"),
                        Some(_) => json!("bad"),
                        None => Value::Null,
                    };
                    let w = json!({"t": "welcome", "v": 0, "id": id, "name": name, "model": "Test TV",
                                   "probe": hello["probe"], "token": token});
                    let _ = wr.write_all(format!("{w}\n").as_bytes()).await;
                    // A probe says bye and goes.
                    if let Ok(Some(bye)) = lines.next_line().await {
                        assert!(bye.contains("bye"));
                    }
                });
            }
        });
        FakeReceiver { port }
    }

    /// The sweep with the real probe over real sockets: two receivers, something else on the port, a silent
    /// address, and many addresses with nothing.
    #[tokio::test]
    async fn the_sweep_finds_receivers_with_the_real_handshake() {
        let a = fake_receiver(
            "127.0.0.1",
            "3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47",
            "Living room TV",
            false,
        )
        .await;
        let b = fake_receiver(
            "127.0.0.1",
            "9a1b2c3d-0000-4000-8000-000000000001",
            "Bedroom TV",
            false,
        )
        .await;
        let junk = fake_receiver("127.0.0.1", "", "", true).await;
        let silent = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let silent_port = silent.local_addr().unwrap().port();
        // Ports with nothing behind them stand in for the empty addresses of a subnet.
        let mut targets: Vec<u16> = (0..200).map(|i| 1 + i).collect();
        targets.extend([a.port, junk.port, silent_port, b.port]);
        let t0 = std::time::Instant::now();
        use futures_util::StreamExt;
        let mut got: Vec<String> = scan::sweep(targets, 64, |port| async move {
            let w = crate::probe(
                SocketAddr::from(([127, 0, 0, 1], port)),
                None,
                "sender",
                "Linux Laptop",
                None,
                scan::CONNECT_TIMEOUT,
            )
            .await
            .ok()?;
            receiver_from(&w, Ipv4Addr::LOCALHOST, port, "lo", 1).map(|r| r.name)
        })
        .collect()
        .await;
        got.sort();
        assert_eq!(got, vec!["Bedroom TV", "Living room TV"]);
        // The silent one costs the welcome timeout, not more.
        assert!(
            t0.elapsed() < scan::WELCOME_TIMEOUT + Duration::from_secs(1),
            "{:?}",
            t0.elapsed()
        );
        drop(silent);
    }

    #[tokio::test]
    async fn a_probe_carries_the_token_and_says_it_is_a_probe() {
        let a = fake_receiver(
            "127.0.0.1",
            "3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47",
            "Living room TV",
            false,
        )
        .await;
        let at = SocketAddr::from(([127, 0, 0, 1], a.port));
        let w = crate::probe(
            at,
            None,
            "s",
            "Linux Laptop",
            Some("0badf00d"),
            scan::CONNECT_TIMEOUT,
        )
        .await
        .unwrap();
        assert_eq!(
            (w["probe"].clone(), w["token"].clone()),
            (json!(true), json!("ok"))
        );
        let w = crate::probe(at, None, "s", "Linux Laptop", None, scan::CONNECT_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(w["token"], Value::Null);
        let r = receiver_from(&w, Ipv4Addr::LOCALHOST, a.port, "lo", 1).unwrap();
        assert_eq!(
            (r.name.as_str(), r.model.as_str(), r.service.as_str()),
            ("Living room TV", "Test TV", "")
        );
        assert!(receiver_from(
            &json!({"t": "welcome", "v": 1, "id": "x"}),
            Ipv4Addr::LOCALHOST,
            1,
            "lo",
            1
        )
        .is_none());
        assert!(receiver_from(&json!({"t": "error"}), Ipv4Addr::LOCALHOST, 1, "lo", 1).is_none());
    }
}
