//! "Report a problem" (docs/dbus.md, Report): one archive a tester hands over as it is — this computer's log, the
//! logs of the receivers in use now or lately (luch-android PROTOCOL.md, `logs`), what they are, the crash reports
//! they gave and what the person wrote. Addresses and names are replaced unless the person keeps them. Nothing is
//! sent anywhere: the file goes to Downloads, and the person chooses where it goes from there.

use std::{
    collections::HashMap,
    fmt::Write as _,
    io::{self, Read, Write},
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use base64::Engine;
use serde_json::{json, Value};
use tokio::{sync::broadcast, time::timeout};

use crate::{mirror::Cmd, Conn, Reader, Receiver, Writer};

/// Where a problem with this sender is filed.
pub const ISSUES: &str = "https://altlinux.space/Luch/luch-linux/issues/new";
/// This computer's log in the report: its last ~1 MB, as much as each receiver is asked for.
const LOG_BYTES: usize = 1024 * 1024;
const LOG_KB: u64 = 1024;
/// The GNOME extension's lines are few; this keeps an endless error loop from taking over the report.
const SHELL_BYTES: usize = 256 * 1024;
/// How long a receiver has to hand its log over.
const WAIT: Duration = Duration::from_secs(10);
/// A receiver's log unpacked is cut here: a slice that unpacks endlessly fills no memory.
const MAX_TEXT: u64 = 8 * 1024 * 1024;
/// About 1 MB of gzip comes in some 30 slices of 48 KB of base64; more is not a log.
const MAX_PARTS: u64 = 512;
/// A browser takes long links, but not endlessly: the description in the issue's body is cut to stay under this.
const MAX_URL: usize = 7000;
const OLD: &str = "its Luch does not hand over logs yet (update it)";

// ---------- redaction ----------

/// Words that may be a name but are also what Luch's own lines say: never replaced.
const COMMON: [&str; 12] = [
    "linux",
    "localhost",
    "luch",
    "luchd",
    "receiver",
    "computer",
    "user",
    "root",
    "android",
    "unknown",
    "none",
    "home",
];

/// Replaces addresses and names with stand-ins, the same one each time within a report: IPv4 → 192.0.2.N (then the
/// other documentation blocks), IPv6 → 2001:db8::N, MAC → 02:00:00:00:00:NN, the computer's name → "Computer", a
/// receiver's → "Receiver N", the login name → "user", the home directory → "~". Loopback, multicast and the like
/// stay: they say nothing about anyone. Off, nothing is changed.
pub struct Redact {
    on: bool,
    /// What → its stand-in, longest first (a name inside a longer one does not cut it).
    names: Vec<(String, String)>,
    home: Option<String>,
    receivers: HashMap<String, String>,
    v4: HashMap<Ipv4Addr, String>,
    v6: HashMap<Ipv6Addr, String>,
    macs: HashMap<String, String>,
}

impl Redact {
    pub fn new(on: bool) -> Redact {
        Redact {
            on,
            names: Vec::new(),
            home: None,
            receivers: HashMap::new(),
            v4: HashMap::new(),
            v6: HashMap::new(),
            macs: HashMap::new(),
        }
    }

    pub fn computer(&mut self, host: &str, sender_id: &str) {
        self.name(host, "Computer");
        self.name(sender_id, "computer-id");
    }

    pub fn login(&mut self, user: &str, home: &str) {
        let home = home.trim_end_matches('/');
        if home.len() > 1 {
            self.home = Some(home.to_string());
        }
        self.name(user, "user");
    }

    /// A receiver's label, "Receiver N" for every name it goes by (discovery, its welcome) and its id; its name when
    /// names are kept.
    pub fn receiver(&mut self, id: &str, names: &[&str]) -> String {
        if !self.on {
            let name = names.iter().find(|n| !n.trim().is_empty()).unwrap_or(&id);
            return name.to_string();
        }
        if let Some(l) = self.receivers.get(id) {
            return l.clone();
        }
        let n = self.receivers.len() + 1;
        let label = format!("Receiver {n}");
        for name in names {
            self.name(name, &label);
        }
        self.name(id, &format!("receiver-{n}-id"));
        self.receivers.insert(id.to_string(), label.clone());
        label
    }

    fn name(&mut self, what: &str, label: &str) {
        let w = what.trim();
        if w.chars().count() < 3
            || COMMON.contains(&w.to_lowercase().as_str())
            || self.names.iter().any(|(n, _)| n.eq_ignore_ascii_case(w))
        {
            return;
        }
        self.names.push((w.to_string(), label.to_string()));
        self.names.sort_by_key(|(n, _)| std::cmp::Reverse(n.len()));
    }

    pub fn text(&mut self, s: &str) -> String {
        if !self.on {
            return s.to_string();
        }
        let mut out = match &self.home {
            Some(h) => s.replace(h.as_str(), "~"),
            None => s.to_string(),
        };
        for (w, label) in &self.names {
            out = replace_word(&out, w, label);
        }
        let out = runs(
            &out,
            |c| c.is_ascii_hexdigit() || c == ':' || c == '-',
            |r| self.mac(r),
        );
        let out = runs(&out, |c| c.is_ascii_digit() || c == '.', |r| self.ipv4(r));
        runs(
            &out,
            |c| c.is_ascii_hexdigit() || c == ':' || c == '.',
            |r| self.ipv6(r),
        )
    }

    fn mac(&mut self, run: &str) -> Option<String> {
        let core = run.trim_end_matches([':', '-']);
        let b = core.as_bytes();
        let sep = *b.get(2)?;
        let shaped = b.len() == 17
            && (sep == b':' || sep == b'-')
            && (0..17).all(|i| {
                if i % 3 == 2 {
                    b[i] == sep
                } else {
                    b[i].is_ascii_hexdigit()
                }
            });
        if !shaped {
            return None;
        }
        let key = core.to_ascii_lowercase().replace('-', ":");
        if key == "00:00:00:00:00:00"
            || key == "ff:ff:ff:ff:ff:ff"
            || key.starts_with("02:00:00:00:")
        {
            return None;
        }
        let n = self.macs.len() + 1;
        let stand_in = self
            .macs
            .entry(key)
            .or_insert_with(|| format!("02:00:00:00:{:02x}:{:02x}", n >> 8, n & 0xff));
        Some(format!("{stand_in}{}", &run[core.len()..]))
    }

    fn ipv4(&mut self, run: &str) -> Option<String> {
        let core = run.trim_end_matches('.');
        let ip: Ipv4Addr = core.parse().ok()?;
        let o = ip.octets();
        let documentation = matches!(o, [192, 0, 2, _] | [198, 51, 100, _] | [203, 0, 113, _]);
        if ip.is_loopback() || ip.is_multicast() || ip.is_broadcast() || o[0] == 0 || documentation
        {
            return None;
        }
        let n = self.v4.len() + 1;
        let stand_in = self.v4.entry(ip).or_insert_with(|| match n {
            1..=254 => format!("192.0.2.{n}"),
            255..=508 => format!("198.51.100.{}", n - 254),
            _ => format!("203.0.113.{}", (n - 509) % 254 + 1),
        });
        Some(format!("{stand_in}{}", &run[core.len()..]))
    }

    fn ipv6(&mut self, run: &str) -> Option<String> {
        // A digit too: words of hex letters around "::" are Rust paths (a::b), not addresses.
        if (!run.contains("::") && run.matches(':').count() < 7)
            || !run.bytes().any(|b| b.is_ascii_digit())
        {
            return None;
        }
        // An address followed by a colon or a full stop: the address without them.
        let (core, ip) = [run, run.trim_end_matches(['.', ':'])]
            .into_iter()
            .find_map(|c| Some((c, c.parse::<Ipv6Addr>().ok()?)))?;
        let documentation = ip.segments()[..2] == [0x2001, 0xdb8];
        if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() || documentation {
            return None;
        }
        let n = self.v6.len() + 1;
        let stand_in = self
            .v6
            .entry(ip)
            .or_insert_with(|| format!("2001:db8::{n:x}"));
        Some(format!("{stand_in}{}", &run[core.len()..]))
    }
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `hay` with every whole-word `needle` (ASCII case aside) replaced.
fn replace_word(hay: &str, needle: &str, with: &str) -> String {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    let mut out = String::with_capacity(hay.len());
    let (mut i, mut last) = (0, 0);
    while i + n.len() <= h.len() {
        // Bytes outside ASCII only match themselves, so a match ends on a character boundary too.
        if hay.is_char_boundary(i)
            && h[i..i + n.len()].eq_ignore_ascii_case(n)
            && !hay[..i].chars().next_back().is_some_and(word_char)
            && !hay[i + n.len()..].chars().next().is_some_and(word_char)
        {
            out.push_str(&hay[last..i]);
            out.push_str(with);
            i += n.len();
            last = i;
        } else {
            i += 1;
        }
    }
    out.push_str(&hay[last..]);
    out
}

/// `s` with each longest run of `in_run` characters that is not part of a word put through `f` (None: kept).
fn runs(
    s: &str,
    in_run: impl Fn(char) -> bool,
    mut f: impl FnMut(&str) -> Option<String>,
) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev: Option<char> = None;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if !in_run(c) {
            out.push(c);
            prev = Some(c);
            continue;
        }
        let mut end = i + c.len_utf8();
        while let Some(&(j, d)) = chars.peek() {
            if !in_run(d) {
                break;
            }
            end = j + d.len_utf8();
            chars.next();
        }
        let run = &s[i..end];
        let alone = !prev.is_some_and(word_char) && !s[end..].chars().next().is_some_and(word_char);
        match alone.then(|| f(run)).flatten() {
            Some(r) => out.push_str(&r),
            None => out.push_str(run),
        }
        prev = run.chars().next_back();
    }
    out
}

