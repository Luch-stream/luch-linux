//! luchd — Luch v0 sender daemon: announces `_luchsrc._tcp`, browses `_luch._tcp`
//! receivers via Avahi's D-Bus API, keeps control channels, mirrors the screen, exposes dev.luch.Luch1.

mod discovery;
mod display;
mod enc;
mod mirror;
mod web_tls;

use std::{collections::HashMap, error::Error, io, net::SocketAddr, sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use luch_proto::{self as p, ReceiverRow};
use mirror::Cmd;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::{tcp::OwnedWriteHalf, TcpSocket},
    sync::{broadcast, mpsc, Mutex},
    time::{timeout, Instant},
};
use tokio_tungstenite::{accept_async, tungstenite::Message, WebSocketStream};
use zbus::{
    fdo, interface, message::Type as MsgType, object_server::SignalEmitter,
    zvariant::OwnedObjectPath, Connection, MatchRule, MessageStream,
};

const AVAHI: &str = "org.freedesktop.Avahi";
const AVAHI_SERVER: &str = "org.freedesktop.Avahi.Server";
const AVAHI_IF_UNSPEC: i32 = -1;
const AVAHI_PROTO_INET: i32 = 0;
const PING_EVERY: Duration = Duration::from_secs(1); // also the RTT sample rate
const DEAD_AFTER: Duration = Duration::from_secs(30);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Receiver {
    id: String,
    name: String,
    model: String,
    address: String,
    ifname: String,
    ifindex: i32,
    port: u16,
    service: String, // Avahi instance name, for ItemRemove
}

/// A live control channel: commands to it, receiver messages (ready, keyframe, stop, ...) from it.
#[derive(Clone)]
struct Conn {
    cmd: mpsc::Sender<Cmd>,
    events: broadcast::Sender<Value>,
    /// Web receiver only: media frames (header + data) to its websocket writer task.
    media: Option<mpsc::Sender<Vec<u8>>>,
}

#[derive(Default)]
struct State {
    found: HashMap<String, Receiver>,
    conns: HashMap<String, Conn>,
    mirrors: HashMap<String, mpsc::Sender<()>>, // id -> stop signal
    stats: HashMap<String, mirror::Stats>,      // id -> live mirroring stats
    welcomes: HashMap<String, Value>,           // id -> last welcome (hw passport, decoder bench)
}
type Shared = Arc<Mutex<State>>;

struct Luch {
    st: Shared,
    sender_id: String,
    host: String,
    discovery: discovery::Discovery,
}

#[interface(name = "dev.luch.Luch1")]
impl Luch {
    async fn receivers(&self) -> Vec<ReceiverRow> {
        let st = self.st.lock().await;
        let mut rows: Vec<ReceiverRow> = st
            .found
            .values()
            .map(|r| {
                let (c, m) = (st.conns.contains_key(&r.id), st.mirrors.contains_key(&r.id));
                (
                    r.id.clone(),
                    r.name.clone(),
                    r.model.clone(),
                    r.address.clone(),
                    r.ifname.clone(),
                    r.port,
                    c,
                    m,
                )
            })
            .collect();
        rows.sort();
        rows
    }

    /// Starts an immediate Avahi rescan; results arrive via ReceiversChanged.
    async fn refresh(&self) {
        self.discovery.refresh();
    }

