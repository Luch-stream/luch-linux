//! A cast from this PC (PROTOCOL.md 4a, 4b, 4g): photos, a video or a sound, or a PDF, handed to the receiver to
//! show by itself, the files served from here over HTTP to that receiver only. luchd's cast.rs, smaller: files only
//! (no links, subtitles or qualities), no office files (Windows has no LibreOffice to count on), the receiver given
//! by its address, and the plain control port, where the TV asks its person to Allow each connection (section 3a).

use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::control::Channel;

/// The receiver takes at most so many photos in one list (PROTOCOL.md 4a).
pub const MAX_PHOTOS: usize = 200;

fn extension(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// As luchd's `mime_of`, for files.
pub fn mime(p: &Path) -> &'static str {
    match extension(p).as_str() {
        "mp4" | "m4v" | "mov" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "ts" | "m2ts" => "video/mp2t",
        "3gp" => "video/3gpp",
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
        "pdf" => "application/pdf",
        _ => "",
    }
}

fn name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The message for `files`, as luchd chooses: a PDF first, then photos when all are photos, then the first video
/// or sound. `share` gives the link the receiver fetches a file by.
pub fn message(
    files: &[PathBuf],
    mut share: impl FnMut(&Path, &'static str) -> String,
) -> Result<Value, String> {
    if let Some(pdf) = files.iter().find(|f| extension(f) == "pdf") {
        return Ok(json!({"t": "doc", "url": share(pdf, "application/pdf"),
                         "mime": "application/pdf", "name": name(pdf), "page": 0}));
    }
    let photos: Vec<_> = files
        .iter()
        .filter(|f| mime(f).starts_with("image/"))
        .collect();
    let first = files
        .iter()
        .find(|f| mime(f).starts_with("video/") || mime(f).starts_with("audio/"));
    if let Some(f) = first {
        let m = mime(f);
        let title = f
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        return Ok(
            json!({"t": "media", "via": "sender", "src": [{"url": share(f, m), "mime": m}],
                         "title": title}),
        );
    }
    if photos.is_empty() {
        let f = files.first().ok_or("nothing to show")?;
        return Err(
            if ["doc", "docx", "odt", "ppt", "pptx", "odp", "xls", "xlsx"]
                .contains(&extension(f).as_str())
            {
                format!(
                    "{}: from Windows only PDF documents for now (save it as PDF)",
                    name(f)
                )
            } else {
                format!("{}: not a photo, video, sound or PDF", name(f))
            },
        );
    }
    let items: Vec<Value> = photos
        .into_iter()
        .take(MAX_PHOTOS)
        .map(|f| {
            let m = mime(f);
            json!({"url": share(f, m), "mime": m, "name": name(f)})
        })
        .collect();
    Ok(json!({"t": "photo", "index": 0, "items": items}))
}

/// 128 random bits as hex: the token in a file's link. RandomState is seeded from the system's random source.
pub fn token() -> String {
    let half = || {
        let mut h = RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default(),
        );
        h.finish()
    };
    format!("{:016x}{:016x}", half(), half())
}

fn percent(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// What a `Range` header asks of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Whole,
    /// First and last byte, inclusive (206).
    Bytes(u64, u64),
    /// Nothing the file has (416).
    Outside,
}

pub fn range(header: Option<&str>, len: u64) -> Part {
    match bytes(header, len) {
        Ok(None) => Part::Whole,
        Ok(Some((a, b))) => Part::Bytes(a, b),
        Err(()) => Part::Outside,
    }
}

fn bytes(header: Option<&str>, len: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(spec) = header.and_then(|h| h.trim().strip_prefix("bytes=")) else {
        return Ok(None);
    };
    // Several ranges are answered with the first only, as a single range is what players ask for.
    let spec = spec.split(',').next().unwrap_or("").trim();
    let (a, b) = spec.split_once('-').ok_or(())?;
    let (first, last) = match (a.trim(), b.trim()) {
        ("", n) => {
            let n: u64 = n.parse().map_err(|_| ())?;
            if n == 0 {
                return Err(());
            }
            (len.saturating_sub(n), len.wrapping_sub(1))
        }
        (a, "") => (a.parse().map_err(|_| ())?, len.wrapping_sub(1)),
        (a, b) => {
            let (a, b): (u64, u64) = (a.parse().map_err(|_| ())?, b.parse().map_err(|_| ())?);
            (a, b.min(len.wrapping_sub(1)))
        }
    };
    if len == 0 || first >= len || first > last {
        return Err(());
    }
    Ok(Some((first, last)))
}

