//! Bluetooth LE (PROTOCOL.md 1c): a receiver advertises its card, so a sender in the same room finds it when the
//! Wi-Fi keeps devices apart or drops multicast; the signal strength puts the nearest TV first. The sender
//! advertises "looking for Luch" meanwhile, so a TV may offer itself.
//!
//! Through BlueZ on the system bus, as wake.rs does; the functions take the connection so tests can hand them a
//! fake BlueZ on a private bus.

use std::{collections::HashMap, time::Duration};

use futures_util::StreamExt;
use zbus::{
    message::Type as MsgType,
    zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value as ZValue},
    Connection, MatchRule, MessageStream,
};

use super::card::{Card, RECEIVER};

/// The manufacturer-data key of the card: 0xFFFF is the company id the Bluetooth SIG keeps for tests and for
/// projects without one of their own; the card's "Lu" magic tells it apart from others using it.
pub const COMPANY: u16 = 0xffff;
const BLUEZ: &str = "org.bluez";
const LOOKING_PATH: &str = "/dev/luch/Luch1/looking";

/// A receiver heard over the air.
#[derive(Clone, Debug, PartialEq)]
pub struct Heard {
    pub card: Card,
    /// dBm; higher is nearer.
    pub rssi: Option<i16>,
    /// The BlueZ device object it came from.
    pub device: String,
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

/// The card in a BlueZ Device1 property map, if its manufacturer data holds one.
pub fn card_in(props: &HashMap<String, OwnedValue>) -> Option<Card> {
    let md = props.get("ManufacturerData")?;
    let ZValue::Dict(d) = &**md else { return None };
    let v: OwnedValue = d.get(&COMPANY).ok()??;
    Card::decode_ble(&bytes(&v)?)
}

pub fn rssi_in(props: &HashMap<String, OwnedValue>) -> Option<i16> {
    props.get("RSSI").and_then(|v| i16::try_from(v).ok())
}

/// A heard device's Bluetooth address and its type ("public" or "random"), for a connection to it (direct.rs).
pub async fn address(sys: &Connection, device: &str) -> Result<(String, String), String> {
    let props: HashMap<String, OwnedValue> = sys
        .call_method(
            Some(BLUEZ),
            device,
            Some("org.freedesktop.DBus.Properties"),
            "GetAll",
            &("org.bluez.Device1",),
        )
        .await
        .map_err(|e| format!("BlueZ no longer knows the TV ({e}): look for it again"))?
        .body()
        .deserialize()
        .map_err(|e| e.to_string())?;
    let text = |k: &str| {
        props
            .get(k)
            .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
    };
    Ok((
        text("Address").ok_or("BlueZ gave no address")?,
        text("AddressType").unwrap_or_else(|| "public".into()),
    ))
}

/// Receivers by signal strength, nearest first; one per id (the strongest), unknown strength last.
pub fn nearest_first(mut heard: Vec<Heard>) -> Vec<Heard> {
    heard.sort_by_key(|h| std::cmp::Reverse(h.rssi.unwrap_or(i16::MIN)));
    let mut seen = std::collections::HashSet::new();
    heard.retain(|h| seen.insert(h.card.id.clone()));
    heard
}

/// The "looking for Luch" advertisement (org.bluez.LEAdvertisement1): non-connectable, only the card.
struct Looking {
    card: Vec<u8>,
}

#[zbus::interface(name = "org.bluez.LEAdvertisement1")]
impl Looking {
    fn release(&self) {}
    #[zbus(property, name = "Type")]
    fn kind(&self) -> String {
        "broadcast".into()
    }
    #[zbus(property)]
    fn manufacturer_data(&self) -> HashMap<u16, OwnedValue> {
        let mut m = HashMap::new();
        if let Ok(v) = OwnedValue::try_from(ZValue::from(self.card.clone())) {
            m.insert(COMPANY, v);
        }
        m
    }
}

type Objects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

async fn objects(sys: &Connection) -> Result<Objects, String> {
    sys.call_method(
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
    .map_err(|e| e.to_string())
}

/// The first powered adapter, and whether it can advertise.
async fn adapter(sys: &Connection) -> Result<(OwnedObjectPath, bool), String> {
    let all = objects(sys).await?;
    let mut off = false;
    let mut on: Vec<_> = all
        .iter()
        .filter_map(|(p, ifs)| {
            let a = ifs.get("org.bluez.Adapter1")?;
            let powered = a
                .get("Powered")
                .and_then(|v| bool::try_from(v).ok())
                .unwrap_or(false);
            off |= !powered;
            powered.then(|| {
                (
                    p.clone(),
                    ifs.contains_key("org.bluez.LEAdvertisingManager1"),
                )
            })
        })
        .collect();
    on.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    on.into_iter().next().ok_or_else(|| {
        if off {
            "Bluetooth is off"
        } else {
            "no Bluetooth adapter"
        }
        .to_string()
    })
}

/// Advertises `looking` (when the adapter can) and listens for receivers' cards for `secs`; returns them nearest
/// first. Each new receiver also goes to `heard` the moment it is heard, for the list to grow while it listens.
pub async fn search(
    sys: &Connection,
    looking: &Card,
    secs: Duration,
    heard: tokio::sync::mpsc::UnboundedSender<Heard>,
) -> Result<Vec<Heard>, String> {
    let (adapter, can_advertise) = adapter(sys).await?;
    // One stream for both kinds of signal, so a device's RSSI update never overtakes the card it belongs to.
    let rule = MatchRule::builder()
        .msg_type(MsgType::Signal)
        .sender(BLUEZ)
        .map(|b| b.build())
        .map_err(|e| e.to_string())?;
    let mut signals = MessageStream::for_match_rule(rule, sys, None)
        .await
        .map_err(|e| e.to_string())?;

    let server = sys.object_server();
    let path = ObjectPath::try_from(LOOKING_PATH).map_err(|e| e.to_string())?;
    let mgr = Some("org.bluez.LEAdvertisingManager1");
    let mut advertising = false;
    if can_advertise
        && server
            .at(
                LOOKING_PATH,
                Looking {
                    card: looking.encode(),
                },
            )
            .await
            .is_ok()
    {
        advertising = sys
            .call_method(
                Some(BLUEZ),
                &adapter,
                mgr,
                "RegisterAdvertisement",
                &(&path, HashMap::<String, OwnedValue>::new()),
            )
            .await
            .map_err(|e| eprintln!("[luchd] cannot say we are looking over Bluetooth: {e}"))
            .is_ok();
    }

    let a1 = Some("org.bluez.Adapter1");
    let mut filter: HashMap<&str, ZValue> = HashMap::new();
    filter.insert("Transport", ZValue::from("le"));
    // Every advertisement, not only the first per device: the RSSI follows the person moving the laptop.
    filter.insert("DuplicateData", ZValue::from(true));
    let _ = sys
        .call_method(Some(BLUEZ), &adapter, a1, "SetDiscoveryFilter", &(filter,))
        .await;
    let started = sys
        .call_method(Some(BLUEZ), &adapter, a1, "StartDiscovery", &())
        .await
        .map_err(|e| format!("cannot scan: {e}"));

    // Cards BlueZ already knows (heard by an earlier scan) count too; then every advertisement as it comes.
    let mut cards: HashMap<String, Heard> = HashMap::new();
    let note =
        |device: &str, props: &HashMap<String, OwnedValue>, cards: &mut HashMap<String, Heard>| {
            if let Some(card) = card_in(props).filter(|c| c.kind == RECEIVER) {
                let fresh = !cards.contains_key(device);
                let h = Heard {
                    card,
                    rssi: rssi_in(props),
                    device: device.to_string(),
                };
                if fresh {
                    let _ = heard.send(h.clone());
                }
                cards.insert(device.to_string(), h);
            } else if let (Some(h), Some(r)) = (cards.get_mut(device), rssi_in(props)) {
                h.rssi = Some(r);
            }
        };
    if started.is_ok() {
        if let Ok(all) = objects(sys).await {
            for (p, ifs) in &all {
                if let Some(d) = ifs.get("org.bluez.Device1") {
                    note(p.as_str(), d, &mut cards);
                }
            }
        }
        let _ = tokio::time::timeout(secs, async {
            while let Some(Ok(m)) = signals.next().await {
                let body = m.body();
                if m.header().member().is_some_and(|n| n == "InterfacesAdded") {
                    if let Ok((p, mut ifs)) = body.deserialize::<(
                        OwnedObjectPath,
                        HashMap<String, HashMap<String, OwnedValue>>,
                    )>() {
                        if let Some(d) = ifs.remove("org.bluez.Device1") {
                            note(p.as_str(), &d, &mut cards);
                        }
                    }
                } else if let Ok((i, props, _)) =
                    body.deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>()
                {
                    if i == "org.bluez.Device1" {
                        if let Some(p) = m.header().path() {
                            note(p.as_str(), &props, &mut cards);
                        }
                    }
                }
            }
        })
        .await;
        let _ = sys
            .call_method(Some(BLUEZ), &adapter, a1, "StopDiscovery", &())
            .await;
    }
    if advertising {
        let _ = sys
            .call_method(
                Some(BLUEZ),
                &adapter,
                mgr,
                "UnregisterAdvertisement",
                &(&path,),
            )
            .await;
    }
    let _ = server.remove::<Looking, _>(LOOKING_PATH).await;
    started?;
    Ok(nearest_first(cards.into_values().collect()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::Ipv4Addr, sync::Arc};

    fn tv(id: &str, last: u8) -> Card {
        Card {
            kind: RECEIVER,
            version: 0,
            id: id.into(),
            addr: Some((Ipv4Addr::new(192, 168, 1, last), 47800)),
            token: None,
            psm: None,
        }
    }

    fn device_props(company: u16, data: Vec<u8>, rssi: Option<i16>) -> HashMap<String, OwnedValue> {
        let mut md: HashMap<u16, OwnedValue> = HashMap::new();
        md.insert(company, OwnedValue::try_from(ZValue::from(data)).unwrap());
        let mut props = HashMap::new();
        props.insert(
            "ManufacturerData".to_string(),
            OwnedValue::try_from(ZValue::from(md)).unwrap(),
        );
        if let Some(r) = rssi {
            props.insert("RSSI".to_string(), OwnedValue::from(r));
        }
        props
    }

    #[test]
    fn the_nearest_receiver_comes_first_once() {
        let h = |id: &str, rssi| Heard {
            card: tv(id, 20),
            rssi,
            device: id.into(),
        };
        let got = nearest_first(vec![
            h("aaaaaaaa", Some(-80)),
            h("bbbbbbbb", Some(-40)),
            h("cccccccc", None),
            h("aaaaaaaa", Some(-50)),
        ]);
        let ids: Vec<_> = got.iter().map(|h| (h.card.id.as_str(), h.rssi)).collect();
        assert_eq!(
            ids,
            vec![
                ("bbbbbbbb", Some(-40)),
                ("aaaaaaaa", Some(-50)),
                ("cccccccc", None)
            ]
        );
    }

    #[test]
    fn a_card_is_read_from_bluez_properties() {
        let p = device_props(COMPANY, tv("3f0c2a8e", 20).encode(), Some(-61));
        assert_eq!(card_in(&p), Some(tv("3f0c2a8e", 20)));
        assert_eq!(rssi_in(&p), Some(-61));
        // A receiver with no address on this network that offers a direct connection.
        let direct = Card {
            addr: None,
            psm: Some(0x80),
            ..tv("0b6f1c2a", 20)
        };
        assert_eq!(
            card_in(&device_props(COMPANY, direct.encode(), None)),
            Some(direct)
        );
        // Someone else's test data under the same company id is not a card.
        assert_eq!(
            card_in(&device_props(COMPANY, vec![1, 2, 3, 4, 5, 6, 7, 8], None)),
            None
        );
        assert_eq!(
            card_in(&device_props(0x004c, tv("3f0c2a8e", 20).encode(), None)),
            None
        );
    }

    /// BlueZ, faked: one adapter that takes advertisements and, when scanning, hears two TVs, an unrelated device
    /// and another sender, then the far TV coming closer.
    struct Adapter;

    #[zbus::interface(name = "org.bluez.Adapter1")]
    impl Adapter {
        #[zbus(property)]
        fn powered(&self) -> bool {
            true
        }
        fn set_discovery_filter(&self, _filter: HashMap<String, OwnedValue>) {}
        fn start_discovery(&self, #[zbus(connection)] c: &Connection) {
            let c = c.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let devices = [
                    (
                        "/org/bluez/hci0/dev_02_00_00_00_00_01",
                        device_props(COMPANY, tv("aaaaaaaa", 21).encode(), Some(-78)),
                    ),
                    (
                        "/org/bluez/hci0/dev_02_00_00_00_00_02",
                        device_props(COMPANY, tv("bbbbbbbb", 22).encode(), Some(-52)),
                    ),
                    (
                        "/org/bluez/hci0/dev_02_00_00_00_00_03",
                        device_props(0x004c, vec![1, 2, 3], Some(-30)),
                    ),
                    (
                        "/org/bluez/hci0/dev_02_00_00_00_00_04",
                        device_props(COMPANY, Card::looking("dddddddd").encode(), Some(-20)),
                    ),
                ];
                for (path, props) in devices {
                    let mut ifs = HashMap::new();
                    ifs.insert("org.bluez.Device1".to_string(), props);
                    c.emit_signal(
                        None::<zbus::names::BusName>,
                        "/",
                        "org.freedesktop.DBus.ObjectManager",
                        "InterfacesAdded",
                        &(ObjectPath::try_from(path).unwrap(), ifs),
                    )
                    .await
                    .unwrap();
                }
                // The far one, again: only its RSSI changes.
                let mut props: HashMap<String, OwnedValue> = HashMap::new();
                props.insert("RSSI".into(), OwnedValue::from(-35i16));
                c.emit_signal(
                    None::<zbus::names::BusName>,
                    "/org/bluez/hci0/dev_02_00_00_00_00_01",
                    "org.freedesktop.DBus.Properties",
                    "PropertiesChanged",
                    &("org.bluez.Device1", props, Vec::<String>::new()),
                )
                .await
                .unwrap();
            });
        }
        fn stop_discovery(&self) {}
    }

    #[derive(Default)]
    struct Seen {
        kind: String,
        card: Option<Card>,
        unregistered: bool,
    }

    struct Advertising(Arc<std::sync::Mutex<Seen>>);

    #[zbus::interface(name = "org.bluez.LEAdvertisingManager1")]
    impl Advertising {
        async fn register_advertisement(
            &self,
            path: OwnedObjectPath,
            _options: HashMap<String, OwnedValue>,
            #[zbus(header)] h: zbus::message::Header<'_>,
            #[zbus(connection)] c: &Connection,
        ) -> zbus::fdo::Result<()> {
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
            seen.card = card_in(&props);
            Ok(())
        }
        fn unregister_advertisement(&self, _path: OwnedObjectPath) {
            self.0.lock().unwrap().unregistered = true;
        }
    }

    #[tokio::test]
    async fn receivers_are_heard_through_bluez_nearest_first_while_we_say_we_look() {
        let Ok(client) = Connection::session().await else {
            eprintln!("no session bus: skipped");
            return;
        };
        let _one_bluez = crate::FAKE_BLUEZ.lock().await;
        let seen = Arc::new(std::sync::Mutex::new(Seen::default()));
        let bluez = zbus::connection::Builder::session()
            .unwrap()
            .name(BLUEZ)
            .unwrap()
            .serve_at("/", zbus::fdo::ObjectManager)
            .unwrap()
            .serve_at("/org/bluez/hci0", Adapter)
            .unwrap()
            .serve_at("/org/bluez/hci0", Advertising(seen.clone()))
            .unwrap()
            .build()
            .await
            .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let me = Card::looking("9a1b2c3d-0000-4000-8000-000000000000");
        let got = search(&client, &me, Duration::from_millis(600), tx)
            .await
            .unwrap();
        let order: Vec<_> = got.iter().map(|h| (h.card.id.as_str(), h.rssi)).collect();
        assert_eq!(
            order,
            vec![("aaaaaaaa", Some(-35)), ("bbbbbbbb", Some(-52))]
        );
        assert_eq!(
            got[0].card.addr,
            Some((Ipv4Addr::new(192, 168, 1, 21), 47800))
        );
        // Each receiver was passed on as soon as it was heard, once.
        let mut live = Vec::new();
        while let Ok(h) = rx.try_recv() {
            live.push(h.card.id);
        }
        assert_eq!(live, vec!["aaaaaaaa", "bbbbbbbb"]);
        {
            let seen = seen.lock().unwrap();
            assert_eq!(seen.kind, "broadcast");
            assert_eq!(seen.card.as_ref(), Some(&me));
            assert!(seen.unregistered);
        }
        drop(bluez);
    }

    #[tokio::test]
    async fn no_adapter_is_said_plainly() {
        let Ok(client) = Connection::session().await else {
            return;
        };
        let _one_bluez = crate::FAKE_BLUEZ.lock().await;
        let bluez = zbus::connection::Builder::session()
            .unwrap()
            .name(BLUEZ)
            .unwrap()
            .serve_at("/", zbus::fdo::ObjectManager)
            .unwrap()
            .build()
            .await
            .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let e = search(
            &client,
            &Card::looking("9a1b2c3d"),
            Duration::from_millis(50),
            tx,
        )
        .await
        .unwrap_err();
        assert_eq!(e, "no Bluetooth adapter");
        drop(bluez);
    }
}