    /// Opens and keeps a control channel; returns the receiver's welcome as JSON.
    async fn connect(
        &self,
        id: String,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<String> {
        {
            let st = self.st.lock().await;
            if st.conns.contains_key(&id) {
                return Err(fdo::Error::Failed(format!("already connected: {id}")));
            }
            if !st.found.contains_key(&id) {
                return Err(fdo::Error::InvalidArgs(format!("unknown receiver: {id}")));
            }
        }
        let r = self
            .discovery
            .receiver(id.clone())
            .await
            .map_err(fdo::Error::Failed)?;
        let (welcome, rd, wr) = handshake(&r, &self.sender_id, &self.host)
            .await
            .map_err(|e| {
                fdo::Error::Failed(format!("{} ({}%{}): {e}", r.name, r.address, r.ifname))
            })?;
        let (tx, rx) = mpsc::channel(8);
        let (events, _) = broadcast::channel(16);
        {
            let mut st = self.st.lock().await;
            st.conns.insert(
                id.clone(),
                Conn {
                    cmd: tx,
                    events: events.clone(),
                    media: None,
                },
            );
            st.welcomes.insert(id.clone(), welcome.clone());
        }
        eprintln!(
            "[luchd] connected to {} ({}:{} via {})",
            r.name, r.address, r.port, r.ifname
        );
        changed(conn).await;
        tokio::spawn(run_channel(
            r.name,
            id,
            rd,
            wr,
            rx,
            events,
            self.st.clone(),
            conn.clone(),
        ));
        Ok(welcome.to_string())
    }

    async fn disconnect(&self, id: String) -> fdo::Result<()> {
        let c = self.st.lock().await.conns.get(&id).cloned();
        let c = c.ok_or_else(|| fdo::Error::InvalidArgs(format!("not connected: {id}")))?;
        let _ = c.cmd.send(Cmd::Bye).await;
        Ok(())
    }

    /// Mirror to a receiver (connecting first if needed). `source`:
    /// "fast" (full screen straight from KMS scanout via luch-kms, lowest latency),
    /// "monitor", "window" or "virtual" (a new GNOME virtual monitor, i.e. extend) via the
    /// screencast portal: its picker is shown the first time, later runs reuse a saved token.
    /// Returns once the session is set up; it then runs until StopMirror or the receiver stops it.
    async fn mirror(
        &self,
        id: String,
        source: String,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        self.mirror_sized(id, source, 0, 0, conn).await
    }

    async fn mirror_sized(
        &self,
        id: String,
        source: String,
        width: u32,
        height: u32,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        if (width, height) != (0, 0)
            && (!(320..=7680).contains(&width)
                || !(240..=4320).contains(&height)
                || width % 2 != 0
                || height % 2 != 0)
        {
            return Err(fdo::Error::InvalidArgs(format!(
                "bad size {width}x{height}"
            )));
        }
        if !self.st.lock().await.conns.contains_key(&id) {
            self.connect(id.clone(), conn).await?;
        }
        start_mirror(&self.st, conn, &id, &source, (width, height)).await
    }

    /// Capture sources usable on this machine, best first (`fast`, `fast169`, `virtual`, `monitor`, `window`).
    async fn sources(&self) -> Vec<String> {
        mirror::available_sources()
            .into_iter()
            .map(String::from)
            .collect()
    }

    /// The receiver's last `welcome` as JSON (decoders, `hw.display.modes`, `bench`), "{}" until
    /// the control channel has been opened once. Does not connect by itself.
    async fn capabilities(&self, id: String) -> String {
        self.st
            .lock()
            .await
            .welcomes
            .get(&id)
            .map(|w| w.to_string())
            .unwrap_or_else(|| "{}".into())
    }

    /// Live mirroring numbers as JSON (sender side + the receiver's last stats), "{}" if idle.
    async fn stats(&self, id: String) -> String {
        let s = self.st.lock().await.stats.get(&id).cloned();
        s.map(|s| s.lock().unwrap().to_string())
            .unwrap_or_else(|| "{}".into())
    }

    async fn stop_mirror(&self, id: String) -> fdo::Result<()> {
        let tx = self.st.lock().await.mirrors.get(&id).cloned();
        let tx = tx.ok_or_else(|| fdo::Error::InvalidArgs(format!("not mirroring: {id}")))?;
        let _ = tx.send(()).await;
        Ok(())
    }

    /// Whether the sender keeps playing sound itself while mirroring. Off: its sound goes to the
    /// receiver only (a null sink is the default output for the session). Saved across restarts.
    #[zbus(property)]
    async fn local_audio(&self) -> bool {
        mirror::local_audio()
    }

    #[zbus(property)]
    async fn set_local_audio(&self, on: bool) {
        mirror::set_local_audio(on);
        let active = !self.st.lock().await.mirrors.is_empty();
        mirror::sync_mute(active);
    }

    #[zbus(property)]
    async fn sender_id(&self) -> String {
        self.sender_id.clone()
    }

    /// The built-in web receiver's page (`https://<lan-ip>:47801/`): open it in any browser with
    /// WebCodecs — a TV will do — and it becomes a receiver. Empty with no default route.
    #[zbus(property)]
    async fn web_url(&self) -> String {
        lan_ip()
            .map(|ip| format!("https://{ip}:{}/", p::WEB_PORT))
            .unwrap_or_default()
    }

    #[zbus(signal)]
    async fn receivers_changed(e: &SignalEmitter<'_>) -> zbus::Result<()>;
}

async fn changed(conn: &Connection) {
    if let Ok(e) = SignalEmitter::new(conn, p::DBUS_PATH) {
        let _ = Luch::receivers_changed(&e).await;
    }
}

// ---------- control channel ----------

type Reader = BufReader<tokio::net::tcp::OwnedReadHalf>;

/// One JSON line, at most MAX_LINE bytes. Ok(None) on EOF.
async fn read_msg(rd: &mut Reader) -> io::Result<Option<Value>> {
    let mut buf = Vec::new();
    let n = (&mut *rd)
        .take(p::MAX_LINE as u64 + 1)
        .read_until(b'\n', &mut buf)
        .await?;
    if n == 0 {
        return Ok(None);
    }
    if buf.last() != Some(&b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "line too long or truncated",
        ));
    }
    serde_json::from_slice(&buf)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

async fn send(wr: &mut OwnedWriteHalf, v: &Value) -> io::Result<()> {
    let mut line = v.to_string();
    line.push('\n');
    wr.write_all(line.as_bytes()).await
}

/// Socket pinned to the interface the receiver was discovered on: the default
/// route may be a full-tunnel VPN, so it must never decide where this goes.
async fn handshake(
    r: &Receiver,
    id: &str,
    host: &str,
) -> io::Result<(Value, Reader, OwnedWriteHalf)> {
    let addr: SocketAddr = format!("{}:{}", r.address, r.port)
        .parse()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let sock = TcpSocket::new_v4()?;
    sock.bind_device(Some(r.ifname.as_bytes()))?;
    let stream = timeout(HANDSHAKE_TIMEOUT, sock.connect(addr))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connect timed out"))??;
    let (rd, mut wr) = stream.into_split();
    let mut rd = BufReader::new(rd);
    let hello = json!({"t": "hello", "v": p::VERSION, "id": id, "name": host, "app": p::APP,
                       "want": ["mirror", "media", "photo"]});
    send(&mut wr, &hello).await?;
    let welcome = timeout(HANDSHAKE_TIMEOUT, read_msg(&mut rd))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no welcome"))??
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "closed before welcome"))?;
    if welcome["t"] != "welcome" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected welcome, got {welcome}"),
        ));
    }
    Ok((welcome, rd, wr))
}

