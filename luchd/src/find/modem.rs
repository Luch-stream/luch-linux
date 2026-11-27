//! Data over sound (PROTOCOL.md 1d): a receiver says its card (address, port, a pairing token) as a short run of
//! tones near the top of what people hear, and the sender listens with its microphone. Modelled on ggwave: 16-tone
//! FSK (one tone of sixteen = 4 bits per 40 ms symbol), a fixed preamble to find the frame, Reed-Solomon to mend it.
//!
//! Pure code: samples in, bytes out. Only `listen` touches a device, and only the microphone. The encoder is here
//! too, though luchd only listens: the tests make their frames with it, and it is the reference the receiver's
//! Chirp.java is checked against (the same tones for the same card).

use super::rs;

pub const RATE: u32 = 48_000;
/// 40 ms: long enough for a room's echo to fade under the next tone, short enough for a 2 s frame.
pub const SYMBOL: usize = 1920;
pub const TONES: usize = 16;
/// 100 Hz between tones, three times the analysis window's resolution: neighbours do not bleed into each other.
pub const SPACING: f32 = 100.0;
/// The tone 0 frequency of each band. Near-ultrasonic first (17.0–18.5 kHz: most adults hardly hear it, laptop
/// microphones and TV speakers still carry it); the audible band is the fallback for speakers that cut the top.
pub const BANDS: [f32; 2] = [17_000.0, 1_800.0];
/// All different: shifted by a symbol or more, the preamble matches nothing of itself.
pub const PREAMBLE: [u8; 8] = [0, 15, 3, 12, 6, 9, 1, 14];
pub const PARITY: usize = 8;
pub const MAX_PAYLOAD: usize = 32;
/// Raised-cosine edges of every tone (2 ms), so symbol changes do not click.
#[cfg_attr(not(test), allow(dead_code))] // the sender only listens: see the module's doc
const RAMP: usize = 96;
/// The part of a symbol that is analysed: its middle three quarters, away from the edges and the echo of the last.
const WIN_START: usize = SYMBOL / 8;
const WIN_LEN: usize = SYMBOL * 3 / 4;

#[cfg_attr(not(test), allow(dead_code))] // the sender only listens: see the module's doc
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

/// Tone indices of a frame: preamble, then the Reed-Solomon codeword of `[len, payload..]`, high nibble first.
#[cfg_attr(not(test), allow(dead_code))] // the sender only listens: see the module's doc
pub fn symbols(payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() <= MAX_PAYLOAD);
    let mut msg = vec![payload.len() as u8];
    msg.extend_from_slice(payload);
    let word = rs::encode(&msg, PARITY);
    let mut out = PREAMBLE.to_vec();
    for b in word {
        out.push(b >> 4);
        out.push(b & 15);
    }
    out
}

/// The frame as samples at `RATE`, peak `amp`.
#[cfg_attr(not(test), allow(dead_code))] // the sender only listens: see the module's doc
pub fn modulate(payload: &[u8], band: f32, amp: f32) -> Vec<f32> {
    synth(payload, band, amp, RATE)
}

/// The frame at another output rate (a TV whose sound runs at 44.1 kHz): the same tones and symbol times.
#[cfg_attr(not(test), allow(dead_code))] // the sender only listens: see the module's doc
pub fn synth(payload: &[u8], band: f32, amp: f32, rate: u32) -> Vec<f32> {
    let syms = symbols(payload);
    let per = SYMBOL * rate as usize / RATE as usize;
    let ramp = RAMP * rate as usize / RATE as usize;
    let mut out = Vec::with_capacity(syms.len() * per);
    for s in syms {
        let w = 2.0 * std::f64::consts::PI * (band + s as f32 * SPACING) as f64 / rate as f64;
        for i in 0..per {
            let edge = i.min(per - 1 - i);
            let env = if edge < ramp {
                0.5 - 0.5 * (std::f64::consts::PI * edge as f64 / ramp as f64).cos()
            } else {
                1.0
            };
            out.push((amp as f64 * env * (w * i as f64).sin()) as f32);
        }
    }
    out
}

