//! A fake Chromecast for the tests: TLS with a self-signed certificate (as a real one has), length-prefixed
//! CastMessages, virtual connections that must be opened before anything goes through them, a heartbeat that drops
//! a silent sender, the platform's `receiver` namespace (LAUNCH, STOP, GET_STATUS, SET_VOLUME) and the Default Media
//! Receiver's `media` namespace (LOAD, PLAY, PAUSE, SEEK, SET_PLAYBACK_RATE, EDIT_TRACKS_INFO, GET_STATUS) with
//! mediaSessionId checks. It fetches what it is given like the receiver's page does: the video as a <video> would,
//! text tracks with an Origin, refused without CORS.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde_json::{json, Value};
use tokio::{io::AsyncWriteExt, net::TcpListener, sync::mpsc};
use tokio_rustls::{
    rustls::{
        self,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    },
    TlsAcceptor,
};

use super::castv2::{
    read_frame, write_frame, Message, DEFAULT_MEDIA_RECEIVER, NS_CONNECTION, NS_HEARTBEAT,
    NS_MEDIA, NS_RECEIVER, PLATFORM,
};

#[derive(Debug, Default)]
pub struct Tv {
    pub app: Option<String>,
    pub volume: f64,
    pub muted: bool,
    pub media_session: u64,
    pub content: String,
    pub state: &'static str,
    pub idle_reason: Option<&'static str>,
    pos: f64,
    since: Option<Instant>,
    pub rate: f64,
    pub dur: f64,
    pub active_tracks: Vec<u64>,
    /// Text tracks fetched: (content type, had CORS).
    pub tracks: Vec<(String, bool)>,
    /// What came in: "namespace type".
    pub log: Vec<String>,
    pub pings_from_sender: u32,
    /// A status to push to the sender now (the TV's remote, the end of the video).
    push: Option<Value>,
}

impl Tv {
    fn advance(&mut self) {
        if self.state == "PLAYING" {
            if let Some(s) = self.since {
                self.pos += s.elapsed().as_secs_f64() * self.rate;
                self.since = Some(Instant::now());
            }
            if self.pos >= self.dur {
                self.pos = self.dur;
                self.state = "IDLE";
                self.idle_reason = Some("FINISHED");
                self.since = None;
                self.push = Some(self.media_status(0));
            }
        }
    }

    fn receiver_status(&self, request: u64) -> Value {
        let apps: Vec<Value> = self
            .app
            .iter()
            .map(|s| {
                json!({"appId": DEFAULT_MEDIA_RECEIVER, "displayName": "Default Media Receiver", "sessionId": s,
                       "transportId": s, "statusText": "Ready To Cast", "isIdleScreen": false,
                       "namespaces": [{"name": NS_MEDIA}, {"name": "urn:x-cast:com.google.cast.debugoverlay"}]})
            })
            .collect();
        json!({"type": "RECEIVER_STATUS", "requestId": request,
               "status": {"applications": apps, "isActiveInput": true, "isStandBy": false,
                          "volume": {"controlType": "attenuation", "level": self.volume, "muted": self.muted, "stepInterval": 0.05}}})
    }

    fn media_status(&self, request: u64) -> Value {
        if self.media_session == 0 {
            return json!({"type": "MEDIA_STATUS", "requestId": request, "status": []});
        }
        let mut s = json!({"mediaSessionId": self.media_session, "playbackRate": self.rate, "playerState": self.state,
                           "currentTime": self.pos, "supportedMediaCommands": 274447, "activeTrackIds": self.active_tracks,
                           "volume": {"level": 1, "muted": false},
                           "media": {"contentId": self.content, "streamType": "BUFFERED", "duration": self.dur}});
        if let Some(r) = self.idle_reason {
            s["idleReason"] = json!(r);
        }
        json!({"type": "MEDIA_STATUS", "requestId": request, "status": [s]})
    }
}

pub struct FakeCast {
    pub port: u16,
    pub tv: Arc<Mutex<Tv>>,
}

impl FakeCast {
    /// Starts one on loopback; a video it is given lasts `dur` seconds.
    pub async fn start(dur: f64) -> FakeCast {
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["Chromecast".into()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let config = rustls::ServerConfig::builder_with_provider(
            rustls::crypto::ring::default_provider().into(),
        )
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert.der().to_vec())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
        )
        .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let tv = Arc::new(Mutex::new(Tv {
            volume: 0.3,
            rate: 1.0,
            dur,
            state: "IDLE",
            ..Default::default()
        }));
        let t = tv.clone();
        tokio::spawn(async move {
            loop {
                let (s, _) = l.accept().await.unwrap();
                let (acceptor, t) = (acceptor.clone(), t.clone());
                tokio::spawn(async move {
                    if let Ok(tls) = acceptor.accept(s).await {
                        connection(tls, t).await;
                    }
                });
            }
        });
        FakeCast { port, tv }
    }

    /// The TV's remote: pause.
    pub fn press_pause(&self) {
        let mut t = self.tv.lock().unwrap();
        t.advance();
        t.state = "PAUSED";
        t.since = None;
        t.push = Some(t.media_status(0));
    }

    /// Another phone starts its own app: ours is gone.
    pub fn take_over(&self) {
        let mut t = self.tv.lock().unwrap();
        t.app = None;
        t.media_session = 0;
        let mut s = t.receiver_status(0);
        s["status"]["applications"] = json!([{"appId": "233637DE", "displayName": "YouTube", "sessionId": "other",
                                                "transportId": "other", "namespaces": []}]);
        t.push = Some(s);
    }
}

