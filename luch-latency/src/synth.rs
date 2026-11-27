//! A synthetic camera filming a laptop and a TV, for proving the analyser without devices.
//!
//! The laptop shows the sender's pattern, its counter going up every vsync. The TV shows the same picture `latency`
//! later (plus jitter, dropped frames, its own refresh and scanout), with the receiver's own pattern over its bottom
//! right corner. The camera has a rolling shutter, an exposure that blurs changes, a perspective view of each screen,
//! optical blur and noise. Panels can have a slow response. Since every time here is known, so is the true
//! photon-to-photon latency of every frame: `truth`.

use crate::geom::{pt, Homography, Pt};
use crate::pattern::{self, Meta, Qr, Spot};

/// Screens are 16:9; screen coordinates are in screen heights, (0,0) the top-left corner.
const ASPECT: f64 = 16.0 / 9.0;

#[derive(Clone, Debug)]
pub struct Scene {
    pub width: usize,
    pub height: usize,
    pub fps: f64,
    /// Exposure as a fraction of the frame time: 1.0 is a 1/240 s shutter at 240 fps.
    pub exposure: f64,
    pub seconds: f64,
    /// Screen corners in the camera image: top-left, top-right, bottom-right, bottom-left.
    pub laptop: [Pt; 4],
    pub tv: [Pt; 4],
    pub sender_hz: f64,
    pub tv_hz: f64,
    /// When the TV's refresh starts, relative to the laptop's.
    pub tv_phase: f64,
    /// From the laptop showing a frame (its top row) to the TV having it, in seconds.
    pub latency: f64,
    /// Each frame's latency varies by up to this much either way.
    pub jitter: f64,
    /// The share of frames that never reach the TV.
    pub drop: f64,
    /// false: the TV flips buffers mid-scanout, so a refresh shows the top of one frame and the bottom of another.
    pub vsync: bool,
    /// Rolling shutter: from the camera's top row to its bottom row, in seconds.
    pub readout: f64,
    /// Optical blur (Gaussian sigma) and sensor noise (sigma, of 255), in camera pixels.
    pub blur: f64,
    pub noise: f64,
    /// Panel response time constant, in seconds.
    pub response: f64,
    /// The receiver draws its own pattern; its counter trails the refresh by `overlay_lag` refreshes.
    pub overlay: bool,
    pub overlay_lag: u32,
    pub seed: u64,
    pub sender_meta: Meta,
    pub receiver_meta: Meta,
}

impl Default for Scene {
    fn default() -> Scene {
        Scene {
            width: 1280,
            height: 720,
            fps: 240.0,
            exposure: 1.0,
            seconds: 1.5,
            // Side by side, at the same height, each looked at a little from the side.
            laptop: [
                pt(30.0, 170.0),
                pt(610.0, 150.0),
                pt(604.0, 482.0),
                pt(36.0, 476.0),
            ],
            tv: [
                pt(640.0, 140.0),
                pt(1255.0, 160.0),
                pt(1248.0, 510.0),
                pt(632.0, 498.0),
            ],
            sender_hz: 60.0,
            tv_hz: 60.0,
            tv_phase: 0.0047,
            latency: 0.050,
            jitter: 0.0,
            drop: 0.0,
            vsync: true,
            readout: 0.0,
            blur: 0.8,
            noise: 3.0,
            response: 0.002,
            overlay: true,
            overlay_lag: 1,
            seed: 1,
            sender_meta: Meta::new(pattern::Role::Sender)
                .with("v", "synthetic")
                .with("m", "fast")
                .with("e", "11.0")
                .with("hz", "60.00"),
            receiver_meta: Meta::new(pattern::Role::Receiver)
                .with("mo", "Synthetic TV")
                .with("hz", "60.00"),
        }
    }
}

/// What a synthetic camera frame is made of, for one sub-pixel that changes with time.
#[derive(Clone, Copy)]
struct Live {
    tv: bool,
    overlay: bool,
    /// Screen row, in screen heights.
    y: f32,
    cell: u8,
}

/// Which screen and which layer a sub-pixel shows.
enum Seen {
    Background(f32),
    Fixed(f32),
    Live(Live),
}

#[derive(Clone, Copy)]
struct Panel {
    black: f32,
    white: f32,
}

const LAPTOP: Panel = Panel {
    black: 18.0,
    white: 232.0,
};
const TV: Panel = Panel {
    black: 38.0,
    white: 196.0,
};

pub struct Camera {
    scene: Scene,
    /// Frames the TV got: (time it has it, counter), in time order.
    arrivals: Vec<(f64, i64)>,
    /// Per pixel: the sum of its fixed sub-pixels, and its live sub-pixels in `live[start..end]`.
    fixed: Vec<f32>,
    spans: Vec<(u32, u32)>,
    live: Vec<Live>,
}

/// Frames sent before the camera starts.
const PRE: i64 = 120;
/// What the counters show at time 0: they wrap around 4096 within the first second.
const COUNTER_START: i64 = 4040;