// ---------- a receiver's log ----------

/// What a sender asks a receiver for.
fn request() -> Value {
    json!({"t": "logs", "max_kb": LOG_KB})
}

fn hands_over_logs(welcome: &Value) -> bool {
    welcome["modes"]
        .as_array()
        .is_some_and(|m| m.iter().any(|x| x == "logs"))
}

/// A receiver's log as it comes: `logs` lines, each a slice of one gzip, joined and unpacked once all are in. `part`
/// counts from 0; one counting from 1 is taken too.
#[derive(Default)]
pub struct Slices {
    parts: Vec<Option<Vec<u8>>>,
}

impl Slices {
    /// Takes one message: the log once it is whole, or why there is none (an `error` for `logs`, a bad slice).
    pub fn take(&mut self, v: &Value) -> Option<Result<String, String>> {
        if v["t"] == "error" && v["for"] == "logs" {
            let code = v["code"].as_str().unwrap_or("error");
            return Some(Err(match v["detail"].as_str() {
                Some(d) => format!("{code}: {d}"),
                None => code.to_string(),
            }));
        }
        if v["t"] != "logs" {
            return None;
        }
        let (Some(n), Some(i)) = (v["parts"].as_u64(), v["part"].as_u64()) else {
            return Some(Err("a slice of the log without part and parts".into()));
        };
        if n == 0 || n > MAX_PARTS || i > n {
            return Some(Err(format!("slice {i} of {n} of the log")));
        }
        if self.parts.is_empty() {
            self.parts = vec![None; n as usize + 1];
        } else if self.parts.len() != n as usize + 1 {
            return Some(Err("the number of slices of the log changed".into()));
        }
        let gz = match base64::engine::general_purpose::STANDARD
            .decode(v["gz"].as_str().unwrap_or(""))
        {
            Ok(b) => b,
            Err(e) => return Some(Err(format!("slice {i} of the log: {e}"))),
        };
        self.parts[i as usize] = Some(gz);
        let n = n as usize;
        let whole = if self.parts[..n].iter().all(Option::is_some) {
            0..n
        } else if self.parts[1..].iter().all(Option::is_some) {
            1..n + 1
        } else {
            return None;
        };
        let gz: Vec<u8> = self.parts[whole]
            .iter()
            .flatten()
            .flatten()
            .copied()
            .collect();
        Some(gunzip(&gz))
    }
}

fn gunzip(gz: &[u8]) -> Result<String, String> {
    let mut text = Vec::new();
    flate2::read::MultiGzDecoder::new(gz)
        .take(MAX_TEXT)
        .read_to_end(&mut text)
        .map_err(|e| format!("the log is not gzip: {e}"))?;
    Ok(String::from_utf8_lossy(&text).into_owned())
}

