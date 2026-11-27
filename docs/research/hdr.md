# HDR in Luch: research and a design

Status: research, November 2026. Nothing here is implemented yet. Protocol sections refer to
`luch-android/PROTOCOL.md`.

Two different problems:

1. **Media casting** (section 4b): the TV plays the file or stream itself. HDR is passthrough: the job is to pick
   the right source, let the platform player decode it, and say truthfully what the TV can show.
2. **Mirroring a desktop** (section 4): the sender captures, encodes and sends. HDR means capturing 10-bit PQ
   pixels, encoding HEVC Main10 (or AV1 10-bit) with the right colour signalling, and the receiver switching its
   display to HDR, with a tone-mapped SDR fallback.

## 1. Summary

- **Formats**: HDR10 = PQ (SMPTE ST 2084) + BT.2020 + static metadata (mastering display, MaxCLL/MaxFALL).
  HLG = a backward-compatible transfer, no metadata (broadcast). HDR10+ = HDR10 + per-frame dynamic metadata.
  Dolby Vision = a base layer plus an RPU of dynamic metadata, decoded by a vendor (Dolby-licensed) decoder;
  profile 8.1 has an HDR10-compatible base layer, 8.4 an HLG one, profile 5 has no fallback at all, profile 7 is the
  dual-layer Blu-ray form most Android devices do not play.
- **Media casting: recommendation**. Report capabilities exactly (display HDR types per mode, decoder HDR
  profiles including Dolby Vision), let `src` items carry an `hdr` hint, choose the source by them, and leave
  decoding and display switching to `MediaPlayer` (it already does it on HDR TVs). Small receiver change, no sender
  work. Never transcode on the sender.
