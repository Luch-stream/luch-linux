//! Sync of several receivers (PROTOCOL.md 4k, docs/research/sync-multiroom.md): the clock packets this sender
//! answers, the playout delay D it chooses for a group, and the corrections it keeps per receiver.
//!
//! The receiver's side (the clock estimator, the playout) lives in the Android receiver; the policy here is the same
//! as its `SenderPolicy` and both run the same test vectors.

use serde_json::{json, Value};

pub const KIND_CLOCK: u8 = 3;
pub const F_TIMELINE: u8 = 8;
pub const F_REPLY: u8 = 16;
const HEADER: usize = 24;

/// The pts clock in µs (the pipeline's running time), None before it runs.
pub type Now = std::sync::Arc<dyn Fn() -> Option<u64> + Send + Sync>;

/// The reply to `pkt` when it is a clock request, t2 and t3 read from `now`; None for anything else.
pub fn answer(pkt: &[u8], now: &Now) -> Option<[u8; HEADER + 24]> {
    parse_request(pkt)?;
    let t2 = now()?;
    let t3 = now().unwrap_or(t2);
    Some(reply(pkt, t2, t3))
}

/// A clock request: the receiver's probe number and its send time t1 (its µs).
pub fn parse_request(b: &[u8]) -> Option<(u32, u64)> {
    if b.len() < HEADER + 8 || b[0] != KIND_CLOCK || b[1] & F_REPLY != 0 {
        return None;
    }
    let seq = u32::from_be_bytes(b[8..12].try_into().ok()?);
    let t1 = u64::from_be_bytes(b[HEADER..HEADER + 8].try_into().ok()?);
    Some((seq, t1))
}

/// The reply to a request: its header with the reply flag, then t1 as received, t2 (received) and t3 (sent), both on
/// the pts clock in µs.
pub fn reply(req: &[u8], t2: u64, t3: u64) -> [u8; HEADER + 24] {
    let mut r = [0u8; HEADER + 24];
    r[..HEADER].copy_from_slice(&req[..HEADER]);
    r[1] = F_REPLY;
    r[16..20].copy_from_slice(&24u32.to_be_bytes());
    r[HEADER..HEADER + 8].copy_from_slice(&req[HEADER..HEADER + 8]);
    r[HEADER + 8..HEADER + 16].copy_from_slice(&t2.to_be_bytes());
    r[HEADER + 16..].copy_from_slice(&t3.to_be_bytes());
    r
}

/// D in the header's unit (0.1 ms), clamped to the field.
pub fn header_units(ms: f64) -> u16 {
    (ms * 10.0).round().clamp(0.0, u16::MAX as f64) as u16
}

/// Whether a receiver's welcome says it plays at pts + D (older receivers get D = 0 and no group).
pub fn capable(welcome: &Value) -> bool {
    welcome["sync"]["v"].as_u64().is_some_and(|v| v >= 1) && welcome["sync"]["clock"] == true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Interactive,
    Watching,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "interactive" => Some(Mode::Interactive),
            "watch" | "watching" => Some(Mode::Watching),
            _ => None,
        }
    }

    pub fn wire(self) -> &'static str {
        match self {
            Mode::Interactive => "interactive",
            Mode::Watching => "watch",
        }
    }
}

/// How D is chosen for a group (docs: sync, 6.6). Watching: 10 ms above the slowest need, at least the floor, up at
/// once and down in one step after 30 s well below. Interactive: the picture at once, the sound at the later of its
/// own need and the picture; the picture waits only for a speaker beyond the tolerance, by the excess.
/// Watching: D is set this far above the slowest need, so that its small changes leave D where it is.
pub const HEADROOM_MS: f64 = 10.0;

pub struct Policy {
    mode: Mode,
    tolerance_ms: f64,
    floor_ms: f64,
    dv: f64,
    da: f64,
    low_since: Option<f64>,
    first: bool,
}