/// The log of a receiver luchd is connected to, through its channel (run_channel passes `logs` on).
async fn ask_channel(c: &Conn) -> Result<String, String> {
    let mut events = c.events.subscribe();
    c.cmd
        .send(Cmd::Send(request()))
        .await
        .map_err(|_| "the connection closed".to_string())?;
    let mut slices = Slices::default();
    loop {
        match events.recv().await {
            Ok(v) => {
                if let Some(r) = slices.take(&v) {
                    return r;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                return Err("some of it was lost on the way".into())
            }
            Err(broadcast::error::RecvError::Closed) => return Err("the connection closed".into()),
        }
    }
}

/// The log over a channel of its own, just opened to a paired receiver: answers its pings and keeps the crash
/// reports it hands over (section 6), as the usual channel would. The caller bounds the time.
pub(crate) async fn ask_over(
    rd: &mut Reader,
    wr: &mut Writer,
    model: &str,
) -> Result<String, String> {
    crate::send(wr, &request())
        .await
        .map_err(|e| e.to_string())?;
    let mut slices = Slices::default();
    loop {
        let v = match crate::read_msg(rd).await {
            Ok(Some(v)) => v,
            Ok(None) => return Err("it closed the connection".into()),
            Err(e) => return Err(e.to_string()),
        };
        match v["t"].as_str().unwrap_or("") {
            "ping" => {
                let _ = crate::send(wr, &json!({"t": "pong", "n": v["n"]})).await;
            }
            "reports" => {
                let items = v["items"].as_array().cloned().unwrap_or_default();
                let (ids, _) = crate::reports::store(model, &items);
                if !ids.is_empty() {
                    let _ = crate::send(wr, &json!({"t": "reports_taken", "ids": ids})).await;
                }
            }
            _ => {
                if let Some(r) = slices.take(&v) {
                    return r;
                }
            }
        }
    }
}

/// A paired receiver luchd is not connected to: a short channel of its own, TLS with its pinned certificate, so the
/// TV shows nothing; its welcome too (what it is). An unpaired one is not asked: that would put a prompt on the TV.
async fn ask_paired(l: &crate::Luch, r: &Receiver) -> (Option<Value>, Result<String, String>) {
    let pinned = crate::config_dir()
        .ok()
        .and_then(|d| crate::pair::pinned_fp(&d, &r.id));
    let (Some(pin), Some(me)) = (pinned, l.ident.as_deref().filter(|_| r.tls_port > 0)) else {
        return (
            None,
            Err("not paired with this computer: asking would show a prompt on the TV".into()),
        );
    };
    let mut hello = crate::hello(&l.sender_id, &l.host, None, false);
    hello["want"] = json!(["reports"]);
    let mut welcome = None;
    let asked = timeout(WAIT, async {
        let addr = SocketAddr::new(
            r.address
                .parse()
                .map_err(|_| format!("not an address: {}", r.address))?,
            r.tls_port,
        );
        let stream = crate::connect_from(addr, Some(&r.ifname), crate::HANDSHAKE_TIMEOUT)
            .await
            .map_err(|e| e.to_string())?;
        let mut o = crate::open_channel(stream, &hello, Some(me), Some(pin))
            .await
            .map_err(|e| e.to_string())?;
        let got = if o.first["t"] != "welcome" {
            Err(format!("answered {} instead of its welcome", o.first["t"]))
        } else if !hands_over_logs(&o.first) {
            welcome = Some(o.first.clone());
            Err(OLD.to_string())
        } else {
            welcome = Some(o.first.clone());
            ask_over(&mut o.rd, &mut o.wr, &r.model).await
        };
        let _ = crate::send(&mut o.wr, &json!({"t": "bye"})).await;
        got
    })
    .await
    .unwrap_or_else(|_| Err(format!("did not answer in {} s", WAIT.as_secs())));
    (welcome, asked)
}

// ---------- this computer ----------

/// The last `max` bytes (whole lines) of what `program args` prints, only the lines with `filter` in them (any case)
/// when given; or one line saying why there is nothing, `empty` added when it ran and found nothing.
pub fn journal(
    program: &str,
    args: &[&str],
    max: usize,
    filter: Option<&str>,
    empty: &str,
) -> String {
    let out = match std::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
    {
        Ok(o) => o,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return format!("({program} is not installed: no log from the journal)\n")
        }
        Err(e) => return format!("({program}: {e})\n"),
    };
    let all = String::from_utf8_lossy(&out.stdout);
    let text: String = match filter {
        Some(f) => all
            .lines()
            .filter(|l| l.to_lowercase().contains(f))
            .map(|l| format!("{l}\n"))
            .collect(),
        None => all.into_owned(),
    };
    if text.trim().is_empty() || text.trim() == "-- No entries --" {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        return format!(
            "(no lines{}{empty})\n",
            if err.is_empty() {
                String::new()
            } else {
                format!(": {err}")
            }
        );
    }
    tail(&text, max).to_string()
}

/// At most the last `max` bytes of `s`, from the start of a line.
fn tail(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut i = s.len() - max;
    while !s.is_char_boundary(i) {
        i += 1;
    }
    match s[i..].find('\n') {
        Some(nl) => &s[i + nl + 1..],
        None => &s[i..],
    }
}

fn read_trim(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// What this computer is, for about.txt.
fn system() -> Vec<(&'static str, String)> {
    let os = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                .map(|v| v.trim_matches('"').to_string())
        })
        .unwrap_or_else(|| "Linux".into());
    let kernel = read_trim("/proc/sys/kernel/osrelease").unwrap_or_default();
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    let machine = [
        "/sys/class/dmi/id/sys_vendor",
        "/sys/class/dmi/id/product_name",
        "/sys/class/dmi/id/product_version",
    ]
    .iter()
    .filter_map(|p| read_trim(p))
    .filter(|s| !s.eq_ignore_ascii_case("to be filled by o.e.m."))
    .collect::<Vec<_>>()
    .join(" ");
    vec![
        ("Luch", luch_proto::APP.to_string()),
        ("OS", format!("{os}, kernel {kernel}")),
        (
            "desktop",
            format!(
                "{} ({})",
                env("XDG_CURRENT_DESKTOP"),
                env("XDG_SESSION_TYPE")
            ),
        ),
        ("machine", machine),
        ("GStreamer", gstreamer::version_string().to_string()),
    ]
}

/// Downloads from the person's user-dirs.dirs (XDG_DOWNLOAD_DIR="$HOME/Downloads"), if it names one.
fn download_dir_from(user_dirs: &str, home: &Path) -> Option<PathBuf> {
    let v = user_dirs
        .lines()
        .find_map(|l| l.trim().strip_prefix("XDG_DOWNLOAD_DIR="))?
        .trim()
        .trim_matches('"');
    let p = match v.strip_prefix("$HOME") {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None => PathBuf::from(v),
    };
    (p.is_absolute() && p != home).then_some(p)
}

fn downloads() -> io::Result<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or_else(|| io::Error::other("no HOME"))?);
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let dir = std::fs::read_to_string(config.join("user-dirs.dirs"))
        .ok()
        .and_then(|s| download_dir_from(&s, &home))
        .unwrap_or_else(|| home.join("Downloads"));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Local time, for the file's name and about.txt.
#[derive(Clone, Copy, Debug)]
pub struct When {
    pub y: i32,
    pub mo: u32,
    pub d: u32,
    pub h: u32,
    pub mi: u32,
    pub s: u32,
}

impl When {
    fn now() -> When {
        // SAFETY: localtime_r only writes the tm it is given.
        let tm = unsafe {
            let t = libc::time(std::ptr::null_mut());
            let mut tm: libc::tm = std::mem::zeroed();
            libc::localtime_r(&t, &mut tm);
            tm
        };
        When {
            y: tm.tm_year + 1900,
            mo: tm.tm_mon as u32 + 1,
            d: tm.tm_mday as u32,
            h: tm.tm_hour as u32,
            mi: tm.tm_min as u32,
            s: tm.tm_sec as u32,
        }
    }

    fn file_name(&self) -> String {
        format!(
            "Luch-report-{:04}-{:02}-{:02}-{:02}{:02}",
            self.y, self.mo, self.d, self.h, self.mi
        )
    }
}

// ---------- the report ----------

/// What the report says about one receiver.
pub struct Device {
    pub id: String,
    /// Every name it goes by: as discovery found it, as its welcome says.
    pub names: Vec<String>,
    pub model: String,
    /// Its last welcome (versions, decoders, caps), Null when it never said one to this luchd.
    pub welcome: Value,
    /// "connected", "paired, asked for this report", ...
    pub how: String,
    pub log: Result<String, String>,
    /// The numbers of the session now, or of the last one.
    pub stats: Option<Value>,
}