/// Energy of each tone in the window of the symbol starting at `start` (Goertzel, Hann-windowed).
fn energies(x: &[f32], start: isize, band: f32, hann: &[f32]) -> Option<[f32; TONES]> {
    let from = start + WIN_START as isize;
    if from < 0 || from as usize + WIN_LEN > x.len() {
        return None;
    }
    let win = &x[from as usize..from as usize + WIN_LEN];
    let mut e = [0f32; TONES];
    for (k, out) in e.iter_mut().enumerate() {
        let w = 2.0 * std::f32::consts::PI * (band + k as f32 * SPACING) / RATE as f32;
        let coeff = 2.0 * w.cos();
        let (mut s1, mut s2) = (0f32, 0f32);
        for (v, h) in win.iter().zip(hann) {
            let s0 = v * h + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        *out = s1 * s1 + s2 * s2 - coeff * s1 * s2;
    }
    Some(e)
}

/// How much of the energy is in `tone`: 1 for a clean tone, about 1/16 for noise.
fn share(e: &[f32; TONES], tone: u8) -> f32 {
    let sum: f32 = e.iter().sum();
    if sum <= 0.0 {
        0.0
    } else {
        e[tone as usize] / sum
    }
}

fn best(e: &[f32; TONES]) -> (u8, f32) {
    let (k, _) = e
        .iter()
        .enumerate()
        .fold((0, f32::MIN), |b, (k, &v)| if v > b.1 { (k, v) } else { b });
    (k as u8, share(e, k as u8))
}

fn hann() -> Vec<f32> {
    (0..WIN_LEN)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (WIN_LEN - 1) as f32).cos())
        .collect()
}

/// Where a preamble starts, best first: a coarse search on an eighth of a symbol, then a fine one around each hit.
fn preambles(x: &[f32], band: f32, h: &[f32]) -> Vec<isize> {
    const STEP: usize = SYMBOL / 8;
    let span = PREAMBLE.len() * SYMBOL;
    if x.len() < span {
        return Vec::new();
    }
    // Energies at every grid point once: the preamble's symbols fall on the same grid, 8 points apart.
    let grid: Vec<Option<[f32; TONES]>> = (0..=(x.len() - WIN_START - WIN_LEN) / STEP)
        .map(|g| energies(x, (g * STEP) as isize, band, h))
        .collect();
    let per_symbol = SYMBOL / STEP;
    let score_at = |g: usize| -> f32 {
        PREAMBLE
            .iter()
            .enumerate()
            .map(|(i, &t)| {
                grid.get(g + i * per_symbol)
                    .copied()
                    .flatten()
                    .map_or(0.0, |e| share(&e, t))
            })
            .sum::<f32>()
            / PREAMBLE.len() as f32
    };
    let scores: Vec<f32> = (0..grid.len()).map(score_at).collect();
    let score_fine = |t: isize| -> f32 {
        PREAMBLE
            .iter()
            .enumerate()
            .map(|(i, &tone)| {
                energies(x, t + (i * SYMBOL) as isize, band, h).map_or(0.0, |e| share(&e, tone))
            })
            .sum::<f32>()
            / PREAMBLE.len() as f32
    };
    // Peaks: the best score within a symbol either side. The preamble's tones are all different, so a window
    // that is a symbol or more off matches next to nothing and cannot make a peak of its own.
    let mut hits = Vec::new();
    for (g, &s) in scores.iter().enumerate() {
        let near = g.saturating_sub(per_symbol)..(g + per_symbol + 1).min(scores.len());
        let peak = s > 0.5
            && near
                .clone()
                .all(|n| scores[n] < s || (scores[n] == s && n >= g));
        if peak {
            let coarse = (g * STEP) as isize;
            let (fine, fs) = (-(STEP as isize)..=STEP as isize)
                .step_by(16)
                .map(|d| (coarse + d, score_fine(coarse + d)))
                .fold((coarse, f32::MIN), |b, c| if c.1 > b.1 { c } else { b });
            hits.push((fine, fs));
        }
    }
    hits.sort_by(|a, b| b.1.total_cmp(&a.1));
    hits.into_iter().map(|(t, _)| t).collect()
}

