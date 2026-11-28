//! Media and photos for a receiver (PROTOCOL.md 4a, 4b): the message to send, and the HTTP server the receiver
//! fetches from. Local files are served as they are; a link can be relayed ("via sender"): luchd fetches it with
//! the browser's headers and passes the bytes on unchanged, and HLS playlists are rewritten to point back here. That is
//! what lets a TV play a video only this machine can reach (a VPN, a site's login, Wi-Fi Direct).
//!
//! Every served thing has a random token in its path and answers only the receiver it was shared with.

use std::{
    collections::HashMap,
    io,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};

/// The port receivers fetch from; a fixed one, so a firewall can be opened for it (next to 47802/47803).
pub const PORT: u16 = 47810;
const MAX_HEAD: usize = 16 * 1024;
const MAX_PLAYLIST: usize = 4 * 1024 * 1024;
pub const MAX_PHOTOS: usize = 200;
/// Headers a page may hand on, as in PROTOCOL.md 4b.
pub const PASSED_HEADERS: [&str; 5] =
    ["Referer", "User-Agent", "Origin", "Cookie", "Authorization"];
/// `contentFeatures.dlna.org` for what plays (DLNA 7.4.1.3.17): byte seeks work (OP=01), streaming transfer, DLNA
/// 1.5. Many TVs will not play a link whose server does not say so when asked.
pub const DLNA_STREAM: &str =
    "DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000";
/// The same for a picture: interactive transfer, no seeking.
pub const DLNA_PHOTO: &str =
    "DLNA.ORG_OP=00;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=00D00000000000000000000000000000";

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    File(PathBuf),
    Relay {
        url: String,
        headers: Vec<(String, String)>,
    },
    /// A folder at the origin: our link's path after the token is fetched from under `base` (a DASH manifest's
    /// pieces, whose names the player makes from templates).
    RelayFolder {
        base: String,
        headers: Vec<(String, String)>,
    },
}

#[derive(Clone, Debug)]
struct Entry {
    item: Item,
    client: IpAddr,
    mime: String,
    /// Sent with every answer for it (a DLNA TV's `CaptionInfo.sec`).
    headers: Vec<(String, String)>,
    /// A SubRip file served as WebVTT, the only text a Chromecast reads.
    vtt: bool,
}

#[derive(Clone)]
pub struct Server {
    shared: Arc<Mutex<HashMap<String, Entry>>>,
    port: u16,
    http: reqwest::Client,
}

impl Server {
    /// Listens on [`PORT`], or any free port if it is taken.
    pub async fn start() -> io::Result<Server> {
        let l = match TcpListener::bind(("0.0.0.0", PORT)).await {
            Ok(l) => l,
            Err(_) => TcpListener::bind(("0.0.0.0", 0)).await?,
        };
        Self::serve(l)
    }

    /// Serves on a listener of the caller's (tests bind loopback).
    pub fn serve(l: TcpListener) -> io::Result<Server> {
        let port = l.local_addr()?.port();
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(io::Error::other)?;
        let s = Server {
            shared: Arc::default(),
            port,
            http,
        };
        let me = s.clone();
        tokio::spawn(async move {
            loop {
                let Ok((sock, peer)) = l.accept().await else {
                    continue;
                };
                let me = me.clone();
                tokio::spawn(async move {
                    let _ = me.connection(sock, peer).await;
                });
            }
        });
        Ok(s)
    }

    #[cfg(test)]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Makes `item` fetchable by `client`; returns its token. A folder already shared with the client keeps its
    /// token (a live manifest names the same ones every few seconds).
    pub fn share(&self, item: Item, client: IpAddr, mime: &str) -> String {
        if matches!(item, Item::RelayFolder { .. }) {
            let shared = self.shared.lock().unwrap();
            if let Some((t, _)) = shared
                .iter()
                .find(|(_, e)| e.item == item && e.client == client)
            {
                return t.clone();
            }
        }
        let token = uuid::Uuid::new_v4().simple().to_string();
        self.shared.lock().unwrap().insert(
            token.clone(),
            Entry {
                item,
                client,
                mime: mime.to_string(),
                headers: Vec::new(),
                vtt: false,
            },
        );
        token
    }

    pub fn url(&self, host: IpAddr, token: &str, name: &str) -> String {
        format!(
            "http://{}/{token}/{}",
            SocketAddr::new(host, self.port),
            encode(name)
        )
    }

    /// The token of one of our links.
    fn token_of(url: &str) -> Option<&str> {
        url.split("://").nth(1)?.split('/').nth(1)
    }

    /// Adds a header to every answer for the shared link `url` (one of ours).
    pub fn add_header(&self, url: &str, name: &str, value: &str) {
        let Some(t) = Self::token_of(url) else { return };
        if let Some(e) = self.shared.lock().unwrap().get_mut(t) {
            e.headers.push((name.to_string(), value.to_string()));
        }
    }

    /// Serves the shared SubRip file `url` as WebVTT from now on; false when it is no local SubRip file of ours.
    pub fn as_vtt(&self, url: &str) -> bool {
        let Some(t) = Self::token_of(url) else {
            return false;
        };
        let mut all = self.shared.lock().unwrap();
        match all.get_mut(t) {
            Some(e) if matches!(&e.item, Item::File(p) if extension(&p.to_string_lossy()) == "srt") =>
            {
                e.vtt = true;
                e.mime = "text/vtt".into();
                true
            }
            _ => false,
        }
    }

    /// Everything shared with `client` stops being served (its session ended).
    pub fn forget(&self, client: IpAddr) {
        self.shared
            .lock()
            .unwrap()
            .retain(|_, e| e.client != client);
    }

    /// Whether `token` is still served (a stop forgets a receiver's links).
    pub fn has(&self, token: &str) -> bool {
        self.shared.lock().unwrap().contains_key(token)
    }

    #[cfg(test)]
    pub fn shared_count(&self) -> usize {
        self.shared.lock().unwrap().len()
    }

    async fn connection(&self, sock: TcpStream, peer: SocketAddr) -> io::Result<()> {
        let local = sock.local_addr()?.ip();
        let (rd, mut wr) = sock.into_split();
        let mut rd = BufReader::new(rd);
        loop {
            let Some(req) = read_request(&mut rd).await? else {
                return Ok(());
            };
            let entry = req
                .path
                .trim_start_matches('/')
                .split('/')
                .next()
                .and_then(|t| self.shared.lock().unwrap().get(t).cloned());
            if req.options {
                // A Chromecast's page asks first whether it may fetch with a Range header (CORS preflight).
                respond(&mut wr, 204, &CORS_PREFLIGHT, b"").await?;
                if req.close {
                    return Ok(());
                }
                continue;
            }
            let Some(entry) = entry else {
                respond(&mut wr, 404, &[], b"").await?;
                if req.close {
                    return Ok(());
                }
                continue;
            };
            if canonical(entry.client) != canonical(peer.ip()) {
                respond(&mut wr, 403, &[], b"").await?;
                if req.close {
                    return Ok(());
                }
                continue;
            }
            let extra = extra_headers(&entry, &req);
            let keep = match &entry.item {
                Item::File(p) if entry.vtt => {
                    let text = tokio::fs::read(p).await.unwrap_or_default();
                    let vtt = srt_to_vtt(&String::from_utf8_lossy(&text));
                    let mut h: Vec<(&str, &str)> =
                        vec![("Content-Type", "text/vtt; charset=utf-8")];
                    h.extend(extra.iter().map(|(k, v)| (k.as_str(), v.as_str())));
                    respond(
                        &mut wr,
                        200,
                        &h,
                        if req.head { b"" } else { vtt.as_bytes() },
                    )
                    .await?;
                    true
                }
                Item::File(p) => send_file(&mut wr, p, &entry.mime, &req, &extra).await?,
                Item::Relay { url, headers } => {
                    self.relay(&mut wr, url, headers, &req, entry.client, local, &extra)
                        .await?
                }
                Item::RelayFolder { base, headers } => {
                    // "/<token>/<path under the folder>?<query>"
                    let rest = req.path.trim_start_matches('/');
                    let rest = rest.split_once('/').map(|(_, r)| r).unwrap_or("");
                    let url = format!("{base}{rest}");
                    self.relay(&mut wr, &url, headers, &req, entry.client, local, &extra)
                        .await?
                }
            };
            if !keep || req.close {
                return Ok(());
            }
        }
    }