type Files = Arc<Mutex<HashMap<String, (PathBuf, &'static str)>>>;

/// The files of one cast over HTTP/1.1 with Range (a player seeks), each under a random token, to `client` only.
pub struct FileServer {
    addr: SocketAddrV4,
    files: Files,
}

impl FileServer {
    /// Any free port: the link carries it, and an older cast's server may still hold another.
    pub fn start(bind: Ipv4Addr, client: Ipv4Addr) -> io::Result<FileServer> {
        let l = TcpListener::bind(SocketAddrV4::new(bind, 0))?;
        let addr = match l.local_addr()? {
            std::net::SocketAddr::V4(a) => a,
            a => return Err(io::Error::other(format!("not IPv4: {a}"))),
        };
        let files: Files = Arc::default();
        let served = files.clone();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                if s.peer_addr().ok().map(|a| a.ip()) != Some(IpAddr::V4(client)) {
                    continue;
                }
                let files = served.clone();
                std::thread::spawn(move || {
                    let _ = serve(s, &files);
                });
            }
        });
        Ok(FileServer { addr, files })
    }

    /// The link the receiver fetches `path` by.
    pub fn share(&self, path: &Path, mime: &'static str) -> String {
        let t = token();
        let url = format!("http://{}/{t}/{}", self.addr, percent(&name(path)));
        self.files
            .lock()
            .unwrap()
            .insert(t, (path.to_path_buf(), mime));
        url
    }
}

fn serve(s: TcpStream, files: &Files) -> io::Result<()> {
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut rd = BufReader::new(s.try_clone()?);
    let mut wr = s;
    let mut request = String::new();
    rd.read_line(&mut request)?;
    let mut ranges = None;
    loop {
        let mut line = String::new();
        if rd.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("range") {
                ranges = Some(v.trim().to_string());
            }
        }
    }
    let mut parts = request.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let token = path.trim_start_matches('/').split('/').next().unwrap_or("");
    let found = files.lock().unwrap().get(token).cloned();
    let say = |wr: &mut TcpStream, status: &str, headers: &str| {
        wr.write_all(format!("HTTP/1.1 {status}\r\n{headers}Connection: close\r\n\r\n").as_bytes())
    };
    let Some((file, mime)) = found.filter(|_| method == "GET" || method == "HEAD") else {
        return say(&mut wr, "404 Not Found", "Content-Length: 0\r\n");
    };
    let mut f = std::fs::File::open(&file)?;
    let len = f.metadata()?.len();
    let mime = if mime.is_empty() {
        "application/octet-stream"
    } else {
        mime
    };
    let (status, first, count, extra) = match range(ranges.as_deref(), len) {
        Part::Whole => ("200 OK", 0, len, String::new()),
        Part::Bytes(a, b) => (
            "206 Partial Content",
            a,
            b - a + 1,
            format!("Content-Range: bytes {a}-{b}/{len}\r\n"),
        ),
        Part::Outside => {
            return say(
                &mut wr,
                "416 Range Not Satisfiable",
                &format!("Content-Range: bytes */{len}\r\nContent-Length: 0\r\n"),
            )
        }
    };
    say(
        &mut wr,
        status,
        &format!(
            "Content-Type: {mime}\r\nContent-Length: {count}\r\nAccept-Ranges: bytes\r\n{extra}"
        ),
    )?;
    if method == "HEAD" {
        return Ok(());
    }
    io::Seek::seek(&mut f, io::SeekFrom::Start(first))?;
    io::copy(&mut f.take(count), &mut wr)?;
    Ok(())
}

/// What a cast is doing, for the person: the TV asks them first, or it shows the files now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    WaitingForAllow,
    Showing(String),
}

