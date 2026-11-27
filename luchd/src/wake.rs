//! Turning a receiver on from standby. A receiver that is off is not on the LAN, so luchd remembers every receiver
//! it has talked to (~/.config/luch/known.json) together with the ways it can be woken, and tries each of them:
//!
//! - `wol`: a Wake-on-LAN magic packet to its MAC, learned from the ARP table while it was on. For TVs that keep
//!   their network up in standby (Sony "Remote start", TCL "Network standby", LG webOS, Samsung).
//! - `ble`: the Bluetooth LE advertisement a MediaTek TV remote sends when its power key is pressed (company id
//!   0x0046). XGIMI projectors and others switch Wi-Fi off in standby and wake only on this. Learned from the
//!   remote (`luchctl wake <r> learn`) or set by hand. Its first byte is a counter the remote bumps on every press,
//!   so after the learned advertisement every other counter value is sent as well.
//!
//! Everything stays on the LAN and in Bluetooth range: no cloud service is asked.

use std::{
    collections::HashMap,
    net::{Ipv4Addr, UdpSocket},
    path::{Path, PathBuf},
    time::Duration,
};

use futures_util::StreamExt;
use serde_json::{json, Map, Value};
use zbus::{
    message::Type as MsgType,
    zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value as ZValue},
    Connection, MatchRule, MessageStream,
};

/// MediaTek's Bluetooth company id: the key of the remote's manufacturer data.
pub const MEDIATEK: u16 = 0x0046;
const BLUEZ: &str = "org.bluez";
const ADV_PATH: &str = "/dev/luch/Luch1/wake";
/// How long the learned advertisement is on the air, about one press of the remote (it repeats a press some 50 times
/// in a second). The very same advertisement wakes a MoGo 4 again and again: its counter byte is not checked. A sweep
/// over the other counter values right after it kept the MoGo asleep, as if the presses cancelled each other.
const HOLD: Duration = Duration::from_millis(2000);

fn file() -> std::io::Result<PathBuf> {
    Ok(crate::config_dir()?.join("known.json"))
}

fn load_from(path: &Path) -> Map<String, Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn save_to(path: &Path, all: &Map<String, Value>) {
    let _ = std::fs::write(path, Value::Object(all.clone()).to_string());
}

pub fn load() -> Map<String, Value> {
    file().map(|f| load_from(&f)).unwrap_or_default()
}

/// Notes a receiver luchd has just talked to: its name, model, address and, from the ARP table, its MAC.
pub fn remember(id: &str, name: &str, model: &str, address: &str) {
    let Ok(f) = file() else { return };
    let mac = std::fs::read_to_string("/proc/net/arp")
        .ok()
        .and_then(|t| arp_mac(&t, address));
    let mut all = load_from(&f);
    merge(&mut all, id, name, model, address, mac);
    save_to(&f, &all);
}

fn merge(
    all: &mut Map<String, Value>,
    id: &str,
    name: &str,
    model: &str,
    address: &str,
    mac: Option<String>,
) {
    let r = all.entry(id.to_string()).or_insert_with(|| json!({}));
    r["name"] = json!(name);
    r["model"] = json!(model);
    r["address"] = json!(address);
    if let Some(mac) = mac {
        r["mac"] = json!(mac);
    }
    r["seen"] = json!(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0));
}

/// Sets (or with "" clears) a receiver's `mac` or `ble`, checked first.
pub fn set(id: &str, kind: &str, value: &str) -> Result<(), String> {
    let f = file().map_err(|e| e.to_string())?;
    let mut all = load_from(&f);
    set_in(&mut all, id, kind, value)?;
    save_to(&f, &all);
    Ok(())
}

fn set_in(all: &mut Map<String, Value>, id: &str, kind: &str, value: &str) -> Result<(), String> {
    let normal = match (kind, value) {
        (_, "") => None,
        ("mac", v) => Some(
            parse_mac(v)
                .map(|m| fmt_mac(&m))
                .ok_or(format!("not a MAC address: {v}"))?,
        ),
        ("ble", v) => Some(hex(&parse_hex(v)
            .filter(|b| (2..=26).contains(&b.len()))
            .ok_or(format!(
            "not the hex bytes of an advertisement: {v}"
        ))?)),
        _ => return Err(format!("unknown wake setting {kind} (mac or ble)")),
    };
    let r = all
        .get_mut(id)
        .ok_or(format!("unknown receiver {id}: connect to it once first"))?;
    match normal {
        Some(v) => r[kind] = json!(v),
        None => {
            r.as_object_mut().map(|o| o.remove(kind));
        }
    }
    Ok(())
}

