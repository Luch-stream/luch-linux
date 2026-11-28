//! The control channel of PROTOCOL.md section 3 and 4: one JSON object per line, and the messages a mirroring
//! sender builds. Only the framing and the message shapes live here; sockets and session state stay with each sender.

use serde_json::{json, Value};

use crate::{APP, MAX_LINE, VERSION};

#[derive(Debug, PartialEq, Eq)]
pub enum LineError {
    /// More than MAX_LINE bytes, or the connection ended in the middle of a line.
    TooLongOrTruncated,
    Json(String),
}

impl std::fmt::Display for LineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LineError::TooLongOrTruncated => f.write_str("line too long or truncated"),
            LineError::Json(e) => write!(f, "bad json: {e}"),
        }
    }
}

impl std::error::Error for LineError {}

/// A message as it goes on the wire: compact JSON and `\n`.
pub fn line(v: &Value) -> String {
    let mut s = v.to_string();
    s.push('\n');
    s
}

/// A message from the bytes of one line as read (with its `\n`). Read at most MAX_LINE + 1 bytes before calling this,
/// so an endless line is refused instead of filling memory.
pub fn parse_line(buf: &[u8]) -> Result<Value, LineError> {
    if buf.len() > MAX_LINE || buf.last() != Some(&b'\n') {
        return Err(LineError::TooLongOrTruncated);
    }
    serde_json::from_slice(buf).map_err(|e| LineError::Json(e.to_string()))
}

/// The sender's first line.
pub fn hello(id: &str, name: &str, want: &[&str]) -> Value {
    json!({"t": "hello", "v": VERSION, "id": id, "name": name, "app": APP, "want": want})
}

/// What a mirroring session sends: codec as in `decoders` ("video/hevc", "video/avc").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Video {
    pub codec: &'static str,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// `start` for mirroring (`video` set) or sound only (PROTOCOL.md 4d, `video` None). `audio` is the `audio` object as
/// the sender chose it, or None for the picture only.
pub fn start(video: Option<&Video>, audio: Option<&Value>) -> Value {
    let mut v = match video {
        Some(s) => json!({"t": "start", "mode": "mirror",
            "video": {"codec": s.codec, "width": s.width, "height": s.height, "fps": s.fps}}),
        None => json!({"t": "start", "mode": "audio"}),
    };
    if let Some(a) = audio {
        v["audio"] = a.clone();
    }
    v
}

/// Asks the receiver for its heat badge in this session (`"watch_device":true`, PROTOCOL.md 5). Off adds nothing:
/// a start without it is what older senders send, and gets no badge.
pub fn watch_device(start: &mut Value, on: bool) {
    if on {
        start["watch_device"] = Value::Bool(true);
    }
}

/// The UDP ports of a `ready`, both of them valid, or None.
pub fn ready_ports(ready: &Value) -> Option<(u16, u16)> {
    let port = |k: &str| {
        ready[k]
            .as_u64()
            .filter(|p| *p > 0 && *p < 65536)
            .map(|p| p as u16)
    };
    port("video_port").zip(port("audio_port"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_one_json_object_and_a_newline() {
        let l = line(&json!({"t": "ping", "n": 3}));
        assert_eq!(l, "{\"n\":3,\"t\":\"ping\"}\n");
        assert_eq!(parse_line(l.as_bytes()).unwrap()["n"], 3);
    }

    #[test]
    fn long_truncated_and_bad_lines_are_refused() {
        assert_eq!(
            parse_line(b"{\"t\":\"ping\"}"),
            Err(LineError::TooLongOrTruncated)
        );
        let mut long = vec![b' '; MAX_LINE];
        long.push(b'\n');
        assert_eq!(parse_line(&long), Err(LineError::TooLongOrTruncated));
        assert!(matches!(parse_line(b"{oops\n"), Err(LineError::Json(_))));
    }

    #[test]
    fn hello_carries_version_and_app() {
        let h = hello("id-1", "Linux Laptop", &["mirror"]);
        assert_eq!(
            (&h["t"], &h["v"], &h["app"]),
            (&json!("hello"), &json!(VERSION), &json!(APP))
        );
        assert_eq!(h["want"], json!(["mirror"]));
    }

    #[test]
    fn start_for_mirror_and_for_sound_only() {
        let v = Video {
            codec: "video/hevc",
            width: 1920,
            height: 1080,
            fps: 60,
        };
        let audio = json!({"codec": "audio/opus", "rate": 48000});
        let s = start(Some(&v), Some(&audio));
        assert_eq!(s["mode"], "mirror");
        assert_eq!(s["video"]["codec"], "video/hevc");
        assert_eq!(s["video"]["width"], 1920);
        assert_eq!(s["audio"], audio);
        let s = start(None, None);
        assert_eq!(s, json!({"t": "start", "mode": "audio"}));
    }

    #[test]
    fn watching_the_device_is_asked_for_or_left_out() {
        let mut s = start(None, None);
        watch_device(&mut s, false);
        assert_eq!(s, json!({"t": "start", "mode": "audio"}));
        watch_device(&mut s, true);
        assert_eq!(s["watch_device"], true);
    }

    #[test]
    fn ready_needs_both_ports_in_range() {
        assert_eq!(
            ready_ports(&json!({"t": "ready", "video_port": 47802, "audio_port": 47803})),
            Some((47802, 47803))
        );
        assert_eq!(ready_ports(&json!({"video_port": 47802})), None);
        assert_eq!(
            ready_ports(&json!({"video_port": 0, "audio_port": 1})),
            None
        );
        assert_eq!(
            ready_ports(&json!({"video_port": 70000, "audio_port": 1})),
            None
        );
    }
}
