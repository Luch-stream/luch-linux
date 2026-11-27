//! From a camera's video of both screens to the photon-to-photon latency of every frame.
//!
//! 1. Find the patterns: QR codes saying `LUCH1;S` (the computer's, seen twice: on its own screen and mirrored on
//!    the TV) and `LUCH1;R` (the TV's own). Their corners place every strip cell in the image.
//! 2. Read every frame's strips: each a 12-bit Gray-code counter, judged against the white and black of the same
//!    spot in the same frame.
//! 3. Unwrap the counters into series, drop single misreadings, and note when each number first appeared.
//! 4. The copy that is ahead is the computer's screen. Its counter is a vsync count, so it fits a straight line
//!    against camera time; that line says when the computer showed any frame, to a fraction of a camera frame.
//! 5. Latency of frame N = when the TV first showed N − when the computer showed N.

use std::collections::HashMap;
use std::io;

use crate::geom::{pt, Homography, Pt};
use crate::pattern::{self, Meta, Role};
use crate::video::{Frame, Source};

#[derive(Clone, Debug)]
pub struct Options {
    /// The camera's rolling shutter, top row to bottom row. Zero when unknown: then both screens should be at the
    /// same height in the picture, or the difference of their heights costs up to this much.
    pub readout_ms: f64,
    /// How many frames to look for QR codes in, spread over the video.
    pub detect_frames: usize,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            readout_ms: 0.0,
            detect_frames: 60,
        }
    }
}

/// A pattern found in the picture: who drew it and where its QR code's corners are.
#[derive(Clone, Debug)]
pub struct Found {
    pub meta: Meta,
    pub corners: [Pt; 4],
}

/// Every Luch pattern whose QR code reads in this frame.
pub fn find(f: &Frame) -> Vec<Found> {
    use rxing::multi::MultipleBarcodeReader;
    let Ok(src) = rxing::Luma8LuminanceSource::new(f.px.clone(), f.width as u32, f.height as u32)
    else {
        return Vec::new();
    };
    let mut bitmap = rxing::BinaryBitmap::new(rxing::common::HybridBinarizer::new(src));
    let hints = rxing::DecodeHints::default().with(rxing::DecodeHintValue::TryHarder(true));
    let found = rxing::qrcode::cpp_port::QrReader
        .decode_multiple_with_hints(&mut bitmap, &hints)
        .unwrap_or_default();
    found
        .iter()
        .filter_map(|r| {
            let meta = Meta::parse(r.getText())?;
            // The symbol's corners as the code sees itself: top-left, top-right, bottom-left, bottom-right.
            let p = r.getPoints();
            if p.len() < 4 {
                return None;
            }
            let c = |i: usize| pt(p[i].x as f64, p[i].y as f64);
            Some(Found {
                meta,
                corners: [c(0), c(1), c(3), c(2)],
            })
        })
        .collect()
}

/// Frames averaged for finding the patterns.
const AVERAGE: usize = 4;

/// A frame twice as big, smoothly: finds QR codes whose modules are only two or three pixels.
fn enlarged(f: &Frame) -> Frame {
    let (w, h) = (f.width * 2, f.height * 2);
    let mut px = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            px[y * w + x] = sample(f, pt(x as f64 / 2.0 + 0.25, y as f64 / 2.0 + 0.25))
                .unwrap_or_else(|| {
                    f.px[(y / 2).min(f.height - 1) * f.width + (x / 2).min(f.width - 1)] as f32
                }) as u8;
        }
    }
    Frame {
        t: f.t,
        width: w,
        height: h,
        px,
    }
}

/// One pattern followed through the video.
struct Track {
    role: Role,
    meta: Meta,
    /// (frame index, where its QR code was then)
    seen: Vec<(usize, Homography)>,
    centre: Pt,
    side: f64,
    /// Its counter in each frame, when it could be read.
    reads: Vec<Option<Reading>>,
    /// The middle of its strip, as a share of the image height (rolling shutter).
    row: f64,
}

