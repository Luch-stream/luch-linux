//! "Send to the developers" (docs/dbus.md, SendReport): a report Report made goes, at the person's press, to the
//! developers' GlitchTip (a Sentry-compatible server) as one Sentry envelope with an event (what happened, about.txt
//! and the tails of the logs, as redacted as the file) and, best effort, a second with the zip attached. No account:
//! the DSN's key is baked into the build and only says "this is Luch". Nothing is sent by itself.

use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The DSN this build sends to; empty (the default) hides the button. A packager sets it with
/// `LUCH_REPORT_DSN=... cargo build`; the same variable at run time overrides it.
const DSN: &str = match option_env!("LUCH_REPORT_DSN") {
    Some(d) => d,
    None => "",
};
/// Each text in the event at most (its tail), and all of them: GlitchTip takes events up to 1 MB.
const EACH: usize = 64 * 1024;
const TOTAL: usize = 700 * 1024;
const WAIT: Duration = Duration::from_secs(60);

/// Where a DSN says events go.
#[derive(Debug, PartialEq)]
pub struct Dsn {
    pub key: String,
    /// `<scheme>://<host>/<path>/api/<project>/envelope/`.
    pub url: String,
}

impl Dsn {
    /// `https://<public_key>@<host>[:port][/path]/<project_id>`.
    pub fn parse(dsn: &str) -> Result<Dsn, String> {
        let bad = || format!("not a DSN: {dsn}");
        let (scheme, rest) = dsn.trim().split_once("://").ok_or_else(bad)?;
        if scheme != "https" && scheme != "http" {
            return Err(bad());
        }
        let (key, rest) = rest.split_once('@').ok_or_else(bad)?;
        // A secret after the key (key:secret) is the old form; the key alone authenticates.
        let key = key.split(':').next().unwrap_or("");
        let (host, path) = rest.split_once('/').ok_or_else(bad)?;
        let path = path.trim_end_matches('/');
        let (prefix, project) = match path.rsplit_once('/') {
            Some((p, id)) => (format!("/{p}"), id),
            None => (String::new(), path),
        };
        if key.is_empty()
            || host.is_empty()
            || project.is_empty()
            || !project.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(bad());
        }
        Ok(Dsn {
            key: key.to_string(),
            url: format!("{scheme}://{host}{prefix}/api/{project}/envelope/"),
        })
    }
}

/// The DSN to send to: `LUCH_REPORT_DSN` when set, else the build's; None when sending is off.
pub fn dsn() -> Option<Dsn> {
    let d = std::env::var("LUCH_REPORT_DSN").unwrap_or_else(|_| DSN.to_string());
    if d.trim().is_empty() {
        return None;
    }
    Dsn::parse(&d)
        .map_err(|e| eprintln!("[luchd] report: {e}"))
        .ok()
}

/// What a report made by this luchd holds for its event, all as redacted as the file.
#[derive(Clone)]
pub struct Made {
    pub file: String,
    pub what: String,
    /// The archive's texts (name, text), about.txt first.
    pub texts: Vec<(String, String)>,
    /// "luchd/0.4.0".
    pub app: String,
    pub os: String,
    pub kernel: String,
    pub machine: String,
    pub models: Vec<String>,
    pub codec: String,
}

/// The reports this luchd made, by path: only these are sent, so no other file can leave through SendReport.
static MADE: Mutex<Option<HashMap<PathBuf, Made>>> = Mutex::new(None);

pub fn keep(path: &Path, made: Made) {
    MADE.lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(path.to_path_buf(), made);
}

fn made(path: &Path) -> Option<Made> {
    MADE.lock().unwrap().as_ref()?.get(path).cloned()
}

/// "LUCH-" and the event id's first 8 hex digits: what the person tells in a chat, and finds the event by.
pub fn number(event_id: &str) -> String {
    format!("LUCH-{}", event_id[..8.min(event_id.len())].to_uppercase())
}

/// A Sentry envelope: the header line, then each item's header (with its payload's `length`) and payload.
pub fn envelope(header: &Value, items: &[(Value, &[u8])]) -> Vec<u8> {
    let mut out = header.to_string().into_bytes();
    out.push(b'\n');
    for (h, payload) in items {
        let mut h = h.clone();
        h["length"] = json!(payload.len());
        out.extend(h.to_string().as_bytes());
        out.push(b'\n');
        out.extend(*payload);
        out.push(b'\n');
    }
    out
}