/// Reads the frame whose preamble starts at `t0`, following a sample clock that runs a little fast or slow.
fn frame_at(x: &[f32], t0: isize, band: f32, h: &[f32]) -> Option<Vec<u8>> {
    const NUDGE: isize = (SYMBOL / 64) as isize;
    let most = 2 * (1 + MAX_PAYLOAD + PARITY);
    let mut offset = 0isize;
    let mut tones = Vec::with_capacity(most);
    for k in 0..most {
        let at = t0 + ((PREAMBLE.len() + k) * SYMBOL) as isize + offset;
        let mut choice: Option<(isize, u8, f32)> = None;
        for d in [0, -NUDGE, NUDGE] {
            let Some(e) = energies(x, at + d, band, h) else {
                continue;
            };
            let (tone, s) = best(&e);
            // Staying put wins ties: noise must not walk the clock away.
            let s = if d == 0 { s * 1.05 } else { s };
            if choice.is_none_or(|c| s > c.2) {
                choice = Some((d, tone, s));
            }
        }
        let Some((d, tone, _)) = choice else { break };
        offset += d;
        tones.push(tone);
    }
    let bytes: Vec<u8> = tones.chunks_exact(2).map(|p| p[0] << 4 | p[1]).collect();
    for len in 0..=MAX_PAYLOAD {
        let n = len + 1 + PARITY;
        if n > bytes.len() {
            break;
        }
        let mut word = bytes[..n].to_vec();
        if rs::correct(&mut word, PARITY).is_some() && word[0] as usize == len {
            return Some(word[1..=len].to_vec());
        }
    }
    None
}

