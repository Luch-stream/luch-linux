//! The control channel from the sender's side (PROTOCOL.md sections 3 and 4), blocking and small: connect from the
//! LAN interface, hello and welcome, start and ready, then a thread that turns the receiver's lines into `Signals`.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use luch_proto::{control, MAX_LINE};
use serde_json::{json, Value};

use crate::pipeline::Signals;

const ANSWER: Duration = Duration::from_secs(5);

pub struct Channel {
    rd: BufReader<TcpStream>,
    wr: TcpStream,
    pub welcome: Value,
}

fn bad(what: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

/// One line, at most MAX_LINE bytes. Ok(None) at the end of the connection.
fn read_msg(rd: &mut BufReader<TcpStream>) -> io::Result<Option<Value>> {
    let mut buf = Vec::new();
    if rd
        .by_ref()
        .take(MAX_LINE as u64 + 1)
        .read_until(b'\n', &mut buf)?
        == 0
    {
        return Ok(None);
    }
    control::parse_line(&buf)
        .map(Some)
        .map_err(|e| bad(e.to_string()))
}

impl Channel {
    /// Connects from `local` (the address of the interface the receiver was found on, PROTOCOL.md section 2) and
    /// says hello.
    pub fn open(
        to: SocketAddrV4,
        local: Option<Ipv4Addr>,
        id: &str,
        name: &str,
    ) -> io::Result<Channel> {
        use socket2::{Domain, Protocol, Socket, Type};
        let s = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?;
        if let Some(ip) = local {
            s.bind(&SocketAddrV4::new(ip, 0).into())?;
        }
        s.connect_timeout(&to.into(), ANSWER)?;
        s.set_tcp_nodelay(true)?;
        let wr: TcpStream = s.into();
        wr.set_read_timeout(Some(ANSWER))?;
        let mut rd = BufReader::new(wr.try_clone()?);
        let mut w = &wr;
        w.write_all(control::line(&control::hello(id, name, &["mirror"])).as_bytes())?;
        let welcome = read_msg(&mut rd)?.ok_or_else(|| bad("closed before welcome".into()))?;
        if welcome["t"] != "welcome" {
            return Err(bad(format!("expected welcome, got {welcome}")));
        }
        Ok(Channel { rd, wr, welcome })
    }

    pub fn send(&mut self, v: &Value) -> io::Result<()> {
        self.wr.write_all(control::line(v).as_bytes())
    }

    /// Sends `start` and waits for `ready`, answering pings on the way. Returns the video and audio ports.
    pub fn start(&mut self, video: &control::Video) -> io::Result<(u16, u16)> {
        self.send(&control::start(Some(video), None))?;
        loop {
            let v = read_msg(&mut self.rd)?.ok_or_else(|| bad("closed before ready".into()))?;
            match v["t"].as_str() {
                Some("ready") => {
                    return control::ready_ports(&v).ok_or_else(|| bad(format!("bad ready: {v}")))
                }
                Some("error") | Some("stop") => return Err(bad(format!("receiver: {v}"))),
                Some("ping") => self.send(&json!({"t": "pong", "n": v["n"]}))?,
                _ => {}
            }
        }
    }

    /// From here on a thread reads the channel: `keyframe` asks the pipeline for an IDR, `stop`, `bye` or the end
    /// of the connection stop it. The returned stream writes (stats, `stop` from our side).
    pub fn follow(self, signals: Arc<Signals>) -> io::Result<TcpStream> {
        let Channel { mut rd, wr, .. } = self;
        let mut pong = wr.try_clone()?;
        rd.get_ref().set_read_timeout(None)?;
        std::thread::spawn(move || {
            while let Ok(Some(v)) = read_msg(&mut rd) {
                match v["t"].as_str() {
                    Some("keyframe") => signals.want_idr.store(true, Ordering::Relaxed),
                    Some("stop") | Some("bye") => break,
                    Some("ping") => {
                        let _ = pong.write_all(
                            control::line(&json!({"t": "pong", "n": v["n"]})).as_bytes(),
                        );
                    }
                    _ => {}
                }
            }
            signals.stop.store(true, Ordering::Relaxed);
        });
        Ok(wr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A receiver that speaks just enough of the protocol: `on_start` is what it says to `start`.
    fn receiver(on_start: Vec<Value>) -> (SocketAddrV4, std::thread::JoinHandle<Vec<Value>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = match l.local_addr().unwrap() {
            std::net::SocketAddr::V4(a) => a,
            _ => unreachable!(),
        };
        let h = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut rd = BufReader::new(s.try_clone().unwrap());
            let mut wr = s;
            let mut got = Vec::new();
            let mut say = |v: Value| wr.write_all(control::line(&v).as_bytes()).unwrap();
            let mut line = String::new();
            while rd.read_line(&mut line).unwrap_or(0) > 0 {
                let v: Value = serde_json::from_str(&line).unwrap();
                line.clear();
                match v["t"].as_str() {
                    Some("hello") => say(json!({"t": "welcome", "v": 0, "decoders": ["video/avc"],
                        "bench": {"prefer": "video/avc"}})),
                    Some("start") => on_start.iter().cloned().for_each(&mut say),
                    _ => {}
                }
                got.push(v);
                if got.len() == 4 {
                    break;
                }
            }
            got
        });
        (addr, h)
    }

    fn video() -> control::Video {
        control::Video {
            codec: "video/avc",
            width: 1920,
            height: 1080,
            fps: 60,
        }
    }

    #[test]
    fn hello_start_ready_then_keyframe_and_stop() {
        let (addr, rx) = receiver(vec![
            json!({"t": "ping", "n": 1}),
            json!({"t": "ready", "video_port": 47802, "audio_port": 47803}),
            json!({"t": "keyframe"}),
            json!({"t": "ping", "n": 2}),
            json!({"t": "stop", "by": "receiver", "reason": "back"}),
        ]);
        let mut ch =
            Channel::open(addr, Some(Ipv4Addr::LOCALHOST), "id-1", "Linux Laptop").unwrap();
        assert_eq!(
            luch_proto::session::choose_codec(&ch.welcome, ""),
            "video/avc"
        );
        assert_eq!(ch.start(&video()).unwrap(), (47802, 47803));
        let signals = Arc::new(Signals::default());
        let _wr = ch.follow(signals.clone()).unwrap();
        let got = rx.join().unwrap();
        for _ in 0..200 {
            if signals.stop.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(signals.want_idr.load(Ordering::Relaxed));
        assert!(signals.stop.load(Ordering::Relaxed));
        let kinds: Vec<_> = got.iter().map(|v| v["t"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["hello", "start", "pong", "pong"]);
        assert_eq!(got[0]["name"], "Linux Laptop");
        assert_eq!(got[1]["video"]["fps"], 60);
        assert_eq!(got[3]["n"], 2, "the follower answers pings too");
    }

    #[test]
    fn a_refused_start_is_an_error() {
        let (addr, rx) = receiver(vec![json!({"t": "error", "code": "start_failed"})]);
        let mut ch = Channel::open(addr, None, "id-1", "Linux Laptop").unwrap();
        let e = ch.start(&video()).unwrap_err();
        assert!(e.to_string().contains("start_failed"), "{e}");
        drop(ch);
        assert_eq!(rx.join().unwrap().len(), 2);
    }
}
