//! The command line of the analyser, shared by `luch-latency` and `luchctl latency`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::analyze::{self, Options};
use crate::synth::{Camera, Scene};
use crate::video::{Source, Video};
use crate::{pattern, report};

pub const ANALYZE_HELP: &str = "\
analyze: reads a camera's video of the computer's screen and the TV (both in the picture, the latency lab on), and
writes PREFIX.md and PREFIX.json (default: next to the video) to attach to an issue.
  --fps N         the camera's frame rate, when the file's timestamps are wrong
  --readout-ms MS the camera's rolling shutter (top to bottom), when the screens are not side by side";

fn value<'a>(args: &'a [String], i: &mut usize, name: &str) -> Result<&'a str, String> {
    *i += 1;
    args.get(*i)
        .map(String::as_str)
        .ok_or(format!("{name} needs a value"))
}

fn number(args: &[String], i: &mut usize, name: &str) -> Result<f64, String> {
    let v = value(args, i, name)?;
    v.parse().map_err(|_| format!("{name}: not a number: {v}"))
}

pub fn analyze(args: &[String]) -> Result<(), String> {
    let (mut video, mut out, mut fps, mut opt) = (None, None, None, Options::default());
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => out = Some(PathBuf::from(value(args, &mut i, "--out")?)),
            "--fps" => fps = Some(number(args, &mut i, "--fps")?),
            "--readout-ms" => opt.readout_ms = number(args, &mut i, "--readout-ms")?,
            a if a.starts_with('-') => return Err(format!("unknown option {a}\n\n{ANALYZE_HELP}")),
            a => video = Some(PathBuf::from(a)),
        }
        i += 1;
    }
    let video = video.ok_or(format!("which video?\n\n{ANALYZE_HELP}"))?;
    let src = Video::open(&video, fps).map_err(|e| e.to_string())?;
    eprintln!("reading {} frames of {}…", src.len(), video.display());
    let a = analyze::analyze(&src, &opt).map_err(|e| e.to_string())?;
    let md = report::markdown(&a);
    let prefix = out.unwrap_or_else(|| video.with_extension("latency"));
    let (md_path, json_path) = (prefix.with_extension("md"), prefix.with_extension("json"));
    std::fs::write(&md_path, &md).map_err(|e| format!("{}: {e}", md_path.display()))?;
    let json = serde_json::to_string_pretty(&report::json(&a)).expect("json");
    std::fs::write(&json_path, json + "\n").map_err(|e| format!("{}: {e}", json_path.display()))?;
    println!("{md}");
    eprintln!("written: {} and {}", md_path.display(), json_path.display());
    if a.latency.count == 0 {
        return Err("no latency measured, see the warnings above".into());
    }
    Ok(())
}

/// Encodes the synthetic camera's frames with ffmpeg, as a phone would.
pub fn encode(cam: &Camera, out: &Path) -> Result<(), String> {
    let s = cam.scene();
    let mut child = Command::new("ffmpeg")
        .args([
            "-y", "-v", "error", "-f", "rawvideo", "-pix_fmt", "gray", "-s",
        ])
        .arg(format!("{}x{}", s.width, s.height))
        .args([
            "-r",
            &s.fps.to_string(),
            "-i",
            "-",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "16",
        ])
        .args(["-pix_fmt", "yuv420p"])
        .arg(out)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    let mut pipe = child.stdin.take().expect("piped");
    for k in 0..cam.frame_count() {
        pipe.write_all(&cam.frame(k))
            .map_err(|e| format!("ffmpeg: {e}"))?;
    }
    drop(pipe);
    let status = child.wait().map_err(|e| e.to_string())?;
    status.success().then_some(()).ok_or("ffmpeg failed".into())
}

pub fn synth(args: &[String]) -> Result<(), String> {
    let mut scene = Scene::default();
    let mut out = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--latency-ms" => scene.latency = number(args, &mut i, "--latency-ms")? / 1000.0,
            "--seconds" => scene.seconds = number(args, &mut i, "--seconds")?,
            a => out = Some(PathBuf::from(a)),
        }
        i += 1;
    }
    let out = out.ok_or("where to? luch-latency synth OUT.mp4")?;
    let cam = Camera::new(scene);
    encode(&cam, &out)?;
    let truth: Vec<f64> = cam.truth().iter().map(|t| t.1 * 1000.0).collect();
    let s = analyze::Summary::of(&truth);
    println!(
        "{}: true latency p50 {:.1} ms, p95 {:.1} ms",
        out.display(),
        s.p50,
        s.p95
    );
    Ok(())
}

/// The patterns in one picture and what their strips read.
pub fn scan(image: &Path) -> Result<(), String> {
    let src = Video::open(image, Some(1.0)).map_err(|e| e.to_string())?;
    let mut frames = src.play().map_err(|e| e.to_string())?;
    let f = frames
        .next()
        .ok_or("no picture")?
        .map_err(|e| e.to_string())?;
    let found = analyze::find(&f);
    if found.is_empty() {
        return Err("no Luch pattern in this picture".into());
    }
    for p in found {
        let n = crate::geom::Homography::from_unit_square(p.corners)
            .and_then(|h| analyze::read_strip(&f, &h));
        let who = match p.meta.role {
            pattern::Role::Sender => "computer",
            pattern::Role::Receiver => "TV",
        };
        let at = p.corners[0];
        println!(
            "{who} at ({:.0}, {:.0}): counter {} · {}",
            at.x,
            at.y,
            n.map_or("unreadable".into(), |r| r.n.to_string()),
            p.meta.to_text()
        );
    }
    Ok(())
}
