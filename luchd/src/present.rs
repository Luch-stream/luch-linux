//! The presenter view: a page on this computer for a document shown on a receiver (PROTOCOL.md 4g, 4h). It shows
//! the current and the next slide, the speaker notes, a timer and the slide counter; keys, clicks and a clicker
//! turn the slides, the mouse over the slide moves the receiver's laser pointer and dragging draws on it.
//!
//! Served over plain HTTP on 127.0.0.1 only, under a random token, and only to a Host of this machine (a web page
//! elsewhere cannot reach it through DNS rebinding). The page talks to luchd over a websocket: page turns, pointer and
//! ink go to the receiver at once, pointer moves coalesced; the receiver's own page turns and blanking come back.

use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{broadcast, mpsc, watch},
};
use tokio_tungstenite::tungstenite::Message;

use crate::mirror::Cmd;

const PAGE: &str = include_str!("present.html");
const SCRIPT: &str = include_str!("present.js");

/// The receiver's frame: pointer moves closer together than this are merged into the newest.
pub const POINTER_EVERY: Duration = Duration::from_millis(16);
const MAX_POINTS: usize = 2000;

// ---------- messages from the page (or D-Bus) to the receiver ----------

fn unit(v: &Value) -> Option<f64> {
    let x = v.as_f64()?;
    x.is_finite()
        .then(|| (x.clamp(0.0, 1.0) * 10_000.0).round() / 10_000.0)
}