async fn run_channel(
    name: String,
    id: String,
    mut rd: Reader,
    mut wr: OwnedWriteHalf,
    mut cmds: mpsc::Receiver<Cmd>,
    events: broadcast::Sender<Value>,
    st: Shared,
    conn: Connection,
) {
    // Reading lives in its own task: read_until is not cancel-safe inside select!.
    let (in_tx, mut inbox) = mpsc::channel::<Value>(16);
    let reader = tokio::spawn(async move {
        loop {
            match read_msg(&mut rd).await {
                Ok(Some(v)) => {
                    if in_tx.send(v).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                    eprintln!("[luchd] bad line: {e}")
                }
                Err(_) => break,
            }
        }
    });

    let mut last_rx = Instant::now();
    let mut n: u64 = 0;
    let mut ping_sent = Instant::now();
    let mut tick = tokio::time::interval(PING_EVERY);
    tick.tick().await;
    let why = loop {
        tokio::select! {
            c = cmds.recv() => match c {
                Some(Cmd::Send(v)) => {
                    if send(&mut wr, &v).await.is_err() {
                        break "write failed";
                    }
                }
                Some(Cmd::Bye) | None => {
                    let _ = send(&mut wr, &json!({"t": "bye"})).await;
                    break "disconnect requested";
                }
            },
            _ = tick.tick() => {
                if last_rx.elapsed() >= DEAD_AFTER {
                    break "silent for 30 s";
                }
                n += 1;
                ping_sent = Instant::now();
                if send(&mut wr, &json!({"t": "ping", "n": n})).await.is_err() {
                    break "write failed";
                }
            }
            m = inbox.recv() => {
                let Some(v) = m else { break "closed by receiver" };
                last_rx = Instant::now();
                match v["t"].as_str().unwrap_or("") {
                    "pong" if v["n"] == n => {
                        // Internal event for the mirror task; never sent on the wire.
                        let rtt = ping_sent.elapsed().as_secs_f64() * 1000.0;
                        let _ = events.send(json!({"t": "_rtt", "ms": rtt}));
                    }
                    "pong" | "welcome" => {}
                    "ping" => {
                        if send(&mut wr, &json!({"t": "pong", "n": v["n"]})).await.is_err() {
                            break "write failed";
                        }
                    }
                    "bye" => break "bye from receiver",
                    "error" => {
                        eprintln!("[luchd] {name}: error {v}");
                        let _ = events.send(v);
                    }
                    "ready" | "keyframe" | "stop" | "stats" => {
                        let _ = events.send(v);
                    }
                    t => {
                        let _ = send(&mut wr, &json!({"t": "error", "code": "unknown", "for": t})).await;
                    }
                }
            }
        }
    };
    reader.abort();
    let _ = wr.shutdown().await;
    eprintln!("[luchd] {name}: channel closed ({why})");
    st.lock().await.conns.remove(&id);
    changed(&conn).await;
}

// ---------- mirroring ----------

