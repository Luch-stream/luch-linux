# Latency lab: photon-to-photon latency with a phone

`luchctl stats` shows luchd's estimate: capture, encode, network and decode. It cannot see the two displays, the
receiver's compositor or the TV's picture processing, and those are often the larger part. The latency lab measures
all of it, from the light of the computer's screen to the light of the TV, with nothing but a phone that films at
240 fps. The result is a report to attach to an issue: the numbers and both ends' setup, read from the video itself.

## How it works

- **The computer** shows a page (`luchctl latency on`) with a QR code and two rows of black and white cells. The cells
  are a 12-bit counter in Gray code that goes up once per refresh of that screen; the QR code says how the stream is
  set up (luchd version, capture mode, codec, bitrate, stream size, ping, luchd's own latency figures, the page's
  measured refresh and missed refreshes).
- **The TV** shows that page mirrored, and the receiver draws its own pattern in the bottom right corner: the same kind
  of counter, going up once per refresh of the TV, and a QR code with the model, Android version, decoder, display mode,
  measured refresh and whether the low-latency display mode was asked for (PROTOCOL.md, section 4j).
- **The phone** films both screens at once. `luchctl latency analyze` finds the three QR codes in the video, reads the
  counters in every frame and, for every frame of the computer's that reached the TV, takes the time from the computer
  showing it to the TV showing it.

Why Gray code and not a clock with milliseconds: from one number to the next exactly one cell changes. A camera frame
taken while a screen changes (the exposure spans the change, a slow panel, a tear, the camera's rolling shutter) sees
that one cell grey and reads either the old number or the new one, never a third; a blurred decimal clock is
unreadable. How grey the changing cell is says when, within the camera frame, the change happened, so the analyser
places changes to a fraction of a camera frame (4.2 ms at 240 fps). The computer's counter is a count of its vsyncs,
a straight line in time, so its frames are placed better still.

The TV's own pattern goes through the same compositor and panel as the picture, so the report also tells the TV's real
refresh rate and how evenly it draws.

## What you need

- A phone that records **240 fps** for real (iPhone: Camera → Slo-mo, 1080p at 240 fps; Android: "slow motion" at
  240 fps; not 960 fps "super slow motion", which is interpolated).
- `ffmpeg` (with `ffprobe`) on the computer that analyses the video; luchd is not needed there:
  `luch-latency analyze` is the same analyser without D-Bus.
- The receiver app with the latency lab (it lists `latency` in its welcome). With an older one the latency is measured
  all the same; only the TV's own pattern and its setup are missing.

## Procedure

1. Start mirroring in the mode to measure, e.g. `luchctl mirror fast` (or `monitor` for the portal).
2. `luchctl latency on`. The page opens in the browser: click it for full screen. The TV shows the page, and the
   receiver's pattern appears in its bottom right corner. Leave the mouse alone: the cursor is part of the picture.
3. Place the phone (see below), start recording, hold still for 10 seconds, stop.
4. Copy the **original** file to the computer (iPhone: AirDrop with "All Photos Data" in Options, or a cable; a
   message or a share sheet re-encodes it at 30 fps, which the analyser notices and corrects, but the original is
   better).
5. `luchctl latency analyze VIDEO.mov` prints the report and writes `VIDEO.latency.md` and `VIDEO.latency.json`.
6. For a comparison, repeat from step 1 in the other mode with the phone where it was.
7. `luchctl latency off`.

### Placing the camera

- **Still**: a tripod, or the phone leaning on something. A hand-held phone works only if it hardly moves.
- **Both screens large in the picture**: the analyser has to read the QR codes. The computer's QR code should be at
  least about 120 pixels tall in the video (each of its squares 3 pixels or more). Put the laptop in front of the TV,
  close to the phone, the TV behind it; both screens whole, neither covering the other's patterns (the computer's on its
  left, the TV's in its bottom right corner).
- **Side by side, at the same height in the picture**: a phone's sensor reads its rows one after the other (rolling
  shutter, up to about 4 ms at 240 fps). Two screens at different heights are seen up to that much apart. The report
  warns when they are; `--readout-ms` corrects it when the phone's readout time is known.