fn is_color(s: &str) -> bool {
    (s.len() == 7 || s.len() == 9)
        && s.starts_with('#')
        && s[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// A pointer, ink, blank or page message made safe to pass on (PROTOCOL.md 4h): known fields only, coordinates
/// clamped and rounded, sizes limited. None for anything else.
pub fn sanitize(v: &Value) -> Option<Value> {
    match v["t"].as_str()? {
        "pointer" if v["show"] == false => Some(json!({"t": "pointer", "show": false})),
        "pointer" => Some(json!({"t": "pointer", "x": unit(&v["x"])?, "y": unit(&v["y"])?})),
        "blank" => Some(json!({"t": "blank", "on": v["on"].as_bool()?})),
        "doc_page" => Some(json!({"t": "doc_page", "page": v["page"].as_u64()?})),
        "ink" => match v["do"].as_str()? {
            d @ ("undo" | "clear") => Some(json!({"t": "ink", "do": d})),
            "stroke" => {
                let raw = v["points"].as_array()?;
                if raw.len() % 2 != 0 || raw.len() > 2 * MAX_POINTS {
                    return None;
                }
                let points: Option<Vec<f64>> = raw.iter().map(unit).collect();
                let mut m =
                    json!({"t": "ink", "do": "stroke", "id": v["id"].as_u64()?, "points": points?});
                if let Some(c) = v["color"].as_str().filter(|c| is_color(c)) {
                    m["color"] = json!(c.to_ascii_lowercase());
                }
                if let Some(w) = v["width"].as_f64().filter(|w| *w > 0.0 && *w <= 0.1) {
                    m["width"] = json!((w * 100_000.0).round() / 100_000.0);
                }
                Some(m)
            }
            _ => None,
        },
        _ => None,
    }
}

/// Pointer moves to the receiver at most every `every`: the first at once, then the newest one when its turn
/// comes; the last one is never lost.
pub struct Coalescer {
    every: Duration,
    last: Option<Instant>,
    pending: Option<Value>,
}

impl Coalescer {
    pub fn new(every: Duration) -> Coalescer {
        Coalescer {
            every,
            last: None,
            pending: None,
        }
    }

    /// A new pointer message: returned when it may go now, else kept (replacing an older one) until `deadline`.
    pub fn offer(&mut self, now: Instant, v: Value) -> Option<Value> {
        if self.pending.is_none() && self.last.is_none_or(|l| now >= l + self.every) {
            self.last = Some(now);
            return Some(v);
        }
        self.pending = Some(v);
        None
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.pending
            .as_ref()
            .map(|_| self.last.map_or(Instant::now(), |l| l + self.every))
    }

    /// The kept message, once its time has come.
    pub fn due(&mut self, now: Instant) -> Option<Value> {
        if self.deadline().is_some_and(|d| now >= d) {
            self.last = Some(now);
            return self.pending.take();
        }
        None
    }
}

// ---------- slides as pictures ----------

/// Page `page` (from 0) of `pdf` as PNG, `width` pixels wide, through poppler's pdftoppm or MuPDF's mutool. Blocking.
pub fn render(pdf: &Path, page: u64, width: u32, dir: &Path) -> Result<PathBuf, String> {
    let out = dir.join(format!("p{page}-w{width}.png"));
    if out.is_file() {
        return Ok(out);
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let n = (page + 1).to_string();
    let w = width.to_string();
    let root = dir.join(format!("p{page}-w{width}"));
    let poppler = std::process::Command::new("pdftoppm")
        .args([
            "-png",
            "-singlefile",
            "-f",
            &n,
            "-l",
            &n,
            "-scale-to-x",
            &w,
            "-scale-to-y",
            "-1",
        ])
        .arg(pdf)
        .arg(&root)
        .output();
    if poppler.is_ok_and(|o| o.status.success()) && out.is_file() {
        return Ok(out);
    }
    let mupdf = std::process::Command::new("mutool")
        .args(["draw", "-q", "-w", &w, "-o"])
        .arg(&out)
        .arg(pdf)
        .arg(&n)
        .output();
    if mupdf.is_ok_and(|o| o.status.success()) && out.is_file() {
        return Ok(out);
    }
    Err("install poppler-utils (pdftoppm) or mupdf-tools to see the slides here".into())
}

/// Widths in steps, so a resized window does not render every slide again for each pixel.
pub fn width_step(asked: u32) -> u32 {
    asked.clamp(160, 3840).div_ceil(160) * 160
}

// ---------- the shows and their server ----------

/// A document shown on a receiver for this presenter view.
pub struct Show {
    pub receiver: String,
    pub receiver_name: String,
    pub name: String,
    pub pdf: PathBuf,
    pub notes: Vec<String>,
    pub ink: bool,
    pub cmd: mpsc::Sender<Cmd>,
    pub events: broadcast::Sender<Value>,
    state: Mutex<(u64, u64, bool)>, // page, pages, blank
    /// True once it is over: the receiver closed it, the sender stopped it, or another show took its place.
    ended: watch::Sender<bool>,
    cache: PathBuf,
    /// Do not disturb while it is shown.
    _quiet: Option<crate::quiet::Session>,
}

impl Show {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        receiver: String,
        receiver_name: String,
        name: String,
        pdf: PathBuf,
        notes: Vec<String>,
        ink: bool,
        cmd: mpsc::Sender<Cmd>,
        events: broadcast::Sender<Value>,
        page: u64,
        pages: u64,
        quiet: Option<crate::quiet::Session>,
    ) -> Show {
        Show {
            receiver,
            receiver_name,
            name,
            pdf,
            notes,
            ink,
            cmd,
            events,
            state: Mutex::new((page, pages, false)),
            ended: watch::channel(false).0,
            cache: std::env::temp_dir().join(format!(
                "luch-present-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            )),
            _quiet: quiet,
        }
    }

    fn hello(&self) -> Value {
        let (page, pages, blank) = *self.state.lock().unwrap();
        json!({"t": "hello", "name": self.name, "receiver": self.receiver_name, "page": page, "pages": pages,
               "blank": blank, "notes": self.notes, "ink": self.ink})
    }

    /// Follows what the receiver says; returns the state for the page when it changed.
    fn heard(&self, v: &Value) -> Option<Value> {
        let mut s = self.state.lock().unwrap();
        match v["t"].as_str()? {
            "doc_page" => {
                s.0 = v["page"].as_u64()?;
                s.2 = false;
            }
            "doc_ready" => s.1 = v["pages"].as_u64()?,
            "blank" => s.2 = v["on"].as_bool()?,
            "stop" => {
                self.ended.send_replace(true);
                return None;
            }
            _ => return None,
        }
        Some(json!({"t": "state", "page": s.0, "pages": s.1, "blank": s.2}))
    }
}

impl Drop for Show {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.cache);
    }
}

pub struct Server {
    port: u16,
    shows: Arc<Mutex<HashMap<String, Arc<Show>>>>,
}

impl Server {
    /// Listens on 127.0.0.1, on any free port.
    pub async fn start() -> io::Result<Server> {
        let l = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = l.local_addr()?.port();
        let shows: Arc<Mutex<HashMap<String, Arc<Show>>>> = Arc::default();
        let all = shows.clone();
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = l.accept().await else {
                    continue;
                };
                let all = all.clone();
                tokio::spawn(async move {
                    if let Err(e) = connection(sock, port, all).await {
                        eprintln!("[luchd] presenter view: {e}");
                    }
                });
            }
        });
        Ok(Server { port, shows })
    }

    /// Adds a show (replacing the one on the same receiver) and returns its page's address.
    pub fn add(&self, show: Show) -> String {
        let token = uuid::Uuid::new_v4().simple().to_string();
        self.end(&show.receiver);
        let show = Arc::new(show);
        self.shows
            .lock()
            .unwrap()
            .insert(token.clone(), show.clone());
        // It ends with the document on the receiver, or with the connection to it.
        let mut events = show.events.subscribe();
        let mut ended = show.ended.subscribe();
        let all = self.shows.clone();
        let t = token.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    e = events.recv() => match e {
                        Ok(v) => { show.heard(&v); }
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(_) => break,
                    },
                    _ = async { let _ = ended.wait_for(|e| *e).await; } => break,
                }
            }
            show.ended.send_replace(true);
            all.lock().unwrap().remove(&t);
        });
        format!("http://127.0.0.1:{}/{token}/", self.port)
    }

    /// The show on a receiver is over (the sender stopped it, or shows another).
    pub fn end(&self, receiver: &str) {
        self.shows.lock().unwrap().retain(|_, s| {
            if s.receiver == receiver {
                s.ended.send_replace(true);
            }
            s.receiver != receiver
        });
    }

    #[cfg(test)]
    pub fn port(&self) -> u16 {
        self.port
    }
}