fn centre(c: &[Pt; 4]) -> Pt {
    pt(
        c.iter().map(|p| p.x).sum::<f64>() / 4.0,
        c.iter().map(|p| p.y).sum::<f64>() / 4.0,
    )
}

fn dist(a: Pt, b: Pt) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

fn sample(f: &Frame, p: Pt) -> Option<f32> {
    let (x, y) = (p.x - 0.5, p.y - 0.5);
    if x < 0.0 || y < 0.0 || x >= (f.width - 1) as f64 || y >= (f.height - 1) as f64 {
        return None;
    }
    let (x0, y0) = (x as usize, y as usize);
    let (fx, fy) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
    let at = |x: usize, y: usize| f.px[y * f.width + x] as f32;
    let top = at(x0, y0) * (1.0 - fx) + at(x0 + 1, y0) * fx;
    let bottom = at(x0, y0 + 1) * (1.0 - fx) + at(x0 + 1, y0 + 1) * fx;
    Some(top * (1.0 - fy) + bottom * fy)
}

fn mean(f: &Frame, h: &Homography, pts: &[(f64, f64)]) -> Option<f32> {
    let mut s = 0.0;
    for &(x, y) in pts {
        s += sample(f, h.map(pt(x, y)))?;
    }
    Some(s / pts.len() as f32)
}

/// A pattern's counter in one frame, and how dark each of its cells was (0 white, 1 black): a cell caught half-way
/// tells when, within the frame, the screen changed.
#[derive(Clone, Copy, Debug)]
pub struct Reading {
    pub n: u32,
    pub dark: [f32; pattern::CELLS],
}

/// The counter in one frame, or None when the cells cannot be trusted: too little contrast (covered, out of the
/// picture), or more than the one changing cell half-way.
pub fn read_strip(f: &Frame, h: &Homography) -> Option<Reading> {
    // A thin band across the middle of each cell: a tear line rarely passes through it.
    let band = |i: usize| {
        let c = pattern::cell(i);
        let (w, mid) = (c[2] - c[0], (c[1] + c[3]) / 2.0);
        let mut v = Vec::with_capacity(9);
        for fx in [0.3, 0.5, 0.7] {
            for dy in [-0.04, 0.0, 0.04] {
                v.push((c[0] + w * fx, mid + dy));
            }
        }
        v
    };
    let black = mean(f, h, &band(1))?;
    let mut cells = [0.0f32; pattern::CELLS];
    let mut code = 0u32;
    let mut unsure = 0;
    for (i, cell) in cells.iter_mut().enumerate() {
        // The white above and below each cell is its own reference: the light across a screen is never even.
        let white = mean(f, h, &pattern::white_refs(i))?;
        if white - black < 12.0 {
            return None;
        }
        let dark = (white - mean(f, h, &band(i))?) / (white - black);
        *cell = dark;
        match i {
            0 if dark > 0.3 => return None, // the white reference is not white: something is in the way
            0 | 1 => continue,
            _ => {}
        }
        if (dark - 0.5).abs() < 0.2 {
            unsure += 1;
        }
        code = code << 1 | (dark > 0.5) as u32;
    }
    (unsure <= 1).then(|| Reading {
        n: pattern::from_gray(code),
        dark: cells,
    })
}

/// How far `a` is ahead of `b` on the 12-bit wheel, between -2048 and 2047.
fn ahead(a: u32, b: u32) -> i64 {
    let m = pattern::MODULUS as i64;
    let d = (a as i64 - b as i64).rem_euclid(m);
    if d >= m / 2 {
        d - m
    } else {
        d
    }
}

/// Readings that go with their neighbours: a counter of a frame misread now and then (a reflection, a hand) is
/// dropped instead of becoming a latency. Items are (frame, counter).
fn despike(s: Vec<(usize, i64)>) -> Vec<(usize, i64)> {
    let mut out = Vec::with_capacity(s.len());
    for i in 0..s.len() {
        let lo = i.saturating_sub(4);
        let hi = (i + 5).min(s.len());
        let mut w: Vec<i64> = s[lo..hi].iter().map(|p| p.1).collect();
        w.sort_unstable();
        if (s[i].1 - w[w.len() / 2]).abs() <= 3 {
            out.push(s[i]);
        }
    }
    out
}

