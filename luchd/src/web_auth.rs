//! Who may watch this screen in a browser (luchd's web receiver): a browser that opens the page is shown a 4-digit
//! code and this computer asks its person, showing the same code, whether to let it in (numeric comparison: the
//! browser on the TV in front of them, not another one asking at the same moment). An allowed browser keeps a
//! long-lived cookie, so it is not asked again; refusals and the pace of questions are limited as on the TV.
use std::{
    collections::HashMap,
    fs, io,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use ring::rand::SecureRandom;
use serde_json::{json, Value};
use zbus::{zvariant, Connection};

/// The cookie an allowed browser keeps.
pub const COOKIE: &str = "luch_web";
/// Browsers remembered at most; the least recently used goes.
pub const MAX: usize = 20;
/// Refusals from an address after which it is not asked about for BAN.
pub const STRIKES: u32 = 3;
pub const BAN: Duration = Duration::from_secs(3600);
/// How long the question waits for an answer.
pub const ASK: Duration = Duration::from_secs(60);

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    ring::rand::SystemRandom::new()
        .fill(&mut b)
        .expect("the system has randomness");
    crate::pair::hex(&b)
}

/// Four digits for the page and the question here.
pub fn code() -> String {
    let mut b = [0u8; 4];
    ring::rand::SystemRandom::new()
        .fill(&mut b)
        .expect("the system has randomness");
    format!("{:04}", u32::from_be_bytes(b) % 10_000)
}

// ---------- allowed browsers ----------

fn hash(token: &str) -> String {
    crate::pair::hex(ring::digest::digest(&ring::digest::SHA256, token.as_bytes()).as_ref())
}

