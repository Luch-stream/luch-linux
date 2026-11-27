//! A direct connection (PROTOCOL.md, "direct"), for when this computer and the TV cannot reach each other over any
//! network (guest Wi-Fi that keeps devices apart, no common LAN): over Bluetooth LE, inside the same TLS as its
//! control port, the TV is asked for a Wi-Fi Direct group; it hands over the group's name and passphrase, this
//! computer joins it as a plain Wi-Fi network (nm.rs), connects to the TV there as to any receiver, and goes back to
//! its own network when that session ends or on `DirectOff`.
//!
//! Never started by itself: it takes the computer off its Wi-Fi, so only a person's `Direct` does it. The find ladder
//! only says which TVs offer it.

mod link;
pub mod nm;
pub mod offers;
pub mod wire;

use std::{
    net::{IpAddr, Ipv4Addr},
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::{json, Value};
use tokio::{io::AsyncBufReadExt, sync::mpsc, time::Instant};
use zbus::{object_server::SignalEmitter, Connection, DBusError};

use crate::{
    find::{ble, card::Card},
    pair, Receiver, Shared,
};
use nm::{Joined, Nm, NmBus};
use offers::Offered;

/// What a direct connection needs of luchd.
#[derive(Clone)]
pub struct Ctx {
    pub st: Shared,
    pub sender_id: String,
    pub host: String,
    pub ident: Option<Arc<pair::Identity>>,
}

/// How long the TV's person has to allow it, and the TV to make its group.
const ASK_WITHIN: Duration = Duration::from_secs(90);
/// The control channel's keep-alive (PROTOCOL.md 3) while the TV asks.
const PING_EVERY: Duration = Duration::from_secs(10);
/// How often the end of the session is looked for, to go back.
const WATCH_EVERY: Duration = Duration::from_secs(2);
const KEPT: &str = "direct.json";

struct Now {
    status: Value,
    busy: bool,
    /// On a TV's network: what to undo, the receiver's id and its address there.
    joined: Option<(Joined, String, Ipv4Addr)>,
}

/// One luchd per user, one direct connection at a time.
static NOW: Mutex<Now> = Mutex::new(Now {
    status: Value::Null,
    busy: false,
    joined: None,
});

/// Where it stands, for DirectStatus and DirectChanged: `step` "idle", "looking" (listening over Bluetooth),
/// "opening" (the TV's Bluetooth channel), "pairing" (a code on the TV: Pair under `key`), "asking" (the TV asks its
/// person), "joining" (its Wi-Fi network), "connecting", "on", "back" (with `note`) or "failed" (with `reason`).
pub fn status() -> Value {
    match &NOW.lock().unwrap().status {
        Value::Null => json!({"step": "idle"}),
        v => v.clone(),
    }
}

async fn say(conn: &Connection, v: Value) {
    eprintln!("[luchd] direct: {v}");
    NOW.lock().unwrap().status = v.clone();
    if let Ok(e) = SignalEmitter::new(conn, luch_proto::DBUS_PATH) {
        let _ = crate::Luch::direct_changed(&e, &v.to_string()).await;
    }
}

/// Whether a connection to `address` goes over a TV's own network: its address and MAC there wake nothing later.
pub fn carries(address: &str) -> bool {
    NOW.lock()
        .unwrap()
        .joined
        .as_ref()
        .is_some_and(|(_, _, a)| a.to_string() == address)
}

/// For a menu: the TVs heard lately that offer it, nearest first, and the Wi-Fi network this computer would leave.
pub async fn offered() -> Value {
    json!({"offers": offers::to_json(&offers::fresh()), "leaves": leaves().await})
}

/// The name of the Wi-Fi network this computer is on now, if any.
async fn leaves() -> Option<String> {
    let sys = Connection::system().await.ok()?;
    NmBus(sys).wifi().await.ok()?.previous.map(|p| p.name)
}

/// Starts a direct connection to the TV `key` names ("" for the nearest; see offers::pick). Returns at once with
/// the network it leaves (`{"leaves": ...}`); the rest comes as DirectChanged.
pub async fn start(ctx: Ctx, conn: &Connection, key: &str) -> Result<String, String> {
    let ident = ctx
        .ident
        .clone()
        .ok_or("this computer has no certificate, and a direct connection needs TLS")?;
    let sys = Connection::system().await.map_err(|e| e.to_string())?;
    let dev = NmBus(sys).wifi().await?;
    {
        let mut now = NOW.lock().unwrap();
        if now.busy {
            return Err("already connecting directly".into());
        }
        if let Some((j, _, _)) = &now.joined {
            return Err(format!(
                "already on the network of a TV ('{}'): `luchctl direct off` first",
                j.ssid
            ));
        }
        now.busy = true;
    }
    let (conn, key) = (conn.clone(), key.to_string());
    tokio::spawn(async move {
        if let Err(e) = run(&ctx, &ident, &conn, &key).await {
            say(&conn, json!({"step": "failed", "reason": e})).await;
        }
        NOW.lock().unwrap().busy = false;
    });
    Ok(json!({"leaves": dev.previous.map(|p| p.name)}).to_string())
}

async fn run(
    ctx: &Ctx,
    ident: &pair::Identity,
    conn: &Connection,
    key: &str,
) -> Result<(), String> {
    let sys = Connection::system().await.map_err(|e| e.to_string())?;
    // The TV: heard lately, else heard now.
    if offers::pick(&offers::fresh(), key).is_err() {
        say(conn, json!({"step": "looking"})).await;
        listen(ctx, &sys).await?;
    }
    let o = offers::pick(&offers::fresh(), key)?.clone();
    say(
        conn,
        json!({"step": "opening", "name": o.name, "key": o.key}),
    )
    .await;
    let (address, kind) = ble::address(&sys, &o.device).await?;
    let stream = link::open(&address, &kind, o.psm).await?;
    let dir = crate::config_dir().map_err(|e| e.to_string())?;
    let (welcome, offer, mut wr) = ask(ctx, ident, conn, stream, &o, &dir).await?;
    let id = welcome["id"].as_str().unwrap_or_default().to_string();
    let name = welcome["name"].as_str().unwrap_or(&o.name).to_string();

    // Its network, then the TV on it as any receiver; the Bluetooth channel is done with once joined.
    say(conn, json!({"step": "joining", "name": name, "ssid": offer.ssid, "band": wire::band(offer.freq)})).await;
    let joined = nm::join(
        &NmBus(sys),
        &offer.ssid,
        &offer.pass,
        &format!("Luch: {name}"),
    )
    .await?;
    keep(&joined, &id, offer.addr);
    let _ = crate::send(&mut wr, &json!({"t": "bye"})).await;
    drop(wr);
    say(
        conn,
        json!({"step": "connecting", "name": name, "ssid": offer.ssid}),
    )
    .await;
    if let Err(e) = connect(ctx, conn, &id, &name, &welcome, &offer, &joined).await {
        let back = back(&ctx.st, conn).await;
        return Err(format!("{e}; {back}"));
    }
    say(
        conn,
        json!({"step": "on", "name": name, "id": id, "ssid": offer.ssid,
               "previous": joined.previous.as_ref().map(|p| &p.name)}),
    )
    .await;
    tokio::spawn(watch(ctx.st.clone(), conn.clone(), id));
    Ok(())
}

/// Over the TV's Bluetooth channel (any stream, for the tests): the TLS of its control port, pinned as there, pairing
/// when it asks; then the request. Its welcome, its network, and the channel's writing end to say bye on.
async fn ask<S>(
    ctx: &Ctx,
    ident: &pair::Identity,
    conn: &Connection,
    stream: S,
    o: &Offered,
    dir: &std::path::Path,
) -> Result<(Value, wire::Offer, crate::Writer), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    let mut name = o.name.clone();
    let pinned = pinned_for(dir, &o.key);
    let nowhere = IpAddr::from(Ipv4Addr::UNSPECIFIED);
    let refused = |e: std::io::Error| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            format!("{name}: {e}. If it was reset or Luch was reinstalled on it, `luchctl forget` it and pair again")
        } else {
            format!("{name} over Bluetooth: {e}")
        }
    };
    let (rd, wr, fp) = crate::tls_halves(stream, nowhere, ident, pinned)
        .await
        .map_err(refused)?;
    let hello = wire::ble_hello(crate::hello(&ctx.sender_id, &ctx.host, None, false));
    let crate::Opened {
        first,
        mut rd,
        mut wr,
        receiver_fp,
        ..
    } = crate::greet(rd, wr, (nowhere, nowhere), fp, &hello, true, pinned)
        .await
        .map_err(refused)?;
    let welcome = match first["t"].as_str().unwrap_or("") {
        "welcome" if pinned.is_none() => {
            return Err(format!(
                "{name} let this computer in without pairing: not trusted, nothing asked"
            ))
        }
        "welcome" => first,
        "pair" => {
            let theirs = receiver_fp.ok_or("pairing asked for without TLS")?;
            pair_ble(
                ctx, conn, &o.key, &name, first, &mut rd, &mut wr, &ident.fp, &theirs, dir,
            )
            .await?
        }
        "pair_refused" => return Err(pair::explain(&pair::refused(&first), &name)),
        _ => return Err(format!("{name}: expected welcome, got {first}")),
    };
    let id = welcome["id"].as_str().unwrap_or("");
    if !id.replace('-', "").starts_with(&o.key) {
        return Err(format!(
            "another receiver answered on the channel of {name}"
        ));
    }
    name = welcome["name"].as_str().unwrap_or(&name).to_string();
    if !wire::can(&welcome) {
        return Err(format!(
            "{name} cannot connect directly now (no Wi-Fi Direct on it, or an older Luch)"
        ));
    }
    // The TV asks its person, then makes its network.
    say(conn, json!({"step": "asking", "name": name})).await;
    crate::send(&mut wr, &wire::request())
        .await
        .map_err(|e| format!("{name} over Bluetooth: {e}"))?;
    let offer = wait_offer(&mut rd, &mut wr, &name).await?;
    Ok((welcome, offer, wr))
}