/// Every frame heard in `x` (both bands), each once.
pub fn demodulate(x: &[f32]) -> Vec<Vec<u8>> {
    let h = hann();
    let mut out: Vec<Vec<u8>> = Vec::new();
    for band in BANDS {
        for t0 in preambles(x, band, &h) {
            if let Some(p) = frame_at(x, t0, band, &h) {
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }
    }
    out
}

/// Listens on the default microphone for `secs` seconds, decoding as it goes; returns the first frames heard.
/// The capture goes through GStreamer (the source is `src`, "autoaudiosrc" for the microphone), resampled to RATE.
pub async fn listen(src: &str, secs: u64) -> Result<Vec<Vec<u8>>, String> {
    use gstreamer::prelude::*;
    let pipeline = gstreamer::parse::launch(&format!(
        "{src} ! audioconvert ! audioresample ! audio/x-raw,format=F32LE,rate={RATE},channels=1 \
         ! appsink name=sink sync=false"
    ))
    .map_err(|e| format!("no microphone pipeline: {e}"))?
    .downcast::<gstreamer::Pipeline>()
    .map_err(|_| "not a pipeline".to_string())?;
    let sink = pipeline
        .by_name("sink")
        .and_then(|s| s.downcast::<gstreamer_app::AppSink>().ok())
        .ok_or("no appsink")?;
    pipeline
        .set_state(gstreamer::State::Playing)
        .map_err(|e| format!("cannot open the microphone: {e}"))?;
    let result = tokio::task::spawn_blocking(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        let mut buf: Vec<f32> = Vec::new();
        let mut since_decode = 0usize;
        // A frame is at most ~3.6 s; keep a little more than that and decode every half second.
        let keep = RATE as usize * 6;
        while std::time::Instant::now() < deadline {
            let Some(sample) = sink.try_pull_sample(gstreamer::ClockTime::from_mseconds(200))
            else {
                if sink.is_eos() {
                    break;
                }
                continue;
            };
            let Some(b) = sample.buffer() else { continue };
            let Ok(map) = b.map_readable() else { continue };
            buf.extend(
                map.chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])),
            );
            since_decode += map.len() / 4;
            if since_decode >= RATE as usize / 2 {
                since_decode = 0;
                let found = demodulate(&buf);
                if !found.is_empty() {
                    return Ok(found);
                }
                if buf.len() > keep {
                    buf.drain(..buf.len() - keep);
                }
            }
        }
        Ok(demodulate(&buf))
    })
    .await
    .map_err(|e| e.to_string())?;
    let _ = pipeline.set_state(gstreamer::State::Null);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Noise(u64);
    impl Noise {
        fn uniform(&mut self) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 40) as f32 + 0.5) / (1u64 << 24) as f32
        }
        /// Box-Muller.
        fn gauss(&mut self) -> f32 {
            let (u, v) = (self.uniform(), self.uniform());
            (-2.0 * u.ln()).sqrt() * (2.0 * std::f32::consts::PI * v).cos()
        }
    }

    /// The card a receiver says: kind 1, id 3f0c2a8e, 192.168.1.20:47800, token 0badf00d.
    const CARD: [u8; 17] = [
        0x4c, 0x75, 0x01, 0x3f, 0x0c, 0x2a, 0x8e, 192, 168, 1, 20, 0xba, 0xb8, 0x0b, 0xad, 0xf0,
        0x0d,
    ];

    /// What a room does: silence before, `gain`, white noise of `noise` RMS, a sample clock `ppm` off.
    fn room(signal: &[f32], lead: usize, gain: f32, noise: f32, ppm: f64, seed: u64) -> Vec<f32> {
        let ratio = 1.0 + ppm * 1e-6;
        let n = (signal.len() as f64 * ratio) as usize;
        let mut rng = Noise(seed);
        let mut out: Vec<f32> = (0..lead).map(|_| noise * rng.gauss()).collect();
        for i in 0..n + RATE as usize / 2 {
            // The listener's clock: its sample i is the speaker's sample i / ratio.
            let pos = i as f64 / ratio;
            let (k, frac) = (pos as usize, (pos - pos.floor()) as f32);
            let s = match (signal.get(k), signal.get(k + 1)) {
                (Some(a), Some(b)) => a + (b - a) * frac,
                (Some(a), None) => *a,
                _ => 0.0,
            };
            out.push(gain * s + noise * rng.gauss());
        }
        out
    }

    #[test]
    fn the_symbols_are_the_receivers_too() {
        // Shared with the receiver's ChirpTest: the tones for the same card must be the same on both sides.
        let s = symbols(&CARD);
        assert_eq!(s.len(), PREAMBLE.len() + 2 * (1 + CARD.len() + PARITY));
        let tones: String = s.iter().map(|t| format!("{t:x}")).collect();
        assert_eq!(tones, GOLDEN_TONES);
    }
    const GOLDEN_TONES: &str = "0f3c691e114c75013f0c2a8ec0a80114bab80badf00d1f8320c0e4ed937c";

    #[test]
    fn a_clean_frame_comes_back() {
        let x = modulate(&CARD, BANDS[0], 0.5);
        assert_eq!(x.len(), symbols(&CARD).len() * SYMBOL);
        assert_eq!(
            demodulate(&room(&x, 5000, 1.0, 0.0, 0.0, 1)),
            vec![CARD.to_vec()]
        );
    }

    #[test]
    fn a_quiet_frame_in_a_noisy_room_with_a_skewed_clock() {
        let x = modulate(&CARD, BANDS[0], 0.5);
        // 40 dB down and noise louder than the signal itself; clocks 500 ppm apart either way (a cheap crystal is
        // within 100).
        for (seed, ppm) in [(2, 500.0), (3, -500.0), (4, 0.0)] {
            let heard = room(&x, 12_345, 0.01, 0.01, ppm, seed);
            assert_eq!(demodulate(&heard), vec![CARD.to_vec()], "ppm {ppm}");
        }
    }

    #[test]
    fn the_audible_band_and_a_hum_on_top() {
        let x = modulate(b"hi", BANDS[1], 0.3);
        let mut heard = room(&x, 777, 0.5, 0.02, 100.0, 5);
        // A steady whistle right on one of the tones, a little quieter than the frame (0.15 here): it takes part of
        // every symbol's energy, never the symbol. (One louder than the TV would win them all.)
        for (i, v) in heard.iter_mut().enumerate() {
            *v += 0.1
                * (2.0 * std::f32::consts::PI * (BANDS[1] + 7.0 * SPACING) * i as f32
                    / RATE as f32)
                    .sin();
        }
        assert_eq!(demodulate(&heard), vec![b"hi".to_vec()]);
    }

    #[test]
    fn a_burst_that_wipes_out_symbols_is_mended() {
        let x = modulate(&CARD, BANDS[0], 0.5);
        let mut heard = room(&x, 4000, 0.2, 0.002, 0.0, 6);
        // A clap: 120 ms (three symbols) of loud broadband noise in the middle of the data.
        let mut rng = Noise(9);
        let from = 4000 + (PREAMBLE.len() + 10) * SYMBOL;
        for v in &mut heard[from..from + 3 * SYMBOL] {
            *v += 0.8 * rng.gauss();
        }
        assert_eq!(demodulate(&heard), vec![CARD.to_vec()]);
    }

    #[test]
    fn noise_alone_says_nothing() {
        let mut rng = Noise(11);
        let x: Vec<f32> = (0..RATE as usize * 4).map(|_| 0.1 * rng.gauss()).collect();
        assert!(demodulate(&x).is_empty());
        assert!(demodulate(&[]).is_empty());
    }

    #[test]
    fn two_frames_back_to_back_are_both_heard() {
        let mut x = modulate(b"first", BANDS[0], 0.5);
        x.extend(vec![0.0; 3000]);
        x.extend(modulate(b"second", BANDS[0], 0.5));
        let got = demodulate(&room(&x, 100, 0.1, 0.005, 50.0, 12));
        assert!(
            got.contains(&b"first".to_vec()) && got.contains(&b"second".to_vec()),
            "{got:?}"
        );
    }

    /// The capture path, without a microphone and without a speaker: GStreamer's test source makes a plain tone
    /// in memory, which goes through the same conversion and must decode to nothing.
    #[tokio::test]
    async fn the_capture_pipeline_runs_on_a_test_source() {
        gstreamer::init().unwrap();
        let got = listen("audiotestsrc num-buffers=50 freq=17300 is-live=false", 3)
            .await
            .unwrap();
        assert!(got.is_empty());
    }

    /// A frame made at 44.1 kHz (what many TVs output) and brought to 48 kHz by GStreamer's resampler, in memory.
    #[test]
    fn a_frame_through_gstreamers_resampler() {
        use gstreamer::prelude::*;
        gstreamer::init().unwrap();
        let y = synth(&CARD, BANDS[0], 0.5, 44_100);
        let pipeline = gstreamer::parse::launch(
            "appsrc name=src format=time caps=audio/x-raw,format=F32LE,rate=44100,channels=1,layout=interleaved \
             ! audioconvert ! audioresample ! audio/x-raw,format=F32LE,rate=48000,channels=1 \
             ! appsink name=sink sync=false",
        )
        .unwrap()
        .downcast::<gstreamer::Pipeline>()
        .unwrap();
        let src = pipeline
            .by_name("src")
            .unwrap()
            .downcast::<gstreamer_app::AppSrc>()
            .unwrap();
        let sink = pipeline
            .by_name("sink")
            .unwrap()
            .downcast::<gstreamer_app::AppSink>()
            .unwrap();
        pipeline.set_state(gstreamer::State::Playing).unwrap();
        let mut bytes = Vec::with_capacity(y.len() * 4);
        for v in std::iter::repeat_n(0.0f32, 4410)
            .chain(y)
            .chain(std::iter::repeat_n(0.0, 22050))
        {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        src.push_buffer(gstreamer::Buffer::from_slice(bytes))
            .unwrap();
        src.end_of_stream().unwrap();
        let mut out = Vec::new();
        while let Some(s) = sink.try_pull_sample(gstreamer::ClockTime::from_seconds(5)) {
            let map = s.buffer().unwrap().map_readable().unwrap();
            out.extend(
                map.chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])),
            );
        }
        pipeline.set_state(gstreamer::State::Null).unwrap();
        assert!(out.len() > 40_000);
        assert_eq!(demodulate(&out), vec![CARD.to_vec()]);
    }
}