fn timed_out(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// The receiver asks its person about this computer and answers not_allowed until they press Allow, saying nothing
/// when they do, so the message is sent again every 1.5 s for a minute (as the Mac's Engine does); Deny closes.
fn shown(ch: &mut Channel, msg: &Value, step: &mut dyn FnMut(Step)) -> Result<(), String> {
    let kind = msg["t"].as_str().unwrap_or("").to_string();
    let ready = format!("{kind}_ready");
    let until = Instant::now() + Duration::from_secs(60);
    let mut told = false;
    while Instant::now() < until {
        ch.send(msg).map_err(|e| e.to_string())?;
        let answer = loop {
            match ch.recv(Duration::from_secs(5)) {
                Ok(Some(v)) if v["t"] == ready.as_str() || v["t"] == "error" => break v,
                Ok(Some(_)) => {}
                Ok(None) => return Err("refused on the TV".into()),
                Err(e) if timed_out(&e) => return Err("the receiver did not answer".into()),
                Err(e) => return Err(e.to_string()),
            }
        };
        if answer["t"] == ready.as_str() {
            step(Step::Showing(kind));
            return Ok(());
        }
        let detail = answer["detail"]
            .as_str()
            .or(answer["code"].as_str())
            .unwrap_or("refused")
            .to_string();
        if answer["code"] != "not_allowed" || detail.contains("refused") {
            return Err(match answer["code"].as_str() {
                Some("busy") => "the TV is showing something else and did not let this in".into(),
                _ => detail,
            });
        }
        if !told {
            told = true;
            step(Step::WaitingForAllow);
        }
        std::thread::sleep(Duration::from_millis(1500));
    }
    Err("nobody pressed Allow on the TV".into())
}

/// Shows `files` on the receiver at `to` and serves them until it is done with them: Back on the remote, the end
/// of a video, another show taking the screen, or the receiver gone (30 s of silence, PROTOCOL.md 3).
pub fn run(
    to: SocketAddrV4,
    from: Option<Ipv4Addr>,
    files: &[PathBuf],
    id: &str,
    host: &str,
    step: &mut dyn FnMut(Step),
) -> Result<(), String> {
    for f in files {
        if !f.is_file() {
            return Err(format!("{}: not a file", f.display()));
        }
    }
    let mut ch = Channel::open(to, from, id, host, &["media", "photo"]).map_err(|e| {
        if timed_out(&e) || e.kind() == io::ErrorKind::ConnectionRefused {
            format!("no receiver answers at {to}: is Luch open on the TV?")
        } else {
            format!("{to}: {e}")
        }
    })?;
    let me = ch.local_ip().map_err(|e| e.to_string())?;
    let server = FileServer::start(me, *to.ip()).map_err(|e| format!("file server: {e}"))?;
    let msg = message(files, |p, m| server.share(p, m))?;
    shown(&mut ch, &msg, step)?;
    loop {
        match ch.recv(Duration::from_secs(35)) {
            Ok(Some(v)) if v["t"] == "stop" || v["t"] == "bye" => return Ok(()),
            Ok(Some(_)) => {}
            Ok(None) => return Ok(()),
            Err(e) if timed_out(&e) => return Ok(()),
            Err(e) => return Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luch_proto::control;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    fn link(p: &Path, m: &str) -> String {
        format!("http://pc/{}#{m}", name(p))
    }

    #[test]
    fn photos_a_video_or_a_pdf() {
        let m = message(&paths(&["/p/a.JPG", "/p/b.png"]), link).unwrap();
        assert_eq!(m["t"], "photo");
        assert_eq!(m["items"][1]["url"], "http://pc/b.png#image/png");
        assert_eq!(m["items"][0]["mime"], "image/jpeg");

        let m = message(&paths(&["a.jpg", "film.mkv"]), link).unwrap();
        assert_eq!(
            m["t"], "media",
            "a video among photos is played, as luchd does"
        );
        assert_eq!(m["title"], "film");
        assert_eq!(m["via"], "sender");
        assert_eq!(m["src"][0]["mime"], "video/x-matroska");

        let m = message(&paths(&["song.flac", "talk.pdf"]), link).unwrap();
        assert_eq!(m["t"], "doc");
        assert_eq!(m["name"], "talk.pdf");

        let many: Vec<_> = (0..250)
            .map(|i| PathBuf::from(format!("{i}.jpg")))
            .collect();
        assert_eq!(
            message(&many, link).unwrap()["items"]
                .as_array()
                .unwrap()
                .len(),
            MAX_PHOTOS
        );
    }

    #[test]
    fn what_cannot_be_shown_from_windows() {
        let e = message(&paths(&["deck.pptx"]), link).unwrap_err();
        assert!(e.contains("save it as PDF"), "{e}");
        assert!(message(&paths(&["a.zip"]), link)
            .unwrap_err()
            .contains("not a photo"));
        assert!(message(&[], link).is_err());
        // Photos with something else among them: the photos.
        assert_eq!(
            message(&paths(&["a.jpg", "notes.txt"]), link).unwrap()["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn ranges() {
        assert_eq!(range(None, 100), Part::Whole);
        assert_eq!(range(Some("bytes=0-"), 100), Part::Bytes(0, 99));
        assert_eq!(range(Some("bytes=10-19"), 100), Part::Bytes(10, 19));
        assert_eq!(range(Some("bytes=90-200"), 100), Part::Bytes(90, 99));
        assert_eq!(range(Some("bytes=-10"), 100), Part::Bytes(90, 99));
        assert_eq!(range(Some("bytes=0-0,5-9"), 100), Part::Bytes(0, 0));
        assert_eq!(range(Some("bytes=100-"), 100), Part::Outside);
        assert_eq!(range(Some("bytes=5-2"), 100), Part::Outside);
        assert_eq!(range(Some("bytes=x-"), 100), Part::Outside);
        assert_eq!(range(Some("bytes=0-"), 0), Part::Outside);
    }

    #[test]
    fn tokens_differ_and_names_are_escaped() {
        assert_ne!(token(), token());
        assert_eq!(token().len(), 32);
        assert_eq!(percent("My film (1).mkv"), "My%20film%20%281%29.mkv");
        assert_eq!(percent("фото.jpg"), "%D1%84%D0%BE%D1%82%D0%BE.jpg");
    }

    fn get(url: &str, extra: &str) -> (String, Vec<u8>) {
        let rest = url.strip_prefix("http://").unwrap();
        let (host, path) = rest.split_once('/').unwrap();
        let mut s = TcpStream::connect(host).unwrap();
        write!(s, "GET /{path} HTTP/1.0\r\n{extra}\r\n").unwrap();
        let mut all = Vec::new();
        s.read_to_end(&mut all).unwrap();
        let at = all.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        (
            String::from_utf8_lossy(&all[..at]).into_owned(),
            all[at + 4..].to_vec(),
        )
    }

    fn temp(name: &str, body: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("luch-win-test-{}", token()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn the_server_gives_a_file_and_its_parts_to_the_receiver() {
        let f = temp("clip one.mp4", b"0123456789");
        let srv = FileServer::start(Ipv4Addr::LOCALHOST, Ipv4Addr::LOCALHOST).unwrap();
        let url = srv.share(&f, "video/mp4");
        assert!(url.ends_with("/clip%20one.mp4"), "{url}");
        let (head, body) = get(&url, "");
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert!(head.contains("Content-Type: video/mp4"));
        assert_eq!(body, b"0123456789");
        let (head, body) = get(&url, "Range: bytes=2-4\r\n");
        assert!(head.starts_with("HTTP/1.1 206"), "{head}");
        assert!(head.contains("Content-Range: bytes 2-4/10"));
        assert_eq!(body, b"234");
        let (head, _) = get(&url, "Range: bytes=20-\r\n");
        assert!(head.starts_with("HTTP/1.1 416"), "{head}");
        let token = url.split('/').nth(3).unwrap();
        let wrong = url.replace(token, &"0".repeat(32));
        assert!(get(&wrong, "").0.starts_with("HTTP/1.1 404"));
    }

    #[test]
    fn nobody_else_gets_the_files() {
        let f = temp("a.jpg", b"x");
        // The receiver is somewhere else: a connection from here is closed unanswered.
        let srv = FileServer::start(Ipv4Addr::LOCALHOST, Ipv4Addr::new(10, 0, 0, 9)).unwrap();
        let url = srv.share(&f, "image/jpeg");
        let host = url
            .strip_prefix("http://")
            .unwrap()
            .split('/')
            .next()
            .unwrap();
        let mut s = TcpStream::connect(host).unwrap();
        let _ = write!(s, "GET / HTTP/1.0\r\n\r\n");
        let mut all = Vec::new();
        let _ = s.read_to_end(&mut all);
        assert!(all.is_empty());
    }

    /// A receiver on the plain port: Allow pressed after `refusals` not_allowed answers, then it fetches the photo
    /// it was sent and the person presses Back.
    type Heard = std::thread::JoinHandle<(Vec<Value>, Vec<u8>)>;

    fn receiver(refusals: usize) -> (SocketAddrV4, Heard) {
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
            let mut left = refusals;
            let mut line = String::new();
            let mut body = Vec::new();
            while rd.read_line(&mut line).unwrap_or(0) > 0 {
                let v: Value = serde_json::from_str(&line).unwrap();
                line.clear();
                got.push(v.clone());
                match v["t"].as_str() {
                    Some("hello") => {
                        say(json!({"t": "welcome", "v": 0, "modes": ["photo", "media"]}))
                    }
                    Some("photo") if left > 0 => {
                        left -= 1;
                        say(json!({"t": "error", "code": "not_allowed", "for": "photo",
                                   "detail": "allow this computer on the TV"}));
                    }
                    Some("photo") => {
                        say(json!({"t": "ping", "n": 7}));
                        say(json!({"t": "photo_ready"}));
                        body = get(v["items"][0]["url"].as_str().unwrap(), "").1;
                        say(json!({"t": "stop", "by": "receiver", "reason": "back"}));
                    }
                    _ => {}
                }
            }
            (got, body)
        });
        (addr, h)
    }

    #[test]
    fn a_cast_waits_for_allow_shows_and_ends_with_back() {
        let f = temp("IMG_1.jpg", b"jpeg bytes");
        let (addr, rx) = receiver(1);
        let mut steps = Vec::new();
        run(addr, None, &[f], "id-1", "Windows PC", &mut |s| {
            steps.push(s)
        })
        .unwrap();
        assert_eq!(
            steps,
            [Step::WaitingForAllow, Step::Showing("photo".into())]
        );
        let (got, body) = rx.join().unwrap();
        let kinds: Vec<_> = got.iter().map(|v| v["t"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["hello", "photo", "photo", "pong"]);
        assert_eq!(got[0]["want"], json!(["media", "photo"]));
        assert_eq!(got[0]["name"], "Windows PC");
        assert_eq!(body, b"jpeg bytes");
    }

    #[test]
    fn a_refusal_is_said() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = match l.local_addr().unwrap() {
            std::net::SocketAddr::V4(a) => a,
            _ => unreachable!(),
        };
        std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut rd = BufReader::new(s.try_clone().unwrap());
            let mut wr = s;
            let mut line = String::new();
            while rd.read_line(&mut line).unwrap_or(0) > 0 {
                let v: Value = serde_json::from_str(&line).unwrap();
                line.clear();
                let answer = match v["t"].as_str() {
                    Some("hello") => json!({"t": "welcome", "v": 0}),
                    _ => json!({"t": "error", "code": "busy", "for": "media"}),
                };
                wr.write_all(control::line(&answer).as_bytes()).unwrap();
            }
        });
        let f = temp("song.mp3", b"id3");
        let e = run(addr, None, &[f], "id-1", "PC", &mut |_| {}).unwrap_err();
        assert!(e.contains("showing something else"), "{e}");
        let e = run(
            addr,
            None,
            &[PathBuf::from("/no/such.jpg")],
            "id-1",
            "PC",
            &mut |_| {},
        )
        .unwrap_err();
        assert!(e.contains("not a file"), "{e}");
    }
}
