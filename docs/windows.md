# Luch on Windows

This document plans a Windows sender that is as fast as `luchd`'s KMS mode. On Linux that mode reads the scanout
buffer, converts it to NV12 on the GPU and encodes with VA-API, in about 5–6 ms from flip to packet. The document
also plans a Windows receiver.

It is based on reading the source of the projects that already do this well. Citations are `path:line` at these
commits:

- Sunshine `224c6bed` (LizardByte/Sunshine)
- ALVR `66ffca36` (alvr-org/ALVR)
- moonlight-qt `8886485`

Sunshine's Windows files are under `src/platform/windows/`, written `win/` below.

## 1. The shape: same as luch-kms

luch-kms uses one thread for the whole path: the newest picture is converted on the GPU, encoded with one frame in
flight, and every access unit is sent as soon as it exists. Windows has a counterpart for each stage:

| Stage | Linux (luch-kms) | Windows (luch-win) |
|---|---|---|
| Capture | KMS scanout FB (`drmModeGetFB2`), needs `CAP_SYS_ADMIN` | DXGI Desktop Duplication (`IDXGIOutput5::DuplicateOutput1`), any user. Windows.Graphics.Capture as the second choice. |
| Timestamp | flip time, `CLOCK_MONOTONIC` | `DXGI_OUTDUPL_FRAME_INFO::LastPresentTime`, QPC ticks |
| Colour conversion | two GLES passes into a VA surface | `ID3D11VideoContext::VideoProcessorBlt`, BGRA to NV12 (Sunshine does this with two pixel shaders, `win/display_vram.cpp:513-556`) |
| Encoder | `h264_vaapi` / `hevc_vaapi`, `async_depth=1` | Media Foundation hardware MFT first; then NVENC SDK, AMF and oneVPL directly |
| Cursor | the hardware cursor plane, blended in the shader | DDA: the pointer shape, blended by us. WGC: drawn by the system. |
| Privilege | file capability on one small binary | none for the user's desktop; SYSTEM for the UAC and lock screens (section 7) |

Sunshine and ALVR differ from this in one way: they use separate capture and encode threads with two D3D11 devices
and a keyed mutex (`win/display_vram.cpp:1968-2006`, `:985-1027`). luch-win uses one device on one thread instead,
which needs no keyed mutex and adds no hand-off. If an encoder blocks the capture for too long, the pipeline can
still be split later.

## 2. Desktop Duplication or Windows.Graphics.Capture

**How Sunshine chooses.** With no `capture` setting it tries DDA first and WGC second (`win/display_base.cpp:1030`,
`:1046`, default empty at `src/config.cpp:777`). It takes the VRAM variant of each when a hardware encoder is used
(`:1031-1059`).