/// Body of the D-Bus `Mirror` method and of the web receiver's auto-start: one session to `id`.
async fn start_mirror(
    st: &Shared,
    conn: &Connection,
    id: &str,
    source: &str,
    requested: (u32, u32),
) -> fdo::Result<()> {
    let mirror169 = source == "fast169";
    let kms = source == "fast" || mirror169;
    let niri_virtual = source == "virtual" && mirror::niri_virtual_available();
    let source = match source {
        "" | "monitor" | "fast" | "fast169" => ashpd::desktop::screencast::SourceType::MONITOR,
        "window" => ashpd::desktop::screencast::SourceType::WINDOW,
        "virtual" => ashpd::desktop::screencast::SourceType::VIRTUAL,
        s => {
            return Err(fdo::Error::InvalidArgs(format!(
                "source must be fast|monitor|window|virtual, not {s}"
            )))
        }
    };
    if st.lock().await.mirrors.contains_key(id) {
        return Err(fdo::Error::Failed(format!("already mirroring to {id}")));
    }
    let (c, r, welcome) = {
        let s = st.lock().await;
        (
            s.conns.get(id).cloned(),
            s.found.get(id).cloned(),
            s.welcomes.get(id).cloned().unwrap_or(Value::Null),
        )
    };
    let (c, r) = c
        .zip(r)
        .ok_or_else(|| fdo::Error::Failed("receiver went away".into()))?;
    let prefer = welcome["bench"]["prefer"]
        .as_str()
        .unwrap_or("video/avc")
        .to_string();
    let size = if requested != (0, 0) {
        display::Size {
            width: requested.0,
            height: requested.1,
        }
    } else if std::env::var("LUCH_SIZE")
        .ok()
        .is_some_and(|v| !v.trim().is_empty())
    {
        let (width, height) = mirror::session_size(
            std::env::var("LUCH_SIZE").ok().as_deref(),
            welcome["max"].as_str(),
        );
        display::Size { width, height }
    } else if source == ashpd::desktop::screencast::SourceType::VIRTUAL {
        display::native_size(&welcome).unwrap_or_default()
    } else {
        display::Size::default()
    };
    let ws = c.media.clone();
    let address = r
        .address
        .parse()
        .map_err(|_| fdo::Error::Failed(format!("not IPv4: {}", r.address)))?;
    let target = mirror::Target {
        name: r.name.clone(),
        address,
        ifname: r.ifname.clone(),
        source,
        kms,
        prefer,
        ws,
        size,
        niri_virtual,
        mirror169,
    };
    let (stop_tx, stop_rx) = mpsc::channel(1);
    let stats = mirror::Stats::default();
    {
        let mut s = st.lock().await;
        s.mirrors.insert(id.to_string(), stop_tx);
        s.stats.insert(id.to_string(), stats.clone());
    }
    changed(conn).await;
    let (st, conn, id) = (st.clone(), conn.clone(), id.to_string());
    // Subscribe here and drop our copy of the sender: the mirror must see the channel close
    // (receiver gone or restarted) instead of streaming into the void.
    let Conn {
        cmd,
        events,
        media: _,
    } = c;
    let events_rx = events.subscribe();
    drop(events);
    let name = target.name.clone();
    tokio::spawn(async move {
        let res = mirror::run(target, cmd, events_rx, stop_rx, stats).await;
        match res {
            Ok(why) => eprintln!("[luchd] {name}: mirroring ended ({why})"),
            Err(e) => eprintln!("[luchd] {name}: mirroring failed: {e}"),
        }
        {
            let mut s = st.lock().await;
            s.mirrors.remove(&id);
            s.stats.remove(&id);
        }
        changed(&conn).await;
    });
    Ok(())
}

// ---------- web receiver ----------

/// The whole receiver in one file, served at `/`.
const PAGE: &str = include_str!("web.html");

/// Source IPv4 of the default route; a UDP connect() only picks it, no packet leaves.
fn lan_ip() -> Option<std::net::IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:53").ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_loopback()).then_some(ip)
}

async fn serve_web(st: Shared, dbus: Connection, sender_id: String, host: String) {
    let mut identity = match config_dir()
        .map_err(Box::<dyn Error>::from)
        .and_then(|dir| web_tls::Identity::load(&dir, lan_ip()))
    {
        Ok(identity) => identity,
        Err(e) => return eprintln!("[luchd] web receiver off: TLS identity: {e}"),
    };
    let l = match tokio::net::TcpListener::bind(("0.0.0.0", p::WEB_PORT)).await {
        Ok(l) => l,
        Err(e) => return eprintln!("[luchd] web receiver off: cannot bind {}: {e}", p::WEB_PORT),
    };
    eprintln!(
        "[luchd] web receiver at https://<this host>:{}/ (local self-signed certificate)",
        p::WEB_PORT
    );
    loop {
        let Ok((mut stream, peer)) = l.accept().await else {
            continue;
        };
        let Ok(local) = stream.local_addr() else {
            continue;
        };
        let acceptor = match identity.for_ip(local.ip()) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("[luchd] web TLS: {e}");
                continue;
            }
        };
        let (st, dbus, sid, host) = (st.clone(), dbus.clone(), sender_id.clone(), host.clone());
        tokio::spawn(async move {
            let result: io::Result<()> = async {
                let mut first = [0u8; 1];
                timeout(HANDSHAKE_TIMEOUT, stream.peek(&mut first)).await
                    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "HTTP/TLS timeout"))??;
                if first[0] != 22 { // TLS handshake record; old HTTP links redirect on the same port.
                    let head = timeout(HANDSHAKE_TIMEOUT, read_http_head(&mut stream)).await
                        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "HTTP header timeout"))??;
                    let text = String::from_utf8_lossy(&head);
                    let path = text.split_whitespace().nth(1).filter(|p| p.starts_with('/')).unwrap_or("/");
                    let response = format!("HTTP/1.1 302 Found\r\nLocation: https://{local}{path}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    stream.write_all(response.as_bytes()).await?;
                    return stream.shutdown().await;
                }
                let stream = timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await
                    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS handshake timeout"))??;
                web_session(stream, peer, st, dbus, sid, host).await
            }.await;
            if let Err(e) = result {
                eprintln!("[luchd] web {peer}: {e}");
            }
        });
    }
}