/// Listens over Bluetooth for TVs that offer it, as the find ladder does.
async fn listen(ctx: &Ctx, sys: &Connection) -> Result<Vec<Offered>, String> {
    let (tx, _rx) = mpsc::unbounded_channel();
    let looking = Card::looking(&ctx.sender_id);
    let heard = ble::search(sys, &looking, crate::find::BLE_LISTEN, tx).await?;
    Ok(note_offers(&ctx.st, &heard).await)
}

/// Keeps the receivers among `heard` that offer a direct connection (find's Bluetooth rung and `listen`), named as
/// this computer knows them.
pub async fn note_offers(st: &Shared, heard: &[ble::Heard]) -> Vec<Offered> {
    let paired = crate::config_dir()
        .map(|d| pair::pinned(&d))
        .unwrap_or_default();
    let s = st.lock().await;
    let mut out = Vec::new();
    for h in heard {
        let Some(psm) = h.card.psm else { continue };
        let key = h.card.id.clone();
        let ours = |id: &str| id.replace('-', "").starts_with(&key);
        let name = s
            .found
            .values()
            .find(|r| ours(&r.id))
            .map(|r| r.name.clone())
            .or_else(|| {
                paired
                    .iter()
                    .find(|(id, _)| ours(id))
                    .and_then(|(_, v)| v["name"].as_str().map(String::from))
            })
            .unwrap_or_else(|| key.clone());
        let o = Offered {
            key,
            name,
            rssi: h.rssi,
            device: h.device.clone(),
            psm,
        };
        offers::heard(o.clone());
        out.push(o);
    }
    out
}

