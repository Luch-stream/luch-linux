//! Google Cast (CASTV2) to a Chromecast or a Google TV: TLS on port 8009, each message a 4-byte big-endian length
//! and a protobuf `CastMessage` (cast_channel.proto) carrying JSON in a namespace. A sender opens a virtual
//! connection (`connection` CONNECT) to the platform (`receiver-0`), keeps it alive (`heartbeat` PING/PONG),
//! launches the Default Media Receiver (`receiver` LAUNCH CC1AD845), connects to the app's `transportId` and drives
//! it over `media` (LOAD, PLAY, PAUSE, SEEK, MEDIA_STATUS).
//!
//! The TV's certificate is not checked against Google's device CA: that needs the `deviceauth` challenge, which only
//! matters against a fake Chromecast on one's own network, and the TV fetches only links made for it anyway.

use std::{collections::HashMap, io, net::IpAddr, sync::Arc, time::Duration};

use serde_json::{json, Value};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, WriteHalf},
    net::TcpStream,
    sync::mpsc,
    time::{timeout, Instant},
};
use tokio_rustls::{
    client::TlsStream,
    rustls::{
        self,
        client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        pki_types::{CertificateDer, ServerName, UnixTime},
        DigitallySignedStruct, SignatureScheme,
    },
};

use super::{session, Ask, Device, Ends, Media, Reply, Session};
use crate::cast;

pub const NS_CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
pub const NS_HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
pub const NS_RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
pub const NS_MEDIA: &str = "urn:x-cast:com.google.cast.media";
pub const PLATFORM: &str = "receiver-0";
pub const DEFAULT_MEDIA_RECEIVER: &str = "CC1AD845";
const SENDER: &str = "sender-luch";
/// The most a message may be (the receiver's own limit).
pub const MAX_MESSAGE: usize = 64 * 1024;
const PING_EVERY: Duration = Duration::from_secs(5);
/// Silence for this long means the TV is gone (a receiver closes after about the same).
const DEAD_AFTER: Duration = Duration::from_secs(15);

// ---------- the wire ----------

#[derive(Clone, Debug, PartialEq)]
pub enum Payload {
    Text(String),
    Binary(Vec<u8>),
}

/// `CastMessage`: protocol_version = 1 (CASTV2_1_0 = 0), source_id = 2, destination_id = 3, namespace = 4,
/// payload_type = 5 (STRING = 0, BINARY = 1), payload_utf8 = 6, payload_binary = 7.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub source: String,
    pub destination: String,
    pub namespace: String,
    pub payload: Payload,
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn bytes_field(out: &mut Vec<u8>, field: u64, b: &[u8]) {
    varint(out, field << 3 | 2);
    varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

impl Message {
    pub fn json(source: &str, destination: &str, namespace: &str, v: &Value) -> Message {
        Message {
            source: source.into(),
            destination: destination.into(),
            namespace: namespace.into(),
            payload: Payload::Text(v.to_string()),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64);
        // proto2 `required` fields are always written, even when 0.
        varint(&mut out, 1 << 3);
        varint(&mut out, 0);
        bytes_field(&mut out, 2, self.source.as_bytes());
        bytes_field(&mut out, 3, self.destination.as_bytes());
        bytes_field(&mut out, 4, self.namespace.as_bytes());
        match &self.payload {
            Payload::Text(t) => {
                varint(&mut out, 5 << 3);
                varint(&mut out, 0);
                bytes_field(&mut out, 6, t.as_bytes());
            }
            Payload::Binary(b) => {
                varint(&mut out, 5 << 3);
                varint(&mut out, 1);
                bytes_field(&mut out, 7, b);
            }
        }
        out
    }

    pub fn decode(mut b: &[u8]) -> io::Result<Message> {
        let bad = |w: &str| io::Error::new(io::ErrorKind::InvalidData, format!("CastMessage: {w}"));
        fn read_varint(b: &mut &[u8]) -> Option<u64> {
            let mut v = 0u64;
            for shift in (0..64).step_by(7) {
                let (&x, rest) = b.split_first()?;
                *b = rest;
                v |= u64::from(x & 0x7f) << shift;
                if x & 0x80 == 0 {
                    return Some(v);
                }
            }
            None
        }
        let mut m = Message {
            source: String::new(),
            destination: String::new(),
            namespace: String::new(),
            payload: Payload::Text(String::new()),
        };
        let mut binary = false;
        let (mut text, mut bin) = (None, None);
        while !b.is_empty() {
            let key = read_varint(&mut b).ok_or_else(|| bad("truncated key"))?;
            let (field, wire) = (key >> 3, key & 7);
            match wire {
                0 => {
                    let v = read_varint(&mut b).ok_or_else(|| bad("truncated varint"))?;
                    if field == 5 {
                        binary = v == 1;
                    }
                }
                2 => {
                    let n = read_varint(&mut b).ok_or_else(|| bad("truncated length"))? as usize;
                    if n > b.len() {
                        return Err(bad("field past the end"));
                    }
                    let (v, rest) = b.split_at(n);
                    b = rest;
                    let s = || String::from_utf8(v.to_vec()).map_err(|_| bad("not UTF-8"));
                    match field {
                        2 => m.source = s()?,
                        3 => m.destination = s()?,
                        4 => m.namespace = s()?,
                        6 => text = Some(s()?),
                        7 => bin = Some(v.to_vec()),
                        _ => {}
                    }
                }
                1 => b = b.get(8..).ok_or_else(|| bad("truncated fixed64"))?,
                5 => b = b.get(4..).ok_or_else(|| bad("truncated fixed32"))?,
                _ => return Err(bad("unknown wire type")),
            }
        }
        m.payload = if binary {
            Payload::Binary(bin.unwrap_or_default())
        } else {
            Payload::Text(text.unwrap_or_default())
        };
        Ok(m)
    }

    /// The JSON in a text payload (Null when there is none or it is not JSON).
    pub fn body(&self) -> Value {
        match &self.payload {
            Payload::Text(t) => serde_json::from_str(t).unwrap_or(Value::Null),
            Payload::Binary(_) => Value::Null,
        }
    }
}

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, m: &Message) -> io::Result<()> {
    let b = m.encode();
    if b.len() > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "message too long",
        ));
    }
    let mut frame = (b.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(&b);
    w.write_all(&frame).await?;
    w.flush().await
}