/// One browser connection: any GET serves the page, `/ws` upgrades and becomes a receiver.
async fn web_session<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    mut stream: S,
    peer: SocketAddr,
    st: Shared,
    dbus: Connection,
    sender_id: String,
    host: String,
) -> io::Result<()> {
    let head = timeout(HANDSHAKE_TIMEOUT, read_http_head(&mut stream))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "HTTP header timeout"))??;
    let text = String::from_utf8_lossy(&head);
    let first = text.split("\r\n").next().unwrap_or_default().to_string();
    let path = first.split(' ').nth(1).unwrap_or_default().to_string();
    if text.to_ascii_lowercase().contains("upgrade: websocket")
        && path.split('?').next() == Some("/ws")
    {
        // Replay the consumed header so tungstenite validates the entire upgrade request.
        let stream = Prefixed {
            inner: std::io::Cursor::new(head).chain(stream),
        };
        let ws = accept_async(stream)
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        return run_web(ws, peer, st, dbus, sender_id, host).await;
    }
    // Any GET (not just /) serves the page: TV browsers append or mangle paths.
    let (code, body, ctype) = if first.starts_with("GET ") {
        ("200 OK", PAGE, "text/html; charset=utf-8")
    } else {
        ("404 Not Found", "not found", "text/plain")
    };
    let resp = format!("HTTP/1.1 {code}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", body.len());
    stream.write_all(resp.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.shutdown().await
}

async fn read_http_head<S: AsyncRead + Unpin>(stream: &mut S) -> io::Result<Vec<u8>> {
    let mut head = Vec::with_capacity(512);
    loop {
        if head.len() == 8192 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP header too large",
            ));
        }
        head.push(stream.read_u8().await?);
        if head.ends_with(b"\r\n\r\n") {
            return Ok(head);
        }
    }
}

/// Read the already consumed HTTP header before continuing with the underlying TCP/TLS stream.
struct Prefixed<S> {
    inner: tokio::io::Chain<std::io::Cursor<Vec<u8>>, S>,
}

impl<S: AsyncRead + Unpin> AsyncRead for Prefixed<S> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for Prefixed<S> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        std::pin::Pin::new(self.inner.get_mut().1).poll_write(cx, buf)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(self.inner.get_mut().1).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(self.inner.get_mut().1).poll_shutdown(cx)
    }
}