/// The ways a known receiver can be woken.
pub fn methods(r: &Value) -> Vec<&'static str> {
    let mut m = Vec::new();
    if r["mac"].is_string() {
        m.push("wol");
    }
    if r["ble"].is_string() {
        m.push("ble");
    }
    m
}

/// What to tell the person about a receiver that cannot be woken yet.
pub fn hint(r: &Value) -> Option<String> {
    if r["ble"].is_string() {
        return None;
    }
    let model = r["model"].as_str().unwrap_or("").to_ascii_lowercase();
    let off_in_standby = ["xgimi"].iter().any(|m| model.contains(m));
    off_in_standby.then(|| {
        "leaves the network in standby: turn it off, run `luchctl wake <r> learn` and press the remote's power key"
            .into()
    })
}

/// Wakes a receiver every way it knows; returns the ways tried.
pub async fn wake(r: &Value) -> Result<Vec<&'static str>, String> {
    let mut tried = Vec::new();
    let mut errors = Vec::new();
    if let Some(mac) = r["mac"].as_str().and_then(parse_mac) {
        match send_wol(&mac, r["address"].as_str()) {
            Ok(()) => tried.push("wol"),
            Err(e) => errors.push(format!("Wake-on-LAN: {e}")),
        }
    }
    if let Some(adv) = r["ble"].as_str().and_then(parse_hex) {
        let sys = Connection::system().await.map_err(|e| e.to_string());
        let on_air = match sys {
            Ok(sys) => advertise_on(&sys, &adv, HOLD).await,
            Err(e) => Err(e),
        };
        match on_air {
            Ok(()) => tried.push("ble"),
            Err(e) => errors.push(format!("Bluetooth: {e}")),
        }
    }
    if tried.is_empty() {
        return Err(if errors.is_empty() {
            hint(r).unwrap_or_else(|| {
                "no way to wake it known: connect to it once while it is on (for its MAC), or `luchctl wake <r> learn`"
                    .into()
            })
        } else {
            errors.join("; ")
        });
    }
    for e in errors {
        eprintln!("[luchd] wake: {e}");
    }
    Ok(tried)
}

// ---------- Wake-on-LAN ----------

pub fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let parts: Vec<_> = s.trim().split([':', '-']).collect();
    let mut mac = [0u8; 6];
    if parts.len() != 6 {
        return None;
    }
    for (b, p) in mac.iter_mut().zip(parts) {
        if p.len() != 2 {
            return None;
        }
        *b = u8::from_str_radix(p, 16).ok()?;
    }
    (mac != [0; 6] && mac != [0xff; 6]).then_some(mac)
}