/// The next message; Ok(None) at the end of the stream.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Option<Message>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let n = u32::from_be_bytes(len) as usize;
    if n > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too long",
        ));
    }
    let mut b = vec![0u8; n];
    r.read_exact(&mut b).await?;
    Message::decode(&b).map(Some)
}

// ---------- TLS ----------

/// Accepts the TV's self-signed certificate, but still checks that the handshake is signed with its key.
#[derive(Debug)]
struct AnyCertificate(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

type Tls = TlsStream<TcpStream>;

async fn connect_tls(address: IpAddr, port: u16) -> io::Result<Tls> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
        .with_no_client_auth();
    let tcp = timeout(Duration::from_secs(5), TcpStream::connect((address, port)))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no answer on the Cast port"))??;
    tcp.set_nodelay(true)?;
    tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(ServerName::IpAddress(address.into()), tcp)
        .await
}

// ---------- the channel ----------

/// One TLS connection to a Cast device: messages out, messages in (a reader task, as reads are not cancel-safe).
struct Channel {
    wr: WriteHalf<Tls>,
    inbox: mpsc::Receiver<io::Result<Message>>,
    next_id: u64,
    last_in: Instant,
}

impl Channel {
    async fn open(address: IpAddr, port: u16) -> io::Result<Channel> {
        let tls = connect_tls(address, port).await?;
        let (mut rd, wr) = tokio::io::split(tls);
        let (tx, inbox) = mpsc::channel(32);
        tokio::spawn(async move {
            loop {
                let m = read_frame(&mut rd).await;
                let end = !matches!(m, Ok(Some(_)));
                let item = match m {
                    Ok(Some(m)) => Ok(m),
                    Ok(None) => Err(io::Error::new(io::ErrorKind::UnexpectedEof, "closed")),
                    Err(e) => Err(e),
                };
                if tx.send(item).await.is_err() || end {
                    return;
                }
            }
        });
        Ok(Channel {
            wr,
            inbox,
            next_id: 0,
            last_in: Instant::now(),
        })
    }

    async fn send(&mut self, to: &str, ns: &str, v: &Value) -> io::Result<()> {
        write_frame(&mut self.wr, &Message::json(SENDER, to, ns, v)).await
    }

