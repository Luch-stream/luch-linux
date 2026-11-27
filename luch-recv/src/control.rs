//! The control channel (PROTOCOL.md section 3) and the session it starts (section 4): hello and welcome, `start` answered
//! with the UDP ports, then the video arrives, goes to the player, and the receiver asks for an IDR after a loss.

use crate::player::Player;
use luch_proto::wire;
use serde_json::{json, Value};
use socket2::{Domain, Socket, Type};
use std::{
    net::UdpSocket,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::mpsc::{unbounded_channel, UnboundedSender},
};

pub struct Identity {
    pub id: String,
    pub name: String,
}

pub async fn serve(listener: TcpListener, ident: Arc<Identity>) {
    loop {
        match listener.accept().await {
            Ok((sock, peer)) => {
                eprintln!("[luch-recv] sender connected: {peer}");
                tokio::spawn(handle(sock, ident.clone()));
            }
            Err(e) => {
                eprintln!("[luch-recv] accept: {e}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

type Out = UnboundedSender<String>;

fn line(v: Value) -> String {
    format!("{v}\n")
}

async fn handle(sock: TcpStream, ident: Arc<Identity>) {
    let (r, mut w) = sock.into_split();
    let mut lines = BufReader::new(r).lines();
    let (tx, mut rx) = unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(l) = rx.recv().await {
            if w.write_all(l.as_bytes()).await.is_err() {
                break;
            }
        }
    });
    let mut sender = String::from("a sender");
    let mut session: Option<Session> = None;
    loop {
        let l = match tokio::time::timeout(Duration::from_secs(30), lines.next_line()).await {
            Ok(Ok(Some(l))) => l,
            _ => break,
        };
        let Ok(v) = serde_json::from_str::<Value>(&l) else {
            continue;
        };
        let t = v["t"].as_str().unwrap_or("");
        match t {
            "hello" => {
                if let Some(n) = v["name"].as_str() {
                    sender = n.to_string();
                }
                let _ = tx.send(line(json!({
                    "t": "welcome", "v": luch_proto::VERSION, "id": ident.id, "name": ident.name, "model": "Linux",
                    "decoders": ["video/avc", "video/hevc"], "max": "3840x2160", "modes": []
                })));
            }
            "ping" => {
                let _ = tx.send(line(json!({"t": "pong", "n": v["n"]})));
            }
            "start" => {
                session = None;
                match Session::start(&v, tx.clone(), &sender) {
                    Ok((s, video, audio)) => {
                        let _ = tx.send(line(
                            json!({"t": "ready", "video_port": video, "audio_port": audio}),
                        ));
                        let _ = tx.send(line(json!({"t": "keyframe"})));
                        session = Some(s);
                    }
                    Err(e) => {
                        let _ = tx.send(line(
                            json!({"t": "error", "code": "start_failed", "detail": e}),
                        ));
                    }
                }
            }
            "stop" => session = None,
            "bye" => break,
            "pong" | "sender_stats" => {}
            other => {
                let _ = tx.send(line(json!({"t": "error", "code": "unknown", "for": other})));
            }
        }
    }
    drop(session);
    writer.abort();
    eprintln!("[luch-recv] {sender} disconnected");
}

/// One running session: the thread that reads the video socket and feeds the player. Dropping it stops them.
struct Session {
    stop: Arc<AtomicBool>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The video and sound ports a firewall can be told about: 47802 and 47803; any free port when those are taken.
pub const VIDEO_PORT: u16 = 47802;
pub const AUDIO_PORT: u16 = 47803;

fn udp(preferred: u16) -> std::io::Result<UdpSocket> {
    let bind = |port: u16| -> std::io::Result<Socket> {
        let s = Socket::new(Domain::IPV4, Type::DGRAM, None)?;
        let _ = s.set_recv_buffer_size(4 << 20);
        s.bind(&std::net::SocketAddr::from(([0, 0, 0, 0], port)).into())?;
        Ok(s)
    };
    let s = bind(preferred).or_else(|_| bind(0))?;
    s.set_read_timeout(Some(Duration::from_millis(100)))?;
    Ok(s.into())
}

impl Session {
    fn start(msg: &Value, out: Out, sender: &str) -> Result<(Session, u16, u16), String> {
        let codec = msg["video"]["codec"]
            .as_str()
            .unwrap_or("video/avc")
            .to_string();
        let player = Player::new(&codec)?;
        let video = udp(VIDEO_PORT).map_err(|e| e.to_string())?;
        let audio = udp(AUDIO_PORT).map_err(|e| e.to_string())?; // the sound is not played yet; its port is only held open
        let ports = (
            video.local_addr().map_err(|e| e.to_string())?.port(),
            audio.local_addr().map_err(|e| e.to_string())?.port(),
        );
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        eprintln!(
            "[luch-recv] {sender}: {codec} {}x{} at {} fps",
            msg["video"]["width"], msg["video"]["height"], msg["video"]["fps"]
        );
        std::thread::spawn(move || {
            let _audio = audio;
            receive(video, player, out, flag);
        });
        Ok((Session { stop }, ports.0, ports.1))
    }
}

fn receive(sock: UdpSocket, player: Player, out: Out, stop: Arc<AtomicBool>) {
    let debug = std::env::var_os("LUCH_RECV_DEBUG").is_some();
    let mut asm = wire::Assembler::new();
    let mut buf = vec![0u8; 2048];
    let (mut frames, mut host_sum, mut shown, mut tick) = (0u32, 0u64, 0u32, Instant::now());
    while !stop.load(Ordering::Relaxed) {
        if let Ok((n, _)) = sock.recv_from(&mut buf) {
            if debug {
                eprintln!("[luch-recv] packet {n} bytes");
            }
            if let Some((h, data)) = wire::Header::parse(&buf[..n]) {
                let r = asm.push(&h, data);
                if let Some(f) = r.frame {
                    player.push(&f.data);
                    frames += 1;
                    host_sum += f.host as u64;
                    shown = f.seq;
                }
                if r.want_keyframe {
                    let _ = out.send(line(json!({"t": "keyframe"})));
                }
            }
        }
        if asm.retry_keyframe(300) {
            let _ = out.send(line(json!({"t": "keyframe"})));
        }
        if tick.elapsed() >= Duration::from_secs(1) {
            let host_ms = if frames == 0 {
                0.0
            } else {
                host_sum as f64 / frames as f64 / 10.0
            };
            let _ = out.send(line(json!({
                "t": "stats", "decode_ms": 0.0, "host_ms": host_ms, "fps": frames, "lost": asm.lost, "dropped": 0,
                "shown_seq": shown, "decoder": "gstreamer"
            })));
            (frames, host_sum, tick) = (0, 0, Instant::now());
        }
    }
}