/// ~/.config/luch/web-browsers.json: [{id, hash (of the token), name, address, allowed, used}]. Only the token's
/// hash is kept: the file does not let anyone in.
fn load(dir: &Path) -> Vec<Value> {
    fs::read(dir.join("web-browsers.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save(dir: &Path, all: &[Value]) -> io::Result<()> {
    fs::write(
        dir.join("web-browsers.json"),
        serde_json::to_vec_pretty(all)?,
    )
}

/// A new token for a browser just allowed; the least recently used beyond MAX is forgotten.
pub fn allow(dir: &Path, name: &str, address: &str) -> io::Result<String> {
    let token = random_hex(32);
    let mut all = load(dir);
    // In front: among browsers used in the same second, the newest stays.
    all.insert(
        0,
        json!({"id": random_hex(8), "hash": hash(&token), "name": name, "address": address,
                         "allowed": now(), "used": now()}),
    );
    all.sort_by_key(|b| std::cmp::Reverse(b["used"].as_u64().unwrap_or(0)));
    all.truncate(MAX);
    save(dir, &all)?;
    Ok(token)
}

/// Whether `token` is an allowed browser's; marks it used.
pub fn known(dir: &Path, token: &str) -> bool {
    let h = hash(token);
    let mut all = load(dir);
    let Some(b) = all.iter_mut().find(|b| b["hash"] == h.as_str()) else {
        return false;
    };
    b["used"] = json!(now());
    let _ = save(dir, &all);
    true
}

/// The allowed browsers, most recently used first, without their hashes.
pub fn list(dir: &Path) -> Vec<Value> {
    let mut all = load(dir);
    all.sort_by_key(|b| std::cmp::Reverse(b["used"].as_u64().unwrap_or(0)));
    for b in &mut all {
        if let Some(o) = b.as_object_mut() {
            o.remove("hash");
        }
    }
    all
}

pub fn forget(dir: &Path, id: &str) -> io::Result<bool> {
    let mut all = load(dir);
    let before = all.len();
    all.retain(|b| b["id"] != id);
    save(dir, &all)?;
    Ok(all.len() != before)
}

/// The `luch_web` cookie of an HTTP request head, if any.
pub fn cookie(head: &str) -> Option<&str> {
    head.split("\r\n")
        .filter(|l| l.len() > 7 && l[..7].eq_ignore_ascii_case("cookie:"))
        .flat_map(|l| l[7..].split(';'))
        .filter_map(|c| c.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE)
        .map(|(_, v)| v)
}

// ---------- refusals and pace ----------

/// Refusals per address and the one question at a time, like the TV's Gate.
#[derive(Default)]
pub struct Limits {
    strikes: HashMap<String, (u32, u64, u64)>, // address -> (count, since, banned until), unix seconds
    asking: bool,
}

pub enum Verdict {
    Ask,
    Banned(u64),
    Busy,
}

impl Limits {
    pub fn check(&mut self, address: &str, now: u64) -> Verdict {
        if let Some((_, _, until)) = self.strikes.get(address) {
            if *until > now {
                return Verdict::Banned(until - now);
            }
        }
        if self.asking {
            return Verdict::Busy;
        }
        self.asking = true;
        Verdict::Ask
    }

    pub fn done(&mut self) {
        self.asking = false;
    }

    pub fn strike(&mut self, address: &str, now: u64) {
        let s = self
            .strikes
            .entry(address.to_string())
            .or_insert((0, now, 0));
        if now - s.1 >= BAN.as_secs() {
            *s = (0, now, 0);
        }
        s.0 += 1;
        if s.0 >= STRIKES {
            *s = (0, now, now + BAN.as_secs());
        }
    }
}

// ---------- the question on this computer ----------

/// Asks this computer's person with a notification (Allow / Deny buttons) and returns their answer; None when it
/// was closed or nobody answered within ASK. `extra` is answered too: luchctl and other front-ends answer through
/// luchd's ApproveWeb, which sends there.
pub async fn ask(
    conn: &Connection,
    name: &str,
    address: &str,
    code: &str,
    extra: tokio::sync::oneshot::Receiver<bool>,
) -> Option<bool> {
    use futures_util::StreamExt;
    let proxy = zbus::Proxy::new(
        conn,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )
    .await
    .ok();
    let mut clicks = match &proxy {
        Some(p) => p.receive_signal("ActionInvoked").await.ok(),
        None => None,
    };
    let mut hints: HashMap<&str, zvariant::Value> = HashMap::new();
    hints.insert("urgency", zvariant::Value::U8(2)); // stays until answered
    let body = format!(
        "A browser at {address} ({name}) wants to show this screen. Allow it only if it shows the code {code}."
    );
    let id: Option<u32> = match &proxy {
        Some(p) => p
            .call(
                "Notify",
                &(
                    "Luch",
                    0u32,
                    "video-display",
                    format!("Let a browser watch this screen? Code {code}"),
                    body,
                    vec!["allow", "Allow", "deny", "Deny"],
                    hints,
                    0i32,
                ),
            )
            .await
            .map_err(|e| eprintln!("[luchd] web receiver question: {e}"))
            .ok(),
        None => None,
    };
    let from_notification = async {
        let (Some(id), Some(clicks)) = (id, clicks.as_mut()) else {
            return std::future::pending().await;
        };
        while let Some(m) = clicks.next().await {
            if let Ok((nid, action)) = m.body().deserialize::<(u32, String)>() {
                if nid == id {
                    return action == "allow";
                }
            }
        }
        std::future::pending().await
    };
    let answer = tokio::time::timeout(ASK, async {
        tokio::select! {
            a = from_notification => Some(a),
            a = extra => a.ok(),
        }
    })
    .await
    .ok()
    .flatten();
    if let (Some(p), Some(id)) = (&proxy, id) {
        let _ = p.call::<_, _, ()>("CloseNotification", &(id,)).await;
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("luch-web-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn an_allowed_browser_is_known_by_its_token_only() {
        let d = dir();
        let token = allow(&d, "Chrome/120", "192.168.1.30").unwrap();
        assert!(known(&d, &token));
        assert!(!known(&d, "0000"));
        let text = fs::read_to_string(d.join("web-browsers.json")).unwrap();
        assert!(!text.contains(&token), "only a hash of the token is kept");
        let listed = list(&d);
        assert_eq!(listed.len(), 1);
        assert!(listed[0].get("hash").is_none());
        assert!(forget(&d, listed[0]["id"].as_str().unwrap()).unwrap());
        assert!(!known(&d, &token));
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn twenty_browsers_at_most() {
        let d = dir();
        let first = allow(&d, "first", "192.168.1.2").unwrap();
        let second = allow(&d, "second", "192.168.1.2").unwrap();
        for i in 0..MAX - 1 {
            allow(&d, &format!("b{i}"), "192.168.1.3").unwrap();
        }
        assert_eq!(load(&d).len(), MAX);
        assert!(!known(&d, &first), "the least recently used is forgotten");
        assert!(known(&d, &second));
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn the_cookie_is_found_among_others() {
        let head = "GET /ws HTTP/1.1\r\nHost: x\r\nCookie: a=1; luch_web=abc123; b=2\r\n\r\n";
        assert_eq!(cookie(head), Some("abc123"));
        assert_eq!(
            cookie("GET / HTTP/1.1\r\ncookie: luch_web=z\r\n\r\n"),
            Some("z")
        );
        assert_eq!(cookie("GET / HTTP/1.1\r\nCookie: other=1\r\n\r\n"), None);
    }

    #[test]
    fn three_refusals_and_an_address_waits_an_hour() {
        let mut l = Limits::default();
        let t = 1_000_000;
        for _ in 0..STRIKES {
            assert!(matches!(l.check("192.168.1.30", t), Verdict::Ask));
            l.strike("192.168.1.30", t);
            l.done();
        }
        assert!(
            matches!(l.check("192.168.1.30", t + 10), Verdict::Banned(s) if s == BAN.as_secs() - 10)
        );
        assert!(matches!(l.check("192.168.1.31", t), Verdict::Ask));
        assert!(
            matches!(l.check("192.168.1.32", t), Verdict::Busy),
            "one question at a time"
        );
        l.done();
        assert!(matches!(
            l.check("192.168.1.30", t + BAN.as_secs()),
            Verdict::Ask
        ));
    }

    #[test]
    fn codes_are_four_digits() {
        for _ in 0..50 {
            let c = code();
            assert!(c.len() == 4 && c.bytes().all(|b| b.is_ascii_digit()));
        }
    }
}