pub struct Snapshot {
    pub text: String,
    pub hide: bool,
    pub host: String,
    pub user: String,
    pub home: String,
    pub sender_id: String,
    pub system: Vec<(&'static str, String)>,
    pub computer_log: String,
    pub shell_log: String,
    pub devices: Vec<Device>,
    /// The other TVs and receivers found (id, name): not in the report, but their names may be in the logs.
    pub others: Vec<(String, String)>,
    pub crashes: Vec<Value>,
    pub when: When,
}

/// File names in the archive: what is left of a name once it cannot be a path.
fn file_safe(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_.".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    match s.trim_matches(['.', ' ']) {
        "" => "receiver".into(),
        t => t.to_string(),
    }
}

fn indent(s: &str, by: &str) -> String {
    s.lines().map(|l| format!("{by}{l}\n")).collect()
}

/// "Android 11, Luch 0.4" from a welcome, what of it is known.
fn versions(w: &Value) -> String {
    let mut v = Vec::new();
    if let Some(sdk) = w["sdk"].as_u64() {
        v.push(crate::limits::android(sdk));
    }
    if let Some(app) = w["app"].as_str() {
        v.push(format!("Luch {app}"));
    }
    v.join(", ")
}

fn about(s: &Snapshot, labels: &[String]) -> String {
    let w = &s.when;
    let mut a = format!(
        "Luch problem report, {:04}-{:02}-{:02} {:02}:{:02}:{:02} (local time)\n\nWhat happened:\n{}\nThis computer\n",
        w.y,
        w.mo,
        w.d,
        w.h,
        w.mi,
        w.s,
        indent(if s.text.trim().is_empty() { "(not described)" } else { s.text.trim() }, "  "),
    );
    for (k, v) in &s.system {
        let _ = writeln!(a, "  {k}: {v}");
    }
    let _ = writeln!(
        a,
        "  addresses and names: {}",
        if s.hide {
            "replaced (192.0.2.N, 2001:db8::N, 02:00:00:00:00:NN, Computer, Receiver N)"
        } else {
            "kept"
        }
    );
    a.push_str("\nReceivers\n");
    if s.devices.is_empty() {
        a.push_str("  none connected or known\n");
    }
    for (d, label) in s.devices.iter().zip(labels) {
        let w = &d.welcome;
        let _ = writeln!(
            a,
            "  {label}{}",
            if d.model.is_empty() {
                String::new()
            } else {
                format!(" ({})", d.model)
            }
        );
        let _ = writeln!(
            a,
            "    {}{}",
            d.how,
            match versions(w) {
                v if v.is_empty() => v,
                v => format!("; {v}"),
            }
        );
        if !w.is_null() {
            let list = |k: &str| {
                w[k].as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default()
            };
            let _ = writeln!(
                a,
                "    decoders: {}; max {}; modes: {}",
                list("decoders"),
                w["max"].as_str().unwrap_or("?"),
                list("modes")
            );
        }
        let _ = match &d.log {
            Ok(_) => writeln!(a, "    log: receivers/{}.log", file_safe(label)),
            Err(why) => writeln!(a, "    log: none, {why}"),
        };
        if let Some(st) = &d.stats {
            let _ = writeln!(a, "    session (now or last): {st}");
        }
        if !w.is_null() {
            let _ = write!(
                a,
                "    welcome:\n{}",
                indent(
                    &serde_json::to_string_pretty(w).unwrap_or_default(),
                    "      "
                )
            );
        }
    }
    let _ = writeln!(a, "\nCrash and error reports kept: {}", s.crashes.len());
    for c in &s.crashes {
        let _ = writeln!(a, "  {}", crate::reports::summary(c));
    }
    a
}