- **Mirroring: recommendation**. HDR10 only (PQ, BT.2020, static metadata), HEVC Main10 through VA-API with P010
  surfaces, captured from the KMS scanout by `luch-kms` when the compositor drives the output in HDR (the Sunshine
  path: read the connector's `HDR_OUTPUT_METADATA`), with the colour description in the HEVC VUI and the mastering
  and light-level SEI (FFmpeg's `hevc_vaapi` writes both: `sei=hdr`). Use it only when the receiver says it can show
  HDR10 and decode Main10 in hardware; otherwise send SDR, tone-mapped by the compositor (portal path) or by one
  extra step in luch-kms's existing shader (BT.2390 EETF). Dolby Vision for mirroring: no; HLG: not needed.
- **Latency cost**: about 0–1 ms on the sender (same shader passes, 10-bit surfaces), nothing on the network, nothing
  in decode on a hardware Main10 decoder; a one-time display mode switch (from a moment up to 1–3 s on HDMI). The
  unknown is the TV's own processing in HDR: measure it, do not assume.
- **What the owner saw on the Steam Deck stream** was almost certainly an HDR10 stream that the *playback device*
  sent to the TV as a Dolby Vision signal, not Dolby Vision content (section 4).

## 2. Media casting (the TV plays the file)

### 2.1 What Android needs

- Display: `Display.getHdrCapabilities().getSupportedHdrTypes()` (API 24) lists `HDR_TYPE_DOLBY_VISION` (1),
  `HDR_TYPE_HDR10` (2), `HDR_TYPE_HLG` (3), `HDR_TYPE_HDR10_PLUS` (4, API 29) and `HDR_TYPE_HLG_PLUS` (6, API 37).
  It is deprecated in API 34 in favour of `Display.getMode().getSupportedHdrTypes()`, which is per display mode:
  a TV may do HDR at 4K60 but not at 4K120. Luminance hints: `getDesiredMaxLuminance()` and friends (from EDID).
- Decoder: `MediaCodecInfo.CodecCapabilities.profileLevels`: `HEVCProfileMain10HDR10`, `HEVCProfileMain10HDR10Plus`,
  `VP9Profile2HDR`, `AV1ProfileMain10HDR10`(+`Plus`), and the `video/dolby-vision` decoder with
  `DolbyVisionProfileDvheSt` (profile 8), `DvheStn` (5), `DvheDtb` (7), `DolbyVisionProfileDvav110` (AV1, 10).
- Player: the platform `MediaPlayer` (what `MediaActivity` uses) picks the decoder from the container and switches
  the display itself when an HDR surface is shown. The AOSP HDR page warns that "only tunneled decoders are
  guaranteed to play HDR"; non-tunneled playback may be flattened to SDR on some devices. That is the platform's
  business in media mode; in mirroring it matters (section 3.4).
- Dolby Vision: a vendor decoder (`video/dolby-vision`) takes base layer, enhancement layer and RPU in one access
  unit; the extractor may expose a DV track and an HEVC base-layer track with the same id. With no DV decoder,
  profile 8.1 still plays as HDR10, profile 5 does not play correctly at all (green and purple picture).
- Android 14 adds HDR conversion (`HdrConversionMode`: passthrough, system, force with a preferred type): the
  system can output another HDR type than the content's. Only system apps may set it; a receiver can read
  `DisplayManager.getHdrConversionMode()` to explain what the TV will see.
- Platform tone mapping: an HDR file on an SDR-only display is tone-mapped by the decoder or the compositor on
  Android 13+; on older or simpler devices it may look washed out. That is why the sender should prefer an SDR
  source for an SDR TV when the page offers one.

### 2.2 What Luch has and lacks

- `Caps.java` reports `display.hdr` from `getHdrCapabilities()` (dolby_vision, hdr10, hlg) and `10bit` per decoder.
  Missing: HDR10+ (type 4) is not reported; on API 34+ the deprecated call ignores the current mode; Dolby Vision
  decoder profiles are not listed; `HEVCProfileMain10HDR10` is folded into `10bit` without saying "HDR10-capable".
- Section 4b chooses among `src` by MIME, codec and size only.

### 2.3 Design

- welcome `hw.display.hdr`: the types of the **current** mode (API 34+: `Display.Mode.getSupportedHdrTypes()`),
  plus `"hdr10plus"`; `hw.video["video/hevc"].hdr`: `["hdr10","hdr10plus"]` from the profiles;
  `hw.video["video/dolby-vision"]`: `{"profiles":[5,8]}` when a DV decoder exists.
- `src` items may carry `"hdr":"sdr"|"pq"|"hlg"|"hdr10plus"|"dv5"|"dv8"` (from the page: `<source>` media
  queries, HLS `VIDEO-RANGE` and `SUPPLEMENTAL-CODECS`, DASH essential properties, or a probe of a local file by
  luchd with `ffprobe`). The receiver ranks: playable first (decoder and display), then the best dynamic range the
  display shows, then size. Without the hint, today's order.
- `media_state` reports `"hdr"` of what plays (from the decoder output format), so the sender can show "HDR10" and a
  test can check it.
- HLS: Android's platform HLS player chooses variants by bandwidth; we found nothing saying it filters
  `VIDEO-RANGE`. To test on a real TV with a master playlist that has PQ and SDR variants.
- For YouTube the TV's own app (section 4f) does HDR itself.

## 3. Mirroring a desktop in HDR

### 3.1 Capture on Linux

- Compositors: GNOME 48 (Mutter) and KDE Plasma 6 drive monitors in HDR and implement the Wayland colour management
  protocol (`wp_color_management_v1`, wayland-protocols 1.41). Screen casting through the portal is a different
  matter: we found no evidence that Mutter or KWin deliver 10-bit PQ buffers with colour metadata to PipeWire
  clients. A KDE user report (February 2026) reads KWin's screencast as offering only 8-bit DRM formats, and KWin
  maps HDR screencasts into SDR (bug 496185, fixed with a relative-colorimetric transform). PipeWire's
  `spa_video_info_raw` already carries transfer function, matrix and primaries, so a portal HDR path can be added
  later without a Luch protocol change: luchd should look at the offered formats and use 10-bit ones
  (`XRGB2101010`, `ABGR16161616F`) with PQ tags if they ever appear.
- KMS capture (`luch-kms`, already the lowest-latency path, Sunshine's design): when the compositor outputs HDR,
  the scanout framebuffer is already 10-bit (typically `XRGB2101010`/`XBGR2101010`, sometimes FP16) in PQ BT.2020,
  and the connector property `HDR_OUTPUT_METADATA` holds the HDMI static metadata (EOTF, primaries, white point,
  max/min mastering luminance, MaxCLL, MaxFALL). Sunshine's `kmsgrab.cpp` does exactly this (`is_hdr()` reads the
  blob, EOTF 2 = PQ, 3 = HLG). That is the path for Luch: no compositor support needed.
- SDR content on an HDR desktop sits at the compositor's "SDR brightness" (BT.2408's reference white is 203 cd/m²);
  it is already inside the PQ pixels we capture.

### 3.2 Encode

- Convert in luch-kms's existing shader pass to P010 (10-bit 4:2:0, BT.2020 non-constant-luminance matrix, limited
  range) instead of NV12, and encode HEVC Main10 with VA-API (`vainfo` must list `VAProfileHEVCMain10` with
  `VAEntrypointEncSlice`; Intel since Kaby Lake, recent AMD VCN). AV1 10-bit (Intel Arc and newer, AMD RDNA3) is the
  later option. Same low-latency settings as now (one reference, no B-frames, IDR on demand).
