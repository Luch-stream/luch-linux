# Sync across devices and multiroom: research and a design for Luch

Status: November 2026. Research, then the chosen design implemented on branch `sync` (luch-android: receiver,
simulator, PROTOCOL.md section 4h; luch-linux: luchd groups). Section 9 says what was built, what the simulator and
two emulators measured, and what changed against this design; the real-device plan (8.2) is refined by it. The
protocol is in `luch-android/PROTOCOL.md`; this note refers to its sections.

The question: a TV shows the picture while another Luch receiver (a speaker) plays the sound, or one sender plays to
N TVs and speakers (a restaurant with 20 TVs showing a match, a home with speakers in several rooms), all in sync,
with the least latency we can get and no drift.

## 1. Summary

- **Every working system uses the same model**: one clock that all devices agree on, media units stamped on it, and
  each device presenting each unit at *stamp + fixed delay* on that clock, correcting its own output clock by
  inserting or dropping single samples. Snapcast, AirPlay 2 (shairport-sync), squeezelite/LMS, AES67/Dante, ROC and
  Bluetooth LE Audio broadcast differ only in the clock protocol and in how big the delay is. Nobody synchronises by
  measuring the output of the other devices while playing.
- **Luch already has half of it**: one µs `pts` on the sender's clock for video and audio (section 4). What is
  missing is (a) each receiver knowing the sender's clock, (b) a shared playout delay `D`, (c) each receiver knowing
  its own true output latency. All three are small.
- **Achievable accuracy on Wi-Fi with cheap Android devices** (estimates, to be measured in phase 1):
  clock offset about 0.1–0.5 ms after min-RTT filtering, bounded by half the smallest round trip (about 1 ms on
  2.4 GHz, less on 5 GHz); audio output timing from `AudioTrack.getTimestamp` to about 1 ms where the HAL supports it;
  video to one display refresh (16.7 ms at 60 Hz) unless the receiver aims at a VSYNC, then a few ms.
  The unknown that dominates is not the network but each TV's own picture processing delay.
- **What the ear needs**: picture vs sound, ITU-R BT.1359 detectability is sound 45 ms early to 125 ms late;
  EBU R37 asks for +40/−60 ms end to end. Two speakers in one room need about 1 ms (beyond that the image moves to
  the earlier speaker, the precedence effect); speakers in different rooms are fine within about 10–20 ms.
  So: clock sync to well under 1 ms is enough for everything, and picture-vs-sound has a tolerance window that we
  should use instead of paying latency for exactness.
- **Recommendation**:
  1. Interactive mirroring: keep today's "no jitter buffer" path for the TV. With a separate speaker, schedule both
     at `pts + D` where `D = max(video path, audio path) + margin`, and let the sound trail the picture by up to
     a tolerance τ ≈ 20–30 ms before holding the picture at all. Minimum added delay: the audio jitter margin
     (about 5–15 ms on good 5 GHz Wi-Fi), and picture delay only when the speaker is slower than the TV by more
     than τ.
  2. Watching/listening: `D` = 150–300 ms, Opus 20 ms frames, a NACK retransmit and/or FEC inside `D`, soft drift
     correction by sample stuffing, joining and leaving without glitches. For many TVs, let each TV fetch the media
     itself (section 4b) and only synchronise the play position on the shared clock.
  3. Clock: a tiny NTP-style exchange (kind 3 packets) on the existing UDP sockets, receiver-initiated, 2 Hz,
     min-RTT filter plus a line fit for skew. Not PTP: Android apps cannot bind ports 319/320 or timestamp in
     hardware, and the math is the same.
  4. Header: the reserved u16 becomes the playout delay `D` in 0.1 ms; 0 keeps today's behaviour (play as soon as
     possible), so old senders and receivers keep working unchanged.
  5. Output latency: what the platform reports (`getTimestamp`, render callbacks) always; a one-button acoustic
     calibration with the sender's or a phone's microphone for what it cannot see (soundbars, projectors), trusted
     only when its confidence test passes; a per-device slider as the fallback that always exists.
- Section 4 lists all viable approaches (O1–O13) for a later, different choice; section 5 picks one for the
  hardest home mix (phone, Android TV, projector, cheap tablet, ARC and Bluetooth outputs); section 6 specifies it
  precisely enough to implement; section 8 is the simulator with its pass criteria.

## 2. What existing systems do

### 2.1 Snapcast (the closest model, open source)

- Server reads PCM, cuts it into chunks, stamps each with server time, sends over TCP. Every client keeps a
  running estimate of server time and plays each chunk at `stamp + buffer` (server `buffer` default 1000 ms;
  client player buffers 80–100 ms on ALSA/Pulse).
- Time sync: the client sends a `Time` message every second (50 quick ones at connect); offset is
  `((t_srv_recv − t_cli_sent) − (t_cli_recv − t_srv_sent)) / 2`, the network is assumed symmetric; the client keeps
  the **median of the last 200** offsets (`time_provider.cpp`).
- Drift: "corrected by playing faster/slower, which is done by removing/duplicating single samples (a sample at
  48 kHz has a duration of ~0.02 ms)". `stream.cpp` keeps three medians over different windows; it starts soft
  correction when the short median exceeds 100 µs, and does a hard resync (skip or silence) when it is beyond 2 ms
  long-term or 50 ms short-term. Typical deviation reported: below 1 ms.
- Lesson for Luch: the same algorithm with a much smaller buffer; we need UDP (no head-of-line blocking) and a
  better clock filter than the median, because Wi-Fi delay is one-sided (queues only add delay).

### 2.2 AirPlay 2 and classic AirPlay (RAOP), via shairport-sync

- Classic AirPlay: a fixed latency negotiated by the source, about 2.0–2.25 s, NTP-like timing.
- AirPlay 2: PTP-based timing (shairport-sync needs the NQPTP helper on ports 319/320); two stream types:
  *realtime* (played after about 2 s) and *buffered* (audio arrives faster than played and is buffered on the
  device; start after a very short delay). AirPlay 2 "can use shorter latencies, around half a second".
- Drift: shairport-sync "will perform interpolation — effectively shortening or lengthening the stream of audio to
  exactly match the output rate", basic stuffing or libsoxr resampling. Clocks agree "usually to within a fraction
  of a millisecond".
- Apple TV "Wireless Audio Sync": the iPhone's microphone listens to test tones played through the TV's own
  speakers to measure the TV's audio/video latency, then wireless speakers are matched to it. The TV's own speaker
  is the reference for the picture. This is the cheapest correct calibration and Luch should copy it.
- Screen mirroring audio on AirPlay is not group-synchronised; sync groups are the buffered path.

### 2.3 Sonos and Google Cast multizone

