//! Joining a receiver's Wi-Fi Direct network as a plain Wi-Fi client through NetworkManager (system bus), and going
//! back to the network this computer was on. The connection made for it lives in NetworkManager's memory only
//! (`persist: volatile`) and is deleted when it goes down: nothing is added to the person's saved networks.
//!
//! The steps go through the `Nm` trait, so the order of joining and going back is tested without NetworkManager.

use std::{collections::HashMap, time::Duration};

use serde_json::{json, Value};
use zbus::{
    zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value as ZValue},
    Connection,
};

/// How long the TV's network may take to show up in a scan (it was made a moment ago).
pub const SCAN_FOR: Duration = Duration::from_secs(20);
/// Association, WPA and DHCP.
pub const JOIN_WITHIN: Duration = Duration::from_secs(40);

/// The Wi-Fi device to use, and the connection it has now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub path: String,
    pub iface: String,
    pub previous: Option<Previous>,
}

/// The network to come back to: its saved connection and its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Previous {
    pub conn: String,
    pub name: String,
}

/// The connection asked of NetworkManager.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wanted {
    pub label: String,
    pub ssid: String,
    pub psk: String,
    pub iface: String,
}

/// On the TV's network: what to undo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Joined {
    pub device: String,
    pub iface: String,
    pub ssid: String,
    /// Our connection (settings) and its activation.
    pub ours: String,
    pub active: String,
    pub previous: Option<Previous>,
}

impl Joined {
    /// Kept on disk while joined, for a luchd that stops meanwhile to go back when it starts again.
    pub fn to_json(&self) -> Value {
        json!({"device": self.device, "iface": self.iface, "ssid": self.ssid, "ours": self.ours,
               "active": self.active,
               "previous": self.previous.as_ref().map(|p| json!({"conn": p.conn, "name": p.name}))})
    }

    pub fn from_json(v: &Value) -> Option<Joined> {
        let s = |v: &Value, k: &str| v[k].as_str().map(String::from);
        Some(Joined {
            device: s(v, "device")?,
            iface: s(v, "iface")?,
            ssid: s(v, "ssid")?,
            ours: s(v, "ours")?,
            active: s(v, "active")?,
            previous: v["previous"].is_object().then(|| Previous {
                conn: s(&v["previous"], "conn").unwrap_or_default(),
                name: s(&v["previous"], "name").unwrap_or_default(),
            }),
        })
    }
}

/// What joining needs of NetworkManager.
pub trait Nm {
    async fn wifi(&self) -> Result<Device, String>;
    /// The access point with `ssid`, scanning for it for at most `within`.
    async fn find_ap(
        &self,
        device: &str,
        ssid: &[u8],
        within: Duration,
    ) -> Result<Option<String>, String>;
    /// Our connection and its activation.
    async fn add_and_activate(
        &self,
        device: &str,
        ap: &str,
        w: &Wanted,
    ) -> Result<(String, String), String>;
    async fn wait_up(&self, active: &str, within: Duration) -> Result<(), String>;
    async fn activate(&self, conn: &str, device: &str) -> Result<(), String>;
    async fn deactivate(&self, active: &str) -> Result<(), String>;
    async fn delete(&self, conn: &str) -> Result<(), String>;
}

/// Joins the TV's network (`label`: the connection's name in NetworkManager); on failure goes back at once.
pub async fn join<N: Nm>(nm: &N, ssid: &str, psk: &str, label: &str) -> Result<Joined, String> {
    let dev = nm.wifi().await?;
    let ap = nm
        .find_ap(&dev.path, ssid.as_bytes(), SCAN_FOR)
        .await?
        .ok_or_else(|| {
            format!("the TV's network '{ssid}' is not heard here: the TV may be too far, or its Wi-Fi off")
        })?;
    let w = Wanted {
        label: label.into(),
        ssid: ssid.into(),
        psk: psk.into(),
        iface: dev.iface.clone(),
    };
    let (ours, active) = nm.add_and_activate(&dev.path, &ap, &w).await?;
    let j = Joined {
        device: dev.path,
        iface: dev.iface,
        ssid: ssid.into(),
        ours,
        active,
        previous: dev.previous,
    };
    if let Err(e) = nm.wait_up(&j.active, JOIN_WITHIN).await {
        let back = leave(nm, &j).await;
        return Err(format!("could not join '{ssid}' ({e}); {back}"));
    }
    Ok(j)
}

