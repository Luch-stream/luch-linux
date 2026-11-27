//! Session numbers every sender picks the same way: the codec, the frame size and the bitrate.

use serde_json::Value;

pub const WIDTH: u32 = 1920;
pub const HEIGHT: u32 = 1080;
pub const FPS: u32 = 60;
pub const VIDEO_KBPS: u32 = 20_000; // at 1920x1080; other sizes scale, see `kbps_for`
pub const AUDIO_BPS: u32 = 128_000;
pub const AUDIO_FRAME_MS: u32 = 5;
const UHD: (u32, u32) = (3840, 2160);

/// Video bitrate for a frame size: grows with the square root of the pixel count (4K gets twice
/// the 1080p rate, not four times: Wi-Fi and TV decoders choke on 80 Mbit/s), never below 1080p's.
pub fn kbps_for(w: u32, h: u32) -> u32 {
    let ratio = (w as f64 * h as f64) / (WIDTH as f64 * HEIGHT as f64);
    (VIDEO_KBPS as f64 * ratio.sqrt()).max(VIDEO_KBPS as f64) as u32
}

/// `WxH`, even (4:2:0 chroma), within what encoders and receivers take.
pub fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.trim().split_once(['x', 'X'])?;
    let (w, h) = (w.trim().parse::<u32>().ok()?, h.trim().parse::<u32>().ok()?);
    (w >= 64 && h >= 64 && w <= 8192 && h <= 8192).then_some((w & !1, h & !1))
}

/// Frame size of a session. `LUCH_SIZE` unset: 1920x1080 as before. `auto`: what the receiver announced
/// as `max` in its welcome (e.g. a 4K TV). `WxH`: that size, never larger than the receiver's `max`.
pub fn session_size(env: Option<&str>, receiver_max: Option<&str>) -> (u32, u32) {
    let max = receiver_max.and_then(parse_size);
    match env.map(str::trim) {
        None | Some("") => (WIDTH, HEIGHT),
        // A receiver's announced max can be more than its decoder takes (a Google TV Streamer says 4096x2304 and
        // drops a 4096x2304@60 HEVC session), so `auto` stops at UHD.
        Some("auto") => max
            .map(|(w, h)| (w.min(UHD.0), h.min(UHD.1)))
            .unwrap_or((WIDTH, HEIGHT)),
        Some(v) => match (parse_size(v), max) {
            (Some((w, h)), Some((mw, mh))) => (w.min(mw), h.min(mh)),
            (Some(size), None) => size,
            _ => (WIDTH, HEIGHT),
        },
    }
}

/// The codec for a session from the receiver's welcome and a person's choice ("avc", "hevc" or "" for automatic).
/// HEVC carries the same picture in less data. The receiver's own pick favours H.264 unless HEVC is clearly faster
/// to decode; here HEVC also wins when it decodes about as fast (within 1 ms), as long as the receiver has a decoder
/// for it and its bench ran. A person's choice beats both.
pub fn choose_codec<'a>(welcome: &'a Value, chosen: &str) -> &'a str {
    let bench_ms = |mime: &str| welcome["bench"][mime]["ms"].as_f64();
    let has_hevc = welcome["decoders"]
        .as_array()
        .is_some_and(|d| d.iter().any(|m| m.as_str() == Some("video/hevc")));
    match (chosen, bench_ms("video/avc"), bench_ms("video/hevc")) {
        ("avc", _, _) => "video/avc",
        ("hevc", _, _) if has_hevc => "video/hevc",
        (_, Some(avc), Some(hevc)) if has_hevc && hevc <= avc + 1.0 => "video/hevc",
        _ => welcome["bench"]["prefer"].as_str().unwrap_or("video/avc"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn codec_follows_choice_then_bench_then_the_receiver() {
        let w = json!({"decoders": ["video/avc", "video/hevc"],
            "bench": {"prefer": "video/avc", "video/avc": {"ms": 4.0}, "video/hevc": {"ms": 4.8}}});
        assert_eq!(choose_codec(&w, ""), "video/hevc", "within 1 ms");
        assert_eq!(choose_codec(&w, "avc"), "video/avc");
        let slow = json!({"decoders": ["video/avc", "video/hevc"],
            "bench": {"prefer": "video/avc", "video/avc": {"ms": 4.0}, "video/hevc": {"ms": 9.0}}});
        assert_eq!(choose_codec(&slow, ""), "video/avc");
        assert_eq!(choose_codec(&slow, "hevc"), "video/hevc");
        let no_hevc = json!({"decoders": ["video/avc"], "bench": {"prefer": "video/avc"}});
        assert_eq!(choose_codec(&no_hevc, "hevc"), "video/avc");
        assert_eq!(choose_codec(&Value::Null, ""), "video/avc");
    }

    #[test]
    fn session_size_defaults_to_full_hd() {
        assert_eq!(session_size(None, Some("3840x2160")), (1920, 1080));
        assert_eq!(session_size(Some(""), None), (1920, 1080));
        assert_eq!(
            session_size(Some("nonsense"), Some("3840x2160")),
            (1920, 1080)
        );
    }

    #[test]
    fn session_size_auto_follows_the_receiver() {
        assert_eq!(session_size(Some("auto"), Some("3840x2160")), (3840, 2160));
        assert_eq!(session_size(Some("auto"), Some("4096x2304")), (3840, 2160)); // announced max above UHD
        assert_eq!(session_size(Some("auto"), Some("1280x720")), (1280, 720));
        assert_eq!(session_size(Some("auto"), None), (1920, 1080));
        assert_eq!(session_size(Some("auto"), Some("0x0")), (1920, 1080));
    }

    #[test]
    fn session_size_explicit_is_capped_by_the_receiver_and_even() {
        assert_eq!(
            session_size(Some("2560x1440"), Some("3840x2160")),
            (2560, 1440)
        );
        assert_eq!(
            session_size(Some("3840x2160"), Some("1920x1080")),
            (1920, 1080)
        );
        assert_eq!(session_size(Some("2561x1441"), None), (2560, 1440));
        assert_eq!(session_size(Some("99999x1080"), None), (1920, 1080));
    }

    #[test]
    fn bitrate_scales_with_pixels_but_not_below_1080p() {
        assert_eq!(kbps_for(1920, 1080), VIDEO_KBPS);
        assert_eq!(kbps_for(1280, 720), VIDEO_KBPS);
        assert_eq!(kbps_for(3840, 2160), VIDEO_KBPS * 2);
    }
}