async fn connection(
    tls: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    tv: Arc<Mutex<Tv>>,
) {
    let (mut rd, mut wr) = tokio::io::split(tls);
    let (tx, mut inbox) = mpsc::channel::<Message>(16);
    tokio::spawn(async move {
        while let Ok(Some(m)) = read_frame(&mut rd).await {
            if tx.send(m).await.is_err() {
                return;
            }
        }
    });
    let mut open: HashSet<(String, String)> = HashSet::new();
    let mut last_in = Instant::now();
    let mut sender = String::new();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut pinged = Instant::now();
    loop {
        let out: Vec<Message> = tokio::select! {
            m = inbox.recv() => {
                let Some(m) = m else { return };
                last_in = Instant::now();
                sender = m.source.clone();
                handle(m, &mut open, &tv).await
            }
            _ = tick.tick() => {
                // A real receiver drops a sender it has not heard from for a while.
                if last_in.elapsed() > Duration::from_secs(10) {
                    let _ = wr.shutdown().await;
                    return;
                }
                let mut out = Vec::new();
                if pinged.elapsed() > Duration::from_secs(1) && !sender.is_empty() {
                    pinged = Instant::now();
                    out.push(Message::json(PLATFORM, &sender, NS_HEARTBEAT, &json!({"type": "PING"})));
                }
                let push = {
                    let mut t = tv.lock().unwrap();
                    t.advance();
                    t.push.take().map(|v| (v, t.app.clone()))
                };
                if let Some((v, app)) = push {
                    let from = if v["type"] == "MEDIA_STATUS" { app.unwrap_or_default() } else { PLATFORM.to_string() };
                    out.push(Message::json(&from, &sender, if v["type"] == "MEDIA_STATUS" { NS_MEDIA } else { NS_RECEIVER }, &v));
                }
                out
            }
        };
        for m in out {
            if write_frame(&mut wr, &m).await.is_err() {
                return;
            }
        }
    }
}

/// One message in, the answers out.
async fn handle(m: Message, open: &mut HashSet<(String, String)>, tv: &Mutex<Tv>) -> Vec<Message> {
    let v = m.body();
    let ty = v["type"].as_str().unwrap_or("").to_string();
    let id = v["requestId"].as_u64().unwrap_or(0);
    let (from, to) = (m.source.clone(), m.destination.clone());
    let reply = |ns: &str, v: Value| vec![Message::json(&to, &from, ns, &v)];
    tv.lock().unwrap().log.push(format!(
        "{} {ty}",
        m.namespace.rsplit('.').next().unwrap_or("")
    ));
    match m.namespace.as_str() {
        NS_CONNECTION => {
            match ty.as_str() {
                "CONNECT" => {
                    open.insert((from.clone(), to.clone()));
                }
                "CLOSE" => {
                    open.remove(&(from.clone(), to.clone()));
                }
                _ => {}
            }
            return vec![];
        }
        NS_HEARTBEAT => {
            if ty == "PING" {
                tv.lock().unwrap().pings_from_sender += 1;
                return reply(NS_HEARTBEAT, json!({"type": "PONG"}));
            }
            return vec![];
        }
        _ => {}
    }
    // Nothing goes through a virtual connection that was not opened.
    if !open.contains(&(from.clone(), to.clone())) {
        return vec![];
    }
    let app = tv.lock().unwrap().app.clone();
    match (m.namespace.as_str(), to.as_str()) {
        (NS_RECEIVER, PLATFORM) => {
            let mut t = tv.lock().unwrap();
            match ty.as_str() {
                "GET_STATUS" => {}
                "LAUNCH" if v["appId"] == DEFAULT_MEDIA_RECEIVER => {
                    t.app = Some(uuid::Uuid::new_v4().to_string());
                    t.media_session = 0;
                }
                "LAUNCH" => {
                    return reply(
                        NS_RECEIVER,
                        json!({"type": "LAUNCH_ERROR", "requestId": id, "reason": "NOT_FOUND"}),
                    )
                }
                "STOP" => {
                    if v["sessionId"].as_str() != t.app.as_deref() {
                        return reply(
                            NS_RECEIVER,
                            json!({"type": "INVALID_REQUEST", "requestId": id, "reason": "INVALID_SESSION_ID"}),
                        );
                    }
                    t.app = None;
                    t.media_session = 0;
                }
                "SET_VOLUME" => {
                    if let Some(l) = v["volume"]["level"].as_f64() {
                        t.volume = l.clamp(0.0, 1.0);
                    }
                    if let Some(mu) = v["volume"]["muted"].as_bool() {
                        t.muted = mu;
                    }
                }
                _ => {
                    return reply(
                        NS_RECEIVER,
                        json!({"type": "INVALID_REQUEST", "requestId": id, "reason": "INVALID_COMMAND"}),
                    )
                }
            }
            reply(NS_RECEIVER, t.receiver_status(id))
        }
        (NS_MEDIA, dest) if Some(dest) == app.as_deref() => match media(&ty, id, &v, tv).await {
            Some(v) => reply(NS_MEDIA, v),
            None => vec![],
        },
        _ => vec![],
    }
}