/// Leaves the TV's network and goes back to the one before; says what it did.
pub async fn leave<N: Nm>(nm: &N, j: &Joined) -> String {
    let said = match &j.previous {
        // Activating it on the same device takes ours down.
        Some(p) => match nm.activate(&p.conn, &j.device).await {
            Ok(()) => format!("back on '{}'", p.name),
            Err(e) => format!(
                "could not go back to '{}' ({e}): choose it in the Wi-Fi menu",
                p.name
            ),
        },
        None => {
            let _ = nm.deactivate(&j.active).await;
            format!("left '{}'", j.ssid)
        }
    };
    // Gone already when it went down (volatile); deleted here when NetworkManager kept it.
    let _ = nm.delete(&j.ours).await;
    said
}

/// The settings of our connection (a{sa{sv}}): never the default route, so a computer with Ethernet keeps its
/// internet, and the TV's DNS is not used; no IPv6, the TV speaks IPv4.
pub fn settings(w: &Wanted) -> HashMap<&'static str, HashMap<&'static str, ZValue<'static>>> {
    let section = |kv: Vec<(&'static str, ZValue<'static>)>| kv.into_iter().collect();
    HashMap::from([
        (
            "connection",
            section(vec![
                ("id", ZValue::from(w.label.clone())),
                ("type", ZValue::from("802-11-wireless")),
                ("autoconnect", ZValue::from(false)),
                ("interface-name", ZValue::from(w.iface.clone())),
            ]),
        ),
        (
            "802-11-wireless",
            section(vec![
                ("ssid", ZValue::from(w.ssid.as_bytes().to_vec())),
                ("mode", ZValue::from("infrastructure")),
            ]),
        ),
        (
            "802-11-wireless-security",
            section(vec![
                ("key-mgmt", ZValue::from("wpa-psk")),
                ("psk", ZValue::from(w.psk.clone())),
            ]),
        ),
        (
            "ipv4",
            section(vec![
                ("method", ZValue::from("auto")),
                ("never-default", ZValue::from(true)),
                ("ignore-auto-dns", ZValue::from(true)),
            ]),
        ),
        ("ipv6", section(vec![("method", ZValue::from("ignore"))])),
    ])
}

// ---------- NetworkManager on the system bus ----------

const NM: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const DEVICE: &str = "org.freedesktop.NetworkManager.Device";
const WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const ACTIVE: &str = "org.freedesktop.NetworkManager.Connection.Active";
const AP: &str = "org.freedesktop.NetworkManager.AccessPoint";
const SETTINGS_CONN: &str = "org.freedesktop.NetworkManager.Settings.Connection";
const DEVICE_TYPE_WIFI: u32 = 2;
const DEVICE_STATE_ACTIVATED: u32 = 100;
const ACTIVE_ACTIVATED: u32 = 2;
const ACTIVE_DEACTIVATING: u32 = 3;

pub struct NmBus(pub Connection);

fn path(p: &str) -> Result<ObjectPath<'_>, String> {
    ObjectPath::try_from(p).map_err(|e| e.to_string())
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