/// Only this machine's names: a page elsewhere that points a host name at 127.0.0.1 gets nothing.
pub fn local_host(host: &str, port: u16) -> bool {
    ["127.0.0.1", "localhost", "[::1]"]
        .iter()
        .any(|h| host.eq_ignore_ascii_case(&format!("{h}:{port}")))
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.split("\r\n").skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

async fn reply(sock: &mut TcpStream, code: &str, ctype: &str, body: &[u8]) -> io::Result<()> {
    let head = format!(
        "HTTP/1.1 {code}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n",
        body.len()
    );
    sock.write_all(head.as_bytes()).await?;
    sock.write_all(body).await?;
    sock.shutdown().await
}

async fn connection(
    mut sock: TcpStream,
    port: u16,
    shows: Arc<Mutex<HashMap<String, Arc<Show>>>>,
) -> io::Result<()> {
    let raw = tokio::time::timeout(Duration::from_secs(5), crate::read_http_head(&mut sock))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "header timeout"))??;
    let head = String::from_utf8_lossy(&raw).into_owned();
    let target = head.split(' ').nth(1).unwrap_or("/").to_string();
    if !head.starts_with("GET ") || !header(&head, "host").is_some_and(|h| local_host(h, port)) {
        return reply(&mut sock, "403 Forbidden", "text/plain", b"forbidden").await;
    }
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let mut parts = path.trim_start_matches('/').splitn(2, '/');
    let token = parts.next().unwrap_or_default().to_string();
    let rest = parts.next().unwrap_or_default().to_string();
    let show = shows.lock().unwrap().get(&token).cloned();
    let Some(show) = show else {
        return reply(
            &mut sock,
            "404 Not Found",
            "text/plain",
            b"this presentation is over",
        )
        .await;
    };
    match rest.as_str() {
        "" => {
            reply(
                &mut sock,
                "200 OK",
                "text/html; charset=utf-8",
                PAGE.as_bytes(),
            )
            .await
        }
        "present.js" => {
            reply(
                &mut sock,
                "200 OK",
                "text/javascript; charset=utf-8",
                SCRIPT.as_bytes(),
            )
            .await
        }
        "ws" if header(&head, "upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket")) => {
            // A websocket is not bound by the same-origin rule: only our own page may open one.
            let origin_ok = header(&head, "origin")
                .and_then(|o| o.strip_prefix("http://"))
                .is_some_and(|o| local_host(o, port));
            if !origin_ok {
                return reply(&mut sock, "403 Forbidden", "text/plain", b"forbidden").await;
            }
            let stream = crate::Prefixed {
                inner: std::io::Cursor::new(raw).chain(sock),
            };
            let ws = tokio_tungstenite::accept_async(stream)
                .await
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            session(ws, show).await;
            Ok(())
        }
        s if s.starts_with("slide/") => {
            let page = s["slide/".len()..]
                .trim_end_matches(".png")
                .parse::<u64>()
                .ok();
            let width = query
                .split('&')
                .find_map(|kv| kv.strip_prefix("w="))
                .and_then(|w| w.parse().ok())
                .unwrap_or(1280);
            let pages = show.state.lock().unwrap().1;
            let Some(page) = page.filter(|p| *p < pages.max(1)) else {
                return reply(&mut sock, "404 Not Found", "text/plain", b"no such slide").await;
            };
            let (pdf, dir) = (show.pdf.clone(), show.cache.clone());
            let png =
                tokio::task::spawn_blocking(move || render(&pdf, page, width_step(width), &dir))
                    .await
                    .map_err(io::Error::other)?;
            match png.and_then(|p| std::fs::read(p).map_err(|e| e.to_string())) {
                Ok(b) => reply(&mut sock, "200 OK", "image/png", &b).await,
                Err(e) => {
                    reply(
                        &mut sock,
                        "503 Service Unavailable",
                        "text/plain",
                        e.as_bytes(),
                    )
                    .await
                }
            }
        }
        _ => reply(&mut sock, "404 Not Found", "text/plain", b"not found").await,
    }
}