    /// Fetches `url` with the page's headers and passes it on; a playlist comes back with its links made ours.
    #[allow(clippy::too_many_arguments)]
    async fn relay(
        &self,
        wr: &mut tokio::net::tcp::OwnedWriteHalf,
        url: &str,
        headers: &[(String, String)],
        req: &Request,
        client: IpAddr,
        local: IpAddr,
        extra: &[(String, String)],
    ) -> io::Result<bool> {
        let mut up = self.http.get(url);
        for (k, v) in headers {
            up = up.header(k.as_str(), v.as_str());
        }
        if let Some(r) = &req.range {
            up = up.header("Range", r.as_str());
        }
        let res = match up.send().await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[luchd] relay {}: {e}", host_of(url));
                respond(wr, 502, &[], b"").await?;
                return Ok(true);
            }
        };
        let status = res.status().as_u16();
        let final_url = res.url().to_string();
        let ctype = res
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if is_dash(&final_url, &ctype) && status < 300 {
            let body = res.bytes().await.map_err(io::Error::other)?;
            let Some(body) = playlist_text(&body, false) else {
                respond(wr, 502, &[], b"").await?;
                return Ok(true);
            };
            let text = String::from_utf8_lossy(&body);
            let out = rewrite_mpd(&text, &final_url, |dir| {
                let t = self.share(
                    Item::RelayFolder {
                        base: dir.to_string(),
                        headers: headers.to_vec(),
                    },
                    client,
                    "",
                );
                format!("http://{}/{t}/", SocketAddr::new(local, self.port))
            });
            let mut h: Vec<(&str, &str)> = vec![("Content-Type", "application/dash+xml")];
            h.extend(extra.iter().map(|(k, v)| (k.as_str(), v.as_str())));
            respond(wr, 200, &h, if req.head { b"" } else { out.as_bytes() }).await?;
            return Ok(true);
        }
        if is_playlist(&final_url, &ctype) && status < 300 {
            let gzip = res
                .headers()
                .get("content-encoding")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.to_ascii_lowercase().contains("gzip"));
            let body = res.bytes().await.map_err(io::Error::other)?;
            let Some(body) = playlist_text(&body, gzip) else {
                respond(wr, 502, &[], b"").await?;
                return Ok(true);
            };
            let text = String::from_utf8_lossy(&body);
            let out = rewrite_playlist(&text, &final_url, |abs| {
                let t = self.share(
                    Item::Relay {
                        url: abs.to_string(),
                        headers: headers.to_vec(),
                    },
                    client,
                    "",
                );
                self.url(local, &t, &last_segment(abs))
            });
            let mut h: Vec<(&str, &str)> = vec![("Content-Type", "application/vnd.apple.mpegurl")];
            h.extend(extra.iter().map(|(k, v)| (k.as_str(), v.as_str())));
            respond(wr, 200, &h, if req.head { b"" } else { out.as_bytes() }).await?;
            return Ok(true);
        }
        let mut head = format!("HTTP/1.1 {status} {}\r\n", reason(status));
        for h in [
            "content-type",
            "content-length",
            "content-range",
            "accept-ranges",
            "last-modified",
            "etag",
        ] {
            if let Some(v) = res.headers().get(h).and_then(|v| v.to_str().ok()) {
                head.push_str(&format!("{h}: {v}\r\n"));
            }
        }
        for (k, v) in extra {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        let chunked = res.headers().get("content-length").is_none();
        if chunked {
            head.push_str("Connection: close\r\n");
        }
        head.push_str("\r\n");
        wr.write_all(head.as_bytes()).await?;
        if req.head {
            return Ok(!chunked);
        }
        let mut res = res;
        loop {
            match res.chunk().await {
                Ok(Some(b)) => wr.write_all(&b).await?,
                Ok(None) => break,
                Err(_) => return Ok(false),
            }
        }
        Ok(!chunked)
    }
}

fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    }
}

fn host_of(url: &str) -> &str {
    url.split("://")
        .nth(1)
        .and_then(|r| r.split('/').next())
        .unwrap_or("?")
}

fn last_segment(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let name = path.rsplit('/').next().unwrap_or("");
    if name.is_empty() {
        "x".into()
    } else {
        name.to_string()
    }
}

/// Percent-encodes what may not stand in a path segment.
fn encode(name: &str) -> String {
    let mut out = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[derive(Debug, Default)]
struct Request {
    path: String,
    head: bool,
    options: bool,
    range: Option<String>,
    close: bool,
    /// A DLNA TV asked `getcontentFeatures.dlna.org: 1`.
    dlna_features: bool,
    /// A DLNA TV's `transferMode.dlna.org`, echoed back.
    transfer_mode: Option<String>,
    /// A Chromecast's page fetches with an `Origin`: the answer needs CORS headers.
    origin: bool,
}

/// What a Chromecast's page may ask before fetching (everything here is behind a token and one client anyway).
const CORS_PREFLIGHT: [(&str, &str); 4] = [
    ("Access-Control-Allow-Origin", "*"),
    ("Access-Control-Allow-Methods", "GET, HEAD, OPTIONS"),
    (
        "Access-Control-Allow-Headers",
        "Range, Content-Type, Origin, Accept",
    ),
    ("Access-Control-Max-Age", "600"),
];

/// The headers an answer for `e` adds: the entry's own, DLNA's when asked, CORS for a page.
fn extra_headers(e: &Entry, req: &Request) -> Vec<(String, String)> {
    let mut h = e.headers.clone();
    if req.dlna_features {
        let f = if e.mime.starts_with("image/") {
            DLNA_PHOTO
        } else {
            DLNA_STREAM
        };
        h.push(("contentFeatures.dlna.org".into(), f.into()));
    }
    if let Some(m) = &req.transfer_mode {
        h.push(("transferMode.dlna.org".into(), m.clone()));
    }
    if req.origin {
        h.push(("Access-Control-Allow-Origin".into(), "*".into()));
        h.push((
            "Access-Control-Expose-Headers".into(),
            "Content-Length, Content-Range, Accept-Ranges".into(),
        ));
    }
    h
}

/// SubRip as WebVTT: a header, and the decimal comma of the times made a point.
pub fn srt_to_vtt(srt: &str) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for line in srt.trim_start_matches('\u{feff}').lines() {
        if line.contains("-->") {
            out.push_str(&line.replace(',', "."));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

async fn read_request<R: tokio::io::AsyncBufRead + Unpin>(
    rd: &mut R,
) -> io::Result<Option<Request>> {
    let mut first = String::new();
    if rd.read_line(&mut first).await? == 0 {
        return Ok(None);
    }
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("");
    let mut req = Request {
        path: parts.next().unwrap_or("/").to_string(),
        head: method == "HEAD",
        options: method == "OPTIONS",
        ..Default::default()
    };
    if parts.next() == Some("HTTP/1.0") {
        req.close = true;
    }
    let mut total = first.len();
    loop {
        let mut line = String::new();
        let n = rd.read_line(&mut line).await?;
        total += n;
        if n == 0 || total > MAX_HEAD {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
            if k == "range" {
                req.range = Some(v.to_string());
            } else if k == "connection" && v.eq_ignore_ascii_case("close") {
                req.close = true;
            } else if k == "getcontentfeatures.dlna.org" && v == "1" {
                req.dlna_features = true;
            } else if k == "transfermode.dlna.org" && !v.contains(['\r', '\n']) {
                req.transfer_mode = Some(v.to_string());
            } else if k == "origin" {
                req.origin = true;
            }
        }
    }
    if method != "GET" && method != "HEAD" {
        req.path = String::new();
    }
    Ok(Some(req))
}

fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        204 => "No Content",
        206 => "Partial Content",
        403 => "Forbidden",
        404 => "Not Found",
        416 => "Range Not Satisfiable",
        502 => "Bad Gateway",
        _ => "",
    }
}

async fn respond<W: tokio::io::AsyncWrite + Unpin>(
    wr: &mut W,
    code: u16,
    headers: &[(&str, &str)],
    body: &[u8],
) -> io::Result<()> {
    let mut head = format!("HTTP/1.1 {code} {}\r\n", reason(code));
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    wr.write_all(head.as_bytes()).await?;
    wr.write_all(body).await
}

/// `bytes=a-b`, `bytes=a-` or `bytes=-n` of a file `size` long: the first and last byte, or None if unsatisfiable.
pub fn parse_range(r: &str, size: u64) -> Option<(u64, u64)> {
    let spec = r.trim().strip_prefix("bytes=")?.split(',').next()?.trim();
    let (a, b) = spec.split_once('-')?;
    if size == 0 {
        return None;
    }
    let (start, end) = if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        (size.saturating_sub(n), size - 1)
    } else {
        let s: u64 = a.parse().ok()?;
        let e = if b.is_empty() {
            size - 1
        } else {
            b.parse::<u64>().ok()?.min(size - 1)
        };
        (s, e)
    };
    (start <= end && start < size).then_some((start, end))
}