impl Policy {
    pub fn new(mode: Mode, tolerance_ms: f64, floor_ms: f64) -> Self {
        Policy {
            mode,
            tolerance_ms,
            floor_ms: if mode == Mode::Watching {
                floor_ms.max(0.0)
            } else {
                0.0
            },
            dv: 0.0,
            da: 0.0,
            low_since: None,
            first: true,
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// One second's reports: each picture receiver's pts → display, each receiver's need (watching: all; interactive:
    /// the sound receivers). Returns (D_v, D_a) in ms.
    pub fn update(&mut self, now_s: f64, video_pts_ms: &[f64], need_ms: &[f64]) -> (f64, f64) {
        let max = |v: &[f64]| v.iter().copied().filter(|x| x.is_finite()).reduce(f64::max);
        let (need, video) = (max(need_ms), max(video_pts_ms));
        match self.mode {
            Mode::Watching => {
                let least = need.map_or(self.floor_ms, |n| self.floor_ms.max(n + 2.0));
                let set = need.map_or(self.floor_ms, |n| self.floor_ms.max(n + HEADROOM_MS));
                let d = self.step(now_s, self.da, least, set, 0.0, 30.0, 10.0);
                self.dv = d;
                self.da = d;
            }
            Mode::Interactive => {
                let (need, video) = (need.unwrap_or(0.0), video.unwrap_or(0.0));
                let least = need.max(video);
                let d = self.step(now_s, self.da, least, least, 1.0, 10.0, 5.0);
                self.dv = if d > video + self.tolerance_ms {
                    d - self.tolerance_ms
                } else {
                    0.0
                };
                self.da = d;
            }
        }
        self.first = false;
        let r = |x: f64| (x * 10.0).round() / 10.0;
        (r(self.dv), r(self.da))
    }

    /// D moves seldom and in one step, since every change is a resync heard on every device at once: up to `set` as
    /// soon as `least` is beyond it (by more than `rise`); down to `set` once it has been more than `below` under D for
    /// `wait_s`. Falling by 1 ms a second instead made a resync every few seconds for minutes (the receivers slew at
    /// 0.1 %, no faster).
    #[allow(clippy::too_many_arguments)]
    fn step(
        &mut self,
        now: f64,
        d: f64,
        least: f64,
        set: f64,
        rise: f64,
        wait_s: f64,
        below: f64,
    ) -> f64 {
        if self.first || least > d + rise || (least > d && d == 0.0) {
            self.low_since = None;
            return set;
        }
        if set < d - below {
            let since = *self.low_since.get_or_insert(now);
            if now - since >= wait_s {
                self.low_since = None;
                return set;
            }
        } else {
            self.low_since = None;
        }
        d
    }
}

/// What a receiver of a group reported in its last stats, as the policy needs it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub need_ms: Option<f64>,
    pub video_pts_ms: Option<f64>,
    pub route: Option<String>,
    pub audio_ms: Option<f64>,
}

impl Report {
    pub fn from_stats(s: &Value) -> Report {
        Report {
            need_ms: s["need_ms"].as_f64(),
            video_pts_ms: s["out"]["video_pts_ms"].as_f64(),
            route: s["out"]["route"].as_str().map(str::to_string),
            audio_ms: s["out"]["audio_ms"].as_f64(),
        }
    }
}

/// The slider's starting point for a route the platform cannot see past (docs: sync, section 5): an HDMI ARC soundbar's
/// DSP, a Bluetooth speaker whose delay the platform does not report (its path then looks as short as a wire's).
pub fn route_preset_ms(route: &str, audio_ms: Option<f64>) -> f64 {
    match route {
        "hdmi_arc" => 40.0,
        "bluetooth" if audio_ms.is_some_and(|a| a < 100.0) => 150.0,
        _ => 0.0,
    }
}

/// Where a receiver's correction is kept: per receiver and output route (a TV with and without its soundbar).
pub fn offset_key(id: &str, route: &str) -> String {
    format!("{id}@{route}")
}

/// A member's part of `start`: the group block (PROTOCOL.md 4k).
pub fn group_block(
    id: &str,
    timeline: u32,
    role: &str,
    mode: Mode,
    tolerance_ms: f64,
    offset_ms: f64,
) -> Value {
    json!({"id": id, "timeline": timeline, "role": role, "mode": mode.wire(),
           "tolerance_ms": tolerance_ms, "offset_ms": offset_ms})
}

// The calibration's arithmetic is ready and tested; it is used once a microphone is wired to luchd (docs: sync, 6.7,
// the real-device plan): until then the receivers only take part in a dry run.

/// One receiver's acoustic calibration (docs: sync, 6.7): when it played each sweep and when the microphone heard it,
/// both on the sender's clock (µs), and whether the detector trusted each arrival.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct Heard {
    pub id: String,
    pub played: Vec<i64>,
    pub heard: Vec<Option<i64>>,
}