/// A web receiver: hello/welcome over the websocket, then it behaves like any other receiver
/// (start/ready, keyframe, pings); media goes back over the same socket as binary frames.
async fn run_web<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    ws: WebSocketStream<S>,
    peer: SocketAddr,
    st: Shared,
    dbus: Connection,
    sender_id: String,
    host: String,
) -> io::Result<()> {
    let (sink, mut stream) = ws.split();
    let hello = match timeout(HANDSHAKE_TIMEOUT, stream.next()).await {
        Ok(Some(Ok(Message::Text(s)))) => {
            serde_json::from_str::<Value>(s.as_str()).unwrap_or(Value::Null)
        }
        Err(_) => {
            eprintln!(
                "[luchd] web {peer}: no hello in {} s",
                HANDSHAKE_TIMEOUT.as_secs()
            );
            return Ok(());
        }
        _ => return Ok(()), // a non-text first message: just drop it
    };
    if hello["t"] != "hello" {
        eprintln!("[luchd] web {peer}: expected hello, got {hello}");
        return Ok(());
    }
    let id = format!("web-{}", uuid::Uuid::new_v4());
    let name = hello["name"].as_str().unwrap_or("Web").to_string();
    let mode = hello["mode"].as_str().unwrap_or("fast").to_string();
    let (cmd_tx, cmd_rx) = mpsc::channel(8);
    let (events, _) = broadcast::channel(16);
    let (media_tx, media_rx) = mpsc::channel(16);
    // Browsers: H.264 everywhere (WebCodecs HEVC support is not universal).
    let welcome = json!({"t": "welcome", "v": p::VERSION, "id": sender_id, "name": host, "app": p::APP,
                         "bench": {"prefer": "video/avc"}, "hw": {"display": hello["display"]}});
    {
        let mut s = st.lock().await;
        s.found.insert(
            id.clone(),
            Receiver {
                name: name.clone(),
                model: hello["model"].as_str().unwrap_or_default().to_string(),
                address: peer.ip().to_string(),
                ifname: String::new(),
                ifindex: 0,
                port: p::WEB_PORT,
                service: String::new(),
                id: id.clone(),
            },
        );
        s.welcomes.insert(id.clone(), welcome.clone());
        s.conns.insert(
            id.clone(),
            Conn {
                cmd: cmd_tx.clone(),
                events: events.clone(),
                media: Some(media_tx),
            },
        );
    }
    eprintln!("[luchd] web receiver: {name} ({peer})");
    changed(&dbus).await;
    let _ = cmd_tx.send(Cmd::Send(welcome)).await; // completes the hello, like any other receiver

    // Writer: control JSON and media binary share the socket.
    let wname = name.clone();
    tokio::spawn(async move {
        let (mut cmd_rx, mut media_rx, mut sink) = (cmd_rx, media_rx, sink);
        let why = loop {
            tokio::select! {
                c = cmd_rx.recv() => match c {
                    Some(Cmd::Send(v)) => { if sink.send(Message::Text(v.to_string().into())).await.is_err() { break "write failed"; } }
                    Some(Cmd::Bye) => { let _ = sink.send(Message::Text(json!({"t": "bye"}).to_string().into())).await; break "disconnect requested"; }
                    None => break "control closed",
                },
                m = media_rx.recv() => match m {
                    Some(b) => { if sink.send(Message::Binary(b.into())).await.is_err() { break "write failed"; } }
                    None => break "media closed",
                },
            }
        };
        let _ = sink.close().await;
        eprintln!("[luchd] web {wname}: writer done ({why})");
    });

    // Reading lives in its own task: next() is not cancel-safe inside select!.
    let (in_tx, mut inbox) = mpsc::channel::<Value>(16);
    let reader = tokio::spawn(async move {
        loop {
            match stream.next().await {
                Some(Ok(Message::Text(s))) => {
                    if let Ok(v) = serde_json::from_str::<Value>(s.as_str()) {
                        if in_tx.send(v).await.is_err() {
                            break;
                        }
                    }
                }
                // Media flows one way; protocol-level pings are answered by tungstenite itself.
                Some(Ok(
                    Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_),
                )) => {}
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
            }
        }
    });

    // One tap on the TV starts the whole thing; `?mode=none` keeps it manual.
    if mode != "none" {
        if let Err(e) = start_mirror(&st, &dbus, &id, &mode, (0, 0)).await {
            let _ = cmd_tx
                .send(Cmd::Send(json!({"t": "error", "msg": e.to_string()})))
                .await;
        }
    }

    let mut last_rx = Instant::now();
    let (mut n, mut ping_sent) = (0u64, Instant::now());
    let mut tick = tokio::time::interval(PING_EVERY);
    tick.tick().await;
    let why = loop {
        tokio::select! {
            _ = tick.tick() => {
                if last_rx.elapsed() >= DEAD_AFTER {
                    break "silent for 30 s";
                }
                n += 1;
                ping_sent = Instant::now();
                if cmd_tx.send(Cmd::Send(json!({"t": "ping", "n": n}))).await.is_err() {
                    break "write failed";
                }
            }
            m = inbox.recv() => {
                let Some(v) = m else { break "closed by receiver" };
                last_rx = Instant::now();
                match v["t"].as_str().unwrap_or("") {
                    // Internal event for the mirror task; never sent on the wire.
                    "pong" if v["n"].as_u64() == Some(n) => {
                        let rtt = ping_sent.elapsed().as_secs_f64() * 1000.0;
                        let _ = events.send(json!({"t": "_rtt", "ms": rtt}));
                    }
                    "pong" | "welcome" | "hello" => {}
                    "ping" => {
                        if cmd_tx.send(Cmd::Send(json!({"t": "pong", "n": v["n"]}))).await.is_err() {
                            break "write failed";
                        }
                    }
                    "bye" => break "bye from receiver",
                    "error" => {
                        eprintln!("[luchd] web {name}: error {v}");
                        let _ = events.send(v);
                    }
                    "ready" | "keyframe" | "stop" | "stats" => {
                        let _ = events.send(v);
                    }
                    t => {
                        if cmd_tx.send(Cmd::Send(json!({"t": "error", "code": "unknown", "for": t}))).await.is_err() {
                            break "write failed";
                        }
                    }
                }
            }
        }
    };
    reader.abort();
    let _ = cmd_tx.send(Cmd::Bye).await; // tells the browser why the socket closes
    {
        let mut s = st.lock().await;
        s.conns.remove(&id);
        s.found.remove(&id); // unlike Avahi receivers, nothing re-announces a web one
        s.welcomes.remove(&id);
    }
    changed(&dbus).await;
    eprintln!("[luchd] web {name}: receiver gone ({why})");
    Ok(())
}

// ---------- Avahi ----------

fn ifname(index: i32) -> String {
    let mut buf = [0 as libc::c_char; libc::IF_NAMESIZE];
    // SAFETY: buf is IF_NAMESIZE bytes, as if_indextoname requires.
    let p = unsafe { libc::if_indextoname(index as u32, buf.as_mut_ptr()) };
    if p.is_null() {
        return String::new();
    }
    // SAFETY: on success the buffer holds a NUL-terminated name.
    unsafe { std::ffi::CStr::from_ptr(p) }
        .to_string_lossy()
        .into_owned()
}

/// tun/wg/ppp: point-to-point links (IFF_POINTOPOINT = 0x10).
fn is_tunnel(ifname: &str) -> bool {
    let flags =
        std::fs::read_to_string(format!("/sys/class/net/{ifname}/flags")).unwrap_or_default();
    u32::from_str_radix(flags.trim().trim_start_matches("0x"), 16).is_ok_and(|f| f & 0x10 != 0)
}

/// Interface of the main-table default route (the physical LAN, even under a full-tunnel VPN).
fn lan_iface() -> String {
    std::fs::read_to_string("/proc/net/route")
        .unwrap_or_default()
        .lines()
        .skip(1)
        .find_map(|l| {
            let p: Vec<_> = l.split_whitespace().collect();
            (p.get(1) == Some(&"00000000")).then(|| p[0].to_string())
        })
        .unwrap_or_default()
}