/// Offers in words, for the find ladder.
pub fn say_offers(offers: &[Offered]) -> String {
    if offers.is_empty() {
        return "no TV nearby offers a direct connection".into();
    }
    let each: Vec<String> = offers
        .iter()
        .map(|o| {
            format!(
                "{} can connect directly: `luchctl direct {}`",
                o.name, o.key
            )
        })
        .collect();
    format!(
        "{} (this computer then leaves its Wi-Fi while showing)",
        each.join("; ")
    )
}

/// The receiver this computer paired with whose id starts with `key`: its pinned certificate.
fn pinned_for(dir: &std::path::Path, key: &str) -> Option<pair::Fingerprint> {
    pair::pinned(dir)
        .keys()
        .find(|id| id.replace('-', "").starts_with(key))
        .and_then(|id| pair::pinned_fp(dir, id))
}

/// Pairing over the Bluetooth channel, as over the TLS port: the code typed here goes to Pair under `key` (the id is
/// not known before the welcome). The welcome after it.
#[allow(clippy::too_many_arguments)]
async fn pair_ble(
    ctx: &Ctx,
    conn: &Connection,
    key: &str,
    name: &str,
    first: Value,
    rd: &mut crate::Reader,
    wr: &mut crate::Writer,
    me: &pair::Fingerprint,
    theirs: &pair::Fingerprint,
    dir: &std::path::Path,
) -> Result<Value, String> {
    let (tx, mut rx) = mpsc::channel(1);
    {
        let mut st = ctx.st.lock().await;
        st.codes.insert(key.into(), tx);
        st.pairing
            .insert(key.into(), crate::pair_state(&first, name));
    }
    say(conn, json!({"step": "pairing", "name": name, "key": key})).await;
    let result = crate::pair_on(&ctx.st, conn, key, name, first, rd, wr, me, theirs, &mut rx).await;
    ctx.st.lock().await.codes.remove(key);
    let (state, out, reply) = match result {
        Ok((w, reply)) => {
            let id = w["id"].as_str().unwrap_or("");
            let pinned = if id.replace('-', "").starts_with(key) {
                pair::pin(dir, id, theirs, w["name"].as_str().unwrap_or(name))
                    .map_err(|e| format!("cannot keep the pairing with {name}: {e}"))
            } else {
                Err(format!(
                    "another receiver answered on the channel of {name}"
                ))
            };
            match pinned {
                Ok(()) => (json!({"step": "paired", "name": name}), Ok(w), reply),
                Err(e) => (
                    json!({"step": "failed", "name": name, "reason": e}),
                    Err(e),
                    reply,
                ),
            }
        }
        Err((why, reply)) => (
            json!({"step": "failed", "name": name, "reason": why}),
            Err(why),
            reply,
        ),
    };
    ctx.st
        .lock()
        .await
        .pairing
        .insert(key.into(), state.clone());
    crate::pair_changed(conn, key, &state).await;
    if let Some(reply) = reply {
        let _ = reply.send(out.as_ref().map(|w| w.to_string()).map_err(String::clone));
    }
    out
}

