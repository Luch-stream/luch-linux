//! The receiver's QR code through this computer's camera (PROTOCOL.md 1e): the person holds the laptop up to the TV.
//! Decoded with rqrr (pure Rust, no system library); frames come from GStreamer, which luchd already uses.

use super::card::{parse_link, Card};

/// Every QR text in a grey picture. The TV draws its code light on dark, so a picture with none is tried inverted.
pub fn texts(width: usize, height: usize, luma: &[u8]) -> Vec<String> {
    let read = |invert: bool| -> Vec<String> {
        let mut img = rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| {
            let v = luma[y * width + x];
            if invert {
                255 - v
            } else {
                v
            }
        });
        img.detect_grids()
            .into_iter()
            .filter_map(|g| g.decode().ok().map(|(_, text)| text))
            .collect()
    };
    let plain = read(false);
    if plain.is_empty() {
        read(true)
    } else {
        plain
    }
}

/// The receivers' cards in a picture: QR codes that are Luch links.
pub fn cards(width: usize, height: usize, luma: &[u8]) -> Vec<Card> {
    texts(width, height, luma)
        .iter()
        .filter(|t| t.starts_with("luch://"))
        .filter_map(|t| parse_link(t))
        .collect()
}

/// Watches `src` (a GStreamer source: "v4l2src" for the camera) for up to `secs` seconds and returns the first
/// receivers whose code it sees.
pub async fn watch(src: &str, secs: u64) -> Result<Vec<Card>, String> {
    use gstreamer::prelude::*;
    let pipeline = gstreamer::parse::launch(&format!(
        "{src} ! videoconvert ! videoscale ! video/x-raw,format=GRAY8 ! appsink name=sink sync=false max-buffers=2 drop=true"
    ))
    .map_err(|e| format!("no camera pipeline: {e}"))?
    .downcast::<gstreamer::Pipeline>()
    .map_err(|_| "not a pipeline".to_string())?;
    let sink = pipeline
        .by_name("sink")
        .and_then(|s| s.downcast::<gstreamer_app::AppSink>().ok())
        .ok_or("no appsink")?;
    pipeline
        .set_state(gstreamer::State::Playing)
        .map_err(|e| format!("cannot open the camera: {e}"))?;
    let found = tokio::task::spawn_blocking(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        while std::time::Instant::now() < deadline {
            let Some(sample) = sink.try_pull_sample(gstreamer::ClockTime::from_mseconds(200))
            else {
                if sink.is_eos() {
                    break;
                }
                continue;
            };
            let Some(info) = sample
                .caps()
                .and_then(|c| gstreamer_video::VideoInfo::from_caps(c).ok())
            else {
                continue;
            };
            let Some(buffer) = sample.buffer() else {
                continue;
            };
            let Ok(map) = buffer.map_readable() else {
                continue;
            };
            let (w, h, stride) = (
                info.width() as usize,
                info.height() as usize,
                info.stride()[0] as usize,
            );
            // Rows may be padded: copy them out tight.
            let luma: Vec<u8> = (0..h)
                .flat_map(|y| map[y * stride..y * stride + w].iter().copied())
                .collect();
            let c = cards(w, h, &luma);
            if !c.is_empty() {
                return c;
            }
        }
        Vec::new()
    })
    .await
    .map_err(|e| e.to_string())?;
    let _ = pipeline.set_state(gstreamer::State::Null);
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    const LINK: &str = "luch://connect?host=192.168.1.20&port=47800&id=3f0c2a8e&t=0badf00d";

    /// A picture of a code: `cell` pixels per module, a quiet zone, dark modules on light, or as the TV draws it
    /// (round amber dots on a dark card).
    fn picture(text: &str, cell: usize, tv_style: bool) -> (usize, Vec<u8>) {
        let code = qrcode::QrCode::with_error_correction_level(text, qrcode::EcLevel::H).unwrap();
        let n = code.width();
        let quiet = 4;
        let size = (n + 2 * quiet) * cell;
        let (on, off) = if tv_style {
            (200u8, 40u8)
        } else {
            (0u8, 255u8)
        };
        let mut img = vec![off; size * size];
        for y in 0..n {
            for x in 0..n {
                if code[(x, y)] != qrcode::Color::Dark {
                    continue;
                }
                for dy in 0..cell {
                    for dx in 0..cell {
                        // A dot: the corners of each module stay dark.
                        let (fx, fy) = (
                            dx as f32 + 0.5 - cell as f32 / 2.0,
                            dy as f32 + 0.5 - cell as f32 / 2.0,
                        );
                        if tv_style && fx * fx + fy * fy > (cell as f32 * 0.5).powi(2) {
                            continue;
                        }
                        img[((y + quiet) * cell + dy) * size + (x + quiet) * cell + dx] = on;
                    }
                }
            }
        }
        (size, img)
    }

    /// A camera's view: the code somewhere in a bigger, noisy, unevenly lit picture.
    fn in_a_room(code: &[u8], size: usize, w: usize, h: usize, at: (usize, usize)) -> Vec<u8> {
        let mut seed = 99u64;
        let mut img = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                let noise = ((seed >> 59) as i32) - 16;
                let base = if (at.0..at.0 + size).contains(&x) && (at.1..at.1 + size).contains(&y) {
                    code[(y - at.1) * size + (x - at.0)] as i32
                } else {
                    90 + (x * 40 / w) as i32
                };
                img[y * w + x] = (base * 9 / 10 + 10 + noise).clamp(0, 255) as u8;
            }
        }
        img
    }

    #[test]
    fn a_printed_code_and_a_tvs_dotted_code_are_read() {
        for tv in [false, true] {
            let (size, img) = picture(LINK, 6, tv);
            let room = in_a_room(&img, size, 640, 480, (200, 100));
            let got = cards(640, 480, &room);
            assert_eq!(got.len(), 1, "tv style {tv}");
            assert_eq!(got[0].addr, Some((Ipv4Addr::new(192, 168, 1, 20), 47800)));
            assert_eq!(got[0].token.as_deref(), Some("0badf00d"));
            assert_eq!(got[0].id, "3f0c2a8e");
        }
    }

    #[test]
    fn other_codes_are_not_receivers() {
        let (size, img) = picture("https://example.org/", 5, false);
        assert_eq!(
            texts(size, size, &img),
            vec!["https://example.org/".to_string()]
        );
        assert!(cards(size, size, &img).is_empty());
        assert!(cards(64, 64, &[128; 64 * 64]).is_empty());
    }

    /// A code made by the receiver's own encoder (QrCode.java, drawn by its QrCodeTest into this fixture) and read
    /// here: both ends of the QR path, written apart.
    #[test]
    fn the_receivers_own_code_is_read() {
        let pgm = include_bytes!("../../tests/receiver-qr.pgm");
        let (w, h, luma) = super::super::tests::read_pgm(pgm);
        let got = cards(w, h, luma);
        assert_eq!(got.len(), 1);
        // The receiver leaves the id out (its token says which TV it is) to keep the code small.
        let c = &got[0];
        assert_eq!(
            (c.addr, c.token.as_deref(), c.id.as_str()),
            (
                Some((Ipv4Addr::new(192, 168, 1, 20), 47800)),
                Some("0badf00d"),
                ""
            )
        );
    }

    /// The camera path without a camera: GStreamer's test pattern through the same conversion, nothing found.
    #[tokio::test]
    async fn the_camera_pipeline_runs_on_a_test_source() {
        gstreamer::init().unwrap();
        let got = watch(
            "videotestsrc num-buffers=5 ! video/x-raw,width=320,height=240",
            3,
        )
        .await
        .unwrap();
        assert!(got.is_empty());
    }
}