| | DDA (`IDXGIOutputDuplication`) | WGC (`Direct3D11CaptureFramePool`) |
|---|---|---|
| **Latency** | Hands over DWM's own surface for the frame. `AcquireNextFrame(timeout)` wakes when DWM presents. Sunshine uses a 0 ms timeout inside a pacing group (`win/display_base.cpp:156`, `:295`, `:308`). | A frame pool of 2 (`win/display_wgc.cpp:142`). `FrameArrived` runs on the pool's thread, and Sunshine keeps only the newest frame (`:181-198`). Without `MinUpdateInterval` it is capped at 60 fps ("Screen capture may be capped to 60fps", `:159-165`). |
| **Timestamp** | `LastPresentTime` (QPC). It is 0 when only the mouse moved (`win/display_vram.cpp:1378-1390`). | `SystemRelativeTime()` (QPC, `win/display_wgc.cpp:232`) |
| **Cursor** | Not in the picture. Sunshine reads `GetFramePointerShape`, builds alpha and XOR textures and blends them (`win/display_vram.cpp:280-426`, `:1663-1693`). On a pointer-only update it re-blends onto the last frame (`:1495-1519`). On HDR the cursor's white is set to 300 nits because "there is no API that provides that information" (`:1815-1833`). | `IsCursorCaptureEnabled(x)` (`win/display_wgc.cpp:244-248`): the system draws it. |
| **HDR** | `DuplicateOutput1` with `R16G16B16A16_FLOAT` first, the scRGB "ideal format for Wide Color Gamut" (`win/display_vram.cpp:2025-2046`). `R10G10B10A2` "seems to actually just clamp" and is avoided. | The format is fixed at creation: FP16 if the client wants HDR, else BGRA8 (`win/display_wgc.cpp:135-139`). |
| **Protected content** | Masked black. Sunshine only logs `ProtectedContentMaskedOut`, which "seems to semi-randomly be TRUE or FALSE" (`win/display_base.cpp:160-166`). | Black as well (from Microsoft's docs; Sunshine has no WGC code for it). |
| **Multi-GPU** | The device must be made on the adapter that owns the output. Sunshine also hooks `NtGdiDdDDIGetCachedHybridQueryValue` so DXGI does not "reparent" outputs to the render GPU, "which breaks DDA" (`win/display_base.cpp:449-485`). It only picks outputs where a test duplication works (`:527`). | Works across adapters; the system copies. |
| **Secure desktop** | Works when the thread is on the input desktop (`OpenInputDesktop` + `SetThreadDesktop` before every `DuplicateOutput`, `win/misc.cpp:247-263`, `win/display_base.cpp:86`, `:423`) and the process is SYSTEM (section 7). `ACCESS_LOST`/`ACCESS_DENIED` mean reopen (`:170-175`). | No. |
| **Border, prompts** | None | `IsBorderRequired(false)` where the property exists (`win/display_wgc.cpp:150-151`) |
| **One window** | No, monitors only | Yes (`CreateForWindow`) |

**luch-win's choice.** DDA is the default. It has the shortest path, it carries the flip time, and it works on the
secure desktop. Two settings follow Sunshine:

- `DuplicateOutput1` gets no silent fallback to `DuplicateOutput`. Sunshine's reason: "we can hit this codepath when
  we're racing with mode changes and we don't want to accidentally fall back to suboptimal capture"
  (`win/display_base.cpp:95-97`).
- `factory->IsCurrent()` is checked for mode and HDR changes (`:266-271`).

WGC will be added for window mirroring and for outputs where DDA fails. `luch-win/src/win/wgc.rs` holds the plan.

## 3. Keeping the frame on the GPU

The path, after Sunshine:

1. DDA's texture is copied into our own texture with `CopyResource`. Sunshine uses `display_vram.cpp:1643` for DDA
   and `:1897` for WGC. The frame must be released before the next acquire, and a still screen is encoded again from
   our copy.
2. The NV12 conversion goes into the encoder's input texture: `D3D11_BIND_RENDER_TARGET`, R8 + R8G8 render target
   views (`win/display_vram.cpp:759-767`).
3. The encoder gets that texture:
   - NVENC registers it once with `nvEncRegisterResource` and maps it per frame (`src/nvenc/nvenc_d3d11_native.cpp:37-67`,
     `src/nvenc/nvenc_base.cpp:728-739`).
   - FFmpeg's D3D11 frames context uses `initial_pool_size = 1` (`win/display_vram.cpp:1152-1164`).
   - QSV needs `SetMultithreadProtected(TRUE)` (`:1172-1184`).
4. Each vendor uses only its own encoder (`win/display_vram.cpp:2059-2131`): AMD only `_amf`, Intel only `_qsv`,
   NVIDIA only `_nvenc`, Qualcomm only `_mf`.

ALVR gets its frames from the SteamVR compositor as shared textures, opened with `OpenSharedResource`
(`alvr/server_openvr/cpp/platform/win32/shared/d3drender.cpp:233-238`). It holds them with
`IDXGIKeyedMutex::AcquireSync(0, 10)` and drops the frame on a timeout (`OvrDirectModeComponent.cpp:238-256`). Before
copying, it waits for the encoder, because "encoder thread ... uses our shared d3d context (which is not thread
safe)" (`:334-358`).