/// A few lines for a chat: what happened, versions, devices, the file.
fn description(s: &Snapshot, labels: &[String], file: &str) -> String {
    let os = s
        .system
        .iter()
        .find(|(k, _)| *k == "OS")
        .map(|(_, v)| v.as_str())
        .unwrap_or("Linux");
    let receivers: Vec<String> = s
        .devices
        .iter()
        .zip(labels)
        .map(|(d, l)| {
            let what = [d.model.clone(), versions(&d.welcome)]
                .into_iter()
                .filter(|x| !x.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
            if what.is_empty() {
                l.clone()
            } else {
                format!("{l} ({what})")
            }
        })
        .collect();
    format!(
        "What happened: {}\nSender: {} on {os}\nReceivers: {}\nReport: {file}",
        if s.text.trim().is_empty() {
            "(not described)".to_string()
        } else {
            s.text.trim().replace('\n', " ")
        },
        luch_proto::APP,
        if receivers.is_empty() {
            "none".to_string()
        } else {
            receivers.join("; ")
        },
    )
}

/// The new-issue link with the title and the description filled in, asking for the file.
fn issue_url(text: &str, description: &str, file: &str) -> String {
    let first = text.trim().lines().next().unwrap_or("").trim();
    let title = if first.is_empty() {
        "Problem report".to_string()
    } else {
        let cut: String = first.chars().take(72).collect();
        format!(
            "Problem: {cut}{}",
            if cut.len() < first.len() { "…" } else { "" }
        )
    };
    let mut d = description.to_string();
    loop {
        let body = format!("{d}\n\nThe report is attached: {file} (drag it into this box).\n");
        let url = format!(
            "{ISSUES}?title={}&body={}",
            crate::reports::encode(&title),
            crate::reports::encode(&body)
        );
        if url.len() <= MAX_URL || d.is_empty() {
            return url;
        }
        let keep = d.chars().count() * 3 / 4;
        d = d.chars().take(keep).collect::<String>() + "…";
        if keep == 0 {
            d.clear();
        }
    }
}

/// Writes the archive into `dir`; returns {path, description, issue, notes} for the front-ends.
pub fn write(s: &Snapshot, dir: &Path) -> io::Result<Value> {
    let mut r = Redact::new(s.hide);
    r.computer(&s.host, &s.sender_id);
    r.login(&s.user, &s.home);
    let labels: Vec<String> = s
        .devices
        .iter()
        .map(|d| {
            let mut names: Vec<&str> = d.names.iter().map(String::as_str).collect();
            names.extend(d.welcome["name"].as_str());
            r.receiver(&d.id, &names)
        })
        .collect();
    for (id, name) in &s.others {
        r.receiver(id, &[name]);
    }
    let base = s.when.file_name();
    let path = (1..)
        .map(|n| {
            dir.join(if n == 1 {
                format!("{base}.zip")
            } else {
                format!("{base}-{n}.zip")
            })
        })
        .find(|p| !p.exists())
        .expect("some name is free");
    let file = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();

    let mut entries: Vec<(String, String)> = vec![
        ("about.txt".into(), about(s, &labels)),
        ("computer/luchd.log".into(), s.computer_log.clone()),
        ("computer/gnome-shell.log".into(), s.shell_log.clone()),
    ];
    for (d, label) in s.devices.iter().zip(&labels) {
        if let Ok(log) = &d.log {
            entries.push((format!("receivers/{}.log", file_safe(label)), log.clone()));
        }
    }
    for (n, c) in s.crashes.iter().enumerate() {
        entries.push((
            format!("crash-reports/{}.json", n + 1),
            serde_json::to_string_pretty(c).unwrap_or_default(),
        ));
    }

    // Redacted in this order (the stand-ins are numbered as they are met), kept for SendReport as written.
    let entries: Vec<(String, String)> =
        entries.into_iter().map(|(n, t)| (n, r.text(&t))).collect();

    // Written aside and renamed: a half-written report is never what is in Downloads.
    let part = path.with_extension("zip.part");
    let w = &s.when;
    let stamp = zip::DateTime::from_date_and_time(
        w.y as u16, w.mo as u8, w.d as u8, w.h as u8, w.mi as u8, w.s as u8,
    )
    .unwrap_or_default();
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(stamp);
    let written = (|| -> zip::result::ZipResult<()> {
        let mut z = zip::ZipWriter::new(std::fs::File::create(&part)?);
        for (name, text) in &entries {
            z.start_file(name.as_str(), opts)?;
            z.write_all(text.as_bytes())?;
        }
        z.finish()?;
        Ok(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&part);
        return Err(io::Error::other(e.to_string()));
    }
    std::fs::rename(&part, &path)?;

    let description = r.text(&description(s, &labels, &file));
    let notes: Vec<String> = s
        .devices
        .iter()
        .zip(&labels)
        .filter_map(|(d, l)| {
            d.log
                .as_ref()
                .err()
                .map(|why| r.text(&format!("{l}: no log, {why}")))
        })
        .collect();
    let what = r.text(&s.text);
    let sys = |k: &str| {
        s.system
            .iter()
            .find(|(key, _)| *key == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let os = sys("OS");
    let (os, kernel) = os
        .split_once(", kernel ")
        .map(|(o, k)| (o.to_string(), k.to_string()))
        .unwrap_or((os.clone(), String::new()));
    crate::report_send::keep(
        &path,
        crate::report_send::Made {
            file: file.clone(),
            what: what.clone(),
            texts: entries,
            app: luch_proto::APP.into(),
            os,
            kernel,
            machine: sys("machine"),
            models: s
                .devices
                .iter()
                .map(|d| d.model.clone())
                .filter(|m| !m.is_empty())
                .collect(),
            codec: codec(&s.devices),
        },
    );
    Ok(json!({
        "path": path.to_string_lossy(),
        "description": description,
        "issue": issue_url(&what, &description, &file),
        "notes": notes,
        "can_send": crate::report_send::dsn().is_some(),
    }))
}

/// The codec of the session now or last (the first receiver with one), from luchd's figures of it.
fn codec(devices: &[Device]) -> String {
    devices
        .iter()
        .find_map(|d| {
            d.stats.as_ref()?["sender"]["encoder"]
                .as_str()
                .map(String::from)
        })
        .map(|e| if e.contains("hevc") { "HEVC" } else { "H.264" }.to_string())
        .unwrap_or_default()
}

/// Report (docs/dbus.md): gathers everything, asks the receivers at once, writes the archive into Downloads.
pub(crate) async fn report(l: &crate::Luch, text: &str, hide: bool) -> io::Result<Value> {
    let (conns, found, welcomes, secure, mut stats) = {
        let s = l.st.lock().await;
        let mut stats: HashMap<String, Value> = s.last_stats.clone();
        for (id, live) in &s.stats {
            stats.insert(id.clone(), live.lock().unwrap().clone());
        }
        (
            s.conns.clone(),
            s.found.clone(),
            s.welcomes.clone(),
            s.secure.clone(),
            stats,
        )
    };
    let known = crate::wake::load();
    // In use now (the web receiver is a browser: it has no log to give), then the others luchd has talked to.
    let mut ids: Vec<String> = conns
        .iter()
        .filter(|(_, c)| c.media.is_none())
        .map(|(id, _)| id.clone())
        .collect();
    ids.sort();
    let mut others: Vec<String> = known
        .keys()
        .filter(|id| !conns.contains_key(*id))
        .cloned()
        .collect();
    others.sort();
    ids.extend(others);

    let asks = ids.iter().map(|id| {
        let (conn, r, welcome) = (
            conns.get(id).cloned(),
            found.get(id).cloned(),
            welcomes.get(id).cloned(),
        );
        async move {
            match (conn, r) {
                (Some(c), _) => {
                    let w = welcome.unwrap_or(Value::Null);
                    let log = if hands_over_logs(&w) {
                        timeout(WAIT, ask_channel(&c)).await.unwrap_or_else(|_| {
                            Err(format!("did not answer in {} s", WAIT.as_secs()))
                        })
                    } else {
                        Err(OLD.to_string())
                    };
                    (w, "connected", log)
                }
                (None, Some(r)) => {
                    let (w, log) = ask_paired(l, &r).await;
                    (
                        w.or(welcome).unwrap_or(Value::Null),
                        "on the network, not connected",
                        log,
                    )
                }
                (None, None) => (
                    welcome.unwrap_or(Value::Null),
                    "known, not on the network now",
                    Err("it is off or away".to_string()),
                ),
            }
        }
    });
    let journal = tokio::task::spawn_blocking(|| {
        (
            journal(
                "journalctl",
                &["--user", "-u", "luchd", "-n", "20000", "--no-pager", "-o", "short-iso"],
                LOG_BYTES,
                None,
                ": luchd may not run as the systemd user unit `luchd`, its lines went where it was started",
            ),
            journal(
                "journalctl",
                &["--user", "-b", "-n", "100000", "--no-pager", "-o", "short-iso", "_COMM=gnome-shell"],
                SHELL_BYTES,
                Some("luch"),
                " from the GNOME extension",
            ),
        )
    });
    let (answers, logs) = tokio::join!(futures_util::future::join_all(asks), journal);
    let (computer_log, shell_log) = logs.map_err(io::Error::other)?;

    let devices = ids
        .iter()
        .zip(answers)
        .map(|(id, (welcome, how, log))| {
            let r = found.get(id);
            let k = known.get(id);
            let names: Vec<String> = [
                r.map(|r| r.name.clone()),
                k.and_then(|k| k["name"].as_str().map(String::from)),
            ]
            .into_iter()
            .flatten()
            .collect();
            let model = r
                .map(|r| r.model.clone())
                .filter(|m| !m.is_empty())
                .or_else(|| k.and_then(|k| k["model"].as_str().map(String::from)))
                .or_else(|| welcome["model"].as_str().map(String::from))
                .unwrap_or_default();
            let how = match (how, secure.contains(id)) {
                ("connected", true) => "connected, TLS".to_string(),
                ("connected", false) => "connected, not encrypted".to_string(),
                (h, _) => h.to_string(),
            };
            Device {
                id: id.clone(),
                names,
                model,
                welcome,
                how,
                log,
                stats: stats.remove(id),
            }
        })
        .collect();
    let snapshot = Snapshot {
        text: text.to_string(),
        hide,
        host: l.host.clone(),
        user: std::env::var("USER").unwrap_or_default(),
        home: std::env::var("HOME").unwrap_or_default(),
        sender_id: l.sender_id.clone(),
        system: system(),
        computer_log,
        shell_log,
        others: found
            .values()
            .filter(|r| !ids.contains(&r.id))
            .map(|r| (r.id.clone(), r.name.clone()))
            .chain(l.tvs.devices().into_iter().map(|d| (d.id, d.name)))
            .collect(),
        devices,
        crashes: crate::reports::list(),
        when: When::now(),
    };
    let dir = downloads()?;
    tokio::task::spawn_blocking(move || write(&snapshot, &dir))
        .await
        .map_err(io::Error::other)?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gz(text: &str) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(text.as_bytes()).unwrap();
        e.finish().unwrap()
    }

    /// The receiver's side: `text` gzipped, cut into `parts` slices of base64, numbered from `from`.
    fn slices(text: &str, parts: usize, from: usize) -> Vec<Value> {
        let gz = gz(text);
        let size = gz.len().div_ceil(parts);
        gz.chunks(size)
            .enumerate()
            .map(|(i, c)| {
                json!({"t": "logs", "part": i + from, "parts": parts,
                       "gz": base64::engine::general_purpose::STANDARD.encode(c)})
            })
            .collect()
    }

    #[test]
    fn addresses_become_stand_ins_the_same_each_time() {
        let mut r = Redact::new(true);
        let line = "connected to 192.168.7.20:47800 via wlan0, then 10.1.2.3, again 192.168.7.20.";
        assert_eq!(
            r.text(line),
            "connected to 192.0.2.1:47800 via wlan0, then 192.0.2.2, again 192.0.2.1."
        );
        assert_eq!(
            r.text("from 10.1.2.3"),
            "from 192.0.2.2",
            "stable across texts of one report"
        );
        assert_eq!(
            r.text("v6 fe80::1c2b:3aff:fe4d:5e6f%wlan0 and [2a02:6b8::42]:47800, fe80::1c2b:3aff:fe4d:5e6f:"),
            "v6 2001:db8::1%wlan0 and [2001:db8::2]:47800, 2001:db8::1:"
        );
        assert_eq!(
            r.text("mac AA:BB:CC:11:22:33 or aa-bb-cc-11-22-33, then 0a:1b:2c:3d:4e:5f"),
            "mac 02:00:00:00:00:01 or 02:00:00:00:00:01, then 02:00:00:00:00:02"
        );
    }

    #[test]
    fn what_is_no_ones_address_is_left_alone() {
        let mut r = Redact::new(true);
        for kept in [
            "2026-11-28T14:05:09+0100 luchd[1234]: listening on 0.0.0.0:47801 and 127.0.0.1, mdns 224.0.0.251",
            "loopback ::1, any ::, multicast ff02::fb, broadcast ff:ff:ff:ff:ff:ff, 255.255.255.255",
            "luchd 0.4.0, gst 1.24.2, std::io::Error, luchd::main, a::b, id 3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47",
            "deadbeef cafe 12:34:56 1.2.3 v192.168.1.1 already 192.0.2.7 2001:db8::5 02:00:00:00:00:09",
        ] {
            assert_eq!(r.text(kept), kept);
        }
    }

    #[test]
    fn names_become_labels_whole_words_only() {
        let mut r = Redact::new(true);
        r.computer("anna-laptop", "11111111-2222-3333-4444-555555555555");
        r.login("anna", "/home/anna");
        let tv = r.receiver(
            "aaaaaaaa-0000-4000-8000-000000000001",
            &["Living room TV", "Living room TV"],
        );
        let kitchen = r.receiver("aaaaaaaa-0000-4000-8000-000000000002", &["Кухня", "TV"]);
        assert_eq!(
            (tv.as_str(), kitchen.as_str()),
            ("Receiver 1", "Receiver 2")
        );
        assert_eq!(
            r.receiver("aaaaaaaa-0000-4000-8000-000000000001", &["other"]),
            "Receiver 1"
        );
        assert_eq!(
            r.text("anna-laptop: connected to living room tv (Кухня off); /home/anna/Videos/a.mkv by anna; annabelle"),
            "Computer: connected to Receiver 1 (Receiver 2 off); ~/Videos/a.mkv by user; annabelle"
        );
        assert_eq!(
            r.text("hello id=11111111-2222-3333-4444-555555555555 to aaaaaaaa-0000-4000-8000-000000000002"),
            "hello id=computer-id to receiver-2-id"
        );
        assert_eq!(
            r.text("a TV, linux, Luch"),
            "a TV, linux, Luch",
            "short and common words stay"
        );
    }

    #[test]
    fn kept_names_change_nothing() {
        let mut r = Redact::new(false);
        r.computer("anna-laptop", "x");
        assert_eq!(
            r.receiver("id-1", &["", "Living room TV"]),
            "Living room TV"
        );
        let line = "anna-laptop 192.168.7.20 aa:bb:cc:11:22:33 Living room TV";
        assert_eq!(r.text(line), line);
    }

    #[test]
    fn a_log_in_slices_is_joined_in_any_order_and_unpacked() {
        let text = "12-01 10:00:00.000 I Luch: session start hevc 1920x1080\n".repeat(500);
        let mut parts = slices(&text, 3, 0);
        parts.reverse();
        let mut s = Slices::default();
        assert!(s.take(&parts[0]).is_none());
        assert!(
            s.take(&json!({"t": "stats"})).is_none(),
            "other messages pass by"
        );
        assert!(s.take(&parts[1]).is_none());
        assert_eq!(s.take(&parts[2]), Some(Ok(text.clone())));

        let mut s = Slices::default();
        let one_based = slices(&text, 2, 1);
        assert!(s.take(&one_based[0]).is_none());
        assert_eq!(s.take(&one_based[1]), Some(Ok(text)));
    }

    #[test]
    fn a_refusal_or_a_bad_slice_ends_the_wait() {
        let mut s = Slices::default();
        let no =
            json!({"t": "error", "for": "logs", "code": "not_allowed", "detail": "pair first"});
        assert_eq!(s.take(&no), Some(Err("not_allowed: pair first".into())));
        assert!(Slices::default()
            .take(&json!({"t": "error", "for": "media", "code": "x"}))
            .is_none());
        let bad = json!({"t": "logs", "part": 0, "parts": 1, "gz": "@@@"});
        assert!(Slices::default().take(&bad).unwrap().is_err());
        let huge = json!({"t": "logs", "part": 0, "parts": 100000, "gz": ""});
        assert!(Slices::default().take(&huge).unwrap().is_err());
        let not_gz = json!({"t": "logs", "part": 0, "parts": 1, "gz": "aGVsbG8="});
        assert!(Slices::default().take(&not_gz).unwrap().is_err());
    }

    #[test]
    fn a_log_that_unpacks_endlessly_is_cut() {
        let zeros = vec![b'0'; MAX_TEXT as usize + 4096];
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        e.write_all(&zeros).unwrap();
        let gz = e.finish().unwrap();
        assert_eq!(gunzip(&gz).unwrap().len(), MAX_TEXT as usize);
    }

    #[test]
    fn the_journal_is_its_tail_or_says_why_not() {
        let missing = journal("luch-no-such-journalctl", &[], 100, None, "");
        assert_eq!(
            missing,
            "(luch-no-such-journalctl is not installed: no log from the journal)\n"
        );
        let lines = journal(
            "sh",
            &["-c", "printf 'one\\ntwo\\nthree\\nfour\\n'"],
            12,
            None,
            "",
        );
        assert_eq!(
            lines, "three\nfour\n",
            "the last bytes, from the start of a line"
        );
        let only = journal(
            "sh",
            &["-c", "printf 'a Luch: x\\nother\\nluch: y\\n'"],
            100,
            Some("luch"),
            "",
        );
        assert_eq!(only, "a Luch: x\nluch: y\n");
        let none = journal(
            "sh",
            &["-c", "echo '-- No entries --'"],
            100,
            None,
            ": a hint",
        );
        assert_eq!(none, "(no lines: a hint)\n");
    }

    #[test]
    fn downloads_come_from_user_dirs() {
        let home = Path::new("/home/someone");
        let dirs = "# written by xdg-user-dirs-update\nXDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_DOWNLOAD_DIR=\"$HOME/Загрузки\"\n";
        assert_eq!(download_dir_from(dirs, home), Some(home.join("Загрузки")));
        assert_eq!(
            download_dir_from("XDG_DOWNLOAD_DIR=\"/data/dl\"", home),
            Some(PathBuf::from("/data/dl"))
        );
        assert_eq!(
            download_dir_from("XDG_DOWNLOAD_DIR=\"$HOME/\"", home),
            None,
            "home itself is not it"
        );
        assert_eq!(
            download_dir_from("XDG_MUSIC_DIR=\"$HOME/Music\"", home),
            None
        );
    }

    fn snapshot(hide: bool) -> Snapshot {
        Snapshot {
            text: "HEVC stutters on Living room TV\nsecond line".into(),
            hide,
            host: "anna-laptop".into(),
            user: "anna".into(),
            home: "/home/anna".into(),
            sender_id: "11111111-2222-3333-4444-555555555555".into(),
            system: vec![
                ("Luch", "luchd/0.4.0".into()),
                ("OS", "Example OS 1, kernel 6.8".into()),
            ],
            computer_log: "luchd: connected to Living room TV (192.168.7.20:47800 via wlan0)\n\
                           luchd: found (dlna): Neighbours' Telly\n"
                .into(),
            shell_log: "(no lines from the GNOME extension)\n".into(),
            devices: vec![
                Device {
                    id: "aaaaaaaa-0000-4000-8000-000000000001".into(),
                    names: vec!["Living room TV".into()],
                    model: "Acme Box".into(),
                    welcome: json!({"t": "welcome", "name": "Living room TV", "sdk": 22, "app": "0.4",
                                    "decoders": ["video/avc", "video/hevc"], "max": "1920x1080", "modes": ["logs"]}),
                    how: "connected, TLS".into(),
                    log: Ok("I Luch: hello from anna-laptop at 192.168.7.5\n".into()),
                    stats: Some(json!({"fps": 59.9})),
                },
                Device {
                    id: "aaaaaaaa-0000-4000-8000-000000000002".into(),
                    names: vec!["Bedroom".into()],
                    model: String::new(),
                    welcome: Value::Null,
                    how: "known, not on the network now".into(),
                    log: Err("it is off or away".into()),
                    stats: None,
                },
            ],
            others: vec![("dlna:uuid:0000".into(), "Neighbours' Telly".into())],
            crashes: vec![
                json!({"kind": "error", "what": "no decoder", "receiver_model": "Acme Box",
                                 "app": "0.4", "android": "5.1"}),
            ],
            when: When {
                y: 2026,
                mo: 1,
                d: 2,
                h: 3,
                mi: 4,
                s: 5,
            },
        }
    }

    fn unzip(path: &str) -> HashMap<String, String> {
        let mut a = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
        (0..a.len())
            .map(|i| {
                let mut f = a.by_index(i).unwrap();
                let mut s = String::new();
                f.read_to_string(&mut s).unwrap();
                (f.name().to_string(), s)
            })
            .collect()
    }

    #[test]
    fn the_archive_holds_everything_with_names_hidden() {
        let dir = std::env::temp_dir().join(format!("luch-problem-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = write(&snapshot(true), &dir).unwrap();
        let path = out["path"].as_str().unwrap();
        assert!(path.ends_with("Luch-report-2026-01-02-0304.zip"), "{path}");
        let files = unzip(path);
        let mut names: Vec<&str> = files.keys().map(String::as_str).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "about.txt",
                "computer/gnome-shell.log",
                "computer/luchd.log",
                "crash-reports/1.json",
                "receivers/Receiver 1.log"
            ]
        );
        let all: String = files.values().cloned().collect();
        assert!(files["computer/luchd.log"].contains("found (dlna): Receiver 3\n"));
        for secret in [
            "Telly",
            "anna",
            "Living room",
            "Bedroom",
            "192.168.",
            "11111111-2222",
            "aaaaaaaa-0000",
        ] {
            assert!(!all.contains(secret), "{secret} leaked");
        }
        assert_eq!(
            files["receivers/Receiver 1.log"],
            "I Luch: hello from Computer at 192.0.2.2\n"
        );
        assert!(files["computer/luchd.log"]
            .contains("connected to Receiver 1 (192.0.2.1:47800 via wlan0)"));
        let about = &files["about.txt"];
        assert!(
            about.contains("  HEVC stutters on Receiver 1\n  second line\n"),
            "{about}"
        );
        assert!(
            about.contains("  Receiver 1 (Acme Box)\n    connected, TLS; Android 5.1, Luch 0.4\n"),
            "{about}"
        );
        assert!(about.contains("decoders: video/avc, video/hevc; max 1920x1080; modes: logs"));
        assert!(about.contains(
            "  Receiver 2\n    known, not on the network now\n    log: none, it is off or away"
        ));
        assert!(about.contains("session (now or last): {\"fps\":59.9}"));
        assert!(about.contains("Crash and error reports kept: 1\n  Acme Box error: no decoder"));
        assert_eq!(
            out["description"],
            format!(
                "What happened: HEVC stutters on Receiver 1 second line\nSender: {} on Example OS 1, kernel 6.8\n\
                 Receivers: Receiver 1 (Acme Box, Android 5.1, Luch 0.4); Receiver 2\n\
                 Report: Luch-report-2026-01-02-0304.zip",
                luch_proto::APP
            )
        );
        assert_eq!(
            out["notes"],
            json!(["Receiver 2: no log, it is off or away"])
        );
        let issue = out["issue"].as_str().unwrap();
        assert!(issue.starts_with(&format!(
            "{ISSUES}?title=Problem%3A%20HEVC%20stutters%20on%20Receiver%201&body="
        )));
        assert!(issue.contains("Luch-report-2026-01-02-0304.zip"));
        // A second report the same minute does not overwrite the first.
        let again = write(&snapshot(true), &dir).unwrap();
        assert!(again["path"]
            .as_str()
            .unwrap()
            .ends_with("Luch-report-2026-01-02-0304-2.zip"));
        assert!(!dir.join("Luch-report-2026-01-02-0304.zip.part").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn names_are_kept_when_asked() {
        let dir = std::env::temp_dir().join(format!("luch-problem-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = write(&snapshot(false), &dir).unwrap();
        let files = unzip(out["path"].as_str().unwrap());
        assert_eq!(
            files["receivers/Living room TV.log"],
            "I Luch: hello from anna-laptop at 192.168.7.5\n"
        );
        assert!(out["description"]
            .as_str()
            .unwrap()
            .contains("Living room TV (Acme Box"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_sent_report_is_as_hidden_as_its_file() {
        let dir = std::env::temp_dir().join(format!("luch-problem-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = snapshot(true);
        s.devices[0].stats = Some(json!({"sender": {"encoder": "hevc_vaapi"}}));
        let out = write(&s, &dir).unwrap();
        let path = out["path"].as_str().unwrap();
        let files = unzip(path);
        let (port, mut got) =
            crate::report_send::tests::fake_server(vec![(200, "{}"), (200, "{}")]).await;
        let dsn = crate::report_send::Dsn::parse(&format!("http://k@127.0.0.1:{port}/1")).unwrap();
        crate::report_send::send(&reqwest::Client::new(), &dsn, Path::new(path))
            .await
            .unwrap();
        let body = got.recv().await.unwrap().body;
        let (_, items) = crate::report_send::tests::unframe(&body);
        let e: Value = serde_json::from_slice(&items[0].1).unwrap();
        let text = String::from_utf8_lossy(&items[0].1);
        for secret in [
            "Telly",
            "anna",
            "Living room",
            "Bedroom",
            "192.168.",
            "11111111-2222",
            "aaaaaaaa-0000",
        ] {
            assert!(!text.contains(secret), "{secret} leaked");
        }
        assert_eq!(e["message"], "Problem report: HEVC stutters on Receiver 1");
        assert_eq!(
            e["extra"]["what happened"],
            "HEVC stutters on Receiver 1\nsecond line"
        );
        for (name, text) in &files {
            assert_eq!(e["extra"][name], text.as_str(), "{name} as in the file");
        }
        assert_eq!(
            e["tags"],
            json!({"app": luch_proto::APP, "os": "Example OS 1", "receivers": "Acme Box", "codec": "HEVC"})
        );
        assert_eq!(
            e["contexts"]["os"],
            json!({"name": "Example OS 1", "kernel_version": "6.8"})
        );
        let zip = got.recv().await.unwrap().body;
        let (_, items) = crate::report_send::tests::unframe(&zip);
        assert_eq!(items[0].1, std::fs::read(path).unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_long_description_is_cut_to_fit_a_link() {
        let long = "x".repeat(20000);
        let url = issue_url(&long, &long, "Luch-report.zip");
        assert!(url.len() <= MAX_URL, "{}", url.len());
        assert!(url.contains("%E2%80%A6"), "the cut is marked");
        assert!(
            url.contains("Luch-report.zip"),
            "the file is still asked for"
        );
    }

    #[test]
    fn file_names_cannot_climb_out() {
        assert_eq!(file_safe("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(file_safe(".."), "receiver");
        assert_eq!(file_safe("Living room TV"), "Living room TV");
    }

    /// The paired receiver luchd is not connected to: over its own TLS channel to a TV made of Rust, it answers a
    /// ping and hands over a crash report before its log in two slices; the report is taken, the log is whole.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // held on purpose: the test points XDG_CONFIG_HOME at its own directory
    async fn a_paired_receiver_hands_its_log_over_its_own_channel() {
        let _env = crate::tests::CONFIG_HOME
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!("luch-logs-{}", uuid::Uuid::new_v4()));
        std::env::set_var("XDG_CONFIG_HOME", &home);
        std::fs::create_dir_all(&home).unwrap();
        let me = crate::pair::Identity::load(&home).unwrap();
        let tv = crate::tests::tv::Tv::new();
        let text = "I Luch: decoder c2.mtk.hevc.decoder 1920x1080\n".repeat(200);
        let parts = slices(&text, 2, 0);
        let (taken_tx, taken_rx) = tokio::sync::oneshot::channel();
        let addr = tv
            .serve(move |hello, mut rd, mut wr, _| async move {
                assert_eq!(hello["want"], json!(["reports"]));
                let welcome = json!({"t": "welcome", "v": 1, "name": "TV", "modes": ["logs"]});
                crate::send(&mut wr, &welcome).await.unwrap();
                let ask = crate::read_msg(&mut rd).await.unwrap().unwrap();
                assert_eq!(ask, json!({"t": "logs", "max_kb": 1024}));
                crate::send(&mut wr, &json!({"t": "ping", "n": 7}))
                    .await
                    .unwrap();
                assert_eq!(
                    crate::read_msg(&mut rd).await.unwrap().unwrap(),
                    json!({"t": "pong", "n": 7})
                );
                let id = "3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47";
                let report =
                    json!({"t": "reports", "items": [{"id": id, "kind": "crash", "time": 1}]});
                crate::send(&mut wr, &report).await.unwrap();
                let taken = crate::read_msg(&mut rd).await.unwrap().unwrap();
                for p in parts {
                    crate::send(&mut wr, &p).await.unwrap();
                }
                let _ = taken_tx.send(taken);
                let _ = crate::read_msg(&mut rd).await; // the bye
            })
            .await;
        let s = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut hello = crate::hello("sender-id", "Linux Laptop", None, false);
        hello["want"] = json!(["reports"]);
        let mut o = crate::open_channel(s, &hello, Some(&me), Some(tv.fp))
            .await
            .unwrap();
        assert!(hands_over_logs(&o.first));
        let log = timeout(WAIT, ask_over(&mut o.rd, &mut o.wr, "Acme Box"))
            .await
            .unwrap();
        assert_eq!(log, Ok(text));
        let taken = taken_rx.await.unwrap();
        assert_eq!(taken["t"], "reports_taken");
        assert_eq!(crate::reports::list().len(), 1);
        let _ = std::fs::remove_dir_all(home);
    }

    /// A receiver luchd is connected to: the request goes through its channel and the slices come back through it.
    /// Needs a session bus (the channel signals changes on it); skipped without one.
    #[tokio::test]
    async fn a_connected_receiver_hands_its_log_through_its_channel() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let Ok(dbus) = zbus::Connection::session().await else {
            eprintln!("no session bus: skipped");
            return;
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (client, (server, _)) = tokio::join!(tokio::net::TcpStream::connect(addr), async {
            listener.accept().await.unwrap()
        });
        let (rd, wr) = client.unwrap().into_split();
        let (cmd, cmd_rx) = tokio::sync::mpsc::channel(8);
        let (events, _) = broadcast::channel(64);
        let c = Conn {
            cmd,
            events: events.clone(),
            media: None,
            addrs: None,
        };
        tokio::spawn(crate::run_channel(
            "TV".into(),
            "rx".into(),
            tokio::io::BufReader::new(Box::new(rd) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
            Box::new(wr) as Writer,
            cmd_rx,
            events,
            crate::Shared::default(),
            dbus,
        ));
        let text = "I Luch: stats fps=60\n".repeat(100);
        let parts = slices(&text, 3, 0);
        tokio::spawn(async move {
            let (srd, mut swr) = server.into_split();
            let mut srd = tokio::io::BufReader::new(srd);
            loop {
                let mut l = String::new();
                if srd.read_line(&mut l).await.unwrap_or(0) == 0 {
                    return;
                }
                if serde_json::from_str::<Value>(&l).unwrap()["t"] == "logs" {
                    for p in &parts {
                        swr.write_all(luch_proto::control::line(p).as_bytes())
                            .await
                            .unwrap();
                    }
                }
            }
        });
        assert_eq!(timeout(WAIT, ask_channel(&c)).await.unwrap(), Ok(text));
    }
}