async fn send_file(
    wr: &mut tokio::net::tcp::OwnedWriteHalf,
    path: &Path,
    mime: &str,
    req: &Request,
    extra: &[(String, String)],
) -> io::Result<bool> {
    let mut f = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(_) => {
            respond(wr, 404, &[], b"").await?;
            return Ok(true);
        }
    };
    let size = f.metadata().await?.len();
    let (code, start, end) = match req.range.as_deref() {
        None => (200, 0, size.saturating_sub(1)),
        Some(r) => match parse_range(r, size) {
            Some((s, e)) => (206, s, e),
            None => {
                let cr = format!("bytes */{size}");
                respond(wr, 416, &[("Content-Range", &cr)], b"").await?;
                return Ok(true);
            }
        },
    };
    let len = if size == 0 { 0 } else { end - start + 1 };
    let mut head = format!(
        "HTTP/1.1 {code} {}\r\nContent-Type: {mime}\r\nAccept-Ranges: bytes\r\nContent-Length: {len}\r\n",
        reason(code)
    );
    if code == 206 {
        head.push_str(&format!("Content-Range: bytes {start}-{end}/{size}\r\n"));
    }
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    wr.write_all(head.as_bytes()).await?;
    if req.head || len == 0 {
        return Ok(true);
    }
    f.seek(io::SeekFrom::Start(start)).await?;
    let mut left = len;
    let mut buf = vec![0u8; 256 * 1024];
    while left > 0 {
        let want = left.min(buf.len() as u64) as usize;
        let n = f.read(&mut buf[..want]).await?;
        if n == 0 {
            return Ok(false);
        }
        wr.write_all(&buf[..n]).await?;
        left -= n as u64;
    }
    Ok(true)
}

// ---------- playlists ----------

/// A playlist's bytes as text: some servers gzip it whether asked or not (a TV channel's CDN), and reqwest here
/// does not unpack. None when it is too big.
fn playlist_text(body: &[u8], gzip: bool) -> Option<Vec<u8>> {
    use std::io::Read;
    if !gzip && !body.starts_with(&[0x1f, 0x8b]) {
        return (body.len() <= MAX_PLAYLIST).then(|| body.to_vec());
    }
    let mut out = Vec::new();
    let mut dec = flate2::read::MultiGzDecoder::new(body).take(MAX_PLAYLIST as u64 + 1);
    match dec.read_to_end(&mut out) {
        Ok(_) if out.len() <= MAX_PLAYLIST => Some(out),
        Ok(_) => None,
        Err(_) => (body.len() <= MAX_PLAYLIST).then(|| body.to_vec()),
    }
}

pub fn is_playlist(url: &str, ctype: &str) -> bool {
    let c = ctype.to_ascii_lowercase();
    c.contains("mpegurl")
        || url
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
            .ends_with(".m3u8")
}

pub fn is_dash(url: &str, ctype: &str) -> bool {
    ctype.to_ascii_lowercase().contains("dash+xml")
        || url
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
            .ends_with(".mpd")
}

/// `url` cut where a folder ends: after the last "/" ahead of any template ("$Number$", filled in by the player, so
/// it must stay out of a shared folder's fixed part) or query. Returns (folder ending in "/", the rest).
fn split_folder(url: &str) -> (&str, &str) {
    let head = url.find(['$', '?']).unwrap_or(url.len());
    match url[..head].rfind('/') {
        Some(i) => url.split_at(i + 1),
        None => (url, ""),
    }
}

/// A DASH manifest at `base` with its links made ours (the Mac's Dash.swift does the same): absolute BaseURLs,
/// Locations and piece links (`media`, `initialization`, `sourceURL`, `index`) through `folder` (an origin folder →
/// our link for it, ending in "/"), a relative BaseURL at the top made absolute first, and a BaseURL of the
/// manifest's own folder added when there is none at the top. Nested relative bases follow the one above them.
pub fn rewrite_mpd(text: &str, base: &str, mut folder: impl FnMut(&str) -> String) -> String {
    let mut ours = |link: &str| {
        let abs = resolve(base, link);
        let (dir, rest) = split_folder(&abs);
        format!("{}{rest}", folder(dir))
    };
    let mut out = text.to_string();
    let period = out.find("<Period");
    let mut top_base = false;
    for tag in ["BaseURL", "Location"] {
        let mut from = 0;
        while let Some(i) = out[from..].find(&format!("<{tag}")) {
            let open = from + i;
            let Some(gt) = out[open..].find('>').map(|g| open + g) else {
                break;
            };
            let Some(close) = out[gt..].find(&format!("</{tag}>")).map(|c| gt + c) else {
                break;
            };
            let value = out[gt + 1..close].trim().to_string();
            let top = match period {
                Some(p) => open < p,
                None => true,
            };
            if tag == "BaseURL" && top {
                top_base = true;
            }
            let absolute = value.contains("://") || value.starts_with("//");
            let mut next = close;
            if !value.is_empty() && (absolute || top) {
                let new = ours(&value);
                out.replace_range(gt + 1..close, &new);
                next = gt + 1 + new.len();
            }
            from = next;
        }
    }
    for attr in ["media", "initialization", "sourceURL", "index"] {
        let pat = format!(" {attr}=\"");
        let mut from = 0;
        while let Some(i) = out[from..].find(&pat) {
            let s = from + i + pat.len();
            let Some(e) = out[s..].find('"').map(|e| s + e) else {
                break;
            };
            let value = out[s..e].to_string();
            let mut next = e + 1;
            if value.contains("://") || value.starts_with("//") {
                let new = ours(&value);
                out.replace_range(s..e, &new);
                next = s + new.len() + 1;
            }
            from = next;
        }
    }
    if !top_base {
        if let Some(gt) = out
            .find("<MPD")
            .and_then(|m| out[m..].find('>').map(|g| m + g))
        {
            let own = folder(split_folder(base).0);
            out.insert_str(gt + 1, &format!("<BaseURL>{own}</BaseURL>"));
        }
    }
    out
}

/// `rel` resolved against `base` (enough of RFC 3986 for playlists: absolute, scheme-relative, root-relative, relative).
pub fn resolve(base: &str, rel: &str) -> String {
    if rel.contains("://") {
        return rel.to_string();
    }
    let scheme = base.split("://").next().unwrap_or("https");
    if let Some(r) = rel.strip_prefix("//") {
        return format!("{scheme}://{r}");
    }
    let after = &base[scheme.len() + 3..];
    let authority = after.split('/').next().unwrap_or("");
    if rel.starts_with('/') {
        return format!("{scheme}://{authority}{rel}");
    }
    let path = base.split(['?', '#']).next().unwrap_or(base);
    let dir = &path[..path.rfind('/').map(|i| i + 1).unwrap_or(path.len())];
    let mut out = format!("{dir}{rel}");
    // drop "./" and fold "../"
    while let Some(i) = out.find("/./") {
        out.replace_range(i..i + 3, "/");
    }
    while let Some(i) = out.find("/../") {
        let head = &out[..i];
        let cut = head
            .rfind('/')
            .filter(|&j| j > scheme.len() + 2 + authority.len())
            .unwrap_or(i);
        out.replace_range(cut..i + 3, "");
    }
    out
}