impl NmBus {
    async fn call<B>(
        &self,
        at: &str,
        iface: &str,
        method: &str,
        body: &B,
    ) -> Result<zbus::Message, String>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        self.0
            .call_method(Some(NM), path(at)?, Some(iface), method, body)
            .await
            .map_err(|e| match e {
                zbus::Error::MethodError(_, Some(d), _) => d,
                e => e.to_string(),
            })
    }

    async fn get(&self, at: &str, iface: &str, name: &str) -> Result<OwnedValue, String> {
        let v: OwnedValue = self
            .call(at, "org.freedesktop.DBus.Properties", "Get", &(iface, name))
            .await?
            .body()
            .deserialize()
            .map_err(|e| e.to_string())?;
        // Get answers a variant: the value is inside it.
        match &*v {
            ZValue::Value(inner) => OwnedValue::try_from(&**inner).map_err(|e| e.to_string()),
            _ => Ok(v),
        }
    }

    async fn get_u32(&self, at: &str, iface: &str, name: &str) -> Result<u32, String> {
        u32::try_from(self.get(at, iface, name).await?).map_err(|e| e.to_string())
    }

    async fn get_str(&self, at: &str, iface: &str, name: &str) -> Result<String, String> {
        String::try_from(self.get(at, iface, name).await?).map_err(|e| e.to_string())
    }

    async fn get_path(&self, at: &str, iface: &str, name: &str) -> Result<String, String> {
        OwnedObjectPath::try_from(self.get(at, iface, name).await?)
            .map(|p| p.to_string())
            .map_err(|e| e.to_string())
    }
}

impl Nm for NmBus {
    async fn wifi(&self) -> Result<Device, String> {
        let devices: Vec<OwnedObjectPath> = self
            .call(NM_PATH, NM, "GetDevices", &())
            .await
            .map_err(|e| {
                format!("NetworkManager is not running ({e}): a direct connection needs it")
            })?
            .body()
            .deserialize()
            .map_err(|e| e.to_string())?;
        // The Wi-Fi device in use first: that is the network to come back to.
        let mut found: Option<(String, String, u32)> = None;
        for d in &devices {
            let d = d.as_str();
            if self.get_u32(d, DEVICE, "DeviceType").await.ok() != Some(DEVICE_TYPE_WIFI) {
                continue;
            }
            let state = self.get_u32(d, DEVICE, "State").await.unwrap_or(0);
            let iface = self
                .get_str(d, DEVICE, "Interface")
                .await
                .unwrap_or_default();
            if found.as_ref().is_none_or(|f| f.2 != DEVICE_STATE_ACTIVATED) {
                found = Some((d.to_string(), iface, state));
            }
        }
        let (path, iface, _) =
            found.ok_or("this computer has no Wi-Fi that NetworkManager manages")?;
        let active = self
            .get_path(&path, DEVICE, "ActiveConnection")
            .await
            .unwrap_or_default();
        let previous = if active.is_empty() || active == "/" {
            None
        } else {
            Some(Previous {
                conn: self.get_path(&active, ACTIVE, "Connection").await?,
                name: self
                    .get_str(&active, ACTIVE, "Id")
                    .await
                    .unwrap_or_default(),
            })
        };
        Ok(Device {
            path,
            iface,
            previous,
        })
    }