## 4. Encoders

The common rules (also in `luch-win/src/settings.rs`):

- no B-frames
- one reference frame
- an infinite GOP, with an IDR only when asked
- parameter sets in every IDR
- CBR with a VBV of one frame
- one frame in flight

### NVENC: Sunshine uses the SDK directly

All of these are in `src/nvenc/nvenc_base.cpp` unless noted.

- Preset P1 (`nvenc_config.h:32`, `:641`). Tuning `NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY`, `enablePTD = 1` (`:642-643`).
- `gopLength = NVENC_INFINITE_GOPLENGTH`, `frameIntervalP = 1`, `NV_ENC_PARAMS_RC_CBR`, `zeroReorderDelay = 1`,
  `enableLookahead = 0`, `lowDelayKeyFrameScale = 1` (`:258-263`).
- **VBV = one frame**: `vbvBufferSize = bitrate*1000/framerate` (`:276-280`).
- Multipass defaults to quarter resolution (`nvenc_config.h:35`, `:266-272`). It costs a little encode time; measure
  before keeping it.
- `idrPeriod = INFINITE`, `repeatSPSPPS = 1`, `sliceMode = 3` with the client's slice count (`:361-364`, `:392-395`).
- The DPB is 5, but `numRefL0 = 1`: "Limit each frame to one reference while keeping a larger DPB for RFI fallback"
  (`:291-298`). Reference frame invalidation uses `nvEncInvalidateRefFrames` (`:799-833`).
- Intra refresh is off unless the client asks (`:329-347`).
- Async is set but waited on at once (`wait_for_async_event(100)` before `nvEncLockBitstream`, `:752-769`), so in
  effect there is one frame in flight. The bitstream buffer is in system memory (`:501`).
- GPU scheduling priority is REALTIME, except HIGH on NVIDIA with HAGS: "NVIDIA driver has unfixed bug(s) where
  'realtime' can cause unrecoverable encoding freeze or outright driver crash" (`win/display_base.cpp:689-700`).

ALVR (`alvr/server_openvr/cpp/platform/win32/VideoEncoderNVENC.cpp`) differs in a few places:

- Tuning is LowLatency, not UltraLowLatency (`alvr/session/src/settings.rs:1820-1823`).
- VBV is one frame plus 10% (`:390-393`).
- `maxNumRefFrames = 0`: 16 "has caused bad visual quality" (`:231-233`).
- CAVLC by default (`:256-263`).
- The IDR is `NV_ENC_PIC_FLAG_FORCEIDR` (`:125-128`).

### AMF: Sunshine uses FFmpeg `*_amf`

All of these are in `src/video.cpp`.

- `usage=ultralowlatency`, `rc=vbr_latency`, `quality=balanced` (defaults at `src/config.cpp:742-751`).
- `latency=1`, `async_depth=1`, `filler_data=false`, `forced_idr=1`, `frame_skipping=0`, `preencode=0`, and for HEVC
  `header_insertion_mode=idr` (`:1112-1223`).
- The H.264 fallback is `usage=2` (LOW_LATENCY), a workaround for AMF issue 410 (`:1215`).
- AMF version gates (`win/display_vram.cpp:2078-2098`):
  - AV1 ultra-low latency needs 1.4.30.
  - P010 needs 1.4.23; older runtimes "can crash when fed P010 surfaces".

ALVR uses the AMF SDK directly (`VideoEncoderAMF.cpp`):