/// Every link in an HLS playlist (lines that are not tags, and `URI="..."` in tags) made absolute and passed to `map`.
pub fn rewrite_playlist(text: &str, base: &str, mut map: impl FnMut(&str) -> String) -> String {
    let mut out = String::with_capacity(text.len() + 256);
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            out.push('\n');
            continue;
        }
        if t.starts_with('#') {
            let mut l = line.to_string();
            let mut from = 0;
            while let Some(i) = l[from..].find("URI=\"") {
                let s = from + i + 5;
                let Some(e) = l[s..].find('"').map(|e| s + e) else {
                    break;
                };
                let new = map(&resolve(base, &l[s..e]));
                l.replace_range(s..e, &new);
                from = s + new.len() + 1;
            }
            out.push_str(&l);
        } else {
            out.push_str(&map(&resolve(base, t)));
        }
        out.push('\n');
    }
    out
}

// ---------- what to send ----------

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Kind {
    Image,
    Media,
    Subtitle,
    /// A PDF, or a document turned into one first (`to_pdf`).
    Document,
    Unknown,
}

/// Extensions LibreOffice turns into PDF: office files, Keynote, text and code (shown as plain text).
const CONVERTIBLE: [&str; 41] = [
    "doc", "docx", "odt", "rtf", "txt", "md", "xls", "xlsx", "ods", "csv", "ppt", "pptx", "odp",
    "key", "odg", "fodt", "py", "rs", "js", "ts", "c", "h", "cpp", "hpp", "java", "go", "sh",
    "json", "yaml", "yml", "toml", "xml", "html", "css", "kt", "swift", "rb", "php", "sql", "lua",
    "log",
];

fn extension(s: &str) -> String {
    let path = s.split(['?', '#']).next().unwrap_or(s);
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// MIME type by extension, "" when unknown.
pub fn mime_of(s: &str) -> &'static str {
    match extension(s).as_str() {
        "mp4" | "m4v" | "mov" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "ts" | "m2ts" => "video/mp2t",
        "3gp" => "video/3gpp",
        "m3u8" => "application/vnd.apple.mpegurl",
        "mpd" => "application/dash+xml",
        "mp3" => "audio/mpeg",
        "m4a" | "m4b" => "audio/mp4",
        "aac" => "audio/aac",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "heic" | "heif" => "image/heif",
        "vtt" => "text/vtt",
        "srt" => "application/x-subrip",
        "pdf" => "application/pdf",
        _ => "",
    }
}

pub fn kind_of(s: &str) -> Kind {
    let ext = extension(s);
    if ext == "pdf" || (!is_url(s) && CONVERTIBLE.contains(&ext.as_str())) {
        return Kind::Document;
    }
    let m = mime_of(s);
    if m.starts_with("image/") {
        Kind::Image
    } else if m.starts_with("text/") || m == "application/x-subrip" {
        Kind::Subtitle
    } else if m.starts_with("video/") || m.starts_with("audio/") || m.starts_with("application/") {
        Kind::Media
    } else {
        Kind::Unknown
    }
}

fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// What `Cast` was asked: options next to the items, as JSON from D-Bus.
#[derive(Debug, Default, Clone)]
pub struct Options {
    pub title: Option<String>,
    pub start: f64,
    pub subs: Vec<String>,
    /// "direct" (the TV fetches links itself) or "sender" (relayed by luchd); local files always go via the sender.
    pub via: Option<String>,
    pub headers: Vec<(String, String)>,
    pub paused: bool,
    pub rate: Option<f64>,
    /// The kind of a link without an extension, from the page ("video/mp4", "application/vnd.apple.mpegurl").
    pub mime: Option<String>,
    pub poster: Option<String>,
    /// The same video in several qualities, best first: [{url, mime, w, h}] (a page's 360/720/1080 files). The
    /// receiver plays the best its screen shows; the first item is then only the fallback.
    pub sources: Vec<Value>,
    /// The way to the TV when it has several ("luch", "cast", "dlna"); luchd picks the best by itself otherwise.
    pub transport: Option<String>,
}

impl Options {
    pub fn parse(json: &str) -> Result<Options, String> {
        if json.trim().is_empty() {
            return Ok(Options::default());
        }
        let v: Value = serde_json::from_str(json).map_err(|e| format!("options: {e}"))?;
        let mut o = Options {
            title: v["title"].as_str().map(String::from),
            start: v["start"].as_f64().unwrap_or(0.0).max(0.0),
            via: v["via"].as_str().map(String::from),
            paused: v["paused"].as_bool().unwrap_or(false),
            rate: v["rate"].as_f64(),
            mime: v["mime"].as_str().map(String::from),
            poster: v["poster"].as_str().map(String::from),
            transport: v["transport"].as_str().map(String::from),
            sources: v["sources"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter(|s| s["url"].as_str().is_some_and(is_url))
                        .take(12)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
            ..Default::default()
        };
        if let Some(via) = &o.via {
            if via != "direct" && via != "sender" {
                return Err(format!("via must be direct or sender, not {via}"));
            }
        }
        for s in v["subs"].as_array().into_iter().flatten() {
            if let Some(s) = s.as_str() {
                o.subs.push(s.to_string());
            }
        }
        if let Some(h) = v["headers"].as_object() {
            for (k, val) in h {
                let Some(name) = PASSED_HEADERS.iter().find(|n| n.eq_ignore_ascii_case(k)) else {
                    continue;
                };
                if let Some(val) = val.as_str().filter(|s| !s.contains(['\r', '\n'])) {
                    o.headers.push((name.to_string(), val.to_string()));
                }
            }
        }
        Ok(o)
    }
}

fn file_name(s: &str) -> String {
    let path = s.split(['?', '#']).next().unwrap_or(s);
    path.rsplit('/').next().unwrap_or(path).to_string()
}

fn absolute(s: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(s.strip_prefix("file://").unwrap_or(s));
    let p = p.canonicalize().map_err(|e| format!("{s}: {e}"))?;
    if !p.is_file() {
        return Err(format!("{s}: not a file"));
    }
    Ok(p)
}

/// The video id and start time (s) of a YouTube link: watch, youtu.be, shorts, embed, live; `t=` as 90, 90s or 1m30s.
pub fn youtube(url: &str) -> Option<(String, f64)> {
    let u = url.split('#').next()?;
    let (_, rest) = u.split_once("://")?;
    let (host, path_query) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_ascii_lowercase();
    let host = host
        .trim_start_matches("www.")
        .trim_start_matches("m.")
        .trim_start_matches("music.");
    let (path, query) = path_query.split_once('?').unwrap_or((path_query, ""));
    let param = |k: &str| {
        query.split('&').find_map(|kv| {
            kv.split_once('=')
                .filter(|(a, _)| *a == k)
                .map(|(_, v)| v.to_string())
        })
    };
    let id = match host {
        "youtu.be" => path.split('/').next().map(String::from),
        "youtube.com" | "youtube-nocookie.com" => {
            let mut parts = path.split('/');
            match parts.next() {
                Some("watch") => param("v"),
                Some("shorts" | "embed" | "live" | "v") => parts.next().map(String::from),
                _ => None,
            }
        }
        _ => None,
    }?;
    let ok = (6..=20).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !ok {
        return None;
    }
    let start = param("t")
        .or_else(|| param("start"))
        .map(|t| youtube_time(&t))
        .unwrap_or(0.0);
    Some((id, start))
}

/// "90", "90s", "1m30s", "1h2m3s" in seconds.
fn youtube_time(t: &str) -> f64 {
    if let Ok(n) = t.parse::<f64>() {
        return n;
    }
    let (mut total, mut num) = (0.0, String::new());
    for c in t.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let n: f64 = num.parse().unwrap_or(0.0);
        num.clear();
        total += n * match c {
            'h' => 3600.0,
            'm' => 60.0,
            _ => 1.0,
        };
    }
    total + num.parse::<f64>().unwrap_or(0.0)
}