fn fmt_mac(m: &[u8; 6]) -> String {
    m.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// The MAC of `ip` in /proc/net/arp, if the entry is complete.
fn arp_mac(table: &str, ip: &str) -> Option<String> {
    table.lines().skip(1).find_map(|l| {
        let f: Vec<_> = l.split_whitespace().collect();
        (f.len() >= 4 && f[0] == ip && f[2] != "0x0")
            .then(|| parse_mac(f[3]).map(|m| fmt_mac(&m)))
            .flatten()
    })
}

fn magic_packet(mac: &[u8; 6]) -> Vec<u8> {
    let mut p = vec![0xff; 6];
    for _ in 0..16 {
        p.extend_from_slice(mac);
    }
    p
}

/// To the LAN's broadcast and, when the last address is known, to its /24 broadcast too (another interface).
fn send_wol(mac: &[u8; 6], address: Option<&str>) -> std::io::Result<()> {
    let s = UdpSocket::bind("0.0.0.0:0")?;
    s.set_broadcast(true)?;
    let packet = magic_packet(mac);
    let mut targets = vec![Ipv4Addr::BROADCAST];
    if let Some(ip) = address.and_then(|a| a.parse::<Ipv4Addr>().ok()) {
        let o = ip.octets();
        targets.push(Ipv4Addr::new(o[0], o[1], o[2], 255));
    }
    let mut sent = false;
    for t in targets {
        for port in [9, 7] {
            sent |= s.send_to(&packet, (t, port)).is_ok();
        }
    }
    if sent {
        Ok(())
    } else {
        Err(std::io::Error::other("no route for a broadcast"))
    }
}

// ---------- Bluetooth LE ----------

pub fn parse_hex(s: &str) -> Option<Vec<u8>> {
    let s: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ':')
        .collect();
    let s = s.strip_prefix("0x").unwrap_or(&s);
    if s.is_empty() || !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

/// Our advertisement, as BlueZ reads it (org.bluez.LEAdvertisement1). It copies the whole packet of the remote,
/// not only its MediaTek data: a MoGo 4 in standby ignored a non-connectable advertisement with that data alone,
/// and woke to this one (connectable, discoverable, a HID keyboard, as the remote's own ADV_IND).
struct Advertisement {
    data: Vec<u8>,
}

/// GAP appearance of the remote: a keyboard.
const KEYBOARD: u16 = 0x03c1;

#[zbus::interface(name = "org.bluez.LEAdvertisement1")]
impl Advertisement {
    fn release(&self) {}
    #[zbus(property, name = "Type")]
    fn kind(&self) -> String {
        "peripheral".into()
    }
    #[zbus(property, name = "ServiceUUIDs")]
    fn service_uuids(&self) -> Vec<String> {
        vec!["1812".into()] // Human Interface Device
    }
    #[zbus(property)]
    fn appearance(&self) -> u16 {
        KEYBOARD
    }
    #[zbus(property)]
    fn discoverable(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn manufacturer_data(&self) -> HashMap<u16, OwnedValue> {
        let mut m = HashMap::new();
        if let Ok(v) = OwnedValue::try_from(ZValue::from(self.data.clone())) {
            m.insert(MEDIATEK, v);
        }
        m
    }
    #[zbus(property)]
    fn min_interval(&self) -> u32 {
        20
    }
    #[zbus(property)]
    fn max_interval(&self) -> u32 {
        30
    }
}

type Objects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

/// The first powered adapter offering `iface`.
async fn adapter(sys: &Connection, iface: &str) -> Result<OwnedObjectPath, String> {
    let objects: Objects = sys
        .call_method(
            Some(BLUEZ),
            "/",
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        )
        .await
        .map_err(|e| format!("BlueZ is not running ({e})"))?
        .body()
        .deserialize()
        .map_err(|e| e.to_string())?;
    let mut found_off = false;
    let mut paths: Vec<_> = objects
        .iter()
        .filter(|(_, ifs)| ifs.contains_key(iface))
        .filter(|(_, ifs)| {
            let on = ifs
                .get("org.bluez.Adapter1")
                .and_then(|a| a.get("Powered"))
                .and_then(|p| bool::try_from(p).ok())
                .unwrap_or(false);
            found_off |= !on;
            on
        })
        .map(|(p, _)| p.clone())
        .collect();
    paths.sort_by(|a, b| a.as_str().cmp(b.as_str())); // hci0 first
    paths.into_iter().next().ok_or_else(|| {
        if found_off {
            "Bluetooth is off".into()
        } else {
            "no Bluetooth adapter that can advertise".into()
        }
    })
}

/// Puts each advertisement on the air in turn through one registration: the first for `first_hold`, each other one
/// for `each`.
async fn advertise_on(sys: &Connection, adv: &[u8], hold: Duration) -> Result<(), String> {
    let adapter = adapter(sys, "org.bluez.LEAdvertisingManager1").await?;
    let server = sys.object_server();
    server
        .at(ADV_PATH, Advertisement { data: adv.to_vec() })
        .await
        .map_err(|e| e.to_string())?;
    let path = ObjectPath::try_from(ADV_PATH).map_err(|e| e.to_string())?;
    let mgr = "org.bluez.LEAdvertisingManager1";
    let registered = sys
        .call_method(
            Some(BLUEZ),
            &adapter,
            Some(mgr),
            "RegisterAdvertisement",
            &(&path, HashMap::<String, OwnedValue>::new()),
        )
        .await;
    let result = match registered {
        Err(e) => Err(format!("cannot advertise: {e}")),
        Ok(_) => {
            tokio::time::sleep(hold).await;
            let _ = sys
                .call_method(
                    Some(BLUEZ),
                    &adapter,
                    Some(mgr),
                    "UnregisterAdvertisement",
                    &(&path,),
                )
                .await;
            Ok(())
        }
    };
    let _ = server.remove::<Advertisement, _>(ADV_PATH).await;
    result
}

/// The bytes of a MediaTek manufacturer-data entry in a BlueZ property map, if there is one.
fn mediatek_data(props: &HashMap<String, OwnedValue>) -> Option<Vec<u8>> {
    let md = props.get("ManufacturerData")?;
    let ZValue::Dict(d) = &**md else { return None };
    let v: OwnedValue = d.get(&MEDIATEK).ok()??;
    bytes(&v)
}

fn bytes(v: &ZValue) -> Option<Vec<u8>> {
    match v {
        ZValue::Value(inner) => bytes(inner),
        ZValue::Array(a) => a
            .iter()
            .map(|x| match x {
                ZValue::U8(b) => Some(*b),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

/// Listens for `secs` seconds for a remote's power-key advertisement and returns it.
pub async fn learn(secs: u64) -> Result<Vec<u8>, String> {
    let sys = Connection::system().await.map_err(|e| e.to_string())?;
    learn_on(&sys, secs).await
}

async fn learn_on(sys: &Connection, secs: u64) -> Result<Vec<u8>, String> {
    let adapter = adapter(sys, "org.bluez.Adapter1").await?;
    let rule = |iface: &'static str, member: &'static str| {
        MatchRule::builder()
            .msg_type(MsgType::Signal)
            .sender(BLUEZ)
            .and_then(|b| b.interface(iface))
            .and_then(|b| b.member(member))
            .map(|b| b.build())
            .map_err(|e| e.to_string())
    };
    let added = MessageStream::for_match_rule(
        rule("org.freedesktop.DBus.ObjectManager", "InterfacesAdded")?,
        sys,
        None,
    )
    .await
    .map_err(|e| e.to_string())?;
    let changed = MessageStream::for_match_rule(
        rule("org.freedesktop.DBus.Properties", "PropertiesChanged")?,
        sys,
        None,
    )
    .await
    .map_err(|e| e.to_string())?;
    let a1 = Some("org.bluez.Adapter1");
    let mut filter: HashMap<&str, ZValue> = HashMap::new();
    filter.insert("Transport", ZValue::from("le"));
    filter.insert("DuplicateData", ZValue::from(true));
    sys.call_method(Some(BLUEZ), &adapter, a1, "SetDiscoveryFilter", &(filter,))
        .await
        .map_err(|e| e.to_string())?;
    sys.call_method(Some(BLUEZ), &adapter, a1, "StartDiscovery", &())
        .await
        .map_err(|e| format!("cannot scan: {e}"))?;
    let mut all = futures_util::stream::select(added, changed);
    let found = tokio::time::timeout(Duration::from_secs(secs), async {
        while let Some(Ok(m)) = all.next().await {
            let body = m.body();
            let props = if m.header().member().is_some_and(|n| n == "InterfacesAdded") {
                body.deserialize::<(
                    OwnedObjectPath,
                    HashMap<String, HashMap<String, OwnedValue>>,
                )>()
                .ok()
                .and_then(|(_, mut ifs)| ifs.remove("org.bluez.Device1"))
            } else {
                body.deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>()
                    .ok()
                    .filter(|(i, _, _)| i == "org.bluez.Device1")
                    .map(|(_, p, _)| p)
            };
            if let Some(adv) = props.as_ref().and_then(mediatek_data) {
                if adv.len() >= 8 {
                    return Some(adv);
                }
            }
        }
        None
    })
    .await;
    let _ = sys
        .call_method(Some(BLUEZ), &adapter, a1, "StopDiscovery", &())
        .await;
    found.ok().flatten().ok_or_else(|| {
        format!("no remote heard in {secs} s: turn the receiver off, then press the remote's power key near this computer")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macs_parse_and_print() {
        assert_eq!(
            parse_mac("AA-bb-01-02-03-04").map(|m| fmt_mac(&m)),
            Some("aa:bb:01:02:03:04".into())
        );
        assert!(parse_mac("00:00:00:00:00:00").is_none());
        assert!(parse_mac("aa:bb:cc").is_none());
    }

    #[test]
    fn the_mac_comes_from_a_complete_arp_entry() {
        let t = "IP address       HW type     Flags       HW address            Mask     Device\n\
                 192.168.1.20     0x1         0x2         aa:bb:cc:dd:ee:01     *        wlan0\n\
                 192.168.1.21     0x1         0x0         00:00:00:00:00:00     *        wlan0\n";
        assert_eq!(
            arp_mac(t, "192.168.1.20").as_deref(),
            Some("aa:bb:cc:dd:ee:01")
        );
        assert_eq!(arp_mac(t, "192.168.1.21"), None);
        assert_eq!(arp_mac(t, "192.168.1.2"), None);
    }

    #[test]
    fn a_magic_packet_is_six_ff_and_sixteen_macs() {
        let p = magic_packet(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(p.len(), 102);
        assert_eq!(&p[..6], &[0xff; 6]);
        assert_eq!(&p[96..], &[1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn settings_are_checked_and_kept_per_receiver() {
        let mut all = Map::new();
        let id = "3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47";
        assert!(set_in(&mut all, id, "mac", "aa:bb:cc:dd:ee:ff").is_err());
        merge(
            &mut all,
            id,
            "Living room TV",
            "XGIMI WK03K",
            "192.168.1.20",
            None,
        );
        assert_eq!(hint(&all[id]).is_some(), true);
        assert!(methods(&all[id]).is_empty());
        assert!(set_in(&mut all, id, "ble", "zz").is_err());
        assert!(set_in(&mut all, id, "ble", "e71").is_err(), "odd hex");
        set_in(&mut all, id, "ble", "E7129730 35F278FF FFFF3043 524B544D").unwrap();
        set_in(&mut all, id, "mac", "AA-BB-CC-DD-EE-FF").unwrap();
        assert_eq!(all[id]["ble"], "e712973035f278ffffff3043524b544d");
        assert_eq!(all[id]["mac"], "aa:bb:cc:dd:ee:ff");
        assert_eq!(methods(&all[id]), vec!["wol", "ble"]);
        assert!(hint(&all[id]).is_none());
        // A later connection updates the address but keeps what was learned.
        merge(
            &mut all,
            id,
            "Living room TV",
            "XGIMI WK03K",
            "192.168.1.30",
            None,
        );
        assert_eq!(all[id]["address"], "192.168.1.30");
        assert_eq!(methods(&all[id]), vec!["wol", "ble"]);
        set_in(&mut all, id, "mac", "").unwrap();
        assert_eq!(methods(&all[id]), vec!["ble"]);
    }

    #[test]
    fn known_receivers_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("luch-known-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("known.json");
        let mut all = Map::new();
        merge(
            &mut all,
            "a",
            "TV",
            "",
            "192.168.1.5",
            Some("aa:bb:cc:dd:ee:ff".into()),
        );
        save_to(&f, &all);
        assert_eq!(load_from(&f)["a"]["mac"], "aa:bb:cc:dd:ee:ff");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn manufacturer_data_is_read_from_a_bluez_property_map() {
        let mut md: HashMap<u16, OwnedValue> = HashMap::new();
        md.insert(
            MEDIATEK,
            OwnedValue::try_from(ZValue::new(ZValue::from(vec![0x7eu8, 0x80, 0x89]))).unwrap(),
        );
        md.insert(
            0x004c,
            OwnedValue::try_from(ZValue::from(vec![1u8])).unwrap(),
        );
        let mut props = HashMap::new();
        props.insert(
            "ManufacturerData".to_string(),
            OwnedValue::try_from(ZValue::from(md)).unwrap(),
        );
        assert_eq!(mediatek_data(&props), Some(vec![0x7e, 0x80, 0x89]));
    }

    /// A stand-in for BlueZ on the session bus: one powered adapter that takes advertisements and, when asked to
    /// scan, hears an unrelated device and then a remote's power key.
    struct MockAdapter;

    #[zbus::interface(name = "org.bluez.Adapter1")]
    impl MockAdapter {
        #[zbus(property)]
        fn powered(&self) -> bool {
            true
        }
        fn set_discovery_filter(&self, _filter: HashMap<String, OwnedValue>) {}
        fn start_discovery(&self, #[zbus(connection)] c: &Connection) {
            let c = c.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                for (company, data) in [
                    (0x004cu16, vec![1u8, 2, 3, 4, 5, 6, 7, 8, 9]),
                    (MEDIATEK, parse_hex(REMOTE).unwrap()),
                ] {
                    let mut md: HashMap<u16, OwnedValue> = HashMap::new();
                    md.insert(company, OwnedValue::try_from(ZValue::from(data)).unwrap());
                    let mut props: HashMap<String, OwnedValue> = HashMap::new();
                    props.insert(
                        "ManufacturerData".into(),
                        OwnedValue::try_from(ZValue::from(md)).unwrap(),
                    );
                    c.emit_signal(
                        None::<zbus::names::BusName>,
                        "/org/bluez/hci0/dev_1C_F3_02_A8_97_57",
                        "org.freedesktop.DBus.Properties",
                        "PropertiesChanged",
                        &("org.bluez.Device1", props, Vec::<String>::new()),
                    )
                    .await
                    .unwrap();
                }
            });
        }
        fn stop_discovery(&self) {}
    }

    #[derive(Default)]
    struct Seen {
        kind: String,
        appearance: Option<u16>,
        first: Option<u8>,
        unregistered: bool,
    }

    struct MockAdvertising(std::sync::Arc<std::sync::Mutex<Seen>>);

    #[zbus::interface(name = "org.bluez.LEAdvertisingManager1")]
    impl MockAdvertising {
        async fn register_advertisement(
            &self,
            path: OwnedObjectPath,
            _options: HashMap<String, OwnedValue>,
            #[zbus(header)] h: zbus::message::Header<'_>,
            #[zbus(connection)] c: &Connection,
        ) -> zbus::fdo::Result<()> {
            // Like BlueZ: read the advertisement from the client that registered it.
            let sender = h.sender().unwrap().to_owned();
            let reply = c
                .call_method(
                    Some(sender),
                    &path,
                    Some("org.freedesktop.DBus.Properties"),
                    "GetAll",
                    &("org.bluez.LEAdvertisement1",),
                )
                .await?;
            let props: HashMap<String, OwnedValue> = reply.body().deserialize()?;
            let mut seen = self.0.lock().unwrap();
            seen.kind = props
                .get("Type")
                .and_then(|v| String::try_from(v.try_clone().unwrap()).ok())
                .unwrap_or_default();
            seen.appearance = props
                .get("Appearance")
                .and_then(|v| u16::try_from(v.try_clone().unwrap()).ok());
            seen.first = mediatek_data(&props).map(|d| d[0]);
            Ok(())
        }
        fn unregister_advertisement(&self, _path: OwnedObjectPath) {
            self.0.lock().unwrap().unregistered = true;
        }
    }

    const REMOTE: &str = "7e8089b6a22f1cffffff3043524b544d";

    #[tokio::test]
    async fn bluetooth_wake_and_learning_through_bluez() {
        let Ok(client) = Connection::session().await else {
            eprintln!("no session bus: skipped");
            return;
        };
        let _one_bluez = crate::FAKE_BLUEZ.lock().await;
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Seen::default()));
        let bluez = zbus::connection::Builder::session()
            .unwrap()
            .name(BLUEZ)
            .unwrap()
            .serve_at("/", zbus::fdo::ObjectManager)
            .unwrap()
            .serve_at("/org/bluez/hci0", MockAdapter)
            .unwrap()
            .serve_at("/org/bluez/hci0", MockAdvertising(seen.clone()))
            .unwrap()
            .build()
            .await
            .unwrap();

        // Any change of the advertisement while it is on the air, as BlueZ would follow it: there must be none.
        let rule = MatchRule::builder()
            .msg_type(MsgType::Signal)
            .interface("org.freedesktop.DBus.Properties")
            .unwrap()
            .member("PropertiesChanged")
            .unwrap()
            .path(ADV_PATH)
            .unwrap()
            .build();
        let mut changes = MessageStream::for_match_rule(rule, &bluez, None)
            .await
            .unwrap();
        let counters = tokio::spawn(async move {
            let mut out = Vec::new();
            while let Ok(Some(Ok(m))) =
                tokio::time::timeout(Duration::from_secs(2), changes.next()).await
            {
                let (_, props, _): (String, HashMap<String, OwnedValue>, Vec<String>) =
                    m.body().deserialize().unwrap();
                out.push(mediatek_data(&props).unwrap()[0]);
            }
            out
        });

        let learned = parse_hex(REMOTE).unwrap();
        advertise_on(&client, &learned, Duration::from_millis(20))
            .await
            .unwrap();
        let counters = counters.await.unwrap();
        {
            let seen = seen.lock().unwrap();
            assert_eq!(seen.kind, "peripheral");
            assert_eq!(seen.appearance, Some(KEYBOARD));
            assert_eq!(seen.first, Some(0x7e), "the learned advertisement as it is");
            assert!(seen.unregistered);
        }
        assert!(counters.is_empty(), "no other counter value: {counters:?}");

        // Learning skips the unrelated device and keeps the remote's advertisement.
        assert_eq!(learn_on(&client, 5).await.unwrap(), learned);
        drop(bluez);
    }
}