    /// Sends `v` with a new requestId; returns that id.
    async fn request(&mut self, to: &str, ns: &str, mut v: Value) -> io::Result<u64> {
        self.next_id += 1;
        v["requestId"] = json!(self.next_id);
        self.send(to, ns, &v).await?;
        Ok(self.next_id)
    }

    /// The next message that is not a heartbeat (PINGs are answered here).
    async fn next(&mut self) -> io::Result<Message> {
        loop {
            let m =
                self.inbox.recv().await.unwrap_or_else(|| {
                    Err(io::Error::new(io::ErrorKind::UnexpectedEof, "closed"))
                })?;
            self.last_in = Instant::now();
            if m.namespace == NS_HEARTBEAT {
                if m.body()["type"] == "PING" {
                    let to = m.source.clone();
                    self.send(&to, NS_HEARTBEAT, &json!({"type": "PONG"}))
                        .await?;
                }
                continue;
            }
            return Ok(m);
        }
    }

    /// Waits up to `t` for the answer to request `id`.
    async fn answer(&mut self, id: u64, t: Duration) -> Result<Value, String> {
        timeout(t, async {
            loop {
                let v = self.next().await.map_err(|e| e.to_string())?.body();
                if v["requestId"].as_u64() == Some(id) {
                    return Ok(v);
                }
            }
        })
        .await
        .map_err(|_| "the Chromecast did not answer".to_string())?
    }
}

// ---------- playing ----------

/// What `prepare` must do first for a Chromecast: its page reads only WebVTT, so SubRip files are served as such,
/// and subtitles it cannot read are left out.
pub fn prepare(srv: &cast::Server, m: &mut Media) {
    m.subs.retain_mut(|s| {
        if s.mime.contains("vtt") {
            return true;
        }
        if srv.as_vtt(&s.url) {
            s.mime = "text/vtt".into();
            return true;
        }
        false
    });
}

/// The LOAD for `m` in app session `session`.
pub fn load(m: &Media, session: &str) -> Value {
    let mime = if m.mime.is_empty() {
        if m.photo {
            "image/jpeg"
        } else {
            "video/mp4"
        }
    } else {
        &m.mime
    };
    let mut metadata = json!({"metadataType": if m.photo { 4 } else { 0 }, "title": m.title});
    if let Some(p) = &m.poster {
        metadata["images"] = json!([{"url": p}]);
    }
    let tracks: Vec<Value> = m
        .subs
        .iter()
        .enumerate()
        .map(|(n, s)| {
            json!({"trackId": n + 1, "type": "TEXT", "subtype": "SUBTITLES", "trackContentId": s.url,
                   "trackContentType": "text/vtt", "language": s.lang,
                   "name": if s.lang.is_empty() { format!("Subtitles {}", n + 1) } else { s.lang.clone() }})
        })
        .collect();
    let mut media = json!({"contentId": m.url, "contentUrl": m.url, "contentType": mime,
                           "streamType": if m.photo { "NONE" } else { "BUFFERED" }, "metadata": metadata});
    if !tracks.is_empty() {
        media["tracks"] = json!(tracks);
    }
    let mut v = json!({"type": "LOAD", "sessionId": session, "media": media, "autoplay": !m.paused,
                       "currentTime": m.start});
    if !m.subs.is_empty() {
        v["activeTrackIds"] = json!([1]);
    }
    v
}

/// Our app in a RECEIVER_STATUS: its (sessionId, transportId).
fn our_app(status: &Value) -> Option<(String, String)> {
    status["status"]["applications"]
        .as_array()?
        .iter()
        .find(|a| a["appId"] == DEFAULT_MEDIA_RECEIVER)
        .and_then(|a| {
            Some((
                a["sessionId"].as_str()?.to_string(),
                a["transportId"].as_str()?.to_string(),
            ))
        })
}