type Ws = tokio_tungstenite::WebSocketStream<crate::Prefixed<TcpStream>>;

/// One presenter page: its input to the receiver, the receiver's page turns back to it.
async fn session(ws: Ws, show: Arc<Show>) {
    let (mut sink, mut stream) = ws.split();
    let mut events = show.events.subscribe();
    if sink
        .send(Message::Text(show.hello().to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    let mut pointer = Coalescer::new(POINTER_EVERY);
    let mut ended = show.ended.subscribe();
    loop {
        let wait = pointer.deadline();
        tokio::select! {
            m = stream.next() => {
                let Some(Ok(m)) = m else { break };
                let Message::Text(t) = m else { continue };
                let Some(v) = serde_json::from_str::<Value>(t.as_str()).ok().and_then(|v| sanitize(&v)) else { continue };
                let over = *show.ended.borrow();
                if over {
                    continue;
                }
                let out = if v["t"] == "pointer" { pointer.offer(Instant::now(), v) } else { Some(v) };
                if let Some(v) = out {
                    if show.cmd.send(Cmd::Send(v)).await.is_err() {
                        break;
                    }
                }
            }
            _ = async { tokio::time::sleep_until(wait.unwrap().into()).await }, if wait.is_some() => {
                if let Some(v) = pointer.due(Instant::now()) {
                    let _ = show.cmd.send(Cmd::Send(v)).await;
                }
            }
            e = events.recv() => match e {
                Ok(v) => {
                    if let Some(out) = show.heard(&v) {
                        if sink.send(Message::Text(out.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break,
            },
            _ = async { let _ = ended.wait_for(|e| *e).await; } => break,
        }
    }
    let _ = sink
        .send(Message::Text(json!({"t": "ended"}).to_string().into()))
        .await;
    let _ = sink.close().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_cleaned_before_the_receiver_sees_them() {
        assert_eq!(
            sanitize(&json!({"t": "pointer", "x": 0.123456, "y": 1.7, "evil": 1})),
            Some(json!({"t": "pointer", "x": 0.1235, "y": 1.0}))
        );
        assert_eq!(
            sanitize(&json!({"t": "pointer", "show": false})),
            Some(json!({"t": "pointer", "show": false}))
        );
        assert_eq!(sanitize(&json!({"t": "pointer", "x": "a", "y": 0})), None);
        assert_eq!(
            sanitize(
                &json!({"t": "ink", "do": "stroke", "id": 3, "color": "#E53935", "width": 0.004,
                             "points": [0.1, 0.2, -1, 2]})
            ),
            Some(
                json!({"t": "ink", "do": "stroke", "id": 3, "points": [0.1, 0.2, 0.0, 1.0],
                        "color": "#e53935", "width": 0.004})
            )
        );
        // A bad colour or width is dropped, the stroke kept; odd or too many points are refused.
        let s = sanitize(
            &json!({"t": "ink", "do": "stroke", "id": 1, "color": "red", "width": 5, "points": []}),
        )
        .unwrap();
        assert!(s.get("color").is_none() && s.get("width").is_none());
        assert_eq!(
            sanitize(&json!({"t": "ink", "do": "stroke", "id": 1, "points": [0.1]})),
            None
        );
        let many: Vec<f64> = vec![0.5; 2 * MAX_POINTS + 2];
        assert_eq!(
            sanitize(&json!({"t": "ink", "do": "stroke", "id": 1, "points": many})),
            None
        );
        assert_eq!(
            sanitize(&json!({"t": "ink", "do": "undo", "id": 9})),
            Some(json!({"t": "ink", "do": "undo"}))
        );
        assert_eq!(
            sanitize(&json!({"t": "blank", "on": true})),
            Some(json!({"t": "blank", "on": true}))
        );
        assert_eq!(
            sanitize(&json!({"t": "doc_page", "page": 4})),
            Some(json!({"t": "doc_page", "page": 4}))
        );
        assert_eq!(
            sanitize(&json!({"t": "stop"})),
            None,
            "the page cannot send anything else"
        );
        assert_eq!(
            sanitize(
                &json!({"t": "ink", "do": "stroke", "id": 1, "color": "#12345", "points": []})
            )
            .unwrap()
            .get("color"),
            None
        );
    }

    #[test]
    fn pointer_moves_are_coalesced_to_the_newest() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let p = |x: f64| json!({"t": "pointer", "x": x, "y": 0.5});
        let mut c = Coalescer::new(POINTER_EVERY);
        assert_eq!(
            c.offer(ms(0), p(0.1)),
            Some(p(0.1)),
            "the first goes at once"
        );
        assert_eq!(c.offer(ms(2), p(0.2)), None);
        assert_eq!(c.offer(ms(5), p(0.3)), None);
        assert_eq!(c.deadline(), Some(ms(16)));
        assert_eq!(c.due(ms(10)), None, "not yet");
        assert_eq!(c.due(ms(16)), Some(p(0.3)), "only the newest of those");
        assert_eq!(c.deadline(), None);
        assert_eq!(
            c.offer(ms(40), p(0.4)),
            Some(p(0.4)),
            "after a pause it goes at once again"
        );
        // 1000 moves in a second at 1 ms intervals come out as about 60.
        let (mut sent, mut c) = (0, Coalescer::new(POINTER_EVERY));
        for n in 0..1000 {
            sent += c.offer(ms(n), p(n as f64 / 1000.0)).is_some() as u32;
            sent += c.due(ms(n)).is_some() as u32;
        }
        let last = c.due(ms(2000));
        assert!((55..=65).contains(&sent), "{sent}");
        assert_eq!(last, Some(p(0.999)), "the last position is never lost");
    }

    #[test]
    fn only_this_machine_may_ask() {
        assert!(local_host("127.0.0.1:4000", 4000));
        assert!(local_host("LOCALHOST:4000", 4000));
        assert!(!local_host("127.0.0.1:4001", 4000));
        assert!(!local_host("attacker.example:4000", 4000));
        assert_eq!(width_step(1), 160);
        assert_eq!(width_step(1281), 1440);
        assert_eq!(width_step(99999), 3840);
    }

    fn show(cmd: mpsc::Sender<Cmd>, events: broadcast::Sender<Value>, pdf: PathBuf) -> Show {
        Show::new(
            "tv".into(),
            "Living room TV".into(),
            "talk.pdf".into(),
            pdf,
            vec!["Hello".into(), "".into()],
            true,
            cmd,
            events,
            0,
            2,
            None,
        )
    }

    async fn get(port: u16, path: &str, host: &str, extra: &str) -> (u16, Vec<u8>) {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n{extra}\r\n").as_bytes())
            .await
            .unwrap();
        let mut b = Vec::new();
        s.read_to_end(&mut b).await.unwrap();
        let text = String::from_utf8_lossy(&b).to_string();
        let code = text.split(' ').nth(1).unwrap_or("0").parse().unwrap_or(0);
        let body = b
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|p| b[p + 4..].to_vec())
            .unwrap_or_default();
        (code, body)
    }

    #[tokio::test]
    async fn the_page_drives_the_receiver_and_follows_it() {
        let srv = Server::start().await.unwrap();
        let (cmd, mut to_tv) = mpsc::channel(64);
        let (events, _) = broadcast::channel(16);
        let url = srv.add(show(cmd, events.clone(), PathBuf::from("/nonexistent.pdf")));
        let port = srv.port();
        let host = format!("127.0.0.1:{port}");
        let path = url.trim_start_matches(&format!("http://{host}"));
        assert_eq!(get(port, path, &host, "").await.0, 200);
        assert!(
            String::from_utf8(get(port, &format!("{path}present.js"), &host, "").await.1)
                .unwrap()
                .contains("export")
        );
        assert_eq!(
            get(port, path, "evil.example:80", "").await.0,
            403,
            "DNS rebinding"
        );
        assert_eq!(get(port, "/wrong-token/", &host, "").await.0, 404);
        assert_eq!(
            get(port, &format!("{path}slide/9.png"), &host, "").await.0,
            404,
            "no such page"
        );
        // A websocket from another site's page is refused.
        let (code, _) = get(port, &format!("{path}ws"), &host,
            "Upgrade: websocket\r\nConnection: Upgrade\r\nOrigin: http://evil.example\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n").await;
        assert_eq!(code, 403);

        let req = tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
            format!("ws://{host}{path}ws"),
        )
        .unwrap();
        let mut req = req;
        req.headers_mut()
            .insert("Origin", format!("http://{host}").parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        let hello: Value = match ws.next().await.unwrap().unwrap() {
            Message::Text(t) => serde_json::from_str(t.as_str()).unwrap(),
            m => panic!("{m:?}"),
        };
        assert_eq!(
            (hello["pages"].as_u64(), hello["notes"][0].as_str()),
            (Some(2), Some("Hello"))
        );
        for m in [
            json!({"t": "doc_page", "page": 1}),
            json!({"t": "pointer", "x": 0.5, "y": 0.5}),
            json!({"t": "pointer", "x": 0.6, "y": 0.5}),
            json!({"t": "pointer", "x": 0.7, "y": 0.5}),
            json!({"t": "stop"}),
        ] {
            ws.send(Message::Text(m.to_string().into())).await.unwrap();
        }
        let mut got = Vec::new();
        while let Ok(Some(Cmd::Send(v))) =
            tokio::time::timeout(Duration::from_millis(200), to_tv.recv()).await
        {
            got.push(v);
        }
        assert_eq!(
            got,
            vec![
                json!({"t": "doc_page", "page": 1}),
                json!({"t": "pointer", "x": 0.5, "y": 0.5}),
                json!({"t": "pointer", "x": 0.7, "y": 0.5})
            ],
            "coalesced; stop is not the page's"
        );
        // The TV's remote turns the page: the presenter view follows; Back on the TV ends it.
        events.send(json!({"t": "doc_page", "page": 0})).unwrap();
        let next = |m: Message| serde_json::from_str::<Value>(m.to_text().unwrap()).unwrap();
        assert_eq!(
            next(ws.next().await.unwrap().unwrap()),
            json!({"t": "state", "page": 0, "pages": 2, "blank": false})
        );
        events.send(json!({"t": "blank", "on": true})).unwrap();
        assert_eq!(next(ws.next().await.unwrap().unwrap())["blank"], true);
        events
            .send(json!({"t": "stop", "by": "receiver", "reason": "back"}))
            .unwrap();
        assert_eq!(
            next(ws.next().await.unwrap().unwrap()),
            json!({"t": "ended"})
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(get(port, path, &host, "").await.0, 404, "the show is over");
    }

    #[test]
    fn slides_render_when_poppler_is_there() {
        if std::process::Command::new("pdftoppm")
            .arg("-v")
            .output()
            .is_err()
        {
            eprintln!("pdftoppm missing: skipped");
            return;
        }
        let dir = std::env::temp_dir().join(format!("luch-render-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("one.pdf");
        std::fs::write(&pdf, MINIMAL_PDF).unwrap();
        let png = render(&pdf, 0, 320, &dir.join("cache")).unwrap();
        let b = std::fs::read(&png).unwrap();
        assert_eq!(&b[1..4], b"PNG");
        assert_eq!(
            u32::from_be_bytes([b[16], b[17], b[18], b[19]]),
            320,
            "as wide as asked"
        );
        assert!(render(&pdf, 5, 320, &dir.join("cache")).is_err());
    }

    /// One empty A4-ish page, with a correct cross-reference table.
    const MINIMAL_PDF: &[u8] = b"%PDF-1.4
1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj
2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj
3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 400 300]>>endobj
xref
0 4
0000000000 65535 f
0000000009 00000 n
0000000052 00000 n
0000000101 00000 n
trailer<</Size 4/Root 1 0 R>>
startxref
165
%%EOF
";
}