/// When the screen went from `n - 1` (frame at `tp`) to `n` (frame at `tc`): where the one cell that differs crossed
/// half-way, between the two frames. With an exposure as long as the frame time, a frame caught mid-change says it
/// to a fraction of a frame; otherwise this is the middle between the two.
fn crossing(prev: &Reading, cur: &Reading, n: i64, tp: f64, tc: f64) -> f64 {
    let m = pattern::MODULUS as i64;
    let (old, new) = ((n - 1).rem_euclid(m) as u32, n.rem_euclid(m) as u32);
    let Some(c) =
        (2..pattern::CELLS).find(|&c| pattern::cell_dark(c, old) != pattern::cell_dark(c, new))
    else {
        return (tp + tc) / 2.0;
    };
    let towards = |d: f32| {
        if pattern::cell_dark(c, new) {
            d
        } else {
            1.0 - d
        }
    };
    let (a, b) = (towards(prev.dark[c]), towards(cur.dark[c]));
    if b - a < 0.2 {
        return (tp + tc) / 2.0;
    }
    tp + ((0.5 - a) / (b - a)).clamp(0.0, 1.0) as f64 * (tc - tp)
}

/// A pattern's counter through the video: unwrapped by `unwrap(time, 12 bits, last (12 bits, unwrapped))`, cleaned,
/// and when each number first appeared: (counter, time). A number seen after a bigger one is a misreading or a blend
/// and is ignored.
fn follow(
    reads: &[Option<Reading>],
    times: &[f64],
    unwrap: &dyn Fn(f64, u32, Option<(u32, i64)>) -> i64,
) -> Vec<(i64, f64)> {
    let mut series = Vec::new();
    let mut last = None;
    for (k, r) in reads.iter().enumerate() {
        let Some(r) = r else { continue };
        let n = unwrap(times[k], r.n, last);
        last = Some((r.n, n));
        series.push((k, n));
    }
    let series = despike(series);
    let mut out: Vec<(i64, f64)> = Vec::new();
    for (i, &(k, n)) in series.iter().enumerate() {
        if out.last().is_some_and(|l| n <= l.0) {
            continue;
        }
        let t = match i.checked_sub(1).map(|j| series[j]) {
            Some((kp, np)) if kp + 1 == k && np + 1 == n => crossing(
                reads[kp].as_ref().unwrap(),
                reads[k].as_ref().unwrap(),
                n,
                times[kp],
                times[k],
            ),
            _ if k > 0 => (times[k - 1] + times[k]) / 2.0,
            _ => times[k],
        };
        out.push((n, t));
    }
    out
}

/// Unwraps a counter that only goes forward, from its own last reading.
fn in_order(_: f64, r: u32, last: Option<(u32, i64)>) -> i64 {
    match last {
        None => r as i64,
        Some((lr, ln)) => ln + ahead(r, lr),
    }
}

/// t = a + b·n, robustly: the slope is the median of the slopes between points half the series apart (Theil–Sen),
/// so that a page started late or a stretch of misreadings cannot tilt it; then least squares over the points within
/// `tol` of that line, for precision.
fn fit(points: &[(i64, f64)], tol: f64) -> Option<(f64, f64)> {
    let half = points.len() / 2;
    let mut slopes: Vec<f64> = (0..half)
        .filter_map(|i| {
            let (p, q) = (points[i], points[i + half]);
            (q.0 != p.0).then(|| (q.1 - p.1) / (q.0 - p.0) as f64)
        })
        .collect();
    if slopes.is_empty() {
        return None;
    }
    slopes.sort_by(f64::total_cmp);
    let b = slopes[slopes.len() / 2];
    let mut cuts: Vec<f64> = points.iter().map(|p| p.1 - b * p.0 as f64).collect();
    cuts.sort_by(f64::total_cmp);
    let a = cuts[cuts.len() / 2];
    let kept: Vec<&(i64, f64)> = points
        .iter()
        .filter(|p| (p.1 - (a + b * p.0 as f64)).abs() <= tol)
        .collect();
    let n = kept.len() as f64;
    if n < 2.0 {
        return Some((a, b));
    }
    let mx = kept.iter().map(|q| q.0 as f64).sum::<f64>() / n;
    let my = kept.iter().map(|q| q.1).sum::<f64>() / n;
    let sxx: f64 = kept.iter().map(|q| (q.0 as f64 - mx).powi(2)).sum();
    let sxy: f64 = kept.iter().map(|q| (q.0 as f64 - mx) * (q.1 - my)).sum();
    if sxx <= 0.0 {
        return Some((a, b));
    }
    let b = sxy / sxx;
    Some((my - b * mx, b))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub count: usize,
    pub min: f64,
    pub p05: f64,
    pub p50: f64,
    pub p95: f64,
    pub max: f64,
    pub mean: f64,
    /// Standard deviation: how much one frame's latency differs from another's.
    pub jitter: f64,
}