fn txt_map(txt: &[Vec<u8>]) -> HashMap<String, String> {
    txt.iter()
        .filter_map(|e| {
            let s = String::from_utf8_lossy(e);
            let (k, v) = s.split_once('=')?;
            Some((k.to_ascii_lowercase(), v.to_string()))
        })
        .collect()
}

async fn publish(sys: &Connection, id: &str, host: &str) -> zbus::Result<OwnedObjectPath> {
    let group: OwnedObjectPath = sys
        .call_method(Some(AVAHI), "/", Some(AVAHI_SERVER), "EntryGroupNew", &())
        .await?
        .body()
        .deserialize()?;
    let txt: Vec<Vec<u8>> = [
        format!("v={}", p::VERSION),
        format!("id={id}"),
        format!("name={host}"),
    ]
    .into_iter()
    .map(String::into_bytes)
    .collect();
    let eg = Some("org.freedesktop.Avahi.EntryGroup");
    // The sender listens on nothing yet; port 9 (discard) is a placeholder, the TXT is what matters.
    let args = (
        AVAHI_IF_UNSPEC,
        AVAHI_PROTO_INET,
        0u32,
        host,
        p::SENDER_TYPE,
        "",
        "",
        9u16,
        txt,
    );
    sys.call_method(Some(AVAHI), &group, eg, "AddService", &args)
        .await?;
    sys.call_method(Some(AVAHI), &group, eg, "Commit", &())
        .await?;
    Ok(group)
}

async fn resolve(
    sys: &Connection,
    iface: i32,
    proto: i32,
    name: &str,
    ty: &str,
    domain: &str,
) -> zbus::Result<Option<Receiver>> {
    let args = (iface, proto, name, ty, domain, AVAHI_PROTO_INET, 0u32);
    let reply = sys
        .call_method(
            Some(AVAHI),
            "/",
            Some(AVAHI_SERVER),
            "ResolveService",
            &args,
        )
        .await?;
    let (ifindex, _, service, _, _, _host, _, address, port, txt, _): (
        i32,
        i32,
        String,
        String,
        String,
        String,
        i32,
        String,
        u16,
        Vec<Vec<u8>>,
        u32,
    ) = reply.body().deserialize()?;
    let t = txt_map(&txt);
    if t.get("v").is_some_and(|v| v != "0") {
        return Ok(None);
    }
    let Some(id) = t.get("id").cloned() else {
        return Ok(None);
    };
    Ok(Some(Receiver {
        name: t.get("name").cloned().unwrap_or_else(|| service.clone()),
        model: t.get("model").cloned().unwrap_or_default(),
        id,
        address,
        ifname: ifname(ifindex),
        ifindex,
        port,
        service,
    }))
}

// ---------- identity ----------