- Signalling: VUI `colour_primaries = 9` (BT.2020), `transfer_characteristics = 16` (PQ), `matrix_coeffs = 9`,
  `video_full_range_flag = 0`; SEI mastering display colour volume and content light level, filled from
  `HDR_OUTPUT_METADATA`. FFmpeg's `hevc_vaapi` takes the colour fields from the codec context and writes the two SEI
  from frame side data (`AV_FRAME_DATA_MASTERING_DISPLAY_METADATA`, `..._CONTENT_LIGHT_LEVEL`), on by default
  (`sei=hdr`). Repeat them with every IDR (section 4 already repeats parameter sets there).
- Bitrate: about the same as SDR at the same quality; Main10 usually needs no more.

### 3.3 Protocol

- `start.video` gains `"hdr":{"transfer":"pq","primaries":"bt2020","max_lum":1000,"min_lum":0.005,"max_cll":..,
  "max_fall":..}` (absent = SDR, as today). The receiver answers `ready` only if it can show it, else
  `{"t":"error","code":"start_failed","detail":"hdr"}` and the sender falls back to SDR. Metadata changes (the
  user turns HDR off) come as a new `start`-like `{"t":"video_format",...}` before the next IDR, or simply a new
  session; the second is simpler and the first rare.
- The sender asks for HDR only when the welcome has `display.hdr` ∋ `hdr10` and `hw.video["video/hevc"].hdr`
  ∋ `hdr10` (hardware) and the user allows it.

### 3.4 Receiver

- Configure the decoder with `KEY_COLOR_STANDARD = COLOR_STANDARD_BT2020`, `KEY_COLOR_TRANSFER =
  COLOR_TRANSFER_ST2084`, `KEY_COLOR_RANGE = COLOR_RANGE_LIMITED`, `KEY_HDR_STATIC_INFO` (CTA-861.3 Type 1, 25 bytes)
  from `start`, even though the bitstream also carries them: some decoders only look at the format.
- Keep the low-latency decoder (non-tunneled). The AOSP caveat (non-tunneled may flatten HDR) is the risk; the
  bench should decode a short HDR10 clip once and check the output format's colour keys, and the welcome should say
  `hdr_mirror: true|false` from that, not from the profile list.
- The display switches to HDR when the HDR surface is shown; expect one dark moment and ask for a keyframe after
  it if the surface was recreated (the code already handles a recreated surface).

### 3.5 Tone mapping when the TV lacks HDR

- Portal capture: compositors already tone-map screencasts into SDR; nothing to do.
- KMS capture of an HDR scanout for an SDR TV: add the BT.2390 EETF (the curve libplacebo uses as `bt2390`) and a
  BT.2020→BT.709 gamut step to the existing RGB→NV12 shader: a few ALU operations per pixel, well under 0.1 ms
  on any GPU, no extra pass, no extra latency. Map the compositor's SDR reference white to SDR 100%, so text and
  windows look as they do in SDR and only highlights roll off. BT.2446 has heavier methods for broadcast; not
  worth it for a desktop.

### 3.6 Latency cost

| Stage | SDR now | HDR10 |
|---|---|---|
| Capture (KMS) | one import | same |
| Shader conversion | RGB → NV12 | RGB(PQ) → P010: same pass, 2 bytes per sample |
| Encode (VA-API) | H.264/HEVC | HEVC Main10: about the same, slightly more memory bandwidth at 4K |
| Network | — | same bitrate |
| Decode (TV) | hardware | hardware Main10: same |
| Display | — | one mode switch at start; TV processing in HDR game mode: to measure |