/// Two by two sub-pixels per camera pixel.
const SUB: [(f64, f64); 4] = [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)];
/// Moments sampled within one exposure.
const MOMENTS: usize = 6;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    fn gauss(&mut self) -> f64 {
        let (u, v) = (self.next().max(1e-12), self.next());
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }
}

impl Camera {
    pub fn new(scene: Scene) -> Camera {
        let mut rng = Rng(scene.seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let period = 1.0 / scene.sender_hz;
        // The stream runs from well before the camera starts: the TV is never empty in the picture.
        let frames = ((scene.seconds + 1.0) * scene.sender_hz) as i64 + 4;
        let mut arrivals = Vec::new();
        let mut last = f64::MIN;
        for n in -PRE..frames {
            let jitter = (rng.next() * 2.0 - 1.0) * scene.jitter;
            let dropped = rng.next() < scene.drop;
            let at = (n as f64 * period + scene.latency + jitter).max(last + 1e-6);
            if !dropped {
                arrivals.push((at, n));
                last = at;
            }
        }

        let sender_qr = Qr::encode(&scene.sender_meta.to_text());
        let receiver_qr = Qr::encode(&scene.receiver_meta.to_text());
        let screen = [pt(0.0, 0.0), pt(ASPECT, 0.0), pt(ASPECT, 1.0), pt(0.0, 1.0)];
        let to_laptop = Homography::new(scene.laptop, screen).expect("laptop quad");
        let to_tv = Homography::new(scene.tv, screen).expect("tv quad");
        let (so, ro) = (pattern::sender_origin(), pattern::receiver_origin(ASPECT));
        let (ss, rs) = (pattern::SENDER_SIDE, pattern::RECEIVER_SIDE);

        let seen = |p: Pt| -> Seen {
            for (tv, h, panel) in [(false, &to_laptop, LAPTOP), (true, &to_tv, TV)] {
                let s = h.map(p);
                if !(0.0..ASPECT).contains(&s.x) || !(0.0..1.0).contains(&s.y) {
                    continue;
                }
                let level = |dark: bool| if dark { panel.black } else { panel.white };
                if tv && scene.overlay {
                    let spot = pattern::spot_at(&receiver_qr, (s.x - ro.0) / rs, (s.y - ro.1) / rs);
                    match spot {
                        Spot::Outside => {}
                        Spot::Fixed(d) => return Seen::Fixed(level(d)),
                        Spot::Bit(cell) => {
                            return Seen::Live(Live {
                                tv,
                                overlay: true,
                                y: s.y as f32,
                                cell,
                            })
                        }
                    }
                }
                return match pattern::spot_at(&sender_qr, (s.x - so.0) / ss, (s.y - so.1) / ss) {
                    Spot::Outside => Seen::Fixed(panel.black),
                    Spot::Fixed(d) => Seen::Fixed(level(d)),
                    Spot::Bit(cell) => Seen::Live(Live {
                        tv,
                        overlay: false,
                        y: s.y as f32,
                        cell,
                    }),
                };
            }
            // A dim room with a little texture, so that thresholds have something to get wrong.
            Seen::Background((60.0 + 25.0 * ((p.x * 0.05).sin() * (p.y * 0.07).cos())) as f32)
        };

        let (w, h) = (scene.width, scene.height);
        let mut fixed = vec![0.0f32; w * h];
        let mut spans = vec![(0u32, 0u32); w * h];
        let mut live = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let start = live.len() as u32;
                for (dx, dy) in SUB {
                    match seen(pt(x as f64 + dx, y as f64 + dy)) {
                        Seen::Background(v) | Seen::Fixed(v) => fixed[y * w + x] += v,
                        Seen::Live(l) => live.push(l),
                    }
                }
                spans[y * w + x] = (start, live.len() as u32);
            }
        }
        Camera {
            scene,
            arrivals,
            fixed,
            spans,
            live,
        }
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    pub fn frame_count(&self) -> usize {
        (self.scene.seconds * self.scene.fps) as usize
    }

    /// The newest frame the TV has at time `t`.
    fn newest(&self, t: f64) -> Option<i64> {
        let i = self.arrivals.partition_point(|a| a.0 <= t);
        (i > 0).then(|| self.arrivals[i - 1].1)
    }

    /// What screen row `y` of a layer shows at `t`: the counter now, the one before and how long ago it changed.
    fn shown(&self, l: &Live, t: f64) -> Option<(i64, i64, f64)> {
        let y = l.y as f64;
        if !l.tv {
            let p = 1.0 / self.scene.sender_hz;
            let k = ((t - y * p) / p).floor();
            return Some((k as i64, k as i64 - 1, t - (k * p + y * p)));
        }
        let p = 1.0 / self.scene.tv_hz;
        let j = ((t - self.scene.tv_phase - y * p) / p).floor();
        let scan = j * p + self.scene.tv_phase + y * p; // when this row was last refreshed
        if l.overlay {
            let k = j as i64 - self.scene.overlay_lag as i64;
            return Some((k, k - 1, t - scan));
        }
        let latch = if self.scene.vsync { scan - y * p } else { scan };
        let cur = self.newest(latch)?;
        let prev = self.newest(latch - p).unwrap_or(cur);
        Some((cur, prev, t - scan))
    }