- No public protocol. What is documented: Sonos line-in has an "Audio Delay" from 75 ms to 2 s (75 ms is the
  floor, the buffer that keeps grouped rooms aligned over the network). Google Cast groups offer only a manual
  "Group delay correction" slider (speakers 0–40 ms, soundbars 0–80 ms): the TV/soundbar processing delay is
  not known to the system and has to be entered by ear. Both confirm that (a) grouping needs a buffer of tens to
  hundreds of ms, and (b) device output delay is the hard part.

### 2.4 squeezelite / Logitech (Lyrion) Media Server

- SlimProto: each player reports a 1 kHz timestamp (`jiffies`) in STAT messages; the server measures round trip
  with `strm t`, starts all players of a group with `u` at a player-specific time, and corrects by "pause for N ms"
  (`p`) or "skip N ms" (`a`). Coarse (ms), server-driven corrections. Good enough for rooms, not for stereo pairs.

### 2.5 PipeWire RTP, AES67, Dante, Ravenna, ROC

- AES67/Ravenna/Dante: PTPv2 (IEEE 1588) on wired networks with switches that respect it; RTP with 1 ms packets
  (48 samples); receivers play at `RTP timestamp + link offset` on the PTP media clock (RFC 7273 signals which clock).
  Dante latency settings are 0.25–5 ms on wire. Sub-µs clocks, but only on Ethernet.
- PipeWire implements RTP/AES67 sinks and sources (`module-rtp-sink`, `module-rtp-source`) and can follow a PTP
  clock from `ptp4l`; on Wi-Fi it falls back to a latency of tens to hundreds of ms.
- ROC toolkit: RTP + FEC (Reed–Solomon for short blocks and low latency, LDPC-Staircase for long blocks). The FEC
  block bounds the minimum latency ("the jitter buffer can't be smaller than the FEC block size"). Clock drift is
  compensated by a resampler steered by a latency tuner ("responsive" profile for low jitter, "gradual" for high
  jitter); reported best case 7 ms over Ethernet plus sound card latency. Lesson: FEC is the right loss answer when
  a retransmit does not fit in `D`; size the block to `D`.

### 2.6 gPTP / 802.1AS and Wi-Fi time sync

- IEEE 802.1AS (gPTP, AVB/TSN) gives sub-µs sync over Ethernet; 802.1AS-2020 also defines transport over 802.11
  using the Wi-Fi Timing Measurement / Fine Timing Measurement (FTM, 802.11mc/az) frames, where the radio
  timestamps in hardware (FTM ranging needs ns timestamps: 1 ns ≈ 30 cm). Not reachable from an Android app:
  `WifiRttManager` gives distances to APs, not a shared clock.
- Software PTP over off-the-shelf Wi-Fi can reach a µs level with tuned systems and calibration (Chen and Yang,
  USENIX ATC 2021); other measurements show tens of µs with software timestamps and ms with bridged APs. Without
  kernel timestamps (Android Java sockets) the realistic floor is set by scheduling and power save: 0.1–1 ms.
- The key fact for an estimator: on Wi-Fi, delay noise is one-sided (queues, retries, power-save wakeups only add
  delay). The best samples are the ones with the smallest round trip, and the error of an exchange is bounded by
  half of its round trip. NTP's clock filter and Huygens (NSDI 2018) use exactly this (pick the low-delay probes,
  fit a line for skew).

### 2.7 WebRTC A/V sync

- Each RTP stream has its own clock; RTCP sender reports map RTP time to the sender's NTP wall clock (RFC 3550,
  6.4.1). The receiver computes the relative delay of audio and video and raises the minimum playout delay of the
  stream that is ahead (`stream_synchronization.cc`), in small steps. Same idea as Luch's `sync` mode; Luch is
  simpler because one pts clock covers both streams already.

### 2.8 Bluetooth LE Audio broadcast (Auracast)

- The broadcaster announces a *presentation delay* in the BASE; every sink renders each frame at a fixed point
  after the ISO reference anchor + presentation delay. BAP recommends 40 ms (sinks must support it). LC3 frames
  are 7.5 or 10 ms. It is exactly "play at stamp + D", with D announced by the sender. Luch should carry `D` the
  same way: by the sender, in-band, for all at once.

### 2.9 Output latency on Android

- `AudioTrack.getTimestamp()` (API 19): a frame position and "the estimated time when that frame was presented or
  is committed to be presented", on `System.nanoTime()`. It includes the HAL and mixer latency that the buffer size
  does not. Poll often during warm-up and after route changes, then every 10 s to 1 min; "if you need a
  high-resolution mapping between frame position and presentation time, consider implementing that at application
  level". The slope of successive timestamps is the DAC clock rate against `nanoTime`: that is the drift we must
  correct. Some routes (HDMI passthrough, some Bluetooth) give no timestamp: fall back to
  `getPlaybackHeadPosition` and a fixed latency guess.
- Today's receiver (`MirrorSession`) reports `audio_latency_ms = buffer frames / rate`: the buffer only, not the
  mixer, HAL and DAC. Its `sync` mode therefore under-holds the picture. Fixing this needs no protocol change
  (phase 0).
- `MediaCodec.releaseOutputBuffer(index, renderTimestampNs)`: renders "at the VSYNC at or after the buffer
  timestamp", which must be within one second of `System.nanoTime()`; call it about two VSYNCs (33 ms at 60 Hz)
  before the render time for the best result. `OnFrameRenderedListener` (API 23) reports when a frame actually went
  to the display. Today the receiver uses `nanoTime + videoHoldNs`, which already fits this model.
- `MediaPlayer.getTimestamp()` (API 23) gives the media-time ↔ `nanoTime` anchor, and
  `setPlaybackParams(speed)` changes speed: enough to keep a TV playing a file or a stream itself in step with a
  shared clock (multi-TV media mode).