- `ULTRA_LOW_LATENCY` usage (`:154`), `B_PIC_PATTERN 0` (`:199`), CBR with `FILLER_DATA_ENABLE` ("Required for CBR to
  work correctly", `:168-183`).
- VBV is one frame plus 10% (`:305`).
- Pre-analysis is marked "higher latency" (`:233-238`).
- The IDR is `FORCE_PICTURE_TYPE_IDR` with SPS/PPS inserted (`:814-849`).

### Intel QSV: Sunshine uses FFmpeg `*_qsv`

All of these are in `src/video.cpp`.

- `async_depth=1`, `low_delay_brc=1`, `low_power=1`, `forced_idr=1`, no `look_ahead` (`:977-1107`).
- For H.264 also `vcm=1` and `max_dec_frame_buffering=1`.
- It falls back to `low_power=0`, because "Some old/low-end Intel GPUs don't support low power encoding" (`:1080`).
- QSV gets `bit_rate - 1` (CBR_WITH_VBR) and no rate-control buffer limit (`:2503-2527`).

ALVR uses oneVPL (`VideoEncoderVPL.cpp`): `LowPower=ON`, `AsyncDepth=1`, video memory (`:181-183`). It forces H.264
to HEVC, because a forced IDR on H.264 "Results in error -15 (MFX_ERR_INVALID_VIDEO_PARAM)" (`:243-255`).

### Media Foundation

Sunshine uses `h264_mf` only for Qualcomm (`hw_encoding=1`, `rate_control=cbr`, `scenario=display_remoting`,
`src/video.cpp:1228-1288`). Through FFmpeg, MF cannot make an IDR on demand, so Sunshine gives it a fixed GOP of 120
(`:2341-2345`).

luch-win drives the MFT directly instead (`luch-win/src/win/mf.rs`):

- `CODECAPI_AVLowLatencyMode`
- CBR
- B-frames 0
- GOP 0
- `CODECAPI_AVEncVideoForceKeyFrame` for an IDR on request

This works on all four vendors without an SDK, which is why it comes first. The vendor SDKs come next, because MFTs
lack some of the knobs (VBV, slices, intra refresh).

**Common FFmpeg settings** (Sunshine `src/video.cpp`):

- `max_b_frames = 0`, because "B-frames delay decoder output" (`:2334-2335`)
- `gop_size = keyint_min = INT_MAX` (`:2346-2351`)
- `CLOSED_GOP | LOW_DELAY`, `FLAG2_FAST` (`:2365-2367`)
- `rc_min_rate = rc_max_rate = bit_rate` (`:2503-2511`)
- `rc_buffer_size = bitrate/framerate` (`:2517-2527`)

`LowLatency::ffmpeg()` in `settings.rs` holds the per-vendor options for an FFmpeg path like luch-kms's.

## 5. Timing and pacing

- **Pacing groups.** The first frame waits up to 200 ms. Each later frame is due at `start + n/fps` (rational
  maths), with a sleep and then a 0 ms acquire. A missed slot starts a new group (`win/display_base.cpp:276-339`).
  The rate may only go down to match fractional refresh rates: "otherwise client may start accumulating frames"
  (`:231-245`). `luch-win/src/pace.rs` uses the same grid.
- **Timers.** Sunshine uses `CreateWaitableTimerEx(CREATE_WAITABLE_TIMER_HIGH_RESOLUTION)` on Windows 10 1809 and
  later (`win/misc.cpp:1827-1833`). It also calls `NtSetTimerResolution(max)` (0.5 ms), with `timeBeginPeriod(1)` as
  the fallback (`:1218-1225`).
- **Priorities.**
  - Process: `HIGH_PRIORITY_CLASS` (`win/misc.cpp:1228`) and `DwmEnableMMCSS(true)` (`:1216`).
  - Threads: capture `THREAD_PRIORITY_HIGHEST` (`src/video.cpp:1919`), encoder `ABOVE_NORMAL` (`:3159`).
  - GPU: `SetGPUThreadPriority(7)` (`win/display_base.cpp:718`), `SetMaximumFrameLatency(1)` (`:733`).
  - MMCSS is used for audio only ("Pro Audio", `win/audio.cpp:701`). luch-win also puts the video thread in
    "Capture"; this needs measuring.
- **Still screen.** The last frame is encoded again at `minimum_fps_target`, which defaults to half the frame rate.
  The reason is "to avoid image quality issues with static content" (`src/video.cpp:2741-2743`, `:2781-2790`). For
  Luch, this also lets the receiver notice a lost frame.
- **AcquireNextFrame holds the device lock.** A 200 ms DDA timeout adds a 10 ms sleep, because the D3D11 device's
  "unfair lock ... held the entire time that AcquireNextFrame() is running" starves the encoder
  (`win/display_base.cpp:320-337`). luch-win uses one thread, so its wait is the timeout itself: half the frame
  interval of the repeat.
- **Display sleep.** `SetThreadExecutionState(ES_CONTINUOUS|ES_DISPLAY_REQUIRED)` is held during capture
  (`win/display_base.cpp:254-258`).
- **Latency measurement.**
  - The capture time is `max(LastPresentTime, LastMouseUpdateTime)` (`win/display_vram.cpp:1387-1390`).
  - `frame_processing_latency` is send time minus that, sent in the packet header (`src/stream.cpp:1672-1680`).
  - Luch's `host latency` field (PROTOCOL.md section 4) is the same thing, so the numbers compare directly with
    luch-kms.
- **DSCP.** Windows ignores `IP_TOS` from applications. Sunshine uses qWAVE `QOSAddSocketToFlow`, with
  `QOSTrafficTypeAudioVideo` for video and `QOSTrafficTypeVoice` for audio. qWAVE is loaded dynamically because it is
  "not installed by default on Windows Server" (`win/misc.cpp:1683-1777`).

## 6. Audio: WASAPI loopback

- Shared mode, `LOOPBACK | EVENTCALLBACK | AUTOCONVERTPCM | SRC_DEFAULT_QUALITY`, buffer 0 (the engine default),
  float32 at the session's channel count (`win/audio.cpp:373-403`). There is no exclusive mode.
- The wait uses `default_latency / 1000` from `GetDevicePeriod` as a timeout (`:668-670`, `:778`). That is in 100 µs
  units, not ms, so a 10 ms period gives a 100 ms timeout. This is harmless but worth knowing when you read their
  numbers.
- Opus is multistream, `OPUS_APPLICATION_RESTRICTED_LOWDELAY`, CBR (`src/audio.cpp:121-132`), with 5 ms frames by
  default (`src/rtsp.cpp:1092`). This matches Luch's `frame_ms: 5`.
- **Local speakers.** There is no `SetMute`. Sunshine switches the default endpoint to a virtual sink ("Steam
  Streaming Speakers") with the undocumented `IPolicyConfig::SetDefaultEndpoint` (`win/audio.cpp:1131`,
  `:1173-1179`). It installs that driver if it is missing, which needs admin (`:1343-1391`). It switches back on stop
  (`src/audio.cpp:337-349`).
- **Device changes.** Only `OnDefaultDeviceChanged` for `eRender` is acted on, and it causes a reinit
  (`win/audio.cpp:496-501`, `:726-736`).

`luch-win/src/win/audio.rs` captures this way. Opus is the open part: there is no Windows system encoder for it, so
libopus has to come from the build (vcpkg or a vendored build).

## 7. Virtual displays and display modes

- Sunshine itself creates **no** virtual display: there is no IddCx in the repo. Display changes go through the
  external LizardByte/libdisplaydevice (`src/display_device.cpp:742-749`, `:985`). The `dd_*` options are
  resolution, refresh rate, HDR, "ensure only display", and revert on disconnect (`src/config.cpp:1776-1794`). The
  docs mention a VDD only as an HDR workaround (`docs/configuration.md:1347`).
- For Luch, the counterpart of luchd's `niri-virtual` is an IddCx driver: a user-mode indirect display driver that
  adds a monitor of any mode. Open-source IddCx samples and community virtual-display drivers exist; Sunshine forks
  ship one, though that was not checked in code here. It needs driver signing. Until then, Luch on Windows mirrors a
  real monitor, and the 16:9 switch luchd does on niri becomes `ChangeDisplaySettingsEx` with a restore guard.

## 8. Privileges, UAC and the secure desktop

- An ordinary user process can duplicate the user's desktop. The UAC prompt and the lock screen run on the secure
  desktop (Winlogon), which only SYSTEM can open. In the user's process DDA then returns `ACCESS_LOST`/`DENIED`
  until the user's desktop is back. luch-win then reopens the capture (`CaptureError::Reinit`) and the receiver gets
  an IDR.
- Sunshine's answer is a service (`tools/sunshinesvc.cpp`). It duplicates its LocalSystem token, sets
  `TokenSessionId` to the console session (`:107-127`), and launches the streamer with `CreateProcessAsUserW` in a
  kill-on-close job (`:66-82`, `:288-298`). It restarts the streamer on session change (`:308-317`).
- The streamer then runs as SYSTEM in the user's session. It follows the input desktop before each duplication
  (`win/misc.cpp:247-263`), and impersonates the user for anything user-specific (`:592-610`, `:1060-1084`).
- Plan for Luch: the user-mode `luch-win` first (no UAC prompts visible: the receiver keeps the last picture), then
  an optional `luch-win-svc` doing what sunshinesvc does. This is the counterpart of luch-kms's `--serve` root
  helper.

## 9. Gotchas collected from comments

- `// FIXME: Duplicate output on RX580 in combination with DOOM (2016) --> BSOD` (`win/display_base.cpp:70`).
- The capture format is unknown until the first frame; until then Sunshine encodes black (`win/display_vram.cpp:1441-1474`).
- If no output is found, Sunshine wakes the display with `ES_DISPLAY_REQUIRED` and retries after 500 ms
  (`win/display_base.cpp:563-567`).
- `nvEncGetLastErrorString` "gives broken strings more often than not" (`src/nvenc/nvenc_base.cpp:839`).
- HDR metadata primaries are reported as Rec.2020 although DXGI says scRGB (`win/display_base.cpp:802-809`).
- 10-bit 4:4:4 on NVIDIA goes through CUDA interop (`src/nvenc/nvenc_d3d11_native.cpp:32-34`).
- QSV has no 4:4:4 for H.264 or AV1 (`win/display_vram.cpp:2114-2118`).
- ALVR's `m_minIDRFrameInterval` is assigned but never read (`alvr/server_openvr/cpp/alvr_server/IDRScheduler.cpp:11`).
- AMF `INSERT_AUD=false` depends on the driver version (`VideoEncoderAMF.cpp:301-303`).

## 10. What is in the tree now

Code moved out of `luchd` and `luch-recv` into `luch-proto`, so Linux and Windows share one copy. Linux behaviour is
unchanged and its tests still pass.

- `wire`: the datagram header, cutting frames into shards, the receiver's `Assembler`, and `opus_head`.
- `session`: frame size, bitrate, and the codec choice from the welcome's bench.
- `control`: line framing, `hello`, `start`, and `ready` ports.

`luch-win` is a member of the same workspace. It shares `luch-proto` and the lock file, and its Windows API code sits
behind `cfg(windows)`, so a Linux `cargo test` runs its platform-independent half:

| Module | What | Tested on Linux |
|---|---|---|
| `pipeline` | `Capture`/`Encoder`/`Clock` traits and `pump`. `pump` covers IDR on start and on request, repeat on a still screen, reopen after `ACCESS_LOST`, and the end on a new screen size. | yes, with fake capture and encoder |
| `pace` | Sunshine-style frame grid; repeat at half rate | yes |
| `tx` | Luch datagrams with 16-shard bursts, bound to the LAN interface address | yes, loopback UDP through `Assembler` |
| `control` | blocking control channel: hello/welcome, start/ready, keyframe/stop/ping thread | yes, against a scripted receiver |
| `settings` | per-vendor low-latency settings, VBV of one frame | yes |
| `win::dda` | adapter/output pairing, device, `DuplicateOutput1`, acquire/copy, error mapping | `cargo check`/`clippy` for `x86_64-pc-windows-gnu`, links with mingw |
| `win::convert` | D3D11 video processor BGRA to NV12 BT.709 limited, scaled | same |
| `win::mf` | async hardware MFT, low-latency CODECAPI, IDR on request | same |
| `win::audio` | WASAPI loopback, event-driven, MMCSS Pro Audio | same |
| `win::sys` | QPC clock, timer resolution, priority, display awake, MMCSS, input desktop, high-res timer, qWAVE | same |
| `win::wgc` | `IsSupported` and the plan | same |

Not yet: the DDA cursor blend, Opus and the audio stream, NVENC/AMF/oneVPL direct, WGC capture, mDNS discovery
(`DnsServiceBrowse` on Windows 10+), the service for the secure desktop, and HDR.

Cross-check, from the repository root:

```sh
docker run --rm -v "$PWD":/src -w /src -v luch-cargo:/root/.cargo/registry -v luch-target-win:/target \
  -e CARGO_TARGET_DIR=/target luch-linux-dev bash -c \
  'rustup target add x86_64-pc-windows-gnu && cargo clippy -p luch-win --target x86_64-pc-windows-gnu'
```

The `.exe` link also needs `gcc-mingw-w64-x86-64`.

## 11. Real-device test plan (sender)

**Hardware matrix.** Use one machine per row, Windows 11 24H2 and Windows 10 22H2 where possible:

| GPU | Encoder through MF | Then directly | Notes |
|---|---|---|---|
| NVIDIA RTX 30/40 desktop | NVIDIA H.264/HEVC Encoder MFT | NVENC SDK | HAGS on and off (the REALTIME priority bug) |
| AMD RDNA2/3 | AMDh264Encoder MFT | AMF | AMF runtime version in the log |
| Intel Xe iGPU (12th gen+) | Intel Quick Sync MFT | oneVPL | `low_power` H.264 vs HEVC |
| Hybrid laptop (Intel iGPU + NVIDIA dGPU) | both | | internal panel (iGPU) and HDMI (often dGPU): DDA must use the owning adapter |
| Qualcomm Snapdragon X | Qualcomm MFT | none | ARM64 build |

**Steps on each.**

1. Run `luch-win --to <TV address>` with a receiver on the same LAN, such as an Android TV with the Luch receiver, or
   `luch-recv` on a Linux laptop.
2. The log line names the encoder MFT and the vendor. Confirm that it is the GPU's own encoder, not "Microsoft H264
   Encoder" (software).
3. Measure 60 s each:
   - a still desktop
   - a moving window
   - a 60 fps video
   - a game at 144 Hz with the session at 60 fps
4. Read from the receiver's `stats` and from luch-win's counts:
   - `host_ms` (capture to send)
   - receiver `decode_ms`
   - `fps`
   - `lost`
   - encoded/repeated/skipped/reopened
5. Open a UAC prompt, lock the screen, change resolution, and plug in a second monitor. Expect a reopen and a fresh
   IDR within about 300 ms for UAC and lock. A new resolution ends the session with a clear message.
6. Glass to glass: put a millisecond clock on the PC screen and film it next to the TV at 240 fps. Compare with
   luchd's KMS mode on the same TV and network.

**Expected numbers.** DDA plus video processor plus a hardware MFT should give `host_ms` of about 4–8 ms at 1080p60.
The NVENC SDK with P1 and ultra-low latency should reach about 3–5 ms, comparable to luch-kms (5–6 ms).
Significantly more points at the MFT's queue: check that there is one `METransformHaveOutput` per input, or at
`AcquireNextFrame` timing.

**Compared with Linux.** Use the same receiver, codec, size and bitrate, and the same scenes. Compare `host_ms`
median and p99, the receiver's `decode_ms`, and glass to glass.

## 12. The receiver on Windows (plan)

Moonlight's Windows client is the model.

**Decode.**

- FFmpeg with D3D11VA. D3D11VA wins over DXVA2 (`app/streaming/video/ffmpeg.cpp:997-1001`).
- Decoder flags (`:509-542`):
  - `AV_CODEC_FLAG_LOW_DELAY`
  - `OUTPUT_CORRUPT` + `SHOW_ALL`
  - `err_recognition = AV_EF_EXPLODE`, because HEVC hwaccel "can fail silently" and with an infinite GOP the
    artifacts would stay
  - `thread_count = 1` for hardware
  - `extra_hw_frames = 5`
- For H.264, the SPS gets `max_dec_frame_buffering = 1`, and bitstream restrictions are added when missing ("NVENC
  doesn't seem to add bitstream restrictions anymore (591.59)", `:1782-1804`). Luch senders could write that VUI
  themselves.
- At start it decodes a built-in test frame per codec (`:588-708`).
- After 20 failed decodes in a row it resets the device (`:60`, `:2086-2096`). Each error asks for an IDR (`:2100`).
  This is the same as Luch's keyframe request.
- Blacklists, the hybrid-decode list (`ffmpeg-renderers/dxutil.h:8-59`):
  - Haswell, Broadwell and Cherry Trail HEVC
  - Skylake HEVC Main10
  - NVIDIA Kepler/Maxwell HEVC

**Present.**

- `DXGI_SWAP_EFFECT_FLIP_DISCARD` with 5 buffers. 3 "causes performance issues (buffer starvation) on AMD GPUs"
  (`ffmpeg-renderers/d3d11va.cpp:561-577`).
- **No `SetMaximumFrameLatency(1)`**: "counter-intuitively we must avoid it to reduce latency ... our SyncInterval 0
  Present() calls will block on DWM" (`:569-573`).
- No waitable swap chain object.
- `Present(0, ALLOW_TEARING)` when V-sync is off and tearing is supported (`:592-604`, `:827`). Otherwise it presents
  as fast as frames come and DWM keeps the newest (`:785-799`).
- Borderless window only (`:632-633`).
- Decoder output is bound directly as an SRV per texture-array slice, with no copy, on Intel or with separate devices
  (`:426-433`, `:1897-1937`). Other GPUs copy with `CopySubresourceRegion1`.
- The adapter is the one driving the window's monitor, to avoid a cross-GPU copy. The renderer is recreated when the
  window moves to another GPU (`:526-549`, `:1244-1254`).

**Pacing.** Moonlight's pacer is off by default (`app/settings/streamingpreferences.cpp:140`). When on, it waits on
`D3DKMTWaitForVerticalBlankEvent` (`pacer/dxvsyncsource.cpp:40-42`, `:154`) and drops down to one queued frame
(`pacer.cpp:241-275`). For Luch, use the protocol's rule, "of the frames decoded at once only the newest is drawn":
no pacer, `Present(0)` with tearing in full screen, and FLIP_DISCARD windowed.

**Audio.** Moonlight goes through SDL with about 15 ms of buffer at 5 ms frames (`audio/renderers/sdlaud.cpp:24-33`).
It drops when more than 30 ms are pending (`:103-106`). A Luch receiver would use WASAPI shared event-driven output
with the smallest period (`IAudioClient3::InitializeSharedAudioStream`).

**Building it.** Write the receiver in Rust next to `luch-recv`. It would reuse `luch_proto::wire::Assembler` (as
`luch-recv` does now) and `control`. For decoding there are two choices:

- FFmpeg D3D11VA (needs FFmpeg DLLs).
- Media Foundation's hardware H.264/HEVC decoder MFT with `CODECAPI_AVLowLatencyMode`, a DXGI device manager, and
  output sampled straight from the decoder's texture array. This needs no extra DLLs, so it comes first.

The receiver must hold to the protocol's rules: decode a frame the moment its last shard arrives; after a loss, drop
everything until an IDR; ask for an IDR once and again after RTT + 100 ms.

**Receiver test.** Use the same matrix: decode on NVIDIA, AMD and Intel. Compare `decode_ms` with Moonlight on the
same PC, and glass to glass with luchd sending.
