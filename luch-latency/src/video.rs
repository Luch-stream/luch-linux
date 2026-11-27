//! Frames to analyse: from a video file through ffmpeg, or from memory (the synthetic camera, tests).

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// One grey camera frame and when it was taken, in seconds of the video's own clock.
pub struct Frame {
    pub t: f64,
    pub width: usize,
    pub height: usize,
    pub px: Vec<u8>,
}

/// Something that can play its frames from the start, as many times as asked: the analyser reads twice (once to
/// find the patterns, once to read every frame) and keeps no more than one frame in memory.
pub trait Source {
    fn len(&self) -> usize;
    fn play(&self) -> io::Result<Box<dyn Iterator<Item = io::Result<Frame>> + '_>>;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The synthetic camera, frame by frame.
impl Source for crate::synth::Camera {
    fn len(&self) -> usize {
        self.frame_count()
    }

    fn play(&self) -> io::Result<Box<dyn Iterator<Item = io::Result<Frame>> + '_>> {
        let s = self.scene();
        Ok(Box::new((0..self.frame_count()).map(move |k| {
            Ok(Frame {
                t: k as f64 / s.fps,
                width: s.width,
                height: s.height,
                px: self.frame(k),
            })
        })))
    }
}

/// A video file, decoded by ffmpeg to grey frames. Phones store 240 fps video as it was shot, with the real
/// timestamps; those are what count, not the nominal frame rate.
pub struct Video {
    path: PathBuf,
    width: usize,
    height: usize,
    times: Vec<f64>,
}

fn tool(name: &str, args: &[&str]) -> io::Result<String> {
    let out = Command::new(name)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| io::Error::new(e.kind(), format!("{name}: {e} (is ffmpeg installed?)")))?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

impl Video {
    /// `fps`: take this frame rate instead of the file's timestamps (for a file that lost them).
    pub fn open(path: &Path, fps: Option<f64>) -> io::Result<Video> {
        let p = path.to_string_lossy();
        let size = tool(
            "ffprobe",
            &[
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=width,height",
                "-of",
                "csv=p=0",
                &p,
            ],
        )?;
        let mut wh = size.trim().split(',').map(|v| v.trim().parse::<usize>());
        let (Some(Ok(width)), Some(Ok(height))) = (wh.next(), wh.next()) else {
            return Err(io::Error::other(format!("no video stream in {p}")));
        };
        // Packet timestamps, sorted: presentation order without decoding twice.
        let pts = tool(
            "ffprobe",
            &[
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "packet=pts_time",
                "-of",
                "csv=p=0",
                &p,
            ],
        )?;
        let mut times: Vec<f64> = pts
            .lines()
            .filter_map(|l| l.trim().trim_end_matches(',').parse().ok())
            .collect();
        times.sort_by(f64::total_cmp);
        if let Some(fps) = fps {
            times = (0..times.len()).map(|i| i as f64 / fps).collect();
        }
        if let Some(&first) = times.first() {
            times.iter_mut().for_each(|t| *t -= first);
        }
        Ok(Video {
            path: path.to_path_buf(),
            width,
            height,
            times,
        })
    }
}

impl Source for Video {
    fn len(&self) -> usize {
        self.times.len()
    }

    fn play(&self) -> io::Result<Box<dyn Iterator<Item = io::Result<Frame>> + '_>> {
        // As stored, not rotated: the analyser finds the patterns at any angle.
        let mut child = Command::new("ffmpeg")
            .args(["-v", "error", "-noautorotate", "-i"])
            .arg(&self.path)
            .args([
                "-map",
                "0:v:0",
                "-fps_mode",
                "passthrough",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "gray",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()?;
        let mut out = child.stdout.take().expect("piped");
        let (w, h) = (self.width, self.height);
        let mut k = 0;
        Ok(Box::new(std::iter::from_fn(move || {
            if k >= self.times.len() {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            let mut px = vec![0u8; w * h];
            match out.read_exact(&mut px) {
                Ok(()) => {
                    let t = self.times[k];
                    k += 1;
                    Some(Ok(Frame {
                        t,
                        width: w,
                        height: h,
                        px,
                    }))
                }
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    let _ = child.wait();
                    None
                }
                Err(e) => Some(Err(e)),
            }
        })))
    }
}
