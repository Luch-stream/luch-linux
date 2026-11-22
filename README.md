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
can be installed. Certificate trust support depends on the TV browser. Planned: media mode
(send a URL/file and control playback like Chromecast), photo mode, presentation mode with a
presenter view.

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

**GNOME:** Quick Settings → **Luch**. A tap mirrors the whole screen in `fast` mode to the last or
only receiver; the arrow opens the list with all modes. While mirroring, the toggle shows the
estimated latency. On the TV, press OK on the remote to show the statistics overlay.

**Terminal:**

```sh
luchctl                                   # interactive: list, f<N> fast, m<N> screen, w<N> window, e<N> extend, s<N> stop
luchctl list
luchctl refresh                           # rescan now; updates arrive asynchronously in TUI/watch
luchctl mirror                            # fast mode; the receiver name is optional when there is only one
luchctl mirror "XGIMI MoGo 4 Laser" window # or monitor | window | virtual
luchctl stats                             # live latency, fps, bitrate
luchctl stop
```

**Your own front-end:** everything is on the session bus, see [docs/dbus.md](docs/dbus.md).

## Troubleshooting

- `luchctl list` is empty: the receiver app must be running, `avahi-daemon` active, and both devices
  on the same LAN segment (mDNS does not cross routers).
- `fast` fails with «needs CAP_SYS_ADMIN»: run the `setcap` line again (reinstalling drops it).
- Logs: `journalctl --user -u luchd -f`. `LUCH_PROBE=1` in the service environment prints
  per-stage latency.

## License

GPL-3.0-or-later. Latency techniques follow [Sunshine](https://github.com/LizardByte/Sunshine),
[Moonlight](https://github.com/moonlight-stream) and [ALVR](https://github.com/alvr-org/ALVR).