/// From the arrivals of a calibration, each receiver's new correction (ms, relative: all made equal to the slowest),
/// or None for a receiver not measured. A receiver counts only when every sweep was trusted and the arrivals agree
/// within 0.5 ms; the microphone's own capture latency is the same for all and cancels.
#[allow(dead_code)]
pub fn offsets_from_heard(all: &[Heard]) -> Vec<(String, Option<f64>)> {
    let delay = |h: &Heard| -> Option<f64> {
        if h.played.is_empty() || h.played.len() != h.heard.len() {
            return None;
        }
        let mut d: Vec<f64> = h
            .played
            .iter()
            .zip(&h.heard)
            .map(|(p, a)| a.map(|a| (a - p) as f64 / 1000.0))
            .collect::<Option<_>>()?;
        d.sort_by(f64::total_cmp);
        let n = d.len();
        let median = if n % 2 == 1 {
            d[n / 2]
        } else {
            (d[n / 2 - 1] + d[n / 2]) / 2.0
        };
        (d[n - 1] - d[0] <= 0.5).then_some(median)
    };
    let c: Vec<Option<f64>> = all.iter().map(delay).collect();
    let slowest = c.iter().flatten().copied().reduce(f64::max);
    all.iter()
        .zip(c)
        .map(|(h, c)| {
            (
                h.id.clone(),
                c.zip(slowest).map(|(c, s)| ((s - c) * 10.0).round() / 10.0),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
            .collect()
    }

    // The same bytes as the receiver's WireTest: request 7 sent at µs 0x0123456789ab, answered with t2 = 1 000 000 and
    // t3 = 1 000 150.
    const REQUEST: &str = concat!(
        "0300",
        "0000",
        "0000",
        "0000",
        "00000007",
        "456789ab",
        "00000008",
        "0000",
        "0000",
        "00000123456789ab"
    );
    const REPLY: &str = concat!(
        "0310",
        "0000",
        "0000",
        "0000",
        "00000007",
        "456789ab",
        "00000018",
        "0000",
        "0000",
        "00000123456789ab",
        "00000000000f4240",
        "00000000000f42d6"
    );

    #[test]
    fn clock_packets_are_the_receivers_bytes() {
        let req = hex(REQUEST);
        assert_eq!(parse_request(&req), Some((7, 0x0123456789ab)));
        assert_eq!(reply(&req, 1_000_000, 1_000_150).to_vec(), hex(REPLY));
        // A reply is not a request, a short packet is nothing.
        assert_eq!(parse_request(&hex(REPLY)), None);
        assert_eq!(parse_request(&req[..30]), None);
    }

    #[test]
    fn header_units_are_tenths_of_a_millisecond() {
        assert_eq!(header_units(150.5), 1505);
        assert_eq!(header_units(9000.0), u16::MAX);
        assert_eq!(header_units(-3.0), 0);
    }

    const N: f64 = f64::NAN;

    // The same vectors as the receiver's SenderPolicyTest: {now s, video pts ms, need ms, D_v, D_a}.
    const WATCHING: [[f64; 5]; 10] = [
        [0.0, N, N, 150.0, 150.0],
        [1.0, 40.0, 120.0, 150.0, 150.0],
        [2.0, 40.0, 210.0, 220.0, 220.0],
        [3.0, 40.0, 230.5, 240.5, 240.5],
        [4.0, 40.0, 200.0, 240.5, 240.5],
        [33.0, 40.0, 200.0, 240.5, 240.5],
        [34.0, 40.0, 200.0, 210.0, 210.0],
        [35.0, 40.0, 200.0, 210.0, 210.0],
        [36.0, 40.0, 229.0, 239.0, 239.0],
        [37.0, 40.0, 240.0, 250.0, 250.0],
    ];
    const INTERACTIVE: [[f64; 5]; 7] = [
        [0.0, 30.0, 15.0, 0.0, 30.0],
        [1.0, 31.0, 15.0, 0.0, 30.0],
        [2.0, 33.0, 15.0, 0.0, 33.0],
        [3.0, 25.0, 15.0, 0.0, 33.0],
        [13.0, 25.0, 15.0, 0.0, 25.0],
        [14.0, 25.0, 70.0, 50.0, 70.0],
        [15.0, 25.0, 40.0, 50.0, 70.0],
    ];

    fn run(mut p: Policy, rows: &[[f64; 5]]) {
        for r in rows {
            let d = p.update(r[0], &[r[1]], &[r[2]]);
            assert_eq!(d, (r[3], r[4]), "at {} s", r[0]);
        }
    }

    #[test]
    fn policy_watching() {
        run(Policy::new(Mode::Watching, 0.0, 150.0), &WATCHING);
    }

    #[test]
    fn policy_interactive() {
        run(Policy::new(Mode::Interactive, 20.0, 0.0), &INTERACTIVE);
    }

    #[test]
    fn only_receivers_that_say_so_are_capable() {
        assert!(capable(&json!({"sync": {"v": 1, "clock": true}})));
        assert!(!capable(&json!({"sdk": 34})));
        assert!(!capable(&json!({"sync": {"v": 1, "clock": false}})));
    }

    #[test]
    fn reports_come_from_stats() {
        let s = json!({"t": "stats", "need_ms": 45.5,
                       "out": {"video_pts_ms": 31.2, "route": "hdmi_arc", "audio_ms": 22.0}});
        let r = Report::from_stats(&s);
        assert_eq!(r.need_ms, Some(45.5));
        assert_eq!(r.video_pts_ms, Some(31.2));
        assert_eq!(
            route_preset_ms(r.route.as_deref().unwrap(), r.audio_ms),
            40.0
        );
        assert_eq!(
            Report::from_stats(&json!({"t": "stats"})),
            Report::default()
        );
    }

    #[test]
    fn presets_by_route() {
        assert_eq!(route_preset_ms("builtin", Some(20.0)), 0.0);
        // A Bluetooth speaker whose delay report reaches the timestamps looks slow already: no preset on top.
        assert_eq!(route_preset_ms("bluetooth", Some(210.0)), 0.0);
        assert_eq!(route_preset_ms("bluetooth", Some(30.0)), 150.0);
        assert_eq!(offset_key("tv-1", "hdmi_arc"), "tv-1@hdmi_arc");
    }

    #[test]
    fn calibration_makes_everyone_as_late_as_the_slowest_and_refuses_disagreement() {
        let h = |id: &str, played: &[i64], heard: &[Option<i64>]| Heard {
            id: id.into(),
            played: played.to_vec(),
            heard: heard.to_vec(),
        };
        let r = offsets_from_heard(&[
            // A phone: heard 12.0–12.3 ms after it played.
            h("phone", &[0, 700_000], &[Some(12_000), Some(712_300)]),
            // A TV with a soundbar: 72 ms.
            h(
                "tv",
                &[2_000_000, 2_700_000],
                &[Some(2_072_000), Some(2_772_100)],
            ),
            // A projector whose two arrivals disagree by 3 ms: not measured.
            h(
                "projector",
                &[4_000_000, 4_700_000],
                &[Some(4_050_000), Some(4_753_000)],
            ),
            // A tablet whose second sweep was refused by the detector.
            h("tablet", &[6_000_000, 6_700_000], &[Some(6_030_000), None]),
        ]);
        assert_eq!(r[0], ("phone".into(), Some(59.9)));
        assert_eq!(r[1], ("tv".into(), Some(0.0)));
        assert_eq!(r[2], ("projector".into(), None));
        assert_eq!(r[3], ("tablet".into(), None));
    }

    #[test]
    fn group_block_is_the_protocols() {
        let g = group_block("g1", 7, "audio", Mode::Interactive, 20.0, -3.5);
        assert_eq!(
            g,
            json!({"id": "g1", "timeline": 7, "role": "audio", "mode": "interactive",
                             "tolerance_ms": 20.0, "offset_ms": -3.5})
        );
    }
}