/// ~/.config/luch (created on demand).
pub(crate) fn config_dir() -> io::Result<std::path::PathBuf> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config")))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no HOME"))?
        .join("luch");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn sender_id() -> io::Result<String> {
    let dir = config_dir()?;
    let path = dir.join("id");
    if let Ok(s) = std::fs::read_to_string(&path) {
        if !s.trim().is_empty() {
            return Ok(s.trim().to_string());
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    std::fs::write(&path, format!("{id}\n"))?;
    Ok(id)
}

fn short_host() -> String {
    let h = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
    h.trim()
        .split('.')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("linux")
        .to_string()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    gstreamer::init()?;
    mirror::restore_leftovers();
    mirror::restore_audio();
    let id = sender_id()?;
    let host = short_host();
    let st = Shared::default();
    let sys = Connection::system().await?;
    // ponytail: name collisions on the LAN are not handled (EntryGroup Collision state).
    let _group = match publish(&sys, &id, &host).await {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("[luchd] cannot publish {}: {e}", p::SENDER_TYPE);
            None
        }
    };
    let (discovery, worker) = discovery::Discovery::new();
    let conn = zbus::connection::Builder::session()?
        .name(p::DBUS_NAME)?
        .serve_at(
            p::DBUS_PATH,
            Luch {
                st: st.clone(),
                sender_id: id.clone(),
                host: host.clone(),
                discovery,
            },
        )?
        .build()
        .await?;
    eprintln!(
        "[luchd] {} id={id} name={host}; browsing {}",
        p::APP,
        p::RECEIVER_TYPE
    );
    tokio::spawn(serve_web(
        st.clone(),
        conn.clone(),
        id.clone(),
        host.clone(),
    ));
    worker.run(sys, st, conn).await?;
    Err("avahi browse stream ended".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn txt_parses_and_lowercases_keys() {
        let t = txt_map(&[
            b"v=0".to_vec(),
            b"ID=abc".to_vec(),
            b"dec=avc,hevc".to_vec(),
            b"junk".to_vec(),
        ]);
        assert_eq!(t["v"], "0");
        assert_eq!(t["id"], "abc");
        assert_eq!(t["dec"], "avc,hevc");
        assert!(!t.contains_key("junk"));
    }

    #[tokio::test]
    async fn local_https_identity_persists_and_carries_websocket_media() {
        use std::os::unix::fs::PermissionsExt;
        use tokio_rustls::rustls::{
            self,
            pki_types::{pem::PemObject, CertificateDer, ServerName},
        };
        let dir = std::env::temp_dir().join(format!("luch-tls-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let ip = "127.0.0.1".parse().unwrap();
        drop(web_tls::Identity::load(&dir, Some(ip)).unwrap());
        let original = std::fs::read(dir.join("web-cert.pem")).unwrap();
        let key = std::fs::read(dir.join("web-key.pem")).unwrap();
        assert_eq!(
            std::fs::metadata(dir.join("web-key.pem"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let mut identity = web_tls::Identity::load(&dir, Some(ip)).unwrap();
        assert_eq!(original, std::fs::read(dir.join("web-cert.pem")).unwrap());
        identity.for_ip("192.0.2.1".parse().unwrap()).unwrap();
        assert_eq!(key, std::fs::read(dir.join("web-key.pem")).unwrap());
        let renewed = std::fs::read(dir.join("web-cert.pem")).unwrap();
        identity = web_tls::Identity::load(&dir, Some(ip)).unwrap();
        assert_eq!(renewed, std::fs::read(dir.join("web-cert.pem")).unwrap());
        let cert = CertificateDer::from_pem_file(dir.join("web-cert.pem")).unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert).unwrap();
        let config = rustls::ClientConfig::builder_with_provider(
            rustls::crypto::ring::default_provider().into(),
        )
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut tls = identity.acceptor.accept(stream).await.unwrap();
            let head = read_http_head(&mut tls).await.unwrap();
            let mut ws = accept_async(Prefixed {
                inner: std::io::Cursor::new(head).chain(tls),
            })
            .await
            .unwrap();
            let ready = ws.next().await.unwrap().unwrap();
            assert_eq!(ready, Message::Text("{\"t\":\"ready\"}".into()));
            ws.send(Message::Binary(vec![1, 2, 3].into()))
                .await
                .unwrap();
        });
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        // Verify a new LAN IP SAN as well as trust, without disabling certificate validation.
        let tls = connector
            .connect(
                ServerName::IpAddress("192.0.2.1".parse::<std::net::IpAddr>().unwrap().into()),
                stream,
            )
            .await
            .unwrap();
        let (mut ws, response) = tokio_tungstenite::client_async(format!("wss://{addr}/ws"), tls)
            .await
            .unwrap();
        assert_eq!(response.status(), 101);
        ws.send(Message::Text("{\"t\":\"ready\"}".into()))
            .await
            .unwrap();
        assert_eq!(
            ws.next().await.unwrap().unwrap(),
            Message::Binary(vec![1, 2, 3].into())
        );
        task.await.unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn fragmented_http_header_preserves_websocket_handshake() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (mut server, _) = listener.accept().await.unwrap();
        let request = b"GET /ws HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n";
        let task = tokio::spawn(async move {
            let head = timeout(Duration::from_secs(2), read_http_head(&mut server))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(head, request);
            accept_async(Prefixed {
                inner: std::io::Cursor::new(head).chain(server),
            })
            .await
            .unwrap()
        });
        // Keep the initial prefix pending long enough to expose repeated-peek duplication.
        client.write_all(&request[..20]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!task.is_finished());
        for chunk in request[20..].chunks(7) {
            client.write_all(chunk).await.unwrap();
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let mut server = task.await.unwrap();
        let response = read_http_head(&mut client).await.unwrap();
        assert!(response.starts_with(b"HTTP/1.1 101 "));
        let mut client = WebSocketStream::from_raw_socket(
            client,
            tokio_tungstenite::tungstenite::protocol::Role::Client,
            None,
        )
        .await;
        server
            .send(Message::Binary(vec![1, 2, 3].into()))
            .await
            .unwrap();
        assert_eq!(
            client.next().await.unwrap().unwrap(),
            Message::Binary(vec![1, 2, 3].into())
        );
        client
            .send(Message::Text("{\"t\":\"ready\"}".into()))
            .await
            .unwrap();
        assert_eq!(
            server.next().await.unwrap().unwrap(),
            Message::Text("{\"t\":\"ready\"}".into())
        );
    }

    #[tokio::test]
    async fn oversized_http_header_is_rejected() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (mut server, _) = listener.accept().await.unwrap();
        client.write_all(&vec![b'x'; 8193]).await.unwrap();
        let err = timeout(Duration::from_secs(2), read_http_head(&mut server))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn read_msg_limits_and_eof() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            s.write_all(b"{\"t\":\"pong\",\"n\":1}\n").await.unwrap();
            s.write_all(&vec![b'x'; p::MAX_LINE + 10]).await.unwrap();
        });
        let s = tokio::net::TcpStream::connect(a).await.unwrap();
        let (rd, _wr) = s.into_split();
        let mut rd = BufReader::new(rd);
        assert_eq!(read_msg(&mut rd).await.unwrap().unwrap()["t"], "pong");
        assert!(read_msg(&mut rd).await.is_err());
    }
}