async fn media(ty: &str, id: u64, v: &Value, tv: &Mutex<Tv>) -> Option<Value> {
    if ty == "LOAD" {
        let url = v["media"]["contentId"].as_str()?.to_string();
        let ok_stream =
            ["BUFFERED", "LIVE", "NONE"].contains(&v["media"]["streamType"].as_str().unwrap_or(""));
        let http = reqwest::Client::new();
        // The page's <video> asks for the start of the file.
        let video = http
            .get(&url)
            .header("Range", "bytes=0-")
            .timeout(Duration::from_secs(5))
            .send()
            .await;
        let fetched = video.as_ref().is_ok_and(|r| r.status().is_success());
        if !ok_stream || !fetched || v["media"]["contentType"].as_str().is_none() {
            return Some(json!({"type": "LOAD_FAILED", "requestId": id}));
        }
        let mut tracks = Vec::new();
        for t in v["media"]["tracks"].as_array().into_iter().flatten() {
            let r = http
                .get(t["trackContentId"].as_str().unwrap_or(""))
                .header("Origin", "https://www.gstatic.com")
                .send()
                .await
                .ok();
            let cors = r
                .as_ref()
                .is_some_and(|r| r.headers().get("access-control-allow-origin").is_some());
            let ctype = r
                .as_ref()
                .and_then(|r| {
                    r.headers()
                        .get("content-type")
                        .and_then(|c| c.to_str().ok())
                        .map(String::from)
                })
                .unwrap_or_default();
            let body = match r {
                Some(r) => r.text().await.unwrap_or_default(),
                None => String::new(),
            };
            // The page parses WebVTT only; a track without CORS is refused by the browser.
            tracks.push((
                if body.starts_with("WEBVTT") {
                    ctype
                } else {
                    "not WebVTT".into()
                },
                cors,
            ));
        }
        let mut t = tv.lock().unwrap();
        t.tracks = tracks;
        t.media_session += 1;
        t.content = url;
        t.pos = v["currentTime"].as_f64().unwrap_or(0.0);
        t.rate = 1.0;
        t.idle_reason = None;
        t.active_tracks = v["activeTrackIds"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default();
        if v["autoplay"].as_bool().unwrap_or(true) {
            t.state = "PLAYING";
            t.since = Some(Instant::now());
        } else {
            t.state = "PAUSED";
            t.since = None;
        }
        return Some(t.media_status(id));
    }
    let mut t = tv.lock().unwrap();
    t.advance();
    if ty == "GET_STATUS" {
        return Some(t.media_status(id));
    }
    if v["mediaSessionId"].as_u64() != Some(t.media_session) || t.media_session == 0 {
        return Some(
            json!({"type": "INVALID_REQUEST", "requestId": id, "reason": "INVALID_MEDIA_SESSION_ID"}),
        );
    }
    match ty {
        "PLAY" => {
            t.state = "PLAYING";
            t.since = Some(Instant::now());
        }
        "PAUSE" => {
            t.state = "PAUSED";
            t.since = None;
        }
        "SEEK" => {
            t.pos = v["currentTime"].as_f64()?.clamp(0.0, t.dur);
            if t.state == "PLAYING" {
                t.since = Some(Instant::now());
            }
        }
        "SET_PLAYBACK_RATE" => t.rate = v["playbackRate"].as_f64()?,
        "EDIT_TRACKS_INFO" => {
            t.active_tracks = v["activeTrackIds"]
                .as_array()?
                .iter()
                .filter_map(Value::as_u64)
                .collect();
        }
        "STOP" => {
            t.state = "IDLE";
            t.idle_reason = Some("CANCELLED");
        }
        _ => {
            return Some(
                json!({"type": "INVALID_REQUEST", "requestId": id, "reason": "INVALID_COMMAND"}),
            )
        }
    }
    Some(t.media_status(id))
}
