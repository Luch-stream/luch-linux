<p align="center"><img src="assets/luch.svg" width="128" alt="Luch"></p>

# Luch — Linux sender

[Русская версия](README.ru.md) · Receiver: [luch-android](https://github.com/Toxblh/luch-android)

Luch («ray» in Russian) mirrors a Linux desktop to an Android TV / projector with the latency of
game streaming, not of screen casting. It is a small, system-integrated take on the
Sunshine/Moonlight idea: a daemon that finds receivers on the LAN, exposes itself on D-Bus, and
streams the screen and sound in a few milliseconds.

Measured on an Intel Arrow Lake laptop → XGIMI MoGo 4 Laser over Wi‑Fi, full screen, `fast` mode:

| capture + encode (sender) | network (one way) | decode (receiver) | **total, display excluded** |
|---|---|---|---|
| ≈ 5.5 ms | ≈ 2 ms | ≈ 5.5 ms (HEVC) | **≈ 13 ms** |

## What is in this repository

| Part | What it does |
|------|--------------|
| `luchd` | User daemon. Finds receivers (`_luch._tcp` via Avahi), announces this machine (`_luchsrc._tcp`), keeps control channels, captures and streams, serves the D-Bus API `dev.luch.Luch1`. |
| `luch-kms` | Small helper for the lowest-latency mode: reads the scanout framebuffer straight from KMS, converts it to NV12 with EGL shaders, encodes with VA-API. The only part that needs a capability (`CAP_SYS_ADMIN`). |
| `luchctl` | CLI and interactive terminal screen. |
| `gnome-extension/` | GNOME Shell Quick Settings toggle «Luch». |
| `docs/dbus.md` | D-Bus API reference. |

![luchctl: receivers and live latency](docs/img/luchctl.png)

## Mirroring modes

| Mode | How | Sender latency |
|------|-----|----------------|
| `fast` | whole primary monitor from KMS scanout, cursor composited, EGL → NV12 → VA-API (HEVC or H.264, as the receiver's decoder benchmark prefers) | ≈ 5–6 ms |
| `monitor` | screencast portal (PipeWire, DMA-BUF, zero copy) | ≈ 15–25 ms |
| `window` | one window through the portal | as `monitor` |
| `virtual` | extend the desktop: GNOME creates a monitor at the receiver’s display resolution, to the right | as `monitor` |

Sound (Opus, 5 ms frames) is captured from the default output in every mode. Any browser with WebCodecs
(including compatible TV browsers) is also a receiver: open the HTTPS `WebUrl` the daemon serves
(see `docs/dbus.md`), accept the local certificate and press Watch — handy where no Android receiver
can be installed. Certificate trust support depends on the TV browser. Videos, sounds and photos can also be *cast*:
the receiver plays them itself in their own quality, like Chromecast (see **Use**). Planned: presentation mode with
a presenter view.

## How it works

```
KMS scanout ─▶ luch-kms ─(EGL: scale + RGB→NV12 into a VA surface)─▶ hevc/h264_vaapi ─┐
portal/PipeWire ─▶ vapostproc ─▶ DMA-BUF ─▶ h264_vaapi ───────────────────────────────┤
PulseAudio/PipeWire monitor ─▶ Opus 5 ms ─────────────────────────────────────────────┤
                                                                                     ▼
                          luchd: UDP shards (24-byte header), sent from the encoder thread
                          control: TCP JSON lines (hello/welcome, start/ready, keyframe, stats)
```

Latency techniques come from Sunshine, Moonlight and ALVR: no copies anywhere, `async_depth=1`,
no B-frames, one reference frame, infinite GOP with IDR on request, a one-frame VBV, the fastest
encoder preset, and packets leave the moment the encoder returns them. Sockets are bound to the LAN
interface, so a full-tunnel VPN never carries the stream.

## Requirements

- Linux with a VA-API encoder (Intel iHD tested; AMD radeonsi should work) and Mesa EGL.
- GNOME on Wayland for the portal modes; `fast` works under any compositor that scans out through KMS.
- `avahi-daemon`, PipeWire (with the Pulse server), GStreamer ≥ 1.24 with the `va`, `pipewire`,
  `pulse` and `opus` plugins, FFmpeg ≥ 6.1 libraries.
- Rust ≥ 1.80 to build.

Build dependencies:

```sh
# Ubuntu / Debian
sudo apt install build-essential pkg-config libavcodec-dev libavutil-dev libva-dev libdrm-dev \
  libegl-dev libgles-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev
# Fedora
sudo dnf install gcc pkgconf-pkg-config ffmpeg-free-devel libva-devel libdrm-devel mesa-libEGL-devel \
  mesa-libGLES-devel gstreamer1-devel gstreamer1-plugins-base-devel
# Arch
sudo pacman -S base-devel ffmpeg libva libdrm mesa gstreamer gst-plugins-base
# ALT Linux
sudo apt-get install libavcodec-devel libavutil-devel libva-devel libdrm-devel libglvnd-devel \
  gstreamer1.0-devel gst-plugins1.0-devel
```

Runtime plugins (names vary): `gst-plugins-bad` (va), `gst-plugin-pipewire`, `gst-plugins-good`
(pulse), `gst-plugins-base` (opus).

## Install

```sh
git clone https://github.com/Toxblh/luch-linux && cd luch-linux
./install.sh
```

`install.sh` builds and installs `luchd`, `luchctl` and `luch-kms` into `~/.cargo/bin`, grants
`luch-kms` `CAP_SYS_ADMIN` (asks for sudo), installs and starts the `luchd` user service, and
installs the GNOME extension. Log out and back in once so GNOME Shell loads the extension.

Manual steps, if you prefer:

```sh
cargo install --path luchd && cargo install --path luchctl && cargo install --path luch-kms
sudo setcap cap_sys_admin+ep ~/.cargo/bin/luch-kms      # again after every reinstall of luch-kms
install -Dm644 contrib/luchd.service ~/.config/systemd/user/luchd.service
systemctl --user daemon-reload && systemctl --user enable --now luchd
cp -r gnome-extension/luch@toxblh.github.io ~/.local/share/gnome-shell/extensions/
gnome-extensions enable luch@toxblh.github.io           # after re-login on Wayland
```

Why `CAP_SYS_ADMIN`: the kernel only hands out framebuffer handles of other clients to privileged
processes (Sunshine's KMS capture needs the same). It is given to the tiny `luch-kms` binary only;
`luchd` runs unprivileged. Without it, use the portal modes.

## Use

Install and open **Luch** on the receiver (see [luch-android](https://github.com/Toxblh/luch-android)).
It appears on the LAN by itself.

**Pairing.** The first time this computer connects to a TV, the TV shows a six-digit code in a corner of its screen
(if something is playing, it asks "Allow?" first); type it in the GNOME dialog that opens, or in the terminal
(`luchctl pair`, or any command that connects asks for it). From then on both know each other: the control channel is
TLS with pinned certificates, the picture and sound are encrypted, and nothing is asked again. `luchctl forget <tv>`
pairs again; a receiver with another certificate under a paired TV's id is refused.

**GNOME:** Quick Settings → **Luch**. A tap mirrors the whole screen in `fast` mode to the last or
only receiver; the arrow opens the list with all modes. While mirroring, the toggle shows the
estimated latency. On the TV, press OK on the remote to show the statistics overlay.

**Terminal:**

```sh
luchctl                                   # interactive: list, f<N> fast, m<N> screen, w<N> window, e<N> extend, s<N> stop
luchctl list
luchctl refresh                           # rescan now; updates arrive asynchronously in TUI/watch
luchctl find                              # mDNS gets nowhere: every LAN address, Bluetooth, the TV's sound, its QR code
luchctl add 192.168.1.20                  # a receiver by its address or QR link
luchctl mirror                            # fast mode; the receiver name is optional when there is only one
luchctl mirror "XGIMI MoGo 4 Laser" window # or monitor | window | virtual
luchctl stats                             # live latency, fps, bitrate
luchctl stop
luchctl info                              # the receiver's Android and what works worse on it
luchctl audio sync                        # sound: auto | on | off | sync (per receiver, remembered)
luchctl pair                              # type the code the TV shows; luchctl paired, luchctl forget <tv>
luchctl web                               # browsers allowed to watch through the web receiver
luchctl reports                           # crash and error reports the receivers handed over
luchctl report 1 open                     # file one as an issue (or: report 1, report 1 dismiss)
luchctl known                             # every receiver luchd has talked to, on or off, and how to wake it
luchctl wake                              # turn a receiver on from standby and wait for it
luchctl wake "XGIMI MoGo 4 Laser" learn   # pick up its remote's power signal over Bluetooth (receiver off)
luchctl sleep                             # standby, as the remote's power key (needs Luch on in its accessibility settings)
luchctl cast film.mkv film.en.srt         # the receiver plays it itself, in the file's own quality
luchctl cast ~/Pictures/*.jpg             # photos: left and right on the remote go through them
luchctl cast https://example.com/v.m3u8   # the receiver fetches the link; add --relay to fetch it through this computer
luchctl media                             # what plays: position, speed, sound tracks, subtitles
luchctl media pause                       # play | pause | toggle | seek 1:30 | skip -10 | rate 1.5 | audio a2 | sub s0 | sub off
luchctl media watch                       # follow it, also changes made with the TV's remote
```

**Casting.** A cast file is not mirrored: the receiver plays the original, picking the best version its screen can
show, so a 4K file plays in 4K on a 4K TV even from a laptop with a 1080p screen, and the laptop can be closed (for
a link) or stay free. The TV's remote controls it (OK pauses, left/right skip, down opens speed, sound track and
subtitles); luchctl and the D-Bus API see the same state. Local files are served by luchd on TCP port 47810 to that
receiver only; `--relay` passes a link through luchd too, for links only this computer can open (a company VPN, a
site that needs your login). Android receivers play MP4, MKV, WebM, MPEG-TS and HLS; DASH is not supported yet.

**From the browser.** The extension in [browser-extension/](browser-extension/README.md) puts a *To TV* button on
every web video and the TV's controls over the page's player (`luchctl browser install` connects it to luchd).

**Sound on older receivers.** Android 5 and 6 cannot make the receiver's sound short: it trails the picture by
its audio buffer (`luchctl info` says how much). `auto`, the default, then holds the picture back to match
(`sync`); `on` keeps the picture fastest and lets the sound lag; `off` sends the picture only. On Android 7 and
later `auto` plays the sound as it comes, about 20 ms behind.

**Crash reports.** A receiver keeps short reports of its crashes and errors (no logs, addresses or names) and hands
them to luchd, which keeps them in `~/.config/luch/reports` and shows a notification: click it to open a filled-in
issue for the receiver's project. Nothing is sent anywhere unless you file it.

**Turning a receiver on.** luchd remembers every receiver it has connected to, so it can turn one on that is off
(Quick Settings: **Turn on …**, or `luchctl wake`). Nothing goes through the internet; there are two ways, and luchd
tries every one it knows for that receiver:

- *Wake-on-LAN*, for TVs that stay on the network in standby (Sony "Remote start", TCL "Network standby", LG,
  Samsung): luchd notes the TV's MAC address while it is on. Turn that setting on in the TV.
- *The remote's Bluetooth signal*, for projectors and TVs that leave the network in standby (XGIMI and other
  MediaTek-based Google TVs): with the receiver off, run `luchctl wake <r> learn` and press the power key on its
  remote near the computer. luchd then sends that signal itself (through BlueZ; the computer's Bluetooth must be
  on and in range), as the remote does it: a HID keyboard's advertisement for about 2 s. The values can also be set
  by hand: `luchctl wake <r> mac <address>`, `luchctl wake <r> ble <hex>`.

**Your own front-end:** everything is on the session bus, see [docs/dbus.md](docs/dbus.md) (also `Info`,
`SetAudio`/`GetAudio`, `Reports`, `ReportUrl`, `DismissReport`, `Known`, `Wake`, `LearnWake`, `SetWake`).

## Troubleshooting

- `luchctl list` is empty: the receiver app must be running, `avahi-daemon` active, and both devices
  on the same LAN segment (mDNS does not cross routers). luchd also asks every address of the subnet and listens
  over Bluetooth by itself; `luchctl find` goes further (on the TV choose "Say the address by sound", or hold the
  laptop's camera up to the TV's QR code) and says what it tries. Client isolation (many hotels) keeps devices
  apart altogether: then only the TV's own hotspot or a cable helps.
- `fast` fails with «needs CAP_SYS_ADMIN»: run the `setcap` line again (reinstalling drops it).
- Logs: `journalctl --user -u luchd -f`. `LUCH_PROBE=1` in the service environment prints
  per-stage latency.

## License

GPL-3.0-or-later. Latency techniques follow [Sunshine](https://github.com/LizardByte/Sunshine),
[Moonlight](https://github.com/moonlight-stream) and [ALVR](https://github.com/alvr-org/ALVR).