## 4. The Steam Deck "Dolby Vision" stream

- Valve's Steam Deck OLED HDR (gamescope) is HDR10: Steam's July 2026 beta "Added support for HDR streaming on
  Steam Deck OLED" with no mention of Dolby Vision; Moonlight 6.0 added HDR on the Deck and other Linux clients
  (HDR10, Vulkan renderer); Sunshine's Linux HDR is HEVC Main10 / AV1 10-bit with PQ static metadata from KMS.
  No open sender produces a Dolby Vision RPU, and gamescope has no Dolby license.
- A TV shows "Dolby Vision" when its *source device* sends a Dolby Vision signal. Many playback devices do that for
  everything: Android TV boxes with Dolby Vision output turned on stay in DV for HDR and SDR content (a Plex forum
  report), Amlogic boxes run "player-led" Dolby Vision (VS10), converting HDR10 to a DV signal for the TV (CoreELEC
  notes), and Android 14's HDR conversion "force" mode does the same by design. Streaming boxes like Apple TV or
  Chromecast with Google TV are known for staying in Dolby Vision when set to it.
- So the likely chain: HDR10 game stream → client box or TV app → the box's HDR output setting re-signals it as
  Dolby Vision (often "DV low latency", the player-led mode that games use). The picture is the same HDR10
  content; the badge says what the HDMI link carries.