/// `2026-01-02T03:04:05Z` for a unix time.
pub fn utc(secs: u64) -> String {
    let (days, s) = ((secs / 86400) as i64, secs % 86400);
    // Days to a civil date (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

/// At most `max` bytes of `s`'s end, from a char boundary.
fn tail(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut i = s.len() - max;
    while !s.is_char_boundary(i) {
        i += 1;
    }
    &s[i..]
}

/// A tag's value: one line, 200 characters at most (Sentry's limit).
fn tag(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect()
}

/// The event for a report: its title is what happened's first line, its `extra` the texts of the file (tails).
pub fn event(m: &Made, event_id: &str, now: u64) -> Value {
    let first = m.what.trim().lines().next().unwrap_or("").trim();
    let title: String = first.chars().take(100).collect();
    let mut tags = Map::new();
    for (k, v) in [
        ("app", m.app.clone()),
        ("os", m.os.clone()),
        ("receivers", m.models.join("; ")),
        ("codec", m.codec.clone()),
    ] {
        if !v.is_empty() {
            tags.insert(k.into(), json!(tag(&v)));
        }
    }
    let mut extra = Map::new();
    let mut left = TOTAL;
    let what = tail(m.what.trim(), EACH);
    left -= what.len();
    extra.insert("what happened".into(), json!(what));
    for (name, text) in &m.texts {
        let t = tail(text, EACH.min(left));
        left -= t.len();
        extra.insert(name.clone(), json!(t));
    }
    let version = m.app.split('/').nth(1).unwrap_or("?");
    json!({
        "event_id": event_id,
        "timestamp": now,
        "level": "info",
        "platform": "other",
        "logger": "luch.report",
        "message": if title.is_empty() { "Problem report".to_string() } else { format!("Problem report: {title}") },
        "release": format!("luch-linux@{version}"),
        "tags": tags,
        "contexts": {
            "os": {"name": m.os, "kernel_version": m.kernel},
            "device": {"model": m.machine},
        },
        "extra": extra,
    })
}

/// What the server answered, for the person: nothing on a 2xx, else why not.
pub fn answer(status: u16, body: &str, retry_after: Option<&str>) -> Result<(), String> {
    if (200..300).contains(&status) {
        return Ok(());
    }
    // GlitchTip says why in `detail`; a proxy in front of it may answer with a page instead.
    let detail = match serde_json::from_str::<Value>(body) {
        Ok(v) => v["detail"]
            .as_str()
            .or(v["error"].as_str())
            .unwrap_or("")
            .to_string(),
        Err(_) => tag(body),
    };
    Err(match status {
        429 => format!(
            "the developers' server takes no more reports now, try again {}",
            match retry_after.and_then(|s| s.trim().parse::<u64>().ok()) {
                Some(s) if s >= 90 => format!("in {} min", s.div_ceil(60)),
                Some(s) => format!("in {s} s"),
                None => "later".into(),
            }
        ),
        401 | 403 => {
            format!("the developers' server did not take this build's key (HTTP {status})")
        }
        413 => "the report is too big for the developers' server".into(),
        _ if detail.is_empty() => format!("the developers' server answered HTTP {status}"),
        _ => format!("the developers' server answered HTTP {status}: {detail}"),
    })
}

async fn post(http: &reqwest::Client, dsn: &Dsn, body: Vec<u8>) -> Result<(), String> {
    let r = http
        .post(&dsn.url)
        .header(
            "X-Sentry-Auth",
            format!(
                "Sentry sentry_version=7, sentry_key={}, sentry_client=luch/{}",
                dsn.key,
                env!("CARGO_PKG_VERSION")
            ),
        )
        .header("Content-Type", "application/x-sentry-envelope")
        .timeout(WAIT)
        .body(body)
        .send()
        .await
        .map_err(|e| format!("cannot reach the developers' server: {e}"))?;
    let status = r.status().as_u16();
    let retry = r
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let body = r.text().await.unwrap_or_default();
    answer(status, &body, retry.as_deref())
}

/// Sends the report at `path` (one this luchd made): the event, then the zip. JSON: `number`, `event_id`,
/// `attached` and, when the zip did not go, `attach_error`.
pub async fn send(http: &reqwest::Client, dsn: &Dsn, path: &Path) -> Result<Value, String> {
    let m = made(path).ok_or_else(|| {
        format!(
            "{} is not a report luchd made since it started: make a new one",
            path.display()
        )
    })?;
    let zip = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", m.file))?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let header = json!({"event_id": id, "sent_at": utc(now)});
    let ev = event(&m, &id, now).to_string();
    post(
        http,
        dsn,
        envelope(&header, &[(json!({"type": "event"}), ev.as_bytes())]),
    )
    .await?;
    // GlitchTip may refuse attachments (glitchtip-backend #241): the event is in, so this cannot fail the send.
    let attach = json!({"type": "attachment", "filename": m.file, "content_type": "application/zip",
                        "attachment_type": "event.attachment"});
    let attached = post(http, dsn, envelope(&header, &[(attach, &zip)])).await;
    if let Err(e) = &attached {
        eprintln!("[luchd] report {}: the zip was not taken: {e}", number(&id));
    }
    let mut out = json!({"number": number(&id), "event_id": id, "attached": attached.is_ok()});
    if let Err(e) = attached {
        out["attach_error"] = json!(e);
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// One request a fake server got: its first line, headers (names lowercased) and body.
    pub struct Got {
        pub line: String,
        pub headers: HashMap<String, String>,
        pub body: Vec<u8>,
    }

    /// A loopback HTTP server answering each request with the next of `answers` (status, body); what it got comes
    /// back on the channel.
    pub async fn fake_server(
        answers: Vec<(u16, &'static str)>,
    ) -> (u16, tokio::sync::mpsc::UnboundedReceiver<Got>) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            for (status, reply) in answers {
                let (mut c, _) = l.accept().await.unwrap();
                let mut buf = Vec::new();
                let end = loop {
                    let mut chunk = [0u8; 8192];
                    let n = c.read(&mut chunk).await.unwrap();
                    assert!(n > 0, "the request ended early");
                    buf.extend(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..end]).to_string();
                let mut lines = head.split("\r\n");
                let line = lines.next().unwrap().to_string();
                let headers: HashMap<String, String> = lines
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
                    .collect();
                let len: usize = headers["content-length"].parse().unwrap();
                let mut body = buf[end + 4..].to_vec();
                while body.len() < len {
                    let mut chunk = [0u8; 65536];
                    let n = c.read(&mut chunk).await.unwrap();
                    assert!(n > 0, "the body ended early");
                    body.extend(&chunk[..n]);
                }
                tx.send(Got {
                    line,
                    headers,
                    body,
                })
                .unwrap();
                let out = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{reply}",
                    reply.len()
                );
                c.write_all(out.as_bytes()).await.unwrap();
            }
        });
        (port, rx)
    }

    /// An envelope's lines: (header, [(item header, payload)]), the payloads taken by their `length`.
    pub fn unframe(b: &[u8]) -> (Value, Vec<(Value, Vec<u8>)>) {
        let nl = b.iter().position(|&c| c == b'\n').unwrap();
        let header = serde_json::from_slice(&b[..nl]).unwrap();
        let mut rest = &b[nl + 1..];
        let mut out = Vec::new();
        while !rest.is_empty() {
            let nl = rest.iter().position(|&c| c == b'\n').unwrap();
            let h: Value = serde_json::from_slice(&rest[..nl]).unwrap();
            let len = h["length"].as_u64().unwrap() as usize;
            let payload = rest[nl + 1..nl + 1 + len].to_vec();
            assert_eq!(rest[nl + 1 + len], b'\n', "a payload ends with a newline");
            rest = &rest[nl + 2 + len..];
            out.push((h, payload));
        }
        (header, out)
    }

    #[test]
    fn a_dsn_names_the_key_and_the_envelope_endpoint() {
        assert_eq!(
            Dsn::parse("https://0123abcd@reports.example.org/7").unwrap(),
            Dsn {
                key: "0123abcd".into(),
                url: "https://reports.example.org/api/7/envelope/".into()
            }
        );
        assert_eq!(
            Dsn::parse("http://k:secret@127.0.0.1:8000/glitchtip/12/").unwrap(),
            Dsn {
                key: "k".into(),
                url: "http://127.0.0.1:8000/glitchtip/api/12/envelope/".into()
            }
        );
        for bad in [
            "",
            "reports.example.org/7",
            "https://reports.example.org/7",
            "https://k@reports.example.org",
            "https://k@reports.example.org/seven",
            "ftp://k@reports.example.org/7",
        ] {
            assert!(Dsn::parse(bad).is_err(), "{bad}");
        }
    }

    /// The same bytes as the Mac's ReportSenderTests (the senders frame envelopes alike).
    #[test]
    fn an_envelope_frames_each_item_with_its_length() {
        let header =
            json!({"event_id": "0123456789abcdef0123456789abcdef", "sent_at": utc(1_767_323_045)});
        let b = envelope(
            &header,
            &[
                (json!({"type": "event"}), br#"{"a":1}"#),
                (
                    json!({"type": "attachment", "filename": "r.zip"}),
                    b"PK\x03\x04\n\n",
                ),
            ],
        );
        assert_eq!(
            b,
            b"{\"event_id\":\"0123456789abcdef0123456789abcdef\",\"sent_at\":\"2026-01-02T03:04:05Z\"}\n\
              {\"length\":7,\"type\":\"event\"}\n{\"a\":1}\n\
              {\"filename\":\"r.zip\",\"length\":6,\"type\":\"attachment\"}\nPK\x03\x04\n\n\n"
        );
    }

    #[test]
    fn utc_dates() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(951_868_799), "2000-02-29T23:59:59Z");
        assert_eq!(utc(1_767_323_045), "2026-01-02T03:04:05Z");
    }

    #[test]
    fn the_number_is_the_ids_start() {
        assert_eq!(number("7f3c0a1b2c3d4e5f6a7b8c9d0e1f2a3b"), "LUCH-7F3C0A1B");
    }

    fn sample() -> Made {
        Made {
            file: "Luch-report-2026-01-02-0304.zip".into(),
            what: "HEVC stutters on Receiver 1\nsecond line".into(),
            texts: vec![
                ("about.txt".into(), "Luch problem report\n".into()),
                ("computer/luchd.log".into(), "x".repeat(100_000)),
            ],
            app: "luchd/0.4.0".into(),
            os: "Example OS 1".into(),
            kernel: "6.8".into(),
            machine: "Acme Laptop".into(),
            models: vec!["Acme Box".into()],
            codec: "HEVC".into(),
        }
    }

    #[test]
    fn the_event_is_titled_by_what_happened_and_holds_the_tails() {
        let e = event(&sample(), "0123456789abcdef0123456789abcdef", 1_767_323_045);
        assert_eq!(e["message"], "Problem report: HEVC stutters on Receiver 1");
        assert_eq!(e["level"], "info");
        assert_eq!(e["platform"], "other");
        assert_eq!(e["release"], "luch-linux@0.4.0");
        assert_eq!(
            e["tags"],
            json!({"app": "luchd/0.4.0", "os": "Example OS 1", "receivers": "Acme Box", "codec": "HEVC"})
        );
        assert_eq!(e["contexts"]["device"]["model"], "Acme Laptop");
        assert_eq!(
            e["extra"]["what happened"],
            "HEVC stutters on Receiver 1\nsecond line"
        );
        assert_eq!(e["extra"]["about.txt"], "Luch problem report\n");
        assert_eq!(
            e["extra"]["computer/luchd.log"].as_str().unwrap().len(),
            EACH
        );
    }

    #[test]
    fn many_logs_keep_the_event_under_its_limit() {
        let mut m = sample();
        m.texts = (0..30)
            .map(|i| (format!("receivers/{i}.log"), "é".repeat(50_000)))
            .collect();
        m.what = String::new();
        let e = event(&m, "0123456789abcdef0123456789abcdef", 0);
        assert!(e.to_string().len() < 800 * 1024);
        assert_eq!(e["message"], "Problem report");
        assert!(e["extra"]["receivers/0.log"].as_str().unwrap().len() <= EACH);
        assert_eq!(
            e["extra"]["receivers/29.log"], "",
            "nothing left for the last ones"
        );
    }

    #[test]
    fn a_refusal_says_why() {
        assert_eq!(answer(200, r#"{"id":"x"}"#, None), Ok(()));
        assert_eq!(
            answer(429, "", Some("120")),
            Err("the developers' server takes no more reports now, try again in 2 min".into())
        );
        assert_eq!(
            answer(429, "", None),
            Err("the developers' server takes no more reports now, try again later".into())
        );
        assert_eq!(
            answer(403, r#"{"detail":"bad key"}"#, None),
            Err("the developers' server did not take this build's key (HTTP 403)".into())
        );
        assert_eq!(
            answer(400, r#"{"detail":"invalid envelope"}"#, None),
            Err("the developers' server answered HTTP 400: invalid envelope".into())
        );
        assert_eq!(
            answer(502, "", None),
            Err("the developers' server answered HTTP 502".into())
        );
        assert_eq!(
            answer(500, "{}", None),
            Err("the developers' server answered HTTP 500".into())
        );
    }

    #[tokio::test]
    async fn a_report_goes_as_an_event_then_its_zip() {
        let dir = std::env::temp_dir().join(format!("luch-send-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Luch-report-2026-01-02-0304.zip");
        std::fs::write(&path, b"PK\x03\x04 zip bytes").unwrap();
        keep(&path, sample());
        let (port, mut got) = fake_server(vec![(200, r#"{"id":"x"}"#), (200, "{}")]).await;
        let dsn = Dsn::parse(&format!("http://pubkey@127.0.0.1:{port}/3")).unwrap();
        let r = send(&reqwest::Client::new(), &dsn, &path).await.unwrap();

        let ev = got.recv().await.unwrap();
        assert_eq!(ev.line, "POST /api/3/envelope/ HTTP/1.1");
        assert_eq!(
            ev.headers["x-sentry-auth"],
            format!(
                "Sentry sentry_version=7, sentry_key=pubkey, sentry_client=luch/{}",
                env!("CARGO_PKG_VERSION")
            )
        );
        assert_eq!(ev.headers["content-type"], "application/x-sentry-envelope");
        let (header, items) = unframe(&ev.body);
        let id = header["event_id"].as_str().unwrap().to_string();
        assert_eq!(id.len(), 32);
        assert!(header["sent_at"].as_str().unwrap().ends_with('Z'));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].0["type"], "event");
        let e: Value = serde_json::from_slice(&items[0].1).unwrap();
        assert_eq!(e["event_id"], id.as_str());
        assert_eq!(e["message"], "Problem report: HEVC stutters on Receiver 1");

        let att = got.recv().await.unwrap();
        let (h2, items) = unframe(&att.body);
        assert_eq!(
            h2["event_id"],
            id.as_str(),
            "the zip goes to the same event"
        );
        assert_eq!(
            items[0].0,
            json!({"type": "attachment", "length": 14, "filename": "Luch-report-2026-01-02-0304.zip",
                   "content_type": "application/zip", "attachment_type": "event.attachment"})
        );
        assert_eq!(items[0].1, b"PK\x03\x04 zip bytes");
        assert_eq!(
            r,
            json!({"number": number(&id), "event_id": id, "attached": true})
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_refused_zip_still_sends_the_report_a_refused_event_does_not() {
        let dir = std::env::temp_dir().join(format!("luch-send-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Luch-report-2026-01-02-0305.zip");
        std::fs::write(&path, b"PK\x03\x04").unwrap();
        keep(&path, sample());
        let (port, _got) = fake_server(vec![
            (200, "{}"),
            (400, r#"{"detail":"unsupported item"}"#),
            (429, "{}"),
        ])
        .await;
        let dsn = Dsn::parse(&format!("http://k@127.0.0.1:{port}/3")).unwrap();
        let http = reqwest::Client::new();
        let r = send(&http, &dsn, &path).await.unwrap();
        assert_eq!(r["attached"], false);
        assert_eq!(
            r["attach_error"],
            "the developers' server answered HTTP 400: unsupported item"
        );
        let refused = send(&http, &dsn, &path).await.unwrap_err();
        assert!(
            refused.starts_with("the developers' server takes no more reports"),
            "{refused}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn only_a_report_luchd_made_is_sent() {
        let dsn = Dsn::parse("http://k@127.0.0.1:9/3").unwrap();
        let e = send(&reqwest::Client::new(), &dsn, Path::new("/etc/passwd"))
            .await
            .unwrap_err();
        assert!(e.contains("not a report luchd made"), "{e}");
    }
}