impl Summary {
    pub fn of(values: &[f64]) -> Summary {
        if values.is_empty() {
            return Summary::default();
        }
        let mut v = values.to_vec();
        v.sort_by(f64::total_cmp);
        let q = |p: f64| {
            let x = p * (v.len() - 1) as f64;
            let (i, f) = (x.floor() as usize, x.fract());
            v[i] + (v[(i + 1).min(v.len() - 1)] - v[i]) * f
        };
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / v.len() as f64;
        Summary {
            count: v.len(),
            min: v[0],
            p05: q(0.05),
            p50: q(0.5),
            p95: q(0.95),
            max: v[v.len() - 1],
            mean,
            jitter: var.sqrt(),
        }
    }
}

/// Everything the analysis found; `report` turns it into JSON and Markdown.
#[derive(Clone, Debug, Default)]
pub struct Analysis {
    pub frames: usize,
    pub width: usize,
    pub height: usize,
    /// The camera's frames per second, by the video's clock.
    pub camera_fps: f64,
    /// The video's clock against the computer's measured refresh: 1 for a real-time video, 8 for 240 fps slowed down
    /// to 30 (all times here are already corrected by it).
    pub time_scale: f64,
    pub sender: Option<Meta>,
    pub receiver: Option<Meta>,
    /// Photon-to-photon latency per frame shown on the TV, in milliseconds: (counter, ms).
    pub samples: Vec<(i64, f64)>,
    pub latency: Summary,
    /// The computer's refresh as the camera saw its counter go.
    pub sender_hz: f64,
    /// Of the frames the computer showed while the TV was followed, the share the TV showed too.
    pub tv_frames_shown: f64,
    /// The TV's own pattern: its refresh as the camera saw it, the share of refreshes it skipped, and how unevenly
    /// its frames came (standard deviation from a steady beat, ms; at least the camera's frame time / √12).
    pub tv_hz: Option<f64>,
    pub tv_overlay_skipped: Option<f64>,
    pub tv_pacing_ms: Option<f64>,
    /// Share of frames in which each pattern's strip could be read.
    pub readable: Vec<(String, f64)>,
    pub warnings: Vec<String>,
}

