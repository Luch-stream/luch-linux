//! What a direct connection says over the receiver's Bluetooth channel after the welcome (PROTOCOL.md, "direct").

use std::net::Ipv4Addr;

use serde_json::{json, Value};

/// The receiver's Wi-Fi Direct network, and where it listens on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    pub ssid: String,
    pub pass: String,
    /// MHz, 0 when the receiver does not know.
    pub freq: u32,
    pub addr: Ipv4Addr,
    pub port: u16,
    pub tls_port: u16,
}

pub fn request() -> Value {
    json!({"t": "direct"})
}

/// The hello over Bluetooth: the TLS hello, saying the way it came and that it wants the network only.
pub fn ble_hello(mut hello: Value) -> Value {
    hello["via"] = json!("ble");
    hello["want"] = json!(["direct"]);
    hello
}

/// Whether the welcome says the receiver can make a group now.
pub fn can(welcome: &Value) -> bool {
    welcome["modes"]
        .as_array()
        .is_some_and(|m| m.iter().any(|x| x == "direct"))
}

/// The answer to `request` from a message, or why there is none (`tv`: the receiver's name, for the words). None: the
/// message is something else (a ping, a report).
pub fn answer(v: &Value, tv: &str) -> Option<Result<Offer, String>> {
    match v["t"].as_str()? {
        "direct" => {
            Some(offer(v).ok_or_else(|| {
                format!("{tv} answered with a network this computer cannot join: {v}")
            }))
        }
        "error" if v["for"] == "direct" => Some(Err(refusal(v, tv))),
        _ => None,
    }
}

fn offer(v: &Value) -> Option<Offer> {
    let ssid = v["ssid"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 32)?;
    let pass = v["pass"].as_str().filter(|p| (8..=63).contains(&p.len()))?;
    let addr = v["addr"]
        .as_str()?
        .parse::<Ipv4Addr>()
        .ok()
        .filter(|a| !a.is_unspecified())?;
    let port = |k: &str| {
        v[k].as_u64()
            .and_then(|p| u16::try_from(p).ok())
            .filter(|p| *p != 0)
    };
    Some(Offer {
        ssid: ssid.into(),
        pass: pass.into(),
        freq: v["freq"]
            .as_u64()
            .and_then(|f| u32::try_from(f).ok())
            .unwrap_or(0),
        addr,
        port: port("port").unwrap_or(luch_proto::CONTROL_PORT),
        tls_port: port("tls_port")?,
    })
}

/// A refusal in words, for the person at the sender.
fn refusal(v: &Value, tv: &str) -> String {
    let detail = v["detail"].as_str().unwrap_or("");
    let with = |s: String| {
        if detail.is_empty() {
            s
        } else {
            format!("{s}: {detail}")
        }
    };
    match v["code"].as_str().unwrap_or("") {
        "denied" => format!("{tv} refused the direct connection"),
        "no_p2p" => with(format!("{tv} cannot make a Wi-Fi Direct network")),
        "not_allowed" => with(format!("{tv} does not take a direct connection from here")),
        "busy" => format!("{tv} is busy with another request; try again in a moment"),
        "timeout" => format!("nobody answered on {tv} in time"),
        other => with(format!("{tv} refused ({other})")),
    }
}

/// The band of a frequency, for the person ("5 GHz"); empty when unknown.
pub fn band(freq: u32) -> &'static str {
    match freq {
        2400..=2500 => "2.4 GHz",
        4900..=5900 => "5 GHz",
        5925..=7125 => "6 GHz",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_network_is_read_from_the_answer() {
        let v = json!({"t": "direct", "ssid": "DIRECT-xy-Luch", "pass": "s3cret-pass", "freq": 5180,
                       "addr": "192.168.49.1", "port": 47800, "tls_port": 47802});
        let o = answer(&v, "Living room TV").unwrap().unwrap();
        assert_eq!(
            o,
            Offer {
                ssid: "DIRECT-xy-Luch".into(),
                pass: "s3cret-pass".into(),
                freq: 5180,
                addr: Ipv4Addr::new(192, 168, 49, 1),
                port: 47800,
                tls_port: 47802,
            }
        );
        assert_eq!(band(o.freq), "5 GHz");
        // freq 0 and no plain port: still a network.
        let v = json!({"t": "direct", "ssid": "DIRECT-ab", "pass": "12345678", "freq": 0,
                       "addr": "192.168.49.1", "tls_port": 47802});
        let o = answer(&v, "TV").unwrap().unwrap();
        assert_eq!((o.freq, o.port, band(0)), (0, luch_proto::CONTROL_PORT, ""));
    }

    #[test]
    fn a_network_that_cannot_be_joined_is_said() {
        for bad in [
            json!({"t": "direct", "ssid": "", "pass": "12345678", "addr": "192.168.49.1", "tls_port": 1}),
            json!({"t": "direct", "ssid": "DIRECT-ab", "pass": "short", "addr": "192.168.49.1", "tls_port": 1}),
            json!({"t": "direct", "ssid": "DIRECT-ab", "pass": "12345678", "addr": "0.0.0.0", "tls_port": 1}),
            json!({"t": "direct", "ssid": "DIRECT-ab", "pass": "12345678", "addr": "192.168.49.1"}),
            json!({"t": "direct", "ssid": "x".repeat(33), "pass": "12345678", "addr": "192.168.49.1", "tls_port": 1}),
        ] {
            let e = answer(&bad, "TV").unwrap().unwrap_err();
            assert!(e.starts_with("TV answered with a network"), "{e}");
        }
    }

    #[test]
    fn refusals_read_well() {
        let r = |code: &str, detail: &str| {
            answer(
                &json!({"t": "error", "for": "direct", "code": code, "detail": detail}),
                "Living room TV",
            )
            .unwrap()
            .unwrap_err()
        };
        assert_eq!(
            r("denied", ""),
            "Living room TV refused the direct connection"
        );
        assert_eq!(
            r("no_p2p", "no permission for nearby Wi-Fi devices"),
            "Living room TV cannot make a Wi-Fi Direct network: no permission for nearby Wi-Fi devices"
        );
        assert_eq!(
            r("timeout", ""),
            "nobody answered on Living room TV in time"
        );
        assert!(r("busy", "").contains("try again"));
        assert_eq!(r("lunch", ""), "Living room TV refused (lunch)");
    }

    #[test]
    fn other_messages_are_not_answers() {
        assert_eq!(answer(&json!({"t": "ping", "n": 3}), "TV"), None);
        assert_eq!(
            answer(
                &json!({"t": "error", "for": "mirror", "code": "busy"}),
                "TV"
            ),
            None
        );
        assert_eq!(answer(&json!({"no": "t"}), "TV"), None);
    }

    #[test]
    fn the_hello_over_bluetooth_wants_the_network_only() {
        let h = ble_hello(luch_proto::control::hello(
            "9a1b2c3d",
            "Linux Laptop",
            &["mirror"],
        ));
        assert_eq!(
            (h["via"].clone(), h["want"].clone()),
            (json!("ble"), json!(["direct"]))
        );
        assert!(can(&json!({"t": "welcome", "modes": ["mirror", "direct"]})));
        assert!(!can(&json!({"t": "welcome", "modes": []})));
        assert!(!can(&json!({"t": "welcome"})));
    }
}