- **Straight on**, within about 45°, and no lamp reflected in either screen.

### Camera settings

- 240 fps, 1080p. HDR, night mode and stabilisation off.
- Exposure and focus locked (tap and hold on the TV until "AE/AF lock"): the analyser judges every cell against the
  white and black of the same pattern in the same frame, but a changing exposure makes the change of a cell look
  like a change of light.
- A dim room and both screens bright: the cells then go from black to white, not from grey to grey. The shutter is
  at most 1/240 s at 240 fps; that is the best case (a frame caught mid-change is then a measurement).

## The report

It reads like this (the numbers here are made up):

```
## Luch photon-to-photon latency

**32.4 ms** median from the computer's screen to the TV's, 41.0 ms at the 95th percentile (583 frames; one camera
frame is 4.2 ms).

| min | p5 | p50 | p95 | max | mean | jitter (sd) |
...
luchd's own estimate (capture, encode, network, decode) was 13.5 ms: the remaining 18.9 ms are the two displays and
the receiver's compositor.
```

Then what was measured (camera, the computer's refresh as filmed, the share of frames the TV showed, the TV's refresh
as filmed, the TV's refreshes the receiver did not draw, the TV's frame pacing) and the setup of both ends from their
QR codes. The JSON has the same and every frame's latency (`samples`: counter, ms).

What the number includes: from the computer's panel lighting up with a frame to the TV's panel lighting up with it.
The computer's own panel delay is on both sides of the measurement and cancels; everything after it counts: in `fast`
mode the capture reads the very buffer being scanned out, then encode, Wi-Fi, decode, the receiver's compositor, the
wait for the TV's next refresh and the TV's picture processing.

Warnings worth acting on:

- *found only once*: the TV's copy of the computer's pattern is not in the picture, or not readable (too small, a
  reflection).
- *the video's clock runs N times slower*: a slowed-down copy; the analyser corrects it from the computer's measured
  refresh.
- *at different heights*: see Placing the camera.
- *Some frames came out on the TV before the computer*: rolling shutter, or the screens swapped by a mirror.

## What to expect

| | `fast` (KMS) | `monitor` / `window` / `virtual` (portal) |
|---|---|---|
| luchd's estimate (`e` in the QR code, `luchctl stats`) | ≈ 11–14 ms: ≈ 5–6 sender, ≈ 2 network, ≈ 4–6 decode | ≈ 20–35 ms: ≈ 15–25 sender |
| displays and compositor (report: "the remaining") | the same in both modes for one TV | |
| photon-to-photon p50 | ≈ estimate + half a refresh to a refresh of the TV + the TV's processing | ≈ 10–20 ms more than `fast` |
| jitter (sd) | ≈ 3–5 ms at 60 Hz: the TV's refresh is not locked to the computer's | similar, more with a busy desktop |

On a TV in its game mode (the receiver asks for it on Android 11+, `g=1`) the picture processing is typically 5–20 ms;
outside it 50–100 ms and more, which then dwarfs everything else, so check `g` and the TV's picture mode first. The
check that the lab works: the remaining part (measured minus luchd's estimate) is the same in both modes within a few
milliseconds, while the measured difference matches the difference in sender time (≈ 10–20 ms).

## How it is proven without devices

`cargo test -p luch-latency` films a synthetic laptop and TV (`luch-latency/src/synth.rs`): the TV shows each frame a
known time later, with jitter, dropped frames, a tearing 50 Hz TV, slow panels, a rolling shutter, optical blur,
noise and perspective; the true latency of every frame is known. The analyser must find it within one camera frame
(it finds the median within about 0.4 ms, and through H.264 compression of a real video file too). `luch-latency synth
OUT.mp4 --latency-ms 30` writes such a video to try the analyser by hand. The receiver's pattern is checked on an
Android TV emulator (`LatencyLabTest`: the counter read back from the screen goes up once per refresh), and its QR
code is bit for bit the sender's (`LatencyPatternTest`); `luch-latency scan IMAGE` reads the patterns in a screenshot.