pub fn analyze(src: &dyn Source, opt: &Options) -> io::Result<Analysis> {
    let total = src.len();
    let stride = (total / opt.detect_frames.max(1)).max(1);
    let mut tracks: Vec<Track> = Vec::new();
    let mut times = Vec::with_capacity(total);
    let (mut width, mut height) = (0, 0);

    // Pass 1: where are the patterns? Looked for in the average of a few frames in a row (the camera stands still;
    // the noise of a 240 fps sensor goes), and in that average enlarged when the first look misses one.
    let mut sum: Vec<u32> = Vec::new();
    for (k, f) in src.play()?.enumerate() {
        let f = f?;
        times.push(f.t);
        (width, height) = (f.width, f.height);
        let phase = k % stride;
        if phase >= AVERAGE {
            continue;
        }
        if phase == 0 {
            sum = vec![0; f.px.len()];
        }
        sum.iter_mut().zip(&f.px).for_each(|(s, p)| *s += *p as u32);
        if phase + 1 < AVERAGE.min(stride) && k + 1 < total {
            continue;
        }
        let n = phase as u32 + 1;
        let avg = Frame {
            t: f.t,
            width,
            height,
            px: sum.iter().map(|s| (s / n) as u8).collect(),
        };
        let mut found = find(&avg);
        if found.len() < 3 {
            let more = find(&enlarged(&avg));
            for m in more {
                let c = m.corners.map(|p| pt(p.x / 2.0, p.y / 2.0));
                if !found
                    .iter()
                    .any(|f| dist(centre(&f.corners), centre(&c)) < dist(c[0], c[1]) / 2.0)
                {
                    found.push(Found {
                        meta: m.meta,
                        corners: c,
                    });
                }
            }
        }
        for found in found {
            let Some(h) = Homography::from_unit_square(found.corners) else {
                continue;
            };
            let c = centre(&found.corners);
            let side = dist(found.corners[0], found.corners[1])
                .max(dist(found.corners[0], found.corners[3]));
            let same = tracks
                .iter_mut()
                .find(|t| t.role == found.meta.role && dist(t.centre, c) < t.side.max(side) / 2.0);
            match same {
                Some(t) => {
                    t.seen.push((k, h));
                    t.meta = found.meta;
                }
                None => tracks.push(Track {
                    role: found.meta.role,
                    meta: found.meta,
                    seen: vec![(k, h)],
                    centre: c,
                    side,
                    reads: Vec::new(),
                    row: 0.0,
                }),
            }
        }
    }
    let frames = times.len();
    let mut a = Analysis {
        frames,
        width,
        height,
        time_scale: 1.0,
        ..Default::default()
    };
    if frames < 2 {
        return Err(io::Error::other("the video has fewer than two frames"));
    }
    let mut gaps: Vec<f64> = times
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|d| *d > 0.0)
        .collect();
    gaps.sort_by(f64::total_cmp);
    let dt = gaps.get(gaps.len() / 2).copied().unwrap_or(1.0 / 240.0);
    a.camera_fps = 1.0 / dt;

    // Pass 2: every frame's counters, each pattern placed by its nearest sighting in time.
    for t in &mut tracks {
        let mid = pt(
            pattern::PER_ROW as f64 * pattern::CELL_W / 2.0,
            pattern::BETWEEN_ROWS,
        );
        t.row = t.seen[0].1.map(mid).y / height as f64;
        t.reads = Vec::with_capacity(frames);
    }
    for (k, f) in src.play()?.enumerate() {
        let f = f?;
        for t in &mut tracks {
            let i = t.seen.partition_point(|s| s.0 <= k);
            let near = [i.saturating_sub(1), i.min(t.seen.len() - 1)]
                .into_iter()
                .min_by_key(|&j| t.seen[j].0.abs_diff(k))
                .unwrap();
            t.reads.push(read_strip(&f, &t.seen[near].1));
        }
    }
    for t in &tracks {
        let ok = t.reads.iter().filter(|r| r.is_some()).count() as f64 / frames as f64;
        let name = match t.role {
            Role::Sender => "computer's pattern",
            Role::Receiver => "TV's own pattern",
        };
        a.readable.push((name.to_string(), ok));
    }

    // The sender's two copies: the one ahead is the computer's own screen. Keep the two best-read sender tracks.
    let mut senders: Vec<&Track> = tracks.iter().filter(|t| t.role == Role::Sender).collect();
    senders.sort_by_key(|t| std::cmp::Reverse(t.reads.iter().filter(|r| r.is_some()).count()));
    senders.truncate(2);
    let receiver = tracks
        .iter()
        .filter(|t| t.role == Role::Receiver)
        .max_by_key(|t| t.reads.iter().filter(|r| r.is_some()).count());
    a.receiver = receiver.map(|t| t.meta.clone());
    a.sender = senders.first().map(|t| t.meta.clone());
    if senders.len() < 2 {
        a.warnings.push(match senders.len() {
            0 => "No pattern of the computer found: is the latency lab page open full screen, and both screens in the picture?".into(),
            _ => "The computer's pattern was found only once: both the computer's screen and the TV must be in the picture, and the TV must show the mirrored page.".into(),
        });
        a.readable.retain(|r| r.1 > 0.0);
        return Ok(a);
    }
    let mut lead: Vec<i64> = (0..frames)
        .filter_map(|k| Some(ahead(senders[0].reads[k]?.n, senders[1].reads[k]?.n)))
        .collect();
    lead.sort_unstable();
    let (laptop, tv) = if lead.get(lead.len() / 2).copied().unwrap_or(0) >= 0 {
        (senders[0], senders[1])
    } else {
        (senders[1], senders[0])
    };
    let readout = opt.readout_ms / 1000.0;
    let times_of = |t: &Track| -> Vec<f64> { times.iter().map(|v| v + readout * t.row).collect() };

    // The computer: its counter in order, then the straight line of its vsyncs.
    let laptop_on = follow(&laptop.reads, &times_of(laptop), &in_order);
    let Some((la, lb)) = fit(&laptop_on, 1.5 * dt) else {
        a.warnings.push("The computer's counter could not be followed: is the page running (the number going up)?".into());
        return Ok(a);
    };
    a.sender_hz = 1.0 / lb;
    // A slowed-down export of a 240 fps video plays at 30 fps: the computer's own refresh tells.
    if let Some(hz) = laptop.meta.get("hz").and_then(|v| v.parse::<f64>().ok()) {
        let r = hz * lb;
        if (r - 1.0).abs() > 0.03 {
            a.time_scale = r;
            a.warnings.push(format!(
                "The video's clock runs {r:.2} times slower than the computer's refresh ({hz} Hz): \
                 a slowed-down copy? Times are corrected by it; the original file is better."
            ));
        }
    }
    let scale = a.time_scale;
    a.sender_hz *= scale;

    // The TV's copy: a number of the computer's recent past, unwrapped against the computer's line.
    let m = pattern::MODULUS as i64;
    let against_laptop = |t: f64, r: u32, _: Option<(u32, i64)>| {
        let now = ((t - la) / lb).floor() as i64 + 2;
        now - ahead(now.rem_euclid(m) as u32, r).rem_euclid(m)
    };
    let tv_on = follow(&tv.reads, &times_of(tv), &against_laptop);
    let seen_on_laptop: HashMap<i64, f64> = laptop_on.iter().copied().collect();
    let (first, last_n) = (laptop_on[0].0, laptop_on[laptop_on.len() - 1].0);
    for &(n, t_tv) in &tv_on {
        if n < first || n > last_n {
            continue;
        }
        // The line, unless the computer really showed this frame late (a missed vsync of its own compositor).
        let line = la + lb * n as f64;
        let shown = match seen_on_laptop.get(&n) {
            Some(&t) if t - line > 1.5 * dt => t,
            _ => line,
        };
        a.samples.push((n, (t_tv - shown) / scale * 1000.0));
    }
    a.latency = Summary::of(&a.samples.iter().map(|s| s.1).collect::<Vec<_>>());
    if let (Some(f), Some(l)) = (a.samples.first(), a.samples.last()) {
        let on_laptop = laptop_on
            .iter()
            .filter(|o| o.0 >= f.0 && o.0 <= l.0)
            .count();
        a.tv_frames_shown = a.samples.len() as f64 / on_laptop.max(1) as f64;
    }
    if a.samples.is_empty() {
        a.warnings.push("The TV's copy of the computer's pattern could not be read in time with the computer's.".into());
    }
    let early = a
        .samples
        .iter()
        .filter(|s| s.1 < -dt / scale * 1000.0)
        .count();
    if early * 20 > a.samples.len().max(1) {
        a.warnings.push("Some frames came out on the TV before the computer: the camera's rolling shutter? Put both screens at the same height in the picture, or pass --readout-ms.".into());
    }
    let rows = (laptop.row - tv.row).abs();
    if readout == 0.0 && rows > 0.15 {
        a.warnings.push(format!(
            "The two screens are {:.0}% of the picture apart in height: a rolling shutter adds up to that share of its readout \
             (about one camera frame) to the result. Put them side by side, or pass --readout-ms.",
            rows * 100.0
        ));
    }

    // The TV's own pattern: its refresh and how evenly it draws.
    if let Some(r) = receiver {
        let on = follow(&r.reads, &times_of(r), &in_order);
        if let Some((ra, rb)) = fit(&on, 1.5 * dt) {
            a.tv_hz = Some(scale / rb);
            let span = (on[on.len() - 1].0 - on[0].0) as f64;
            a.tv_overlay_skipped = Some(1.0 - (on.len() - 1) as f64 / span.max(1.0));
            let res: Vec<f64> = on
                .iter()
                .map(|o| (o.1 - (ra + rb * o.0 as f64)) / scale * 1000.0)
                .collect();
            a.tv_pacing_ms = Some(Summary::of(&res).jitter);
        }
    } else {
        a.warnings.push("The TV's own pattern was not found: is the receiver up to date? The latency is measured all the same.".into());
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_distance_is_signed() {
        assert_eq!(ahead(5, 3), 2);
        assert_eq!(ahead(3, 5), -2);
        assert_eq!(ahead(1, 4094), 3);
        assert_eq!(ahead(4094, 1), -3);
    }

    fn reading(n: u32) -> Reading {
        let mut dark = [0.0; pattern::CELLS];
        (0..pattern::CELLS).for_each(|c| dark[c] = pattern::cell_dark(c, n) as u8 as f32);
        Reading { n, dark }
    }

    #[test]
    fn spikes_go_and_onsets_ignore_going_back() {
        let reads: Vec<Option<Reading>> = [10, 10, 11, 900, 11, 12, 12, 11, 13, 13, 14]
            .iter()
            .map(|&n| Some(reading(n)))
            .collect();
        let times: Vec<f64> = (0..reads.len()).map(|k| k as f64).collect();
        let on = follow(&reads, &times, &in_order);
        assert_eq!(
            on.iter().map(|o| o.0).collect::<Vec<_>>(),
            vec![10, 11, 12, 13, 14]
        );
        // 12 first in frame 5, after 11 in frame 4: half-way between, as no frame caught the change.
        assert_eq!(on[2], (12, 4.5));
    }

    #[test]
    fn a_frame_caught_mid_change_places_it_within_the_frame() {
        let (prev, mut cur) = (reading(4095), reading(0));
        // From 4095 to 0 only the top bit's cell changes, from dark to light; seen 80% light in the new frame.
        let c = (2..pattern::CELLS)
            .find(|&c| pattern::cell_dark(c, 4095) != pattern::cell_dark(c, 0))
            .unwrap();
        assert_eq!(c, 2);
        let mut early = prev;
        early.dark[c] = 0.9;
        cur.dark[c] = 0.2;
        // 0.9 → 0.2 dark is 0.1 → 0.8 of the way to light: half-way at 4/7 of the frame.
        let t = crossing(&early, &cur, 4096, 1.0, 2.0);
        assert!((t - (1.0 + 4.0 / 7.0)).abs() < 1e-6, "{t}");
    }

    #[test]
    fn a_line_through_outliers() {
        let mut p: Vec<(i64, f64)> = (0..100).map(|n| (n, 0.5 + n as f64 / 60.0)).collect();
        p.insert(0, (-1400, 0.0));
        p[50].1 += 1.0;
        let (a, b) = fit(&p, 0.004).unwrap();
        assert!(
            (a - 0.5).abs() < 1e-9 && (b - 1.0 / 60.0).abs() < 1e-12,
            "{a} {b}"
        );
    }

    #[test]
    fn summary_percentiles() {
        let v: Vec<f64> = (0..=100).map(|i| i as f64).collect();
        let s = Summary::of(&v);
        assert_eq!((s.min, s.p50, s.p95, s.max), (0.0, 50.0, 95.0, 100.0));
    }
}