- What no API tells: the TV panel's own processing delay (scaling, motion smoothing). On HDMI the sink announces
  video and audio latency in the HDMI vendor-specific EDID block (and HDMI 2.2's Latency Indication Protocol),
  but Android does not expose it to apps; game mode / ALLM (`setPreferMinimalPostProcessing`, API 30, already
  used) shrinks it. It must be measured (section 6).

## 3. Numbers that drive the design

| Quantity | Typical value | Source / note |
|---|---|---|
| Crystal tolerance between devices | 20–100 ppm | 100 ppm = 6 ms per minute, 0.36 s per hour |
| DAC clock vs system clock | separate, tens of ppm | seen as the slope of `getTimestamp` |
| Wi-Fi one-way delay, idle 5 GHz | 1–3 ms, one-sided tail | power save adds up to a beacon interval (102.4 ms × DTIM) |
| Clock error bound per exchange | RTT / 2 | best probes: about 0.5–1 ms |
| Inaudible speed change by stuffing | up to about 0.1–0.2 % | 0.1 % ≈ 1.7 cents, below pitch discrimination; 100 ppm needs 4.8 samples/s |
| Picture vs sound tolerance | +45 / −125 ms detectable | ITU-R BT.1359; EBU R37 +40/−60 ms |
| Two speakers, one room | ≲ 1 ms | precedence effect; sound travels 2.9 ms per metre |
| Speakers in different rooms | ≲ 10–20 ms | |
| Android 7+ low-latency AudioTrack | about 20 ms buffer, plus HAL | measured by `getTimestamp` |
| Android 5/6 AudioTrack | 100+ ms | `limits.audio_lag` |

## 4. Catalogue of approaches

Each option below is something a real system does (section 2). They are not exclusive: a design is a clock option,
plus a latency-measurement option, plus a transport option. Accuracy is inter-device, on home Wi-Fi.

**O1. No shared clock, each device plays as soon as it can (today).**
How: every receiver presents on arrival; `sync` holds the picture by the local audio buffer.
Accuracy: devices differ by their whole path difference, 10–250 ms, and drift apart (100 ppm = 6 ms/min).
Added latency: none. Cost: none. Hardware: any. Risk: unusable for two devices playing the same sound.

**O2. Manual per-device delay offset (Google Cast "group delay correction", Sonos line-in delay).**
How: the user moves a slider per device until it sounds right; the system adds that delay.
Accuracy: by ear, ±5–20 ms; only as good as the clock underneath (without O3 it drifts).
Added latency: what the user sets. Cost: trivial UI. Hardware: none. Risk: users do not know which way to move it;
must be redone when the output changes (soundbar on, BT speaker swapped).

**O3. Shared clock by a two-way exchange on UDP, play at `pts + D`, drift by sample stuffing (Snapcast,
shairport-sync, NTP clock filter).**
How: receivers estimate the sender's clock with four-timestamp probes, present every unit at `pts + D` on it, and
correct their DAC rate by inserting or dropping single samples.
Accuracy: clock 0.1–0.5 ms (bound RTT/2 of the best probes); audio output to about 1 ms where
`AudioTrack.getTimestamp` works. Added latency: `D` = the slowest device's need, at least the jitter margin
(5–15 ms interactive, 150–300 ms watching). Cost: small (one packet kind, an estimator, a stuffing loop). Hardware:
any Android 5+ device, any Wi-Fi. Risk: it synchronises what the device *reports*; output delays the platform
does not know (an ARC soundbar's DSP, a projector's picture processing) stay as errors until measured (O8, O2).

**O4. Software PTP (IEEE 1588) instead of the O3 exchange.**
How: standard PTP messages from a master, best-master election. Accuracy: the same as O3 without kernel
timestamps; µs only with tuned kernels (USENIX ATC 2021). Added latency: same as O3. Cost: more code (or ptp4l),
ports 319/320 (privileged on Android: an app cannot bind them). Risk: no gain on Android; only worth it to join an
existing AES67/AirPlay world.

**O5. Hardware Wi-Fi timing (TSF counter, 802.11 Timing Measurement / FTM, gPTP over 802.11).**
How: the radio timestamps frames; 802.1AS-2020 carries gPTP over it. Accuracy: µs or better.
Added latency: none extra. Cost: not reachable from an Android app (`WifiRttManager` gives distances only), needs
driver and AP support. Risk: unavailable on the devices we target. Not viable today.

**O6. Sender-driven corrections from position reports (squeezelite/LMS).**
How: receivers report their play position with a local timestamp; the sender tells laggards to pause or skip N ms.
Accuracy: a few ms, with steps. Added latency: a buffer of hundreds of ms. Cost: medium, all logic on the sender.
Risk: audible steps; coarse; O3 does the same job continuously and inaudibly.

**O7. Continuous acoustic closed loop (a microphone listens during playback and steers the devices).**
How: cross-correlate the microphone signal with what each device should play, all the time.
Accuracy: sub-ms in quiet rooms. Added latency: none extra. Cost: high CPU, a microphone always open (privacy),
fails with music that repeats, fans, talking, several devices playing the same signal (cannot tell them apart).
Risk: high. Not recommended.

**O8. One-shot acoustic calibration (Apple TV Wireless Audio Sync, BeepBeep) on top of O3.**
How: once, on request, each device plays a known probe at a scheduled shared-clock time; one microphone (the
sender's, or a phone running Luch) records; matched filtering gives each device's real acoustic output time;
the difference to the schedule is that device's correction. Accuracy: 0.1–0.5 ms in reasonable rooms; in a bad
room (fans, reverberation) the confidence test may reject it. Added latency: none (it only corrects).
Cost: medium (probe player, a correlator in Java, a few messages). Hardware: a microphone near the listening
place. Risk: bad acoustics, a quiet-hours household, users not wanting test tones; must fall back to O9 + O2.

**O9. Self-reported output latencies (AudioTrack timestamps, MediaCodec render callbacks, Bluetooth delay
reports).**
How: each device measures what its platform knows: `getTimestamp` (mixer, HAL, and on recent Android the
Bluetooth sink's AVDTP delay report), `OnFrameRenderedListener` (decode to display). Accuracy: about 1 ms for what
is covered; blind to external DSP (HDMI ARC soundbar 20–100 ms, projector picture processing 30–100 ms).
Added latency: none. Cost: small. Risk: silent errors on devices whose HAL lies or has no timestamps.

**O10. Camera calibration (a phone films the screen and listens).**
How: the sender shows a flash with a click; the phone camera at 120–240 frames/s gives the picture time, the
phone microphone the sound time. Accuracy: ±4–8 ms. Cost: medium (camera code), manual. Use: picture vs sound on
a projector with a separate sound system, where O8 has no picture reference.

**O11. Media-mode position sync (each TV fetches the stream itself).**
How: the sender says "position P at sender time T"; each TV keeps `MediaPlayer.getTimestamp` on it with small
`setPlaybackParams` speed changes. Accuracy: about one frame (16–40 ms). Added latency: the player's own buffer.
Cost: small on top of O3. Use: many TVs showing the same video; no video traffic from the sender.

**O12. Bluetooth LE Audio broadcast (Auracast).**
How: the radio schedules isochronous frames; every sink renders at anchor + presentation delay (40 ms).
Accuracy: tens of µs between sinks. Cost: needs LE Audio broadcast on the sender and the sinks (rare on TVs and
cheap tablets in 2026). Use: later, a sender that has it could broadcast to earbuds; not a Wi-Fi solution.

**O13. Transport: unicast or multicast** (orthogonal to the above), see 6.8.

## 5. The choice for the hardest realistic mix

The case: an Android phone, an Android TV, an Android projector (its own audio processing delay, loud fans, odd
room), a $100 Android tablet, on ordinary home Wi-Fi; outputs mixed: built-in speakers, an HDMI ARC soundbar, a
Bluetooth speaker; acoustics differ per room.

What breaks what:

| Device / output | What the platform knows (O9) | What it does not know |
|---|---|---|
| Phone, built-in speaker | `getTimestamp` good, 10–40 ms | nothing important |
| Phone or tablet → Bluetooth speaker | timestamp often includes the sink's delay report (150–250 ms) | sinks that report wrong or nothing |
| Android TV, own speakers | timestamp to the HAL | TV's audio DSP, usually matched to its own panel |
| Android TV → HDMI ARC soundbar | timestamp to the HDMI output | the soundbar's DSP, 20–100 ms |
| Projector, own speaker | timestamp to the HAL | picture processing (keystone, smoothing) 30–100 ms, matched or not |
| $100 tablet | old Android, maybe no timestamps (`head` only) | tens of ms |

**Chosen: O3 + O9 as the always-on base, O8 as the one-button refinement, O2 as the always-available fallback.**

- O3 + O9 work on every device class with standard APIs, need no microphone, no user action and no quiet room,
  and fail gracefully: a device without timestamps says so (`audio_src:"head"`) and gets a larger margin. That is
  robustness first.
- O8 fixes exactly what O9 cannot see (soundbar DSP, projector processing, a lying HAL), and is only trusted when
  its own confidence test passes, so a fan or an echo cannot make things worse.
- O2 remains for the rest (and for the user who wants to fine-tune by ear), stored per receiver and output route.
- **Fallback when O8 is impossible or rejected**: O9 + O2, with the slider pre-set from a table of typical delays
  per route (`AudioDeviceInfo` type: HDMI ARC/eARC +40 ms, Bluetooth A2DP not covered by a delay report +150 ms),
  and O10 for a projector whose picture lags its sound system.
- Not chosen: O4/O5 (no gain or unavailable on Android), O6 (steps), O7 (fragile, privacy), O12 (hardware).

## 6. Specification of the chosen design

Everything below is additive: a receiver or sender that does not know it behaves as today.

### 6.1 Capability and session messages (control channel, section 3)

```
receiver → welcome adds "sync":{"v":1,"clock":true,"probe":true,"mic":true|false,
                                "audio_src":"timestamp"|"head","routes":["builtin"|"hdmi"|"hdmi_arc"|"bluetooth"|"usb"]}
sender   → start adds  "group":{"id":"<uuid>","timeline":<u32>,"role":"video"|"audio"|"both",
                                "mode":"interactive"|"watch","tolerance_ms":20,"offset_ms":<user/calibrated, signed>}
receiver → stats adds  "clock":{"err_us":N,"skew_ppm":F,"rtt_us":N},
                       "out":{"audio_ms":N,"audio_src":"timestamp"|"head","video_ms":N,"route":"hdmi_arc"},
                       "need_ms":N, "sync_err_ms":F
sender   → {"t":"offset","ms":<signed>}            the user moved the slider or a calibration finished; takes effect at once
```

- `role`: what this receiver presents from the session (the TV `video`, the speaker `audio`, a TV with its own
  sound `both`). The sender sends only the streams of the role.
- `offset_ms`: added to this receiver's presentation time (positive = later). The sender keeps it per receiver id
  and route (a TV with and without its soundbar are two entries) and sends it in `start` and on change.
- `audio_ms`: pts-to-ear latency the receiver can see (6.4), `video_ms`: pts-to-display (6.5), both medians over
  10 s. `need_ms`: 6.3. `sync_err_ms`: the median of (presented − target) over the last second, for tests and the UI.
- `timeline`: a random u32 per sender pipeline; the same for all receivers of a group.

### 6.2 Packet header and clock packets (section 4)

- Offset 22, u16, was reserved: **`delay`, the playout delay D in 0.1 ms**. `0` = present as soon as possible
  (today). The sender writes it into every packet; a receiver applies the value of the packet it is presenting, so
  a change hits the same frame on every device.
- flags bit3: first packets of a new `timeline` (pts base restarted). flags bit4: reply (kind 3 only).
- **kind 3, clock**: the 24-byte header (shard fields 0, `seq` = probe number, `pts` = low 32 bits of t1, `length`
  = payload bytes), then a big-endian payload:

```
request  (receiver → sender, to the address and port the media comes from):  u64 t1          (receiver µs, monotonic)
reply    (sender → receiver, flags bit4, at once, same socket):              u64 t1, u64 t2, u64 t3
         t2 = sender receive time, t3 = sender send time, both on the pts clock (pipeline running time, µs, 64-bit)
```

- The sender answers every request it can parse, from any receiver of the session, and keeps no state. On Linux,
  t2 comes from `SO_TIMESTAMPNS` if available, else from the read time.
- Audio and video sockets both answer; a receiver uses the audio socket if it has one (it is the latency-critical
  stream), else video.

### 6.3 Clock estimator (receiver)

- On each reply at local time t4: `rtt = (t4 − t1) − (t3 − t2)`, `θ = ((t2 − t1) + (t3 − t4)) / 2` (sender −
  receiver), sample `(t_mid = (t1 + t4)/2, θ, rtt)`. Discard `rtt < 0` or `rtt > 200 ms`.
- Schedule: at session start or join, 16 requests 30 ms apart; then every 500 ms; again a burst after a network
  change, an outage over 3 s, or when `err_us` exceeds 2000.
- Window: the last 64 samples. Selection: samples with `rtt ≤ min_rtt + 300 µs`; if fewer than 6, the 6 with the
  smallest rtt.
- Fit: weighted least squares `θ(t) = a + b·(t − t_ref)` over the selected samples, weight `1 / (rtt − min_rtt + 50 µs)²`;
  the slope `b` is the skew, clamped to ±500 ppm; with fewer than 10 s of samples use `b = 0`.
- Output: `sender(t) = t + a + b·(t − t_ref)`. Before playback starts, the estimate is taken as it is. While
  playing, a new estimate is not jumped to: the difference is slewed into the mapping at up to 0.5 ms per second
  (this is inaudible because the audio loop absorbs it like drift), unless it exceeds 5 ms, which counts as a hard
  resync (6.4).
- `err_us = min_rtt/2 + residual RMS of the fit` (reported in `stats`).
- *Implemented differently (section 9):* the hard selection above keeps the start's burst for the whole 32 s
  window under gamma jitter, and 64 exchanges are too short a base for the skew. The estimator has two scales: the
  skew from the best exchange of each second over four minutes (3 σ test before 10 s), the offset from the last 64
  moved along it, each weighted by `1 / ((rtt − min)/2 + 50 µs + σ_skew · age)²`; requests 4 a second; a re-burst
  when the fit's residual (not `err_us`, which on busy 2.4 GHz is over 2 ms by its base delay alone) exceeds 1 ms.
- `need_ms` (watching mode and audio roles): over the last 10 s, p99 of `(local arrival time of the last shard −
  local(pts))`, plus decode p95, plus the receiver's own output latency (`audio_ms` or `video_ms`), plus 3 ms.

### 6.4 Audio playout (receiver)

Notation: `r` = the DAC rate in frames per ns as seen by `nanoTime`; `(F, T)` = the last `AudioTimestamp`; `W` = frames
written so far; `C` = this receiver's `offset_ms`.

- Warm-up: poll `getTimestamp` every 100 ms until `F` advances twice in a row; then every 1 s for 10 s (to fit `r`
  by least squares over these points), then every 10 s, as the platform documentation recommends. If it never
  advances within 2 s, `audio_src = "head"`: use `getPlaybackHeadPosition` with the buffer size plus a 20 ms guess.
- The first frame of the next chunk to write (pts `p`) will be heard at `T_hear = T + (W − F) / r`, and should be
  heard at `T_want = local(p + D) + C`. Error `e = T_hear − T_want` (positive: late).
- Start: write `−e · rate` frames of silence (if `e < 0`) or drop `e · rate` frames (if `e > 0`), then play.
- Hard resync when `|e| > 20 ms` (a join, a `D` step, an outage, a clock step): the same as start, with a 3 ms fade
  out and in.
- Soft correction otherwise: a PI controller on `e`, `s = Kp·e + Ki·∫e dt` with `Kp = 0.1 s⁻¹`, `Ki = 0.005 s⁻²`,
  `s` clamped to ±1000 ppm (0.1 %, inaudible). An accumulator adds `s` per frame; at +1 one frame is dropped, at −1
  one frame is repeated, each time replacing the two neighbours by their linear interpolation (no click). Dead band:
  `|e| < 0.2 ms` gives `s` from the integral only.
- One loop handles everything: sender-vs-receiver skew (through `local()`), DAC-vs-system clock (through `r`), and
  clock estimate slews.
- *Added in the implementation (section 9):* a feed-forward of the drift already known
  (`1 − (r / (1 + skew)) / rate`), so the PI only cleans up; the track is kept full (a track starts only once its
  buffer is full); a chunk's D is its own, and a change of D (over 2 ms of error) is taken at once with one hard
  resync, on the same chunk everywhere; a second of chunks all after their time (D below this receiver's need)
  starts the playout again on what arrives; concealment lasts the sender's chunk duration; an AudioTimestamp more
  than 1 ms off its own line (an underrun, a reroute) restarts the fit of r.
- A chunk that arrives after `T_want − write-ahead`: write silence of its length (with fades), never stretch time.
- Interactive role `both` with `D = 0` (today's behaviour) skips all of this.

### 6.5 Video playout (receiver)

- For a decoded output buffer with pts `p` and `D > 0`: `T = local(p + D) + C_video − L_panel`, where `C_video` is
  the video part of `offset_ms` (the TV role) and `L_panel` the calibrated panel delay (6.6; default 0).
- Release with `releaseOutputBuffer(index, T − P/2)`, `P` = the display period from `Choreographer` (or
  `Display.getRefreshRate()`), so that "the VSYNC at or after" lands within ±P/2 of `T`. Release about 2 periods
  before `T` (the documented best practice); a buffer due later than `now + 900 ms` is held (D is capped at 900 ms
  for video).
- Late frames: interactive: release at once; watching: drop if a newer frame is already decoded.
- `OnFrameRenderedListener` (API 23+): `video_ms` = median of (render time − local(p)); render later than `T + P`
  counts in `sync_err_ms`.
- *Implemented:* compressed frames wait in the receive thread and go to the decoder at `T − decode − 3P`, so the
  decoder never holds more than a few. `out.video_ms` stays the receiver's own path (decoder input to display),
  `out.video_pts_ms` is pts to display, `out.video_err_ms` presented minus due.

### 6.6 Choosing D on the sender

- Watching (`tolerance 0`): `D = max(need_ms over the group) + 2 ms` for audio and video alike. It rises at once
  when any receiver's `need_ms > D − 1`; it falls by at most 1 ms per second, and only after 30 s with
  `max(need) < D − 10`. Floor 150 ms by default.
- Interactive: video receivers get `D_v = 0` (as soon as possible) unless the audio cannot keep up; audio receivers
  get `D_a = max(need_audio, video_ms of the TV)`; if `need_audio > video_ms + tolerance`, video gets
  `D_v = need_audio − tolerance` instead (the picture waits only for the excess). Defaults: tolerance 20 ms.
- Per kind: video packets carry `D_v`, audio packets `D_a`.
- *Learned on the emulators:* rising "at once" by whatever the need grew makes D creep up by 1–2 ms a second while a
  receiver's path settles (the emulator's grew from 36 to 120 ms over 12 s), and every step is a hard resync. A
  headroom on rising (to need + 5 ms) is the likely fix; to be decided after the real-device numbers (8.2, step 4).

### 6.7 Per-device latency measurement: acoustic calibration and its fallbacks

Messages:

```
sender → each receiver   {"t":"probe","id":"<uuid>","at":<sender µs>,"signal":"sweep","f0":2000,"f1":6000,
                          "ms":200,"n":4,"gap_ms":500,"gain":0.3}
receiver → sender        {"t":"probe_done","id":"..","played":[<sender µs at which each sweep reached the DAC, by its own timestamps>]}
sender → the microphone  {"t":"listen","id":"..","from":<sender µs>,"ms":N,"expect":[{"rx":"<receiver id>","at":[..]}]}
microphone → sender      {"t":"heard","id":"..","rx":[{"rx":"..","at":[<sender µs>..],"psr_db":[..]}]}
```

- The microphone is the sender's (luchd: PipeWire capture, its timestamps mapped to the pts clock) or a phone
  running Luch with `"mic":true` (Android `AudioRecord.getTimestamp`, API 24, mapped through its own clock estimate).
  Correlation runs where the sound is recorded; only times travel.
- Probes are time-divided: receiver k plays its `n` sweeps starting at `at + k·(n·(ms + gap_ms) + 500)`, so only
  one device sounds at a time. The 2–6 kHz log sweep avoids most fan and hum energy (below 1 kHz) and is reproduced
  by every small speaker.
- Detection: band-pass 1.5–7 kHz, matched filter with the sweep over a ±150 ms window around the expected time;
  the first peak within 10 ms before the maximum that is at least half of it is the direct path (reflections come
  later and can be stronger); parabolic interpolation for the sub-sample time. Confidence: peak-to-sidelobe ratio
  ≥ 12 dB for each sweep, and the `n` arrivals agree within 0.5 ms after removing the schedule. Otherwise one retry
  at gain 0.6, then the device is marked "not measured".
- Result per device: `c_k = median(heard − played)` (acoustic output delay after the DAC plus travel to the
  microphone). The sender sets `offset_ms` so that all `c_k` become equal to the largest; relative values are what
  matter, so the microphone's own capture latency cancels.
- Picture: the device with role `video` or `both` that plays through its own speaker is the reference for its
  picture (TVs match their own speakers to their panel). For a projector or TV whose sound goes elsewhere, its
  picture is unmeasured by sound: use O10 or the slider.
- Fallback chain when calibration is impossible or rejected: (1) O9 values alone; (2) the slider, pre-set from the
  route table of section 5; (3) O10 for picture vs sound. The UI says which one is in use per device.
- It makes sound, so it only ever runs when the user presses the button; never in tests on real devices at night.

### 6.8 Unicast or multicast for 20 receivers on Wi-Fi

- Multicast frames on Wi-Fi go at the basic rate (often 6 or 24 Mbit/s), are not acknowledged or retried, and to
  stations in power save are held until the next DTIM beacon (RFC 9119: loss of 5 % or more is common). Many APs
  convert multicast to unicast per station (OpenWrt `multicast_to_unicast`, vendor "multicast optimisation"),
  which then costs what unicast costs. Android needs a `WifiManager.MulticastLock` to receive multicast at all.
- Airtime, roughly (each Wi-Fi frame costs about 100–150 µs of contention, preamble and ACK besides its payload):

| Stream × 20 receivers | Unicast | Multicast at 24 Mbit/s basic rate |
|---|---|---|
| Opus 128 kbit/s, 5 ms frames (200 pkt/s each) | about 20 × 3 % = 60 % airtime: too much | about 3 %, but lossy, DTIM-delayed |
| Opus 128 kbit/s, 20 ms frames (50 pkt/s each) | about 20 × 0.8 % = 15 % | under 1 % |
| Video 10 Mbit/s | 20 × 2–4 % at high MCS = 40–80 % | about 45 %, 1–5 % loss, needs FEC |

- Recommendation: unicast by default (works on every AP, retries, high rates), 5 ms Opus only for one interactive
  speaker, 20 ms Opus in group mode. Multicast with FEC and `D` ≥ 300 ms as an option for many audio receivers on
  an AP that does not convert it. Many TVs with video: media mode (each fetches), or Ethernet, where multicast with
  IGMP snooping is free.

### 6.9 Watching mode details, joining and leaving

- Opus 20 ms frames in watching mode (4× fewer packets than 5 ms, see 6.8); 5 ms stays for interactive.
- Loss inside `D`: the receiver asks `{"t":"nack","kind":2,"seq":[..]}` on the control channel for a frame missing
  when the next one arrives, at most once per frame; the sender keeps the last 500 ms of packets and resends.
  FEC parity shards (section 7 of the protocol) only for multicast.
- Joining: clock burst (0.5 s); the receiver starts presenting at `pts + D` from the next frame (audio) or the
  next keyframe (video; it asks for one as today); then it reports `need_ms`. If its need exceeds `D`, `D` rises
  once for everybody: one hard resync of the difference on every device, at the same frame.
- Leaving: nothing changes for the others; `D` may relax later (6.6).
- Many TVs with one video: media mode with O11 (`media_cmd` with `"at":<sender µs>,"pos":<s>`; each TV keeps
  `|position error| < 20 ms` with `setPlaybackParams` speed within ±0.5 %, and seeks beyond 200 ms).

## 7. Phased implementation plan (smallest valuable step first)

0. **True output latency on one TV, no protocol change** (receiver): `getTimestamp`-based `audio_ms`,
   `OnFrameRenderedListener`-based `video_ms`, `sync` holding by `max(0, audio_ms − video_ms − tolerance)`. Fixes
   today's `sync`, which holds by the buffer size only.
1. **Clock** (6.2, 6.3; kind 3 on both sides; `clock` in `stats`). Then measure real Wi-Fi.
2. **`delay` in the header and the schedule on one receiver** (6.4, 6.5): single-TV sync through the general
   mechanism; `offset_ms` and the slider (O2) come with it.
3. **TV + separate speaker** (`group`, roles, `need_ms`, D on the sender, 6.6). The interactive case.
4. **Watching mode** (6.9): D ≥ 150 ms, Opus 20 ms, NACK, joining and leaving.
5. **Acoustic calibration** (6.7), sender microphone first, then a phone with Luch as the microphone.
6. **Many receivers**: O11 media-mode groups; multicast with FEC as an option.

## 8. Test plan

### 8.1 Simulator (silent, deterministic, the main proof)

A plain-Java simulator in the receiver's JVM tests (`testFdroidDebugUnitTest`, no libraries), seeded and
deterministic, running the real estimator, audio loop and video scheduler classes against fakes:

- **Clocks**: each device has `local = true·(1 + skew) + offset`, skew drawn from ±100 ppm (a ±500 ppm stress
  case), offset ±10 s; a separate DAC clock per device with its own ±100 ppm.
- **Network** per direction: base 1.5 ms + gamma jitter (mean 1 ms) + 2 % outliers of 10–100 ms + power-save
  bursts (packets held to the next 102.4 ms beacon) on one direction only for some devices (asymmetry); a "busy
  2.4 GHz" profile with base 3 ms and 5 % outliers; 1 % loss; a 3 s outage once.
- **Devices** (profiles from section 5): phone (timestamps, 15 ms), TV (timestamps, +60 ms hidden ARC delay),
  projector (+80 ms hidden panel delay, a fan in its microphone recording), tablet (no timestamps, `head`, ±20 ms
  hidden), Bluetooth speaker (200 ms reported, 30 ms of it hidden).
- **Room** for calibration: each device's sweep convolved with a random room response (direct path + exponential
  tail, RT60 0.3–0.8 s, early reflections up to 6 dB stronger than the direct path), plus pink noise and 120 Hz
  hum harmonics (fan) at SNR −5 to +20 dB, travel time from random distances 1–6 m.

Pass criteria:

| Test | Pass |
|---|---|
| Clock, 5 GHz profile | offset error p99 < 0.5 ms after 5 s; skew error < 2 ppm after 60 s |
| Clock, busy 2.4 GHz | offset error p99 < 1.5 ms after 10 s |
| Clock, outage | back within its steady bound 1 s after the network returns |
| Audio, all devices with timestamps, 4 h run | inter-device skew of presentation p99 < 1 ms, max < 2 ms; no accumulated drift (> 1 ms) |
| Audio, stuffing | rate ≤ 0.1 % always; no hard resync after the first except at injected outages and `D` steps |
| Join | a new device within 1 ms of the others 3 s after its first packet; others unaffected unless `D` rose |
| `D` step | exactly one hard resync per device, on the same frame (same pts) |
| Interactive TV + speaker | sound minus picture within [−2, tolerance + 2] ms; picture not held when the speaker is fast |
| Watching, video | presentation within ±(P/2 + 2) ms of target |
| Calibration, SNR ≥ 0 dB | each device's correction within 0.5 ms of the hidden truth |
| Calibration, any SNR | never accepted with an error > 2 ms (false acceptance < 1 % over 10 000 rooms); rejected cases fall back |
| Whole mix, calibrated | all audio within 2 ms of each other at the microphone, every picture within tolerance of its sound |
| Whole mix, fallback (no calibration) | error equals the hidden delays not covered by O9 (the simulator reports them: this documents the fallback's cost) |

- **Sender side** (Rust unit tests in luchd): kind 3 replies (parsing, t2/t3 on the pts clock), `delay` written per
  kind, the D policy of 6.6 driven by scripted `need_ms` sequences.
- **End to end** (Docker): luchd plus several `luch-recv` instances (once they implement the same schedule) on a
  bridge with `tc netem` and `libfaketime` rate factors; receivers write PCM with presentation times to files,
  never to a sound card; a script cross-correlates them. Pass: inter-receiver offset < 1 ms over 30 minutes.
- **Android emulator**: protocol only (kind 3 echo, `delay` parsing, new `stats` fields); the emulator with
  `-no-audio` has no real audio clock.

### 8.2 Real devices (later, by day, with consent: it makes sound)

Refined by what the implementation showed (section 9). Everything below is silent except steps 5–7.

1. **Clock on real Wi-Fi** (silent). `luchctl group watch "Living room TV"=both <phone>=audio`, then
   `luchctl stats`: per member `clock.err_us` and `rtt_us`, on 5 GHz and 2.4 GHz, idle and with a download running.
   Expected: `err_us` 0.3–1.5 ms (half the best round trip plus 0.1–0.3 ms), `skew_ppm` steady within 2 ppm after
   90 s. The emulators gave 0.12–0.17 ms over their virtual network.
2. **The TV's own numbers** (silent: volume 0, or a muted HDMI sink). `out.audio_src` must be `timestamp` on Android
   7+; note `out.audio_ms` per route (built-in, HDMI, ARC soundbar, Bluetooth) and `out.route`. Expected 20–60 ms
   built-in, more on Bluetooth if the delay report reaches the timestamps (then `route_preset_ms` must not add 150).
3. **Picture release** (silent). On a TV in a watch group, `out.video_err_ms`: expected within ±P/2 (±8.3 ms at
   60 Hz). The emulator reported −12 to −18 ms (it stamps renders before the time asked for); a TV that also reads
   about −P means its callback is early too, and only step 6 tells the truth.
4. **D in practice** (silent). Watch `group.d_audio_ms` in `Stats` for 10 minutes per mode: how often it rises and
   by how much (the creep of 6.6), and the members' `audio_resyncs` (expected: none after the first 15 s).
5. **Two speakers, two microphones** (sound, by day). Two Luch receivers with timestamps in a watch group, a stereo
   USB interface with two identical microphones 10 cm from each speaker, a click a second; cross-correlate the two
   channels per click. Expected < 1 ms, flat over 30 minutes (the simulator: p99 0.41 ms over 4 h). Without the
   group the same pair drifts 6 ms a minute at 100 ppm.
6. **Picture against sound** (sound and a flash, by day). A 240 frames/s phone camera on the TV and the speaker in an
   interactive group; luchd shows a white flash with a click. Expected: sound 0 to 20 ms behind the picture; with a
   Bluetooth speaker, the picture held by its excess (`d_video_ms` = `d_audio_ms` − 20).
7. **Calibration** (sound, by day, only after steps 1–6). Turn on the receiver's dry-run setting, check its dry run,
   then the real run (the code for playing the sweeps is the next step: section 9). Expected: each device within
   0.5 ms of step 5's two-microphone figure, and in the projector's room with its fan on: accepted within 0.5 ms or
   refused, never wrong (the simulator: 0 wrong over 10 000 rooms).

## 9. What was built and measured (branch `sync`)

**Receiver (luch-android).** `ru.toxblh.luch.sync`, plain Java and all tested on the JVM: `ClockEstimator`,
`AudioClock` (getTimestamp → when a frame is heard, r fitted), `AudioPlayout` (pts + D, PI with feed-forward,
stuffing, fades, concealment), `VideoScheduler`, `SenderPolicy`, `Wire` (kind 3, D), `Probe` and `Fft` (the
calibration's detector), `DryRun`. On Android: `SyncSession` (the group, the clock on the media socket, stats),
`AudioOut`, and `MirrorSession`'s scheduled paths. Phase 0 is in too: without a group, `sync` holds the picture by
what the sound really takes (`out.audio_ms − out.video_ms`, from timestamps and render callbacks); on the API 34
emulator that is about 75 ms where the buffer said 20 to 60.

**Sender (luchd).** `sync.rs` (clock replies, the same policy and test vectors, route presets, the calibration's
arithmetic), groups in `mirror.rs` (one capture, each member's streams by role on its own sockets, D per packet,
clock answered with the pipeline's running time, the KMS path's pts moved onto it), D-Bus `MirrorGroup` and
`SetOffset`, `luchctl group` and `offset`. Older receivers get no group and D = 0 (luch-recv's test reads a newer
sender's packets as before); a receiver gets no clock request from an older sender (on-device test).

**Simulator** (seeded, the receiver's own classes against fake clocks, Wi-Fi and DACs):

| Test | Pass | Measured |
|---|---|---|
| Clock, 5 GHz, 40 devices, from 5 s | p99 < 0.5 ms | p99 0.24 ms, max 0.60 ms |
| Clock, busy 2.4 GHz, from 10 s | p99 < 1.5 ms | p99 0.46 ms |
| Clock, ±500 ppm crystals | p99 < 0.5 ms | p99 0.22 ms |
| Clock, 3 s outage | in bound 1 s after | p99 0.14 ms from 1 s after |
| Skew | < 2 ppm (asked after 60 s) | 1.5 ppm worst after 90 s; p50 1.0, worst 3.2 ppm at 60 s |
| Audio, 4 devices with timestamps, 4 h | p99 < 1 ms, max < 2 ms, no drift | p99 0.41 ms, max 0.48 ms, drift < 10 µs, 0 resyncs, rate ≤ 188 ppm |
| Join at 60 s | within 1 ms 3 s after | p99 0.51 ms |
| D step 300 → 350 ms | one resync, same frame | one each, all on chunk 19800 |
| Picture, watching, 60 and 50 Hz | ±(P/2 + 2) ms | mean 4.1 ms, 10 of 27 341 frames outside (late packets) |
| Interactive TV + speaker | sound − picture in [−2, 22] ms | p50 18.7 ms, picture not held |
| Calibration, 1000 rooms, 0–20 dB | within 0.5 ms | p99 0.04 ms, worst 0.07 ms, 99.6 % accepted |
| Calibration, 10 000 rooms, −5–20 dB, 15 % clipped | none wrong > 2 ms | 0 wrong, worst accepted 0.72 ms |
| Calibration, noise in the sweep's band | refuse, never wrong | measures to −10 dB, refuses from −20 dB, 0 wrong |
| Whole mix, fallback | documents its cost | 80 ms apart at the microphone (the hidden 60/80/30/15 ms) |
| Whole mix, calibrated | within 2 ms | 0.01 ms apart (mean), every device + 93 ms |

**Two emulators** (API 34 TV, `-no-audio`, through adb and the emulator's UDP redirection; `tools/syncsend.py` as
the sender, digital silence): watch group, TV `both` + speaker `audio`, D 250 → 300 ms at 25 s: clock `err_us`
0.16 / 0.17 ms (rtt 0.19 / 0.20 ms), sound `sync_err_ms` median 0.00 / 0.01 ms (−0.02..0.12), the D step one resync
on both on the same chunk (5011, −50 ms), the picture −12.3 ms (the emulator's early render stamps). Interactive
group, TV `video` + speaker `audio`: the TV's picture at 12 ms from pts; the emulator's sound path grew to 120 ms, so
D_a rose to 126 ms and the picture was held to 106 ms (20 ms tolerance); the speaker then steady at 0.02 ms.

**Changed against sections 6 and 8:** the estimator (6.3), the playout's additions (6.4), the skew criterion (90 s,
not 60), requests 4 a second, a re-burst on the residual. The calibration's detector picks the earliest local
maximum of a third of the largest within 50 ms before it (a reflection 6 dB louder is then still found after the
direct sound), and measures the sidelobes as all of the window before the direct sound, where only noise can be.

**Not done yet, and why:** playing the sweeps and recording them (it makes sound: the receiver answers `probe` only
as a dry run, behind a setting that stays off; luchd has no microphone capture yet; the phone as a microphone needs
`AudioRecord` with its timestamps); NACK and Opus 20 ms for watching (6.9); media-mode position sync (O11);
multicast. Nothing here could check a real DAC, panel or Wi-Fi: that is 8.2.

## 10. Bibliography

- Snapcast README and design: https://github.com/badaix/snapcast
- Snapcast binary protocol (Time message): https://github.com/badaix/snapcast/blob/develop/doc/binary_protocol.md
- Snapcast client time filter and drift correction: https://github.com/badaix/snapcast/blob/develop/client/time_provider.cpp,
  https://github.com/badaix/snapcast/blob/develop/client/stream.cpp
- Shairport Sync (latency, stuffing, timing): https://github.com/mikebrady/shairport-sync/blob/master/README.md
- Shairport Sync, AirPlay 2 stream types: https://github.com/mikebrady/shairport-sync/blob/master/AIRPLAY2.md
- NQPTP: https://github.com/mikebrady/nqptp
- Apple TV Wireless Audio Sync: https://support.apple.com/guide/tv/atvb228b7711/tvos ,
  https://9to5mac.com/2019/06/18/ios-13-uses-your-iphone-microphone-to-fix-apple-tv-audio-sync-issues/
- Google Cast group delay correction: https://support.google.com/chromecast/answer/6318642
- Sonos line-in audio delay: https://support.sonos.com/en-us/article/adjust-line-in-settings
- SlimProto (squeezelite/LMS sync commands): https://wiki.lyrion.org/index.php/SlimProto_TCP_protocol.html
- ROC toolkit FEC: https://roc-streaming.org/toolkit/docs/internals/fec.html ; latency tuner and resamplers:
  https://gavv.net/articles/roc-0.3/
- PipeWire RTP modules: https://docs.pipewire.org/page_module_rtp_sink.html , https://docs.pipewire.org/page_module_rtp_source.html
- AES67 (AES67-2023, high-performance streaming audio over IP): https://aes2.org/standards-store/?id=96
- RFC 7273, RTP clock source signalling: https://www.rfc-editor.org/rfc/rfc7273
- RFC 3550, RTP (sender reports, 6.4.1): https://www.rfc-editor.org/rfc/rfc3550
- RFC 5905, NTPv4 (clock filter): https://www.rfc-editor.org/rfc/rfc5905
- IEEE 802.1AS-2020 (gPTP, 802.11 transport): https://standards.ieee.org/ieee/802.1AS/7121/
- Chen, Yang: Understanding Precision Time Protocol in Today's Wi-Fi Networks, USENIX ATC 2021:
  https://www.usenix.org/conference/atc21/presentation/chen
- Geng et al.: Exploiting a Natural Network Effect for Scalable, Fine-grained Clock Synchronization (Huygens),
  NSDI 2018: https://www.usenix.org/conference/nsdi18/presentation/geng
- Ibrahim et al.: Verification: Accuracy Evaluation of WiFi Fine Time Measurements on an Open Platform:
  https://winlab.rutgers.edu/~vietnh/docs/p417-ibrahim.pdf
- RFC 9119, Multicast Considerations over IEEE 802 Wireless Media: https://www.rfc-editor.org/rfc/rfc9119
- WebRTC A/V sync: https://webrtc.googlesource.com/src/+/refs/heads/main/video/stream_synchronization.cc
- Bluetooth LE Audio (presentation delay, BASE): https://www.bluetooth.com/wp-content/uploads/2022/01/Introducing-Bluetooth-LE-Audio-book.pdf
- ITU-R BT.1359-1, relative timing of sound and vision: https://www.itu.int/dms_pubrec/itu-r/rec/bt/R-REC-BT.1359-1-199811-I!!PDF-E.pdf
- Audio-to-video synchronization (EBU R37 figures): https://en.wikipedia.org/wiki/Audio-to-video_synchronization
- Precedence effect: https://en.wikipedia.org/wiki/Precedence_effect
- HDMI Latency Indication Protocol: https://hdmi.org/spec2sub/lip
- Android `AudioTrack.getTimestamp`: https://developer.android.com/reference/android/media/AudioTrack#getTimestamp(android.media.AudioTimestamp)
- Android `MediaCodec.releaseOutputBuffer(int, long)`: https://developer.android.com/reference/android/media/MediaCodec#releaseOutputBuffer(int,%20long)
- Android `AudioRecord.getTimestamp` (microphone capture time): https://developer.android.com/reference/android/media/AudioRecord#getTimestamp(android.media.AudioTimestamp,%20int)
- Android `MediaPlayer.getTimestamp`: https://developer.android.com/reference/android/media/MediaPlayer#getTimestamp()
- Android `Choreographer`: https://developer.android.com/reference/android/view/Choreographer
- Peng et al.: BeepBeep: a high accuracy acoustic ranging system using COTS mobile devices, SenSys 2007:
  https://doi.org/10.1145/1322263.1322265
- Farina: Simultaneous measurement of impulse response and distortion with a swept-sine technique, AES 108th
  Convention, 2000: https://aes2.org/publications/elibrary-page/?id=10211
- Knapp, Carter: The generalized correlation method for estimation of time delay, IEEE TASSP 1976:
  https://doi.org/10.1109/TASSP.1976.1162830
- libfaketime (clock rate factors for tests): https://github.com/wolfcw/libfaketime