    async fn find_ap(
        &self,
        device: &str,
        ssid: &[u8],
        within: Duration,
    ) -> Result<Option<String>, String> {
        let until = tokio::time::Instant::now() + within;
        for round in 0u32.. {
            // A scan for that name (refused while one runs, which is as good); again every few seconds.
            if round % 5 == 0 {
                let opts: HashMap<&str, ZValue> =
                    HashMap::from([("ssids", ZValue::from(vec![ssid.to_vec()]))]);
                let _ = self.call(device, WIRELESS, "RequestScan", &(opts,)).await;
            }
            let aps: Vec<OwnedObjectPath> = self
                .call(device, WIRELESS, "GetAllAccessPoints", &())
                .await?
                .body()
                .deserialize()
                .map_err(|e| e.to_string())?;
            for ap in &aps {
                let name = self.get(ap.as_str(), AP, "Ssid").await.ok();
                if name.as_deref().and_then(bytes).as_deref() == Some(ssid) {
                    return Ok(Some(ap.to_string()));
                }
            }
            if tokio::time::Instant::now() >= until {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Ok(None)
    }

    async fn add_and_activate(
        &self,
        device: &str,
        ap: &str,
        w: &Wanted,
    ) -> Result<(String, String), String> {
        let opts: HashMap<&str, ZValue> = HashMap::from([("persist", ZValue::from("volatile"))]);
        let (conn, active, _): (
            OwnedObjectPath,
            OwnedObjectPath,
            HashMap<String, OwnedValue>,
        ) = self
            .call(
                NM_PATH,
                NM,
                "AddAndActivateConnection2",
                &(settings(w), path(device)?, path(ap)?, opts),
            )
            .await
            .map_err(|e| format!("NetworkManager refused the TV's network: {e}"))?
            .body()
            .deserialize()
            .map_err(|e| e.to_string())?;
        Ok((conn.to_string(), active.to_string()))
    }

    async fn wait_up(&self, active: &str, within: Duration) -> Result<(), String> {
        let until = tokio::time::Instant::now() + within;
        loop {
            match self.get_u32(active, ACTIVE, "State").await {
                Ok(ACTIVE_ACTIVATED) => return Ok(()),
                // Gone, or going: a wrong passphrase, or the TV's network went away.
                Ok(s) if s >= ACTIVE_DEACTIVATING => {
                    return Err("the network refused or dropped this computer".into())
                }
                Err(_) => return Err("the network refused or dropped this computer".into()),
                Ok(_) => {}
            }
            if tokio::time::Instant::now() >= until {
                return Err("timed out".into());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    async fn activate(&self, conn: &str, device: &str) -> Result<(), String> {
        self.call(
            NM_PATH,
            NM,
            "ActivateConnection",
            &(path(conn)?, path(device)?, path("/")?),
        )
        .await
        .map(|_| ())
    }

    async fn deactivate(&self, active: &str) -> Result<(), String> {
        self.call(NM_PATH, NM, "DeactivateConnection", &(path(active)?,))
            .await
            .map(|_| ())
    }

    async fn delete(&self, conn: &str) -> Result<(), String> {
        self.call(conn, SETTINGS_CONN, "Delete", &())
            .await
            .map(|_| ())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;

    /// NetworkManager, faked: one Wi-Fi device on `previous` (or on nothing), the TV's network heard or not,
    /// joining that works or not. Every step is written down.
    #[derive(Default)]
    pub(crate) struct FakeNm {
        pub previous: Option<Previous>,
        pub hears_tv: bool,
        pub join_fails: bool,
        pub calls: Mutex<Vec<String>>,
        pub wanted: Mutex<Option<Wanted>>,
    }

    impl FakeNm {
        fn note(&self, s: String) {
            self.calls.lock().unwrap().push(s);
        }
        pub fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Nm for FakeNm {
        async fn wifi(&self) -> Result<Device, String> {
            self.note("wifi".into());
            Ok(Device {
                path: "/dev/1".into(),
                iface: "wlan0".into(),
                previous: self.previous.clone(),
            })
        }
        async fn find_ap(
            &self,
            _d: &str,
            ssid: &[u8],
            _w: Duration,
        ) -> Result<Option<String>, String> {
            self.note(format!("scan {}", String::from_utf8_lossy(ssid)));
            Ok(self.hears_tv.then(|| "/ap/7".into()))
        }
        async fn add_and_activate(
            &self,
            d: &str,
            ap: &str,
            w: &Wanted,
        ) -> Result<(String, String), String> {
            self.note(format!("add {d} {ap}"));
            *self.wanted.lock().unwrap() = Some(w.clone());
            Ok(("/settings/9".into(), "/active/9".into()))
        }
        async fn wait_up(&self, a: &str, _w: Duration) -> Result<(), String> {
            self.note(format!("wait {a}"));
            if self.join_fails {
                Err("the network refused or dropped this computer".into())
            } else {
                Ok(())
            }
        }
        async fn activate(&self, c: &str, d: &str) -> Result<(), String> {
            self.note(format!("activate {c} {d}"));
            Ok(())
        }
        async fn deactivate(&self, a: &str) -> Result<(), String> {
            self.note(format!("deactivate {a}"));
            Ok(())
        }
        async fn delete(&self, c: &str) -> Result<(), String> {
            self.note(format!("delete {c}"));
            Ok(())
        }
    }

    fn home() -> Option<Previous> {
        Some(Previous {
            conn: "/settings/1".into(),
            name: "Home".into(),
        })
    }

    #[tokio::test]
    async fn it_joins_and_goes_back_to_the_network_before() {
        let nm = FakeNm {
            previous: home(),
            hears_tv: true,
            ..Default::default()
        };
        let j = join(&nm, "DIRECT-xy-Luch", "s3cret-pass", "Luch: Living room TV")
            .await
            .unwrap();
        assert_eq!(
            nm.wanted.lock().unwrap().clone().unwrap(),
            Wanted {
                label: "Luch: Living room TV".into(),
                ssid: "DIRECT-xy-Luch".into(),
                psk: "s3cret-pass".into(),
                iface: "wlan0".into(),
            }
        );
        assert_eq!((j.iface.as_str(), j.previous.clone()), ("wlan0", home()));
        // Kept on disk and read back the same.
        assert_eq!(Joined::from_json(&j.to_json()), Some(j.clone()));
        assert_eq!(leave(&nm, &j).await, "back on 'Home'");
        assert_eq!(
            nm.calls(),
            vec![
                "wifi",
                "scan DIRECT-xy-Luch",
                "add /dev/1 /ap/7",
                "wait /active/9",
                "activate /settings/1 /dev/1",
                "delete /settings/9"
            ]
        );
    }

    #[tokio::test]
    async fn with_no_network_before_it_only_leaves() {
        let nm = FakeNm {
            hears_tv: true,
            ..Default::default()
        };
        let j = join(&nm, "DIRECT-ab", "12345678", "Luch").await.unwrap();
        assert_eq!(Joined::from_json(&j.to_json()), Some(j.clone()));
        assert_eq!(leave(&nm, &j).await, "left 'DIRECT-ab'");
        assert_eq!(
            &nm.calls()[4..],
            ["deactivate /active/9", "delete /settings/9"]
        );
    }

    #[tokio::test]
    async fn a_network_not_heard_changes_nothing() {
        let nm = FakeNm {
            previous: home(),
            ..Default::default()
        };
        let e = join(&nm, "DIRECT-ab", "12345678", "Luch")
            .await
            .unwrap_err();
        assert!(e.contains("not heard here"), "{e}");
        assert_eq!(nm.calls(), vec!["wifi", "scan DIRECT-ab"]);
    }

    #[tokio::test]
    async fn a_join_that_fails_goes_back_at_once() {
        let nm = FakeNm {
            previous: home(),
            hears_tv: true,
            join_fails: true,
            ..Default::default()
        };
        let e = join(&nm, "DIRECT-ab", "12345678", "Luch")
            .await
            .unwrap_err();
        assert_eq!(
            e,
            "could not join 'DIRECT-ab' (the network refused or dropped this computer); back on 'Home'"
        );
        assert_eq!(
            &nm.calls()[3..],
            [
                "wait /active/9",
                "activate /settings/1 /dev/1",
                "delete /settings/9"
            ]
        );
    }

    #[test]
    fn our_connection_is_never_the_default_route_nor_saved_to_autoconnect() {
        let s = settings(&Wanted {
            label: "Luch: Living room TV".into(),
            ssid: "DIRECT-xy".into(),
            psk: "12345678".into(),
            iface: "wlan0".into(),
        });
        assert_eq!(s["ipv4"]["never-default"], ZValue::from(true));
        assert_eq!(s["ipv4"]["ignore-auto-dns"], ZValue::from(true));
        assert_eq!(s["connection"]["autoconnect"], ZValue::from(false));
        assert_eq!(s["connection"]["interface-name"], ZValue::from("wlan0"));
        assert_eq!(
            s["802-11-wireless"]["ssid"],
            ZValue::from(b"DIRECT-xy".to_vec())
        );
        assert_eq!(
            s["802-11-wireless-security"]["key-mgmt"],
            ZValue::from("wpa-psk")
        );
    }
}