- How to confirm, cheaply, next time: the TV's info banner and the box's display settings (output format, "match
  dynamic range"); on an Android client, `adb shell dumpsys display | grep -i hdr` and the stream's own statistics
  overlay (Moonlight shows the codec and HDR). Not with our devices tonight.
- What it means for Luch: nothing to copy. A Luch receiver on such a box will get the same re-signalling for free
  if the box does it; we send HDR10.

## 5. Phased implementation plan (smallest valuable step first)

0. **Truthful capabilities** (receiver): per-mode HDR types on API 34+, HDR10+, DV decoder profiles, `hdr` per
   HEVC/AV1 decoder; `media_state.hdr` of what plays. Useful at once for the sender's UI and every later step.
1. **Media source choice by `hdr`** (both sides, small): `src[].hdr` hints from the browser extension and luchd's
   file probe; the receiver ranks by them.
2. **HDR10 mirroring through luch-kms**: `HDR_OUTPUT_METADATA` → P010 shader → HEVC Main10 + VUI + SEI; `start.video.hdr`;
   receiver colour keys and `hdr_mirror` from a bench check.
3. **SDR fallback by tone mapping** in the same shader when the capture is HDR and the TV is SDR.
4. **Portal HDR** when GNOME or KDE offer 10-bit PQ screencast buffers (probe formats; no protocol change).
5. Later, if ever asked for: AV1 10-bit; HDR10+ dynamic metadata (`KEY_HDR10_PLUS_INFO`, API 29) for media relayed
   by the sender. Dolby Vision for mirroring: not planned.

## 6. Test plan

### 6.1 Simulated (no devices)

- **Signalling**: generate an HDR10 test clip with FFmpeg (a PQ grey ramp with patches at 100, 203 and 1000 cd/m²,
  `libx265` with `master-display` and `max-cll`); check with `ffprobe -show_frames` that VUI and SEI are as
  intended. Same check on luchd's output in Docker with a software encoder path (this Mac has no VA-API), and a JVM
  test that the receiver's `start.video.hdr` → `KEY_HDR_STATIC_INFO` bytes match CTA-861.3 Type 1 exactly.
- **Tone mapping**: a unit test of the EETF on known PQ values (reference white stays at SDR 100 %, 1000 cd/m²
  rolls to 100 %, monotonic, no clipping below the knee), and a golden-image test of the shader on a PQ ramp
  (software GL in Docker).
- **Capabilities**: unit tests of the capability mapping (all HDR type constants, API 24/29/34 branches, DV
  profiles) with fake `Display.Mode` and codec lists.
- **Emulator**: the Android TV emulator reports no HDR display: checks the SDR fallback and the `start_failed`
  answer; its software HEVC decoder can decode Main10 to check that the colour keys come back in the output format.

### 6.2 Real devices (later)

1. Media: play HDR10, HLG, DV profile 8.1 and 5 sample files and an HLS playlist with PQ and SDR variants on an
   HDR TV and an SDR projector; check the TV's info banner, `media_state.hdr`, and that profile 5 is refused where
   there is no DV decoder.
2. Mirroring: an HDR desktop (GNOME 48+ or Plasma 6 with HDR on) → luch-kms HDR10 → HDR TV: test pattern patches
   visible as distinct steps at 203 and 1000 cd/m²; the TV banner says HDR10; `dumpsys SurfaceFlinger` shows an HDR
   layer.
3. Latency: the existing latency test (flash and a phone camera at 240 frames/s) in SDR and HDR, same TV, game mode
   on; expected difference within one frame on the sender side; report any TV-side difference.
4. Fallback: same desktop to an SDR receiver: tone-mapped picture, UI white equal to the SDR session.

## 7. Bibliography

- AOSP, HDR video playback: https://source.android.com/devices/tech/display/hdr
- `Display.HdrCapabilities`: https://developer.android.com/reference/android/view/Display.HdrCapabilities
- `Display.Mode.getSupportedHdrTypes`: https://developer.android.com/reference/android/view/Display.Mode#getSupportedHdrTypes()
- `HdrConversionMode` (Android 14): https://developer.android.com/reference/android/hardware/display/HdrConversionMode
- `MediaFormat` colour and HDR keys: https://developer.android.com/reference/android/media/MediaFormat#KEY_HDR_STATIC_INFO
- `MediaCodecInfo.CodecProfileLevel`: https://developer.android.com/reference/android/media/MediaCodecInfo.CodecProfileLevel
- Dolby Vision profiles and levels: https://professional.dolby.com/siteassets/pdfs/dolby-vision-profiles-levels_v1.3.6_20210323.pdf
- dovi_tool (Dolby Vision RPU inspection): https://github.com/quietvoid/dovi_tool
- HLS `VIDEO-RANGE`, `SUPPLEMENTAL-CODECS` (RFC 8216bis draft): https://datatracker.ietf.org/doc/draft-pantos-hls-rfc8216bis/
- Linux DRM KMS properties (`HDR_OUTPUT_METADATA`): https://docs.kernel.org/gpu/drm-kms.html
- Sunshine KMS HDR capture: https://github.com/LizardByte/Sunshine/blob/master/src/platform/linux/kmsgrab.cpp
- FFmpeg VA-API HEVC encoder (VUI, `sei=hdr`): https://github.com/FFmpeg/FFmpeg/blob/master/libavcodec/vaapi_encode_h265.c
- Wayland colour management protocol: https://wayland.app/protocols/color-management-v1
- GNOME 48 colour management: https://www.phoronix.com/news/GNOME-wp_color_management_v1
- KWin HDR screencast dim (bug 496185): https://bugs.kde.org/show_bug.cgi?id=496185
- KWin 10-bit screencast question (2026): https://discuss.kde.org/t/kwin-hdr-10-bit-screencast-support-how-to-debug-a-plugin/43983
- PipeWire `spa_video_info_raw`: https://docs.pipewire.org/structspa__video__info__raw.html
- ITU-R BT.2100 (PQ, HLG): https://www.itu.int/rec/R-REC-BT.2100
- ITU-R BT.2390 (EETF): https://www.itu.int/pub/R-REP-BT.2390
- ITU-R BT.2408 (reference white 203 cd/m², down-mapping): https://www.itu.int/dms_pub/itu-r/opb/rep/R-REP-BT.2408-9-2026-PDF-E.pdf
- ITU-R BT.2446 (HDR↔SDR conversion methods): https://www.itu.int/dms_pub/itu-r/opb/rep/R-REP-BT.2446-2019-PDF-E.pdf
- Steam beta, HDR streaming on Steam Deck OLED: https://gamingonlinux.com/2026/07/steam-beta-adds-hdr-streaming-on-steam-deck-oled
- Moonlight HDR on Linux and Steam Deck: https://gamingonlinux.com/2024/06/moonlight-pc-for-game-streaming-via-sunshine-gets-hdr-support-for-linux-steam-deck
- Player-led Dolby Vision on Amlogic boxes (CoreELEC): https://discourse.coreelec.org/t/dolby-vision-vs10-engine-on-ugoos-am6/51323
- Android TV box staying in Dolby Vision (Plex forum): https://forums.plex.tv/t/android-tv-dv-hdr-mode-switching/848881