/// A document as PDF: a PDF as it is, anything else through LibreOffice (headless) into `out`. Blocking.
pub fn to_pdf(path: &Path, out: &Path) -> Result<PathBuf, String> {
    if extension(&path.to_string_lossy()) == "pdf" {
        return Ok(path.to_path_buf());
    }
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let office = ["soffice", "libreoffice"]
        .into_iter()
        .find(|c| {
            std::process::Command::new(c)
                .arg("--version")
                .output()
                .is_ok()
        })
        .ok_or("install LibreOffice to show this kind of document (PDF works without it)")?;
    let r = std::process::Command::new(office)
        .args(["--headless", "--convert-to", "pdf", "--outdir"])
        .arg(out)
        .arg(path)
        .output()
        .map_err(|e| format!("{office}: {e}"))?;
    let pdf = out.join(path.with_extension("pdf").file_name().unwrap_or_default());
    if !r.status.success() || !pdf.is_file() {
        return Err(format!(
            "could not turn {} into PDF: {}",
            path.display(),
            String::from_utf8_lossy(&r.stderr).trim()
        ));
    }
    Ok(pdf)
}

/// The `media`, `photo`, `doc` or `open_app` message for `items` (paths or http(s) links), sharing what needs sharing
/// with `client`. `me` is this machine's address as the receiver reaches it. Documents must be PDF by now (`to_pdf`).
pub fn build(
    items: &[String],
    o: &Options,
    srv: &Server,
    client: IpAddr,
    me: IpAddr,
) -> Result<Value, String> {
    if items.is_empty() {
        return Err("nothing to cast".into());
    }
    // A YouTube link plays in the TV's own YouTube app (PROTOCOL.md 4f): its stream is not to be had from a page.
    if let [one] = items {
        if let Some((id, t)) = youtube(one) {
            let start = if o.start > 0.0 { o.start } else { t };
            return Ok(json!({"t": "open_app", "app": "youtube", "id": id, "start": start}));
        }
    }
    if let Some(d) = items.iter().find(|i| kind_of(i) == Kind::Document) {
        if is_url(d) {
            return Err("a document has to be a file on this computer".into());
        }
        let p = absolute(d)?;
        if extension(&p.to_string_lossy()) != "pdf" {
            return Err(format!("{}: not a PDF yet", p.display()));
        }
        let file = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = o.title.clone().unwrap_or_else(|| file.clone());
        let t = srv.share(Item::File(p.clone()), client, "application/pdf");
        return Ok(
            json!({"t": "doc", "url": srv.url(me, &t, &file), "mime": "application/pdf",
                         "name": name, "page": o.start.max(0.0) as u64}),
        );
    }
    let photos = items.iter().all(|i| kind_of(i) == Kind::Image);
    if photos {
        let mut list = Vec::new();
        for i in items.iter().take(MAX_PHOTOS) {
            let (item, name) = if is_url(i) {
                // The receiver only fetches photos from the sender: links are relayed.
                (
                    Item::Relay {
                        url: i.clone(),
                        headers: o.headers.clone(),
                    },
                    file_name(i),
                )
            } else {
                let p = absolute(i)?;
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                (Item::File(p), name)
            };
            let mime = mime_of(i);
            let t = srv.share(item, client, mime);
            list.push(json!({"url": srv.url(me, &t, &name), "mime": mime, "name": name}));
        }
        return Ok(json!({"t": "photo", "index": 0, "items": list}));
    }
    let first = items
        .iter()
        .find(|i| kind_of(i) == Kind::Media || (is_url(i) && kind_of(i) == Kind::Unknown))
        .ok_or("no video or sound among the items")?;
    let relay = o.via.as_deref() == Some("sender");
    let mime = o.mime.clone().unwrap_or_else(|| mime_of(first).to_string());
    let (url, via) = share_one(first, &mime, relay, o, srv, client, me)?;
    let title = o.title.clone().unwrap_or_else(|| {
        let n = file_name(first);
        n.rsplit_once('.').map(|(a, _)| a.to_string()).unwrap_or(n)
    });
    let mut src = json!({"url": url});
    if !mime.is_empty() {
        src["mime"] = json!(mime);
    }
    let mut list = Vec::new();
    for s in &o.sources {
        // Every quality travels the way the first one does.
        let u = s["url"].as_str().unwrap_or_default();
        let m = s["mime"].as_str().unwrap_or(&mime).to_string();
        let (link, _) = share_one(u, &m, via == "sender", o, srv, client, me)?;
        let mut e = json!({"url": link});
        if !m.is_empty() {
            e["mime"] = json!(m);
        }
        for k in ["w", "h", "codec"] {
            if !s[k].is_null() {
                e[k] = s[k].clone();
            }
        }
        list.push(e);
    }
    if list.is_empty() {
        list.push(src);
    }
    let mut msg = json!({"t": "media", "via": via, "src": list, "title": title});
    if o.start > 0.0 {
        msg["start"] = json!(o.start);
    }
    if o.paused {
        msg["paused"] = json!(true);
    }
    if let Some(r) = o.rate {
        msg["rate"] = json!(r);
    }
    if via == "direct" && !o.headers.is_empty() {
        msg["headers"] = Value::Object(
            o.headers
                .iter()
                .map(|(k, v)| (k.clone(), json!(v)))
                .collect(),
        );
    }
    if let Some(p) = &o.poster {
        if let Ok((u, _)) = share_one(p, mime_of(p), relay || via == "sender", o, srv, client, me) {
            msg["poster"] = json!(u);
        }
    }
    let mut subs = Vec::new();
    let mut s_items: Vec<String> = o.subs.clone();
    s_items.extend(
        items
            .iter()
            .filter(|i| kind_of(i) == Kind::Subtitle)
            .cloned(),
    );
    for (n, s) in s_items.iter().enumerate() {
        // Subtitles go the way of the video: a relayed video has relayed subtitles.
        let (u, _) = share_one(s, mime_of(s), relay || via == "sender", o, srv, client, me)?;
        let name = file_name(s);
        let lang = sub_lang(&name);
        subs.push(
            json!({"id": format!("s{n}"), "url": u, "mime": mime_of(s), "lang": lang,
                         "label": if lang.is_empty() { name.clone() } else { lang.clone() }}),
        );
    }
    if !subs.is_empty() {
        msg["subs"] = json!(subs);
    }
    Ok(msg)
}

/// "film.en.srt" → "en"; "" when the name carries no language.
fn sub_lang(name: &str) -> String {
    let parts: Vec<&str> = name.split('.').collect();
    if parts.len() >= 3 {
        let l = parts[parts.len() - 2];
        if (2..=3).contains(&l.len()) && l.chars().all(|c| c.is_ascii_alphabetic()) {
            return l.to_ascii_lowercase();
        }
    }
    String::new()
}