    fn value(&self, l: &Live, t: f64) -> f32 {
        let panel = if l.tv { TV } else { LAPTOP };
        let level = |n: i64| {
            if pattern::cell_dark(
                l.cell as usize,
                (n + COUNTER_START).rem_euclid(pattern::MODULUS as i64) as u32,
            ) {
                panel.black
            } else {
                panel.white
            }
        };
        let Some((cur, prev, since)) = self.shown(l, t) else {
            return panel.black;
        };
        let (a, b) = (level(prev), level(cur));
        if self.scene.response <= 0.0 || a == b {
            return b;
        }
        let k = 1.0 - (-since / self.scene.response).exp();
        a + (b - a) * k as f32
    }

    /// Camera frame `k`: grey, row-major.
    pub fn frame(&self, k: usize) -> Vec<u8> {
        let s = &self.scene;
        let (w, h) = (s.width, s.height);
        let t0 = k as f64 / s.fps;
        let exposure = s.exposure / s.fps;
        let mut img = vec![0.0f32; w * h];
        std::thread::scope(|scope| {
            let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
            let rows_per = h.div_ceil(threads);
            for (band, chunk) in img.chunks_mut(rows_per * w).enumerate() {
                scope.spawn(move || {
                    for (i, out) in chunk.iter_mut().enumerate() {
                        let (x, y) = (i % w, band * rows_per + i / w);
                        let p = y * w + x;
                        let (a, b) = self.spans[p];
                        let start = t0 + y as f64 / h as f64 * s.readout;
                        let mut v = self.fixed[p];
                        for l in &self.live[a as usize..b as usize] {
                            let mut sum = 0.0;
                            for m in 0..MOMENTS {
                                let t = start + exposure * (m as f64 + 0.5) / MOMENTS as f64;
                                sum += self.value(l, t);
                            }
                            v += sum / MOMENTS as f32;
                        }
                        *out = v / SUB.len() as f32;
                    }
                });
            }
        });
        let img = blur(&img, w, h, s.blur);
        let mut rng = Rng(s.seed ^ (k as u64 + 1).wrapping_mul(0xD1B5_4A32_D192_ED03) | 1);
        img.iter()
            .map(|v| {
                (v + (rng.gauss() * s.noise) as f32)
                    .round()
                    .clamp(0.0, 255.0) as u8
            })
            .collect()
    }

    /// The true photon-to-photon latency of each frame the TV showed, measured at the middle of the strip:
    /// (counter, seconds from the laptop showing it to the TV showing it).
    pub fn truth(&self) -> Vec<(i64, f64)> {
        let s = &self.scene;
        // Between the two rows of cells.
        let y = pattern::sender_origin().1 + pattern::BETWEEN_ROWS * pattern::SENDER_SIDE;
        let (ps, pt_) = (1.0 / s.sender_hz, 1.0 / s.tv_hz);
        let mut out: Vec<(i64, f64)> = Vec::new();
        // From well before the video, so that each frame's first appearance is its own; kept are the frames both
        // screens showed within the video.
        let mut j = (-1.0 / pt_).floor();
        loop {
            let scan = j * pt_ + s.tv_phase + y * pt_;
            if scan > s.seconds {
                break;
            }
            let latch = if s.vsync { scan - y * pt_ } else { scan };
            if let Some(n) = self.newest(latch) {
                if out.last().is_none_or(|l| l.0 != n) {
                    out.push((n, scan - (n as f64 * ps + y * ps)));
                }
            }
            j += 1.0;
        }
        out.retain(|&(n, _)| n as f64 * ps + y * ps >= 0.0);
        out
    }
}

fn blur(img: &[f32], w: usize, h: usize, sigma: f64) -> Vec<f32> {
    if sigma <= 0.0 {
        return img.to_vec();
    }
    let r = (sigma * 3.0).ceil() as isize;
    let k: Vec<f32> = (-r..=r)
        .map(|i| (-(i * i) as f64 / (2.0 * sigma * sigma)).exp() as f32)
        .collect();
    let sum: f32 = k.iter().sum();
    let k: Vec<f32> = k.iter().map(|v| v / sum).collect();
    let pass = |src: &[f32], horizontal: bool| -> Vec<f32> {
        let mut out = vec![0.0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut acc = 0.0;
                for (i, kv) in k.iter().enumerate() {
                    let d = i as isize - r;
                    let (sx, sy) = if horizontal {
                        ((x as isize + d).clamp(0, w as isize - 1) as usize, y)
                    } else {
                        (x, (y as isize + d).clamp(0, h as isize - 1) as usize)
                    };
                    acc += src[sy * w + sx] * kv;
                }
                out[y * w + x] = acc;
            }
        }
        out
    };
    pass(&pass(img, true), false)
}