/// The TV's answer to the request, keeping the channel alive while its person decides.
async fn wait_offer(
    rd: &mut crate::Reader,
    wr: &mut crate::Writer,
    name: &str,
) -> Result<wire::Offer, String> {
    let until = Instant::now() + ASK_WITHIN;
    let mut n = 0u64;
    let lost = |e: std::io::Error| format!("{name} over Bluetooth: {e}");
    loop {
        tokio::select! {
            // fill_buf is cancel-safe; the line is read once something has come.
            _ = rd.fill_buf() => {
                let v = crate::read_msg(&mut *rd)
                    .await
                    .map_err(lost)?
                    .ok_or_else(|| format!("{name} closed the Bluetooth channel"))?;
                match v["t"].as_str().unwrap_or("") {
                    "ping" => crate::send(&mut *wr, &json!({"t": "pong", "n": v["n"]})).await.map_err(lost)?,
                    "bye" => return Err(format!("{name} closed the Bluetooth channel")),
                    _ => {
                        if let Some(answer) = wire::answer(&v, name) {
                            return answer;
                        }
                    }
                }
            }
            _ = tokio::time::sleep(PING_EVERY) => {
                n += 1;
                crate::send(&mut *wr, &json!({"t": "ping", "n": n})).await.map_err(lost)?;
            }
            _ = tokio::time::sleep_until(until) => {
                return Err(format!("nobody answered on {name} in time"));
            }
        }
    }
}

/// Lists the TV at its address on its own network, through the Wi-Fi interface, and connects to it as Connect does.
async fn connect(
    ctx: &Ctx,
    conn: &Connection,
    id: &str,
    name: &str,
    welcome: &Value,
    offer: &wire::Offer,
    joined: &Joined,
) -> Result<(), String> {
    let ifindex = std::ffi::CString::new(joined.iface.as_str())
        .map(|c| unsafe { libc::if_nametoindex(c.as_ptr()) } as i32)
        .unwrap_or(0);
    let r = Receiver {
        id: id.into(),
        name: name.into(),
        model: welcome["model"].as_str().unwrap_or_default().into(),
        address: offer.addr.to_string(),
        ifname: joined.iface.clone(),
        ifindex,
        port: offer.port,
        tls_port: offer.tls_port,
        service: String::new(),
    };
    {
        let mut s = ctx.st.lock().await;
        s.found.insert(id.into(), r);
        s.via.entry(id.into()).or_default().insert("direct");
    }
    crate::changed(conn).await;
    let luch = conn
        .object_server()
        .interface::<_, crate::Luch>(luch_proto::DBUS_PATH)
        .await
        .map_err(|e| e.to_string())?;
    let connected = luch.get().await.connect(id.into(), conn).await;
    connected
        .map(|_| ())
        .map_err(|e| e.description().unwrap_or("could not connect").to_string())
}

