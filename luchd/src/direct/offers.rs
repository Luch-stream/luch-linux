//! Receivers heard over Bluetooth that offer a direct connection (a PSM in their card): kept for a while after the
//! scan that heard them, so `Direct` goes to one at once and a menu can list them.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use serde_json::{json, Value};

/// How long a TV heard once is taken to be still there.
pub const FRESH: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offered {
    /// The start of its id as its card says it (8 hex digits): what `luchctl direct` takes.
    pub key: String,
    /// Its name when this computer knows it (paired, or listed before); else the key.
    pub name: String,
    pub rssi: Option<i16>,
    /// The BlueZ device it was heard from.
    pub device: String,
    pub psm: u16,
}

/// One luchd per user: the offers heard by any scan of this process.
static HEARD: Mutex<Vec<(Offered, Instant)>> = Mutex::new(Vec::new());

/// Notes a receiver heard now (the same one again replaces its old entry).
pub fn heard(o: Offered) {
    let mut all = HEARD.lock().unwrap();
    all.retain(|(x, _)| x.key != o.key);
    all.push((o, Instant::now()));
}

/// The offers heard lately, nearest first.
pub fn fresh() -> Vec<Offered> {
    let mut all = HEARD.lock().unwrap();
    all.retain(|(_, at)| at.elapsed() < FRESH);
    let mut v: Vec<Offered> = all.iter().map(|(o, _)| o.clone()).collect();
    v.sort_by_key(|o| std::cmp::Reverse(o.rssi.unwrap_or(i16::MIN)));
    v
}

/// The offer a person means: "" for the nearest, else the start of its id (4 hex digits or more, dashes allowed) or
/// its name.
pub fn pick<'a>(offers: &'a [Offered], key: &str) -> Result<&'a Offered, String> {
    let key = key.trim();
    if key.is_empty() {
        return offers
            .first()
            .ok_or_else(|| "no TV nearby offers a direct connection".into());
    }
    let hex: String = key
        .chars()
        .filter(|c| *c != '-')
        .collect::<String>()
        .to_lowercase();
    let by_id = |o: &Offered| {
        hex.len() >= 4
            && hex.chars().all(|c| c.is_ascii_hexdigit())
            && (hex.starts_with(&o.key) || o.key.starts_with(&hex))
    };
    let hits: Vec<&Offered> = offers
        .iter()
        .filter(|o| by_id(o) || o.name.eq_ignore_ascii_case(key))
        .collect();
    match hits.as_slice() {
        [one] => Ok(one),
        [] => Err(format!("no TV '{key}' heard offering a direct connection")),
        _ => Err(format!("'{key}' is more than one TV: use more of its id")),
    }
}

pub fn to_json(offers: &[Offered]) -> Value {
    Value::Array(
        offers
            .iter()
            .map(|o| json!({"key": o.key, "name": o.name, "rssi": o.rssi}))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(key: &str, name: &str, rssi: i16) -> Offered {
        Offered {
            key: key.into(),
            name: name.into(),
            rssi: Some(rssi),
            device: format!("/org/bluez/hci0/dev_{key}"),
            psm: 0x80,
        }
    }

    #[test]
    fn a_person_names_an_offer_by_id_or_name() {
        let all = [
            o("0b6f1c2a", "Living room TV", -40),
            o("3f0c2a8e", "Bedroom TV", -70),
            o("3f0d0000", "3f0d0000", -80),
        ];
        assert_eq!(pick(&all, "").unwrap().key, "0b6f1c2a");
        assert_eq!(pick(&all, "bedroom tv").unwrap().key, "3f0c2a8e");
        // The whole id, as `luchctl list` showed it once, or its start.
        assert_eq!(
            pick(&all, "3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47")
                .unwrap()
                .key,
            "3f0c2a8e"
        );
        assert_eq!(pick(&all, "0B6F").unwrap().key, "0b6f1c2a");
        assert!(pick(&all, "3f0c").is_ok());
        assert!(
            pick(&all, "3f0").unwrap_err().contains("heard"),
            "too short to be an id"
        );
        assert!(pick(&all, "Kitchen").is_err());
        assert!(pick(&[], "").unwrap_err().contains("no TV nearby"));
        let twins = [o("aaaa0001", "A", -1), o("aaaa0002", "B", -2)];
        assert!(pick(&twins, "aaaa").unwrap_err().contains("more than one"));
    }

    #[test]
    fn offers_heard_are_kept_nearest_first_once_each() {
        heard(o("cafe0001", "Far TV", -90));
        heard(o("cafe0002", "Near TV", -30));
        heard(o("cafe0001", "Far TV", -50));
        let mine: Vec<(String, Option<i16>)> = fresh()
            .into_iter()
            .filter(|x| x.key.starts_with("cafe"))
            .map(|x| (x.key, x.rssi))
            .collect();
        assert_eq!(
            mine,
            vec![
                ("cafe0002".into(), Some(-30)),
                ("cafe0001".into(), Some(-50))
            ]
        );
        assert_eq!(
            to_json(&[o("cafe0002", "Near TV", -30)]),
            json!([{"key": "cafe0002", "name": "Near TV", "rssi": -30}])
        );
    }
}