#[allow(clippy::too_many_arguments)]
fn share_one(
    s: &str,
    mime: &str,
    relay: bool,
    o: &Options,
    srv: &Server,
    client: IpAddr,
    me: IpAddr,
) -> Result<(String, &'static str), String> {
    if is_url(s) {
        if !relay {
            return Ok((s.to_string(), "direct"));
        }
        let t = srv.share(
            Item::Relay {
                url: s.to_string(),
                headers: o.headers.clone(),
            },
            client,
            mime,
        );
        return Ok((srv.url(me, &t, &file_name(s)), "sender"));
    }
    let p = absolute(s)?;
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mime = if mime.is_empty() {
        "application/octet-stream"
    } else {
        mime
    };
    let t = srv.share(Item::File(p), client, mime);
    Ok((srv.url(me, &t, &name), "sender"))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn get(addr: SocketAddr, path: &str, range: Option<&str>) -> (u16, String, Vec<u8>) {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let mut req = format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n");
        if let Some(r) = range {
            req.push_str(&format!("Range: {r}\r\n"));
        }
        req.push_str("\r\n");
        s.write_all(req.as_bytes()).await.unwrap();
        let mut all = Vec::new();
        s.read_to_end(&mut all).await.unwrap();
        let split = all.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8_lossy(&all[..split]).to_string();
        let code = head[9..12].parse().unwrap();
        (code, head, all[split + 4..].to_vec())
    }

    async fn server() -> (Server, SocketAddr) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        (Server::serve(l).unwrap(), addr)
    }

    fn temp_file(name: &str, data: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("luch-cast-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, data).unwrap();
        p
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-", 100), Some((0, 99)));
        assert_eq!(parse_range("bytes=10-19", 100), Some((10, 19)));
        assert_eq!(parse_range("bytes=90-200", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=100-", 100), None);
        assert_eq!(parse_range("bytes=5-1", 100), None);
        assert_eq!(parse_range("items=0-1", 100), None);
    }

    #[tokio::test]
    async fn files_are_served_with_range_to_their_receiver_only() {
        let (srv, addr) = server().await;
        let data: Vec<u8> = (0..=255u8).cycle().take(10_000).collect();
        let p = temp_file("clip.mp4", &data);
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        let t = srv.share(Item::File(p.clone()), loopback, "video/mp4");
        let url = srv.url(loopback, &t, "clip.mp4");
        let path = &url[url.find(&t).unwrap() - 1..];

        let (code, head, body) = get(addr, path, None).await;
        assert_eq!(code, 200);
        assert!(head.contains("Content-Type: video/mp4"), "{head}");
        assert!(head.contains("Accept-Ranges: bytes"));
        assert_eq!(body, data);

        let (code, head, body) = get(addr, path, Some("bytes=1000-1099")).await;
        assert_eq!(code, 206);
        assert!(
            head.contains("Content-Range: bytes 1000-1099/10000"),
            "{head}"
        );
        assert_eq!(body, &data[1000..1100]);

        let (code, _, _) = get(addr, path, Some("bytes=20000-")).await;
        assert_eq!(code, 416);
        let (code, _, _) = get(addr, "/nope/clip.mp4", None).await;
        assert_eq!(code, 404);

        // Shared with another receiver: this one gets nothing.
        let t2 = srv.share(Item::File(p), "192.168.1.30".parse().unwrap(), "video/mp4");
        let (code, _, _) = get(addr, &format!("/{t2}/clip.mp4"), None).await;
        assert_eq!(code, 403);

        srv.forget(loopback);
        let (code, _, _) = get(addr, path, None).await;
        assert_eq!(code, 404);
    }

    #[tokio::test]
    async fn keep_alive_serves_several_requests() {
        let (srv, addr) = server().await;
        let p = temp_file("a.mp3", b"0123456789");
        let t = srv.share(Item::File(p), "127.0.0.1".parse().unwrap(), "audio/mpeg");
        let mut s = TcpStream::connect(addr).await.unwrap();
        let one = format!("GET /{t}/a.mp3 HTTP/1.1\r\nRange: bytes=0-3\r\n\r\n");
        s.write_all(format!("{one}{one}").as_bytes()).await.unwrap();
        let mut buf = vec![0u8; 4096];
        let mut got = Vec::new();
        while got.windows(4).filter(|w| *w == b"0123").count() < 2 {
            let n = tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf))
                .await
                .unwrap()
                .unwrap();
            assert!(n > 0, "closed early: {}", String::from_utf8_lossy(&got));
            got.extend_from_slice(&buf[..n]);
        }
    }

    /// A tiny origin server: a playlist with relative, absolute and URI= links, and a segment that needs a cookie.
    async fn origin() -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (s, _) = l.accept().await.unwrap();
                tokio::spawn(async move {
                    let (rd, mut wr) = s.into_split();
                    let mut rd = BufReader::new(rd);
                    let mut first = String::new();
                    rd.read_line(&mut first).await.unwrap();
                    let mut cookie = String::new();
                    let mut range = String::new();
                    loop {
                        let mut l = String::new();
                        rd.read_line(&mut l).await.unwrap();
                        if l.trim().is_empty() {
                            break;
                        }
                        let lower = l.to_ascii_lowercase();
                        if lower.starts_with("cookie:") {
                            cookie = l[7..].trim().to_string();
                        }
                        if lower.starts_with("range:") {
                            range = l[6..].trim().to_string();
                        }
                    }
                    let path = first.split_whitespace().nth(1).unwrap().to_string();
                    let (code, ctype, body): (u16, &str, Vec<u8>) = if cookie != "s=1" {
                        (403, "text/plain", b"no".to_vec())
                    } else if path == "/v/index.m3u8" {
                        (200, "application/vnd.apple.mpegurl",
                         b"#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n#EXTINF:4,\nseg0.ts\n#EXTINF:4,\n/v/seg1.ts?x=1\n".to_vec())
                    } else if path == "/v/clip.mpd" {
                        (200, "application/dash+xml",
                         b"<MPD type=\"static\"><Period><SegmentTemplate media=\"seg$Number$.m4s\" initialization=\"seg0.m4s\"/></Period></MPD>".to_vec())
                    } else if path == "/v/gz.m3u8" {
                        let mut gz = flate2::write::GzEncoder::new(
                            Vec::new(),
                            flate2::Compression::default(),
                        );
                        std::io::Write::write_all(&mut gz, b"#EXTM3U\n#EXTINF:4,\nseg0.ts\n")
                            .unwrap();
                        (200, "application/vnd.apple.mpegurl", gz.finish().unwrap())
                    } else if path.starts_with("/v/seg") || path == "/v/key.bin" {
                        (
                            if range.is_empty() { 200 } else { 206 },
                            "video/mp2t",
                            format!("data {path} {range}").into_bytes(),
                        )
                    } else {
                        (404, "text/plain", Vec::new())
                    };
                    // A CDN that gzips its playlists whether asked or not.
                    let enc = if path == "/v/gz.m3u8" {
                        "Content-Encoding: gzip\r\n"
                    } else {
                        ""
                    };
                    let head = format!("HTTP/1.1 {code} X\r\nContent-Type: {ctype}\r\n{enc}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                    let _ = wr.write_all(head.as_bytes()).await;
                    let _ = wr.write_all(&body).await;
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn relay_carries_headers_and_rewrites_playlists() {
        let up = origin().await;
        let (srv, addr) = server().await;
        let me: IpAddr = "127.0.0.1".parse().unwrap();
        let headers = vec![("Cookie".to_string(), "s=1".to_string())];
        let t = srv.share(
            Item::Relay {
                url: format!("http://{up}/v/index.m3u8"),
                headers,
            },
            me,
            "application/vnd.apple.mpegurl",
        );
        let (code, _, body) = get(addr, &format!("/{t}/index.m3u8"), None).await;
        assert_eq!(code, 200);
        let text = String::from_utf8(body).unwrap();
        let ours = format!("http://127.0.0.1:{}/", addr.port());
        let links: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
        assert_eq!(links.len(), 2, "{text}");
        assert!(links.iter().all(|l| l.starts_with(&ours)), "{text}");
        assert!(text.contains(&format!("URI=\"{ours}")), "{text}");
        assert!(
            !text.contains(&up.to_string()),
            "the origin leaks through: {text}"
        );

        // A rewritten segment link goes through with the cookie and the range.
        let seg = links[1];
        let (code, _, body) = get(addr, &seg[ours.len() - 1..], Some("bytes=0-9")).await;
        assert_eq!(code, 206);
        assert_eq!(
            String::from_utf8(body).unwrap(),
            "data /v/seg1.ts?x=1 bytes=0-9"
        );

        // A playlist gzipped without being asked is unpacked before it is rewritten.
        let gz = srv.share(
            Item::Relay {
                url: format!("http://{up}/v/gz.m3u8"),
                headers: vec![("Cookie".to_string(), "s=1".to_string())],
            },
            me,
            "",
        );
        let (code, _, body) = get(addr, &format!("/{gz}/gz.m3u8"), None).await;
        assert_eq!(code, 200);
        let text = String::from_utf8(body).unwrap();
        assert!(text.starts_with("#EXTM3U\n"), "{text}");
        assert!(text.lines().nth(2).unwrap().starts_with(&ours), "{text}");

        // A DASH manifest: its folder shared, a piece under it fetched from the origin's folder with the cookie.
        let headers = vec![("Cookie".to_string(), "s=1".to_string())];
        let dash = srv.share(
            Item::Relay {
                url: format!("http://{up}/v/clip.mpd"),
                headers: headers.clone(),
            },
            me,
            "",
        );
        let (code, _, body) = get(addr, &format!("/{dash}/clip.mpd"), None).await;
        assert_eq!(code, 200);
        let text = String::from_utf8(body).unwrap();
        let base = text
            .split("<BaseURL>")
            .nth(1)
            .and_then(|b| b.split("</BaseURL>").next())
            .unwrap()
            .to_string();
        assert!(base.starts_with(&ours), "{text}");
        assert!(
            !text.contains(&up.to_string()),
            "the origin leaks through: {text}"
        );
        let (code, _, body) =
            get(addr, &format!("{}seg3.m4s", &base[ours.len() - 1..]), None).await;
        assert_eq!(code, 200);
        assert_eq!(String::from_utf8(body).unwrap(), "data /v/seg3.m4s ");
        // Fetched anew (a live manifest), the folder keeps its token.
        let (_, _, again) = get(addr, &format!("/{dash}/clip.mpd"), None).await;
        assert!(String::from_utf8(again).unwrap().contains(&base));

        // Without the page's cookie the origin refuses, and that is passed on.
        let bare = srv.share(
            Item::Relay {
                url: format!("http://{up}/v/seg0.ts"),
                headers: vec![],
            },
            me,
            "",
        );
        let (code, _, _) = get(addr, &format!("/{bare}/seg0.ts"), None).await;
        assert_eq!(code, 403);
    }

    /// The same manifests as the Mac's CastTests.testDashManifestsPointBackHere, so both senders agree.
    #[test]
    fn dash_manifests_point_back_here() {
        let folder = |dir: &str| format!("http://192.168.1.2:47810/T[{dir}]/");
        let plain = "<?xml version=\"1.0\"?>\n<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" type=\"static\"><Period><AdaptationSet><Representation id=\"0\"><SegmentTemplate initialization=\"init-$RepresentationID$.m4s\" media=\"chunk-$RepresentationID$-$Number%05d$.m4s\"/></Representation></AdaptationSet></Period></MPD>";
        let a = rewrite_mpd(plain, "https://cdn.example/v/1/clip.mpd?tok=9", folder);
        assert!(a.contains("type=\"static\"><BaseURL>http://192.168.1.2:47810/T[https://cdn.example/v/1/]/</BaseURL><Period>"), "{a}");
        assert!(
            a.contains("media=\"chunk-$RepresentationID$-$Number%05d$.m4s\""),
            "{a}"
        );
        let abs = "<MPD><BaseURL>https://media.example/a/</BaseURL><Period><AdaptationSet><BaseURL>audio/</BaseURL><SegmentTemplate media=\"https://seg.example/x/$RepresentationID$/$Number$.m4s\"/></AdaptationSet></Period></MPD>";
        let b = rewrite_mpd(abs, "https://cdn.example/m.mpd", folder);
        assert!(
            b.contains(
                "<MPD><BaseURL>http://192.168.1.2:47810/T[https://media.example/a/]/</BaseURL>"
            ),
            "{b}"
        );
        assert!(b.contains("<BaseURL>audio/</BaseURL>"), "{b}");
        assert!(b.contains("media=\"http://192.168.1.2:47810/T[https://seg.example/x/]/$RepresentationID$/$Number$.m4s\""), "{b}");
        assert_eq!(b.matches("<BaseURL>").count(), 2, "{b}");
        let top = rewrite_mpd(
            "<MPD><BaseURL>video/</BaseURL><Period/></MPD>",
            "https://cdn.example/d/m.mpd",
            folder,
        );
        assert_eq!(top, "<MPD><BaseURL>http://192.168.1.2:47810/T[https://cdn.example/d/video/]/</BaseURL><Period/></MPD>");
        assert!(is_dash("https://x.example/a/m.mpd?x=1", ""));
        assert!(is_dash("https://x.example/?id=1", "application/dash+xml"));
        assert!(!is_dash(
            "https://x.example/a.m3u8",
            "application/vnd.apple.mpegurl"
        ));
    }

    async fn raw(addr: SocketAddr, req: &str) -> String {
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(req.as_bytes()).await.unwrap();
        let mut all = Vec::new();
        s.read_to_end(&mut all).await.unwrap();
        String::from_utf8_lossy(&all).into_owned()
    }

    /// What TVs without Luch ask of the file server: DLNA's content features, CORS for a Chromecast's page (and its
    /// preflight), WebVTT made of SubRip, a header added for one link.
    #[tokio::test]
    async fn tvs_get_dlna_features_cors_and_webvtt() {
        let (srv, addr) = server().await;
        let me: IpAddr = "127.0.0.1".parse().unwrap();
        let v = temp_file("a.mp4", b"0123456789");
        let t = srv.share(Item::File(v), me, "video/mp4");
        let url = srv.url(me, &t, "a.mp4");
        srv.add_header(&url, "CaptionInfo.sec", "http://127.0.0.1/s.srt");
        let r = raw(addr, &format!("GET /{t}/a.mp4 HTTP/1.1\r\ngetcontentFeatures.dlna.org: 1\r\ntransferMode.dlna.org: Streaming\r\nConnection: close\r\n\r\n")).await;
        assert!(
            r.contains(&format!("contentFeatures.dlna.org: {DLNA_STREAM}\r\n")),
            "{r}"
        );
        assert!(r.contains("transferMode.dlna.org: Streaming\r\n"), "{r}");
        assert!(
            r.contains("CaptionInfo.sec: http://127.0.0.1/s.srt\r\n"),
            "{r}"
        );
        assert!(
            !r.contains("Access-Control"),
            "no CORS unless a page asks: {r}"
        );
        let r = raw(addr, &format!("GET /{t}/a.mp4 HTTP/1.1\r\nOrigin: https://www.gstatic.com\r\nConnection: close\r\n\r\n")).await;
        assert!(r.contains("Access-Control-Allow-Origin: *\r\n"), "{r}");
        let r = raw(addr, &format!("OPTIONS /{t}/a.mp4 HTTP/1.1\r\nOrigin: https://www.gstatic.com\r\nAccess-Control-Request-Headers: range\r\nConnection: close\r\n\r\n")).await;
        assert!(
            r.starts_with("HTTP/1.1 204 ") && r.contains("Access-Control-Allow-Headers: Range"),
            "{r}"
        );

        let srt = temp_file(
            "a.en.srt",
            "\u{feff}1\r\n00:00:01,000 --> 00:00:02,500\r\nHello, world\r\n".as_bytes(),
        );
        let t = srv.share(Item::File(srt), me, "application/x-subrip");
        let url = srv.url(me, &t, "a.en.srt");
        assert!(srv.as_vtt(&url));
        assert!(!srv.as_vtt("http://127.0.0.1:1/nope/x.srt"));
        let (code, head, body) = get(addr, &format!("/{t}/a.en.srt"), None).await;
        assert_eq!(code, 200);
        assert!(head.contains("text/vtt"), "{head}");
        assert_eq!(
            String::from_utf8(body).unwrap(),
            "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.500\nHello, world\n"
        );
    }

    #[test]
    fn links_resolve() {
        let b = "https://cdn.example/a/b/index.m3u8?t=1";
        assert_eq!(resolve(b, "seg.ts"), "https://cdn.example/a/b/seg.ts");
        assert_eq!(resolve(b, "../c/seg.ts"), "https://cdn.example/a/c/seg.ts");
        assert_eq!(resolve(b, "./seg.ts"), "https://cdn.example/a/b/seg.ts");
        assert_eq!(resolve(b, "/root.ts"), "https://cdn.example/root.ts");
        assert_eq!(
            resolve(b, "//other.example/x.ts"),
            "https://other.example/x.ts"
        );
        assert_eq!(resolve(b, "http://x.example/y.ts"), "http://x.example/y.ts");
    }

    #[test]
    fn youtube_links() {
        let id = "dQw4w9WgXcQ";
        for u in [
            format!("https://www.youtube.com/watch?v={id}"),
            format!("https://m.youtube.com/watch?feature=share&v={id}"),
            format!("https://youtu.be/{id}"),
            format!("https://www.youtube.com/shorts/{id}"),
            format!("https://www.youtube-nocookie.com/embed/{id}?rel=0"),
            format!("https://music.youtube.com/watch?v={id}&list=x"),
        ] {
            assert_eq!(youtube(&u).map(|(i, _)| i).as_deref(), Some(id), "{u}");
        }
        assert_eq!(
            youtube(&format!("https://youtu.be/{id}?t=90")).unwrap().1,
            90.0
        );
        assert_eq!(
            youtube(&format!("https://www.youtube.com/watch?v={id}&t=1m30s"))
                .unwrap()
                .1,
            90.0
        );
        assert_eq!(
            youtube(&format!("https://www.youtube.com/watch?v={id}&t=1h2m3s"))
                .unwrap()
                .1,
            3723.0
        );
        assert!(youtube("https://www.youtube.com/feed/subscriptions").is_none());
        assert!(youtube("https://www.youtube.com/watch?v=../../etc").is_none());
        assert!(youtube("https://notyoutube.com/watch?v=dQw4w9WgXcQ").is_none());
    }

    #[tokio::test]
    async fn youtube_and_documents_become_their_own_messages() {
        let (srv, _) = server().await;
        let tv: IpAddr = "192.168.1.30".parse().unwrap();
        let me: IpAddr = "192.168.1.20".parse().unwrap();
        let yt = build(
            &["https://youtu.be/dQw4w9WgXcQ?t=42".into()],
            &Options::default(),
            &srv,
            tv,
            me,
        )
        .unwrap();
        assert_eq!(
            yt,
            json!({"t": "open_app", "app": "youtube", "id": "dQw4w9WgXcQ", "start": 42.0})
        );
        let pdf = temp_file("Report 1.pdf", b"%PDF-1.4");
        let d = build(
            &[pdf.to_string_lossy().into_owned()],
            &Options::default(),
            &srv,
            tv,
            me,
        )
        .unwrap();
        assert_eq!(d["t"], "doc");
        assert_eq!(d["name"], "Report 1.pdf");
        assert!(d["url"].as_str().unwrap().ends_with("/Report%201.pdf"));
        let docx = temp_file("a.docx", b"x");
        assert!(build(
            &[docx.to_string_lossy().into_owned()],
            &Options::default(),
            &srv,
            tv,
            me
        )
        .is_err());
        assert_eq!(kind_of("main.rs"), Kind::Document);
        assert_eq!(kind_of("https://x.example/a.txt"), Kind::Unknown);
    }

    #[test]
    fn documents_turn_into_pdf_when_libreoffice_is_there() {
        if std::process::Command::new("soffice")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("no LibreOffice: skipped");
            return;
        }
        let txt = temp_file("notes.txt", "Luch\nпривет\n".as_bytes());
        let out = txt.parent().unwrap().join("pdf");
        let pdf = to_pdf(&txt, &out).unwrap();
        assert!(std::fs::read(&pdf).unwrap().starts_with(b"%PDF"));
    }

    #[test]
    fn kinds_and_types() {
        assert_eq!(kind_of("/home/u/Видео/film.MKV"), Kind::Media);
        assert_eq!(
            mime_of("https://x.example/a/index.m3u8?token=1"),
            "application/vnd.apple.mpegurl"
        );
        assert_eq!(kind_of("IMG_1.JPG"), Kind::Image);
        assert_eq!(kind_of("film.en.srt"), Kind::Subtitle);
        assert_eq!(kind_of("notes.txt"), Kind::Document);
        assert_eq!(kind_of("talk.key"), Kind::Document);
        assert_eq!(kind_of("archive.zip"), Kind::Unknown);
        assert_eq!(sub_lang("film.en.srt"), "en");
        assert_eq!(sub_lang("film.srt"), "");
    }

    #[tokio::test]
    async fn messages_for_files_links_and_photos() {
        let (srv, _) = server().await;
        let tv: IpAddr = "192.168.1.30".parse().unwrap();
        let me: IpAddr = "192.168.1.20".parse().unwrap();
        let video = temp_file("Holiday 2026.mp4", b"x");
        let sub = video.with_file_name("Holiday 2026.ru.vtt");
        std::fs::write(&sub, "WEBVTT\n").unwrap();
        let items = vec![
            video.to_string_lossy().into_owned(),
            sub.to_string_lossy().into_owned(),
        ];
        let m = build(
            &items,
            &Options {
                start: 12.0,
                ..Default::default()
            },
            &srv,
            tv,
            me,
        )
        .unwrap();
        assert_eq!(m["t"], "media");
        assert_eq!(m["via"], "sender");
        assert_eq!(m["title"], "Holiday 2026");
        assert_eq!(m["start"], 12.0);
        let url = m["src"][0]["url"].as_str().unwrap();
        assert!(
            url.starts_with(&format!("http://192.168.1.20:{}/", srv.port())),
            "{url}"
        );
        assert!(url.ends_with("/Holiday%202026.mp4"), "{url}");
        assert_eq!(m["src"][0]["mime"], "video/mp4");
        assert_eq!(m["subs"][0]["lang"], "ru");
        assert_eq!(m["subs"][0]["mime"], "text/vtt");
        assert_eq!(srv.shared_count(), 2);

        // A link goes direct with its headers; asked to, it is relayed and the headers stay here.
        let o =
            Options::parse(r#"{"headers":{"cookie":"a=1","Host":"evil"},"title":"Clip"}"#).unwrap();
        let link = vec!["https://cdn.example/v/index.m3u8".to_string()];
        let d = build(&link, &o, &srv, tv, me).unwrap();
        assert_eq!(d["via"], "direct");
        assert_eq!(d["src"][0]["url"], link[0]);
        assert_eq!(d["headers"], json!({"Cookie": "a=1"}));
        let o2 = Options {
            via: Some("sender".into()),
            ..o.clone()
        };
        let r = build(&link, &o2, &srv, tv, me).unwrap();
        assert_eq!(r["via"], "sender");
        assert!(r["src"][0]["url"]
            .as_str()
            .unwrap()
            .starts_with("http://192.168.1.20:"));
        assert!(r.get("headers").is_none());

        // Several qualities of a page's video: all go, sizes kept, relayed alike.
        let q = Options::parse(
            r#"{"via":"sender","sources":[{"url":"https://cdn.example/720/a.mp4","h":720,"w":1280},
                {"url":"https://cdn.example/360/a.mp4","h":360,"w":640},{"url":"file:///etc/passwd"}]}"#,
        )
        .unwrap();
        let m = build(&["https://cdn.example/360/a.mp4".into()], &q, &srv, tv, me).unwrap();
        let src = m["src"].as_array().unwrap();
        assert_eq!(src.len(), 2, "{m}");
        assert_eq!(src[0]["h"], 720);
        assert!(src[0]["url"]
            .as_str()
            .unwrap()
            .starts_with("http://192.168.1.20:"));

        let a = temp_file("a.jpg", b"j");
        let b = a.with_file_name("b.png");
        std::fs::write(&b, b"p").unwrap();
        let photos = vec![
            a.to_string_lossy().into_owned(),
            b.to_string_lossy().into_owned(),
        ];
        let p = build(&photos, &Options::default(), &srv, tv, me).unwrap();
        assert_eq!(p["t"], "photo");
        assert_eq!(p["items"].as_array().unwrap().len(), 2);
        assert_eq!(p["items"][1]["mime"], "image/png");

        assert!(build(
            &["/nonexistent/x.mp4".into()],
            &Options::default(),
            &srv,
            tv,
            me
        )
        .is_err());
        assert!(Options::parse(r#"{"via":"carrier pigeon"}"#).is_err());
    }
}