/// Launches the Default Media Receiver on `d` and loads `m`; returns once it plays or buffers.
pub async fn play(
    d: &Device,
    m: Media,
) -> Result<(Session, mpsc::UnboundedReceiver<Value>), String> {
    let mut ch = Channel::open(d.address, d.port)
        .await
        .map_err(|e| format!("{} ({}:{}): {e}", d.name, d.address, d.port))?;
    let io = |e: io::Error| e.to_string();
    ch.send(PLATFORM, NS_CONNECTION, &json!({"type": "CONNECT"}))
        .await
        .map_err(io)?;
    let id = ch
        .request(
            PLATFORM,
            NS_RECEIVER,
            json!({"type": "LAUNCH", "appId": DEFAULT_MEDIA_RECEIVER}),
        )
        .await
        .map_err(io)?;
    // The app may take a few seconds to come up; a status without it is not yet the answer.
    let (app_session, transport, volume) = timeout(Duration::from_secs(20), async {
        loop {
            let v = ch.next().await.map_err(io)?.body();
            match v["type"].as_str() {
                Some("LAUNCH_ERROR") if v["requestId"].as_u64() == Some(id) => {
                    return Err(format!(
                        "the Chromecast could not start its player: {}",
                        v["reason"].as_str().unwrap_or("?")
                    ))
                }
                Some("RECEIVER_STATUS") => {
                    if let Some((s, t)) = our_app(&v) {
                        return Ok((s, t, v["status"]["volume"].clone()));
                    }
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| "the Chromecast did not start its player".to_string())??;
    ch.send(&transport, NS_CONNECTION, &json!({"type": "CONNECT"}))
        .await
        .map_err(io)?;
    let id = ch
        .request(&transport, NS_MEDIA, load(&m, &app_session))
        .await
        .map_err(io)?;
    let first = ch.answer(id, Duration::from_secs(30)).await?;
    if first["type"] != "MEDIA_STATUS" {
        return Err(format!(
            "the Chromecast could not play it: {}",
            first["reason"]
                .as_str()
                .or(first["type"].as_str())
                .unwrap_or("?")
        ));
    }
    let (s, mut ends, states) = session();
    let mut p = Player {
        ch,
        transport,
        app_session,
        media_session: None,
        dur: None,
        rate: 1.0,
        last: (String::new(), 0.0, Instant::now()),
        volume,
        pending: HashMap::new(),
    };
    match p.status(&first, &mut ends.tell) {
        // Over before it began (a picture shown and done, an error): the session ends with this.
        Ending::Yes(why) => ends.tell.stopped(why),
        Ending::No => {
            tokio::spawn(p.run(ends));
        }
    }
    Ok((s, states))
}

struct Player {
    ch: Channel,
    transport: String,
    app_session: String,
    media_session: Option<u64>,
    dur: Option<f64>,
    rate: f64,
    /// The last state, position and when.
    last: (String, f64, Instant),
    volume: Value,
    pending: HashMap<u64, Reply>,
}

/// How a MEDIA_STATUS ends a session, if it does.
enum Ending {
    No,
    Yes(&'static str),
}

impl Player {
    /// Tells a MEDIA_STATUS as a media_state.
    fn status(&mut self, v: &Value, tell: &mut super::Teller) -> Ending {
        let Some(s) = v["status"].as_array().and_then(|a| a.first()) else {
            // Nothing loaded any more: stopped on the TV (or by another phone).
            return if self.media_session.is_some() {
                Ending::Yes("receiver")
            } else {
                Ending::No
            };
        };
        if let Some(id) = s["mediaSessionId"].as_u64() {
            self.media_session = Some(id);
        }
        if let Some(d) = s["media"]["duration"].as_f64() {
            self.dur = Some(d).filter(|d| d.is_finite() && *d > 0.0);
        }
        if s["media"]["streamType"] == "LIVE" {
            self.dur = None;
        }
        if let Some(r) = s["playbackRate"].as_f64() {
            self.rate = r;
        }
        let pos = s["currentTime"].as_f64().unwrap_or(self.last.1);
        let state = match s["playerState"].as_str().unwrap_or("") {
            "PLAYING" => "playing",
            "PAUSED" => "paused",
            "BUFFERING" | "LOADING" => "loading",
            "IDLE" => match s["idleReason"].as_str() {
                Some("FINISHED") => {
                    tell.tell("ended", self.dur.unwrap_or(pos), self.dur, self.rate);
                    return Ending::Yes("ended");
                }
                Some("ERROR") => {
                    tell.error("the Chromecast could not play it");
                    return Ending::Yes("receiver");
                }
                Some("INTERRUPTED") => return Ending::Yes("replaced"),
                Some(_) => return Ending::Yes("receiver"),
                None => "loading",
            },
            _ => return Ending::No,
        };
        self.last = (state.to_string(), pos, Instant::now());
        tell.tell(state, pos, self.dur, self.rate);
        Ending::No
    }

    fn pos_now(&self) -> f64 {
        let (state, pos, at) = &self.last;
        if state == "playing" {
            pos + at.elapsed().as_secs_f64() * self.rate
        } else {
            *pos
        }
    }

    /// A media_cmd as the media namespace's request; Err when Cast has no such thing.
    fn media_request(&self, c: &Value) -> Result<Value, String> {
        let id = self.media_session.ok_or("nothing loaded yet")?;
        let mut v = match c["do"].as_str().unwrap_or("") {
            "play" => json!({"type": "PLAY"}),
            "pause" => json!({"type": "PAUSE"}),
            "toggle" if self.last.0 == "playing" => json!({"type": "PAUSE"}),
            "toggle" => json!({"type": "PLAY"}),
            "seek" => {
                json!({"type": "SEEK", "currentTime": c["pos"].as_f64().ok_or("seek needs pos")?.max(0.0)})
            }
            "skip" => json!({"type": "SEEK",
                             "currentTime": (self.pos_now() + c["by"].as_f64().ok_or("skip needs by")?).max(0.0)}),
            "rate" => {
                json!({"type": "SET_PLAYBACK_RATE", "playbackRate": c["rate"].as_f64().ok_or("rate needs rate")?})
            }
            "sub" => {
                let ids: Vec<u64> = c["id"]
                    .as_str()
                    .and_then(|s| s.strip_prefix('s'))
                    .and_then(|n| n.parse::<u64>().ok())
                    .map(|n| vec![n + 1])
                    .unwrap_or_default();
                json!({"type": "EDIT_TRACKS_INFO", "activeTrackIds": ids})
            }
            "audio" => return Err("choose sound tracks with the TV's remote".into()),
            "state" => json!({"type": "GET_STATUS"}),
            d => return Err(format!("unknown media command {d}")),
        };
        v["mediaSessionId"] = json!(id);
        Ok(v)
    }

    /// A Luch volume message as SET_VOLUME (level 0..1 on Cast, 0..100 for us).
    fn volume_request(&self, v: &Value) -> Value {
        let level = self.volume["level"].as_f64().unwrap_or(0.5);
        let step = self.volume["stepInterval"]
            .as_f64()
            .filter(|s| *s > 0.0)
            .unwrap_or(0.05);
        let vol = if let Some(l) = v["level"].as_f64() {
            json!({"level": (l / 100.0).clamp(0.0, 1.0)})
        } else if let Some(s) = v["step"].as_f64() {
            json!({"level": (level + s.signum() * step).clamp(0.0, 1.0)})
        } else {
            json!({"muted": v["muted"].as_bool().unwrap_or(false)})
        };
        json!({"type": "SET_VOLUME", "volume": vol})
    }

    fn volume_now(&self) -> Value {
        json!({"level": (self.volume["level"].as_f64().unwrap_or(0.0) * 100.0).round() as i64,
               "muted": self.volume["muted"].as_bool().unwrap_or(false)})
    }

    async fn run(mut self, mut ends: Ends) {
        let mut ping = tokio::time::interval(PING_EVERY);
        let why = loop {
            tokio::select! {
                a = ends.asks.recv() => {
                    let Some((a, reply)) = a else { break "replaced" };
                    let req = match &a {
                        Ask::Leave => {
                            let _ = reply.send(Ok(json!({})));
                            break "replaced";
                        }
                        Ask::Stop => {
                            self.stop().await;
                            let _ = reply.send(Ok(json!({})));
                            break "sender";
                        }
                        Ask::Cmd(c) => {
                            ends.tell.asked();
                            self.media_request(c).map(|r| (self.transport.clone(), NS_MEDIA, r))
                        }
                        Ask::Volume(v) => Ok((PLATFORM.to_string(), NS_RECEIVER, self.volume_request(v))),
                    };
                    match req {
                        Err(e) => { let _ = reply.send(Err(e)); }
                        Ok((to, ns, r)) => match self.ch.request(&to, ns, r).await {
                            Ok(id) => { self.pending.insert(id, reply); }
                            Err(_) => { let _ = reply.send(Err("the Chromecast went away".into())); break "gone"; }
                        },
                    }
                }
                _ = ping.tick() => {
                    if self.ch.last_in.elapsed() > DEAD_AFTER { break "gone"; }
                    if self.ch.send(PLATFORM, NS_HEARTBEAT, &json!({"type": "PING"})).await.is_err() { break "gone"; }
                }
                m = self.ch.next() => {
                    let Ok(m) = m else { break "gone" };
                    let v = m.body();
                    let answer = v["requestId"].as_u64().filter(|id| *id != 0).and_then(|id| self.pending.remove(&id));
                    let ty = v["type"].as_str().unwrap_or("");
                    match ty {
                        "MEDIA_STATUS" => {
                            if let Some(r) = answer { let _ = r.send(Ok(json!({}))); }
                            if let Ending::Yes(why) = self.status(&v, &mut ends.tell) { break why; }
                        }
                        "RECEIVER_STATUS" => {
                            if v["status"]["volume"].is_object() { self.volume = v["status"]["volume"].clone(); }
                            if let Some(r) = answer { let _ = r.send(Ok(self.volume_now())); }
                            // Another app took the TV, or ours was closed there.
                            if our_app(&v).is_none_or(|(s, _)| s != self.app_session) { break "replaced"; }
                        }
                        "CLOSE" if m.namespace == NS_CONNECTION => break "receiver",
                        _ => {
                            if let Some(r) = answer {
                                let _ = r.send(Err(v["reason"].as_str().unwrap_or(ty).to_string()));
                            }
                        }
                    }
                }
            }
        };
        for (_, r) in self.pending.drain() {
            let _ = r.send(Err("the Chromecast is no longer playing".into()));
        }
        ends.tell.stopped(why);
    }

    /// Closes our app on the TV (it goes back to its home screen) and our connections.
    async fn stop(&mut self) {
        if let Ok(id) = self
            .ch
            .request(
                PLATFORM,
                NS_RECEIVER,
                json!({"type": "STOP", "sessionId": self.app_session}),
            )
            .await
        {
            let _ = self.ch.answer(id, Duration::from_secs(3)).await;
        }
        let t = self.transport.clone();
        let _ = self
            .ch
            .send(&t, NS_CONNECTION, &json!({"type": "CLOSE"}))
            .await;
        let _ = self
            .ch
            .send(PLATFORM, NS_CONNECTION, &json!({"type": "CLOSE"}))
            .await;
        let _ = self.ch.wr.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bytes cast_channel.proto gives for a CONNECT, worked out by hand from the field numbers.
    #[test]
    fn a_message_is_the_bytes_the_proto_gives() {
        let m = Message::json(
            "sender-0",
            "receiver-0",
            NS_CONNECTION,
            &json!({"type": "CONNECT"}),
        );
        let b = m.encode();
        let mut want = vec![0x08, 0x00, 0x12, 8];
        want.extend_from_slice(b"sender-0");
        want.extend_from_slice(&[0x1a, 10]);
        want.extend_from_slice(b"receiver-0");
        want.extend_from_slice(&[0x22, NS_CONNECTION.len() as u8]);
        want.extend_from_slice(NS_CONNECTION.as_bytes());
        want.extend_from_slice(&[0x28, 0x00, 0x32, 18]);
        want.extend_from_slice(br#"{"type":"CONNECT"}"#);
        assert_eq!(b, want);
        assert_eq!(Message::decode(&b).unwrap(), m);
        let bin = Message {
            payload: Payload::Binary(vec![0; 300]),
            ..m.clone()
        };
        assert_eq!(Message::decode(&bin.encode()).unwrap(), bin);
        assert!(Message::decode(&[0x12, 50, b'x']).is_err());
    }

    #[test]
    fn a_load_names_the_media_and_its_subtitles() {
        let m = Media {
            url: "http://192.168.1.20:47810/t/a.mp4".into(),
            mime: "video/mp4".into(),
            title: "Holiday".into(),
            start: 12.0,
            subs: vec![super::super::Sub {
                url: "http://192.168.1.20:47810/u/a.en.srt".into(),
                mime: "text/vtt".into(),
                lang: "en".into(),
            }],
            ..Default::default()
        };
        let l = load(&m, "s-1");
        assert_eq!(l["type"], "LOAD");
        assert_eq!(l["sessionId"], "s-1");
        assert_eq!(l["media"]["contentId"], m.url);
        assert_eq!(l["media"]["streamType"], "BUFFERED");
        assert_eq!(l["media"]["tracks"][0]["trackContentType"], "text/vtt");
        assert_eq!(l["activeTrackIds"], json!([1]));
        assert_eq!(l["currentTime"], 12.0);
        assert_eq!(l["autoplay"], true);
    }
}