/// Goes back when the session over the TV's network has ended.
async fn watch(st: Shared, conn: Connection, id: String) {
    loop {
        tokio::time::sleep(WATCH_EVERY).await;
        let still = NOW
            .lock()
            .unwrap()
            .joined
            .as_ref()
            .is_some_and(|(_, i, _)| *i == id);
        if !still {
            return;
        }
        if !st.lock().await.conns.contains_key(&id) {
            let said = back(&st, &conn).await;
            say(
                &conn,
                json!({"step": "back", "note": format!("the session ended; {said}")}),
            )
            .await;
            return;
        }
    }
}

/// `DirectOff`: ends the session over the TV's network and goes back.
pub async fn off(st: &Shared, conn: &Connection) -> Result<String, String> {
    if NOW.lock().unwrap().joined.is_none() {
        return Err("not on a TV's network".into());
    }
    let said = back(st, conn).await;
    say(conn, json!({"step": "back", "note": said})).await;
    Ok(said)
}

/// Says bye to the TV, leaves its network and goes back to the one before; says what it did.
async fn back(st: &Shared, conn: &Connection) -> String {
    let Some((j, id, addr)) = NOW.lock().unwrap().joined.take() else {
        return "not on a TV's network".into();
    };
    let c = st.lock().await.conns.get(&id).cloned();
    if let Some(c) = c {
        let _ = c.cmd.send(crate::mirror::Cmd::Bye).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    {
        // Its entry points at its own network: it is listed again when it is found on this one.
        let mut s = st.lock().await;
        if s.found
            .get(&id)
            .is_some_and(|r| r.address == addr.to_string() && r.ifname == j.iface)
        {
            s.found.remove(&id);
            s.via.remove(&id);
        }
    }
    crate::changed(conn).await;
    let said = match Connection::system().await {
        Ok(sys) => nm::leave(&NmBus(sys), &j).await,
        Err(e) => {
            format!("could not reach NetworkManager ({e}): choose your network in the Wi-Fi menu")
        }
    };
    if let Ok(dir) = crate::config_dir() {
        let _ = std::fs::remove_file(dir.join(KEPT));
    }
    said
}

/// Joined: kept in memory and on disk, for a luchd stopped meanwhile to go back when it starts.
fn keep(j: &Joined, id: &str, addr: Ipv4Addr) {
    if let Ok(dir) = crate::config_dir() {
        let _ = std::fs::write(dir.join(KEPT), j.to_json().to_string());
    }
    NOW.lock().unwrap().joined = Some((j.clone(), id.into(), addr));
}

/// At start: a TV's network joined by a luchd that stopped before going back is left now.
pub async fn go_back_after_restart() {
    let Ok(dir) = crate::config_dir() else { return };
    let Some(j) = std::fs::read(dir.join(KEPT))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| Joined::from_json(&v))
    else {
        return;
    };
    if let Ok(sys) = Connection::system().await {
        let said = nm::leave(&NmBus(sys), &j).await;
        eprintln!("[luchd] direct: still on '{}' from before: {said}", j.ssid);
    }
    let _ = std::fs::remove_file(dir.join(KEPT));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{read_msg, send, tests::tv::Tv};
    use tokio::sync::oneshot;

    const TV_ID: &str = "0b6f1c2a-0000-4000-8000-000000000001";

    fn heard() -> Offered {
        Offered {
            key: "0b6f1c2a".into(),
            name: "0b6f1c2a".into(),
            rssi: Some(-50),
            device: "/org/bluez/hci0/dev_02_00_00_00_00_01".into(),
            psm: 0x80,
        }
    }

    fn welcome(modes: Value) -> Value {
        json!({"t": "welcome", "v": 1, "id": TV_ID, "name": "Living room TV", "modes": modes})
    }

    async fn dial(addr: std::net::SocketAddr) -> tokio::net::TcpStream {
        tokio::net::TcpStream::connect(addr).await.unwrap()
    }

    /// The Bluetooth conversation against a TV made of Rust, over a stream that is not the TV's TLS port: the hello
    /// says how it came, pairing takes the code typed here under the card's id, the request is kept alive while the
    /// TV asks its person, and its network comes back. Then, paired: TLS alone lets it in, and a TV that cannot or
    /// will not make a network is said so.
    #[tokio::test]
    async fn over_bluetooth_it_pairs_asks_and_gets_the_network() {
        let Ok(conn) = Connection::session().await else {
            eprintln!("no session bus: skipped");
            return;
        };
        let dir = std::env::temp_dir().join(format!("luch-direct-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let me = pair::Identity::load(&dir).unwrap();
        let tv = Tv::new();
        let tv_fp = tv.fp;
        let addr = tv
            .serve(move |hello, mut rd, mut wr, sender| async move {
                assert_eq!(
                    (hello["via"].clone(), hello["want"].clone(), hello["pair"].clone()),
                    (json!("ble"), json!(["direct"]), json!(true))
                );
                send(&mut wr, &json!({"t": "pair", "step": "code", "digits": 6}))
                    .await
                    .unwrap();
                assert!(
                    pair::tests::receiver_side(&mut rd, &mut wr, b"123456", &sender, &tv_fp).await
                );
                send(&mut wr, &welcome(json!(["mirror", "direct"])))
                    .await
                    .unwrap();
                assert_eq!(read_msg(&mut rd).await.unwrap().unwrap(), wire::request());
                // The TV's person thinks; the channel stays alive.
                send(&mut wr, &json!({"t": "ping", "n": 7})).await.unwrap();
                assert_eq!(
                    read_msg(&mut rd).await.unwrap().unwrap(),
                    json!({"t": "pong", "n": 7})
                );
                send(&mut wr, &json!({"t": "direct", "ssid": "DIRECT-xy-Luch", "pass": "s3cret-pass",
                                      "freq": 5180, "addr": "192.168.49.1", "port": 47800, "tls_port": 47802}))
                    .await
                    .unwrap();
                while let Ok(Some(_)) = read_msg(&mut rd).await {}
            })
            .await;
        let ctx = Ctx {
            st: Shared::default(),
            sender_id: "9a1b2c3d-0000-4000-8000-000000000000".into(),
            host: "Linux Laptop".into(),
            ident: None,
        };
        // The person types the code once the TV shows it (Pair, under the id the card gave).
        let st = ctx.st.clone();
        let typed = tokio::spawn(async move {
            let tx = loop {
                if let Some(tx) = st.lock().await.codes.get("0b6f1c2a").cloned() {
                    break tx;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            };
            let (reply, answer) = oneshot::channel();
            tx.send((b"123456".to_vec(), reply)).await.unwrap();
            answer.await.unwrap()
        });
        let (w, offer, _wr) = ask(&ctx, &me, &conn, dial(addr).await, &heard(), &dir)
            .await
            .unwrap();
        assert_eq!(w["name"], "Living room TV");
        assert!(typed.await.unwrap().unwrap().contains("Living room TV"));
        assert_eq!(
            (offer.ssid.as_str(), offer.addr, offer.tls_port),
            ("DIRECT-xy-Luch", Ipv4Addr::new(192, 168, 49, 1), 47802)
        );
        assert_eq!(pair::pinned_fp(&dir, TV_ID), Some(tv_fp));
        assert_eq!(ctx.st.lock().await.pairing["0b6f1c2a"]["step"], "paired");

        // Paired: no code, and a TV with no Wi-Fi Direct is not asked.
        let again = tv
            .serve(|hello, _, mut wr, _| async move {
                assert!(hello.get("pair").is_none());
                send(&mut wr, &welcome(json!(["mirror"]))).await.unwrap();
            })
            .await;
        let e = ask(&ctx, &me, &conn, dial(again).await, &heard(), &dir)
            .await
            .err()
            .expect("refused");
        assert_eq!(
            e,
            "Living room TV cannot connect directly now (no Wi-Fi Direct on it, or an older Luch)"
        );

        // Its person says no.
        let denied = tv
            .serve(|_, mut rd, mut wr, _| async move {
                send(&mut wr, &welcome(json!(["direct"]))).await.unwrap();
                read_msg(&mut rd).await.unwrap();
                send(
                    &mut wr,
                    &json!({"t": "error", "for": "direct", "code": "denied"}),
                )
                .await
                .unwrap();
            })
            .await;
        let e = ask(&ctx, &me, &conn, dial(denied).await, &heard(), &dir)
            .await
            .err()
            .expect("refused");
        assert_eq!(e, "Living room TV refused the direct connection");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn offers_are_said_with_the_command_that_takes_them() {
        assert_eq!(say_offers(&[]), "no TV nearby offers a direct connection");
        let o = Offered {
            name: "Living room TV".into(),
            ..heard()
        };
        assert_eq!(
            say_offers(&[o]),
            "Living room TV can connect directly: `luchctl direct 0b6f1c2a` (this computer then leaves its Wi-Fi \
             while showing)"
        );
    }
}
