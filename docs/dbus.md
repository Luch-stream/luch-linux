# D-Bus API — `dev.luch.Luch1`

[Русская версия](dbus.ru.md)

`luchd` owns the well-known name **`dev.luch.Luch1`** on the **session bus** and serves one object,
**`/dev/luch/Luch1`**, with one interface, **`dev.luch.Luch1`**. Everything the CLI (`luchctl`) and the
GNOME extension do goes through it, so any other front-end can do the same.

```sh
gdbus introspect --session --dest dev.luch.Luch1 --object-path /dev/luch/Luch1
```

## Receivers

A receiver is a Luch device on the LAN, found through mDNS (`_luch._tcp`, IPv4 only, via Avahi).
Receivers seen only on point-to-point links (VPN tunnels) are ignored. If one appears on several
interfaces, the one on the main LAN interface wins. Every socket to a receiver is bound to the
interface it was discovered on, so a full-tunnel VPN never carries the stream.

Avahi's `ItemNew`/`ItemRemove` events drive the list. Every minute the daemon re-enumerates
the advertised services and resolves their SRV/TXT/address records again, including previous
timeouts. A temporary resolution failure preserves the last known receiver; `ItemRemove`
removes it and cancels any pending lookup.

## Methods

### `Receivers() → a(sssssqbb)`

Returns every known receiver as a tuple:

| # | Type | Meaning |
|---|------|---------|
| 0 | `s` | receiver id (stable UUID from its TXT record) |
| 1 | `s` | human name, e.g. `XGIMI MoGo 4 Laser` |
| 2 | `s` | model |
| 3 | `s` | IPv4 address |
| 4 | `s` | network interface it was found on |
| 5 | `q` | control port |
| 6 | `b` | a control channel is open |
| 7 | `b` | mirroring is running |

### `Refresh()`

Requests an immediate Avahi rescan and resolution of advertised services. Returns immediately;
read `Receivers()` for the current cache and listen for `ReceiversChanged()` for updates.
Overlapping requests share in-flight lookups. This does not connect to receivers or interrupt
active streams. The GNOME menu calls it when opened; the TUI calls it on entry and on `r`/Enter.
The CLI also exposes `luchctl refresh`.

### `Connect(s id) → s`

Opens the control channel (TCP, JSON lines) and keeps it alive with a ping every second.
Returns the receiver's `welcome` message as a JSON string: decoders, maximum size, hardware
passport (`hw`) and decoder benchmark (`bench`). `Mirror` connects by itself, so calling this is
optional.

Before connecting, resolves the selected receiver again to obtain its current address and port,
sharing an existing lookup if one is running. A resolution error fails the connection attempt
but keeps the last known entry in the list.

Errors: `org.freedesktop.DBus.Error.InvalidArgs` for an unknown id, `org.freedesktop.DBus.Error.Failed`
if already connected or the handshake fails.

### `Disconnect(s id)`

Sends `bye` and closes the control channel. Running mirroring stops with it.

### `Mirror(s id, s source)`

Starts mirroring to the receiver, connecting first if needed. Returns as soon as the session is set
up. `source` selects how the screen is captured:

| `source` | Capture | Latency | Notes |
|----------|---------|---------|-------|
| `fast` | KMS scanout via the `luch-kms` helper, EGL→NV12, VA-API encode | lowest (≈5–6 ms on the sender) | whole primary monitor; `luch-kms` needs `CAP_SYS_ADMIN` |
| `monitor` | screencast portal (PipeWire, DMA-BUF) | ≈15–25 ms on the sender | GNOME asks which screen once, then remembers it |
| `window` | screencast portal | as `monitor` | a single window |
| `virtual` | screencast portal, virtual monitor | as `monitor` | extends the desktop with a new display. Under Niri with virtual outputs (`niri-virtual`): a headless output of exactly the stream size is created for the session and removed afterwards, recorded through Niri's ScreenCast without a picker |
| `fast169` | as `fast` | as `fast` | Niri only: the monitor is switched to a 16:9 mode (the closest one at least as wide as the stream) for the session and restored afterwards, also after a `luchd` crash or restart; no letterbox, no scaling |

The codec follows the receiver's decoder benchmark (`welcome.bench.prefer`): HEVC when it is
faster there, otherwise H.264. Audio (Opus, 5 ms frames) is captured from the default output's
monitor in every mode.

In `virtual` mode, the receiver’s display size is used for the virtual monitor, capture and
encoded stream. Android receivers supply physical display modes (`hw.display.modes`, the largest
resolution wins); browser receivers supply screen dimensions in physical pixels. Decoder `max`
is not used as the display size. Older receivers without display information fall back to
1920×1080; NV12 dimensions are rounded down to even pixels. GNOME places the new monitor to the
right of the rightmost active monitor, aligned at its top edge, using a temporary configuration.
The GNOME extension draws the real cursor texture over this monitor to work around missing
cursor repaints in Mutter, while keeping capture and encoding on the GPU. The stream stats
identify this connector as `sender.virtual_monitor` (null for other capture modes).
GNOME 50 still labels portal-created monitors `MetaVendor`: its API has no display-name setter.

Errors: `InvalidArgs` for a bad `source` or unknown id, `Failed` if already mirroring.

While a session runs, `luchd` holds `org.freedesktop.ScreenSaver.Inhibit` (KDE, GNOME and Steam answer it), so the
screen does not go to sleep under the stream: a sleeping screen has nothing to capture. Nothing happens where nothing
answers that interface.

### `MirrorSized(s id, s source, u width, u height)`

Like `Mirror`, with the stream size in pixels (even numbers, 320x240 to 7680x4320). `Mirror` is
`MirrorSized` with 0x0, which means the default size (`LUCH_SIZE` when set; otherwise the receiver’s native display size for `virtual`, 1920x1080 for other modes). The video bitrate grows with the square root of the pixel count (4K gets
twice the 1080p rate). The sizes a receiver can show are in `welcome.hw.display.modes` and
`welcome.hw.video`; a receiver's decoder may not keep up with 4K at 60 fps, so check `Stats`.

### `MirrorGroup(as ids, as roles, s source, s mode)`

Mirrors to several receivers in step (PROTOCOL.md 4h): `ids[0]` shows the picture, every receiver presents what its
role says (`video`, `audio` or `both`; the first one's is `video` or `both`). `mode` `interactive` keeps the picture
as fast as alone and lets the sound wait for it (a TV and a speaker); `watch` gives every receiver the same playout
delay, at least 150 ms (speakers in several rooms, a wall of TVs). Every receiver must be connected (it is connected
when it is not) and announce `sync` in its welcome; a busy or older one is refused. `StopMirror` on any member ends the
group; a member that leaves by itself is dropped and the others go on. `Stats` of `ids[0]` then has a `group` part:
`d_video_ms`, `d_audio_ms` and per member `need_ms`, `video_pts_ms`, `route`, `offset_ms`.

### `SetOffset(s id, d ms)`

A receiver's own correction in ms, positive = later, within ±1000 (the slider of a group, or a calibration's
result). It is kept per receiver and output route (a TV with and without its soundbar are two entries), saved, and
sent at once when the receiver is playing in a group. A route without a saved value starts from a preset: HDMI ARC
+40 ms, a Bluetooth speaker whose delay the platform does not report +150 ms.

### `Sources() → as`

Capture sources usable on this machine, best first: `fast` (only when `luch-kms` is installed),
`fast169` (Niri), `virtual` (Niri with virtual outputs, otherwise the portal one), `monitor`, `window`. Front-ends should offer just these.

### `Capabilities(s id) → s`

The receiver's last `welcome` as JSON (`hw`, `bench`, decoders), `{}` until the control channel has
been opened once (`Connect` or `Mirror`). It never connects by itself.

### `StopMirror(s id)`

Stops mirroring and tells the receiver (`stop`). The control channel stays open.

### `Stats(s id) → s`

Live numbers as JSON, refreshed every second while mirroring, `{}` otherwise:

```json
{
  "est_ms": 13.5,
  "sender":   {"rtt_ms": 3.7, "host_ms": 5.6, "fps": 45, "mbps": 2.1, "encoder": "hevc_vaapi", "res": "1920x1080"},
  "receiver": {"decode_ms": 6.0, "fps": 47, "lost": 0, "dropped": 3, "decoder": "OMX.MS.HEVC.Decoder"}
}
```

- `rtt_ms` — round trip of the control-channel ping.
- `host_ms` — capture → encoded packet on the sender (scanout flip time in `fast` mode).
- `decode_ms` — the receiver's decoder time, as it reports it.
- `est_ms` — `host_ms + rtt_ms / 2 + decode_ms`; the display's own latency is not included.

The same sender numbers are sent to the receiver (`sender_stats`) for its on-screen overlay.

### `Info(s id) → s`

What a person should know about a receiver, as JSON: `android` (e.g. `Android 5.1`; empty until it has been
connected once), `limits` (what works worse on its Android, in plain words), `audio_latency_ms` (how far its sound
trails the picture) and `audio` (the sound mode in use). From the receiver's `welcome` (`sdk`, `limits`).

### `SetAudio(s id, s mode)` / `GetAudio(s id) → s`

The sound for the next show to a receiver, remembered in `~/.config/luch/audio`:

| `mode` | Sound |
|--------|-------|
| `auto` (default) | `sync` where the receiver cannot make its sound short (`audio_lag`: Android 5 and 6), else `on` |
| `on` | played as it arrives, `audio_latency_ms` after the picture |
| `off` | not sent: the picture only, the lowest latency |
| `sync` | the receiver holds each picture back by its audio delay, so both come out together |

### `Reports() → s`, `ReportUrl(s id) → s`, `DismissReport(s id)`

Crash and error reports the receivers handed over (luchd asks for them with `"want": ["reports"]`), as a JSON array,
oldest first; each as the receiver sent it plus `receiver_model`. Kept in `~/.config/luch/reports` and announced
with a desktop notification that opens the issue. `ReportUrl` is that link (the receiver project's new-issue page
with title and body filled in); `DismissReport` forgets one.

### `Known() → s`

Every receiver luchd has connected to (kept in `~/.config/luch/known.json`), as a JSON array: `id`, `name`, `model`,
`address`, `seen` (unix time of the last connection), `mac` and `ble` when known, `online` (on the LAN now), `wake`
(the ways it can be turned on: `wol`, `ble`) and `hint` when it needs something first (an XGIMI without a learned
remote signal).

### `Wake(s id) → s`

Turns a known receiver on from standby, every way known for it, and returns those ways (`wol,ble`). `wol`: a
Wake-on-LAN magic packet to the MAC noted from the ARP table while the receiver was on. `ble`: the remote's power-key
advertisement (Bluetooth company id 0x0046, MediaTek) through BlueZ on the system bus, as learned, for 2 s, in a
packet like the remote's own (connectable, discoverable, HID service, keyboard appearance). Fails when there is no
way yet or none of them could be sent (no Bluetooth adapter, Bluetooth off). The receiver then appears through
`ReceiversChanged`.

### `LearnWake(s id, u seconds) → s`

Scans Bluetooth LE for up to `seconds` (5–120) for a MediaTek remote's power-key advertisement, keeps it as the
receiver's `ble` and returns it as hex. The receiver should be off, so that its remote advertises instead of
connecting.

### `SetWake(s id, s kind, s value)`

Sets `kind` `mac` (`aa:bb:cc:dd:ee:ff`) or `ble` (the manufacturer data in hex, without the company id) by hand; an
empty `value` forgets it.

### `Cast(s id, as items, s options) → s`

Shows on the receiver (connecting first if needed) local files or links, played or shown by the receiver itself in their
own quality: a 4K file plays in 4K on a 4K TV whatever this screen is (PROTOCOL.md, section 4b). If every item is a
picture they are photos (left/right on the remote goes through them); otherwise the first video or sound is played,
with any subtitle files (`.srt`, `.vtt`) among the items. Returns `"media"` or `"photo"` once the receiver has taken
it; fails with the receiver's reason (`bad_link`, `media_failed`, `busy`) otherwise.

`options` is JSON or `""`: `title`, `start` (seconds), `paused`, `rate`, `subs` (paths or links), `mime` (for a link
without an extension), `poster`, `via` and `headers`. A link is fetched by the receiver itself (`"via":"direct"`, the
default; the computer may go to sleep meanwhile), with `headers` (only `Referer`, `User-Agent`, `Origin`, `Cookie`,
`Authorization`). `"via":"sender"` relays it through luchd: luchd fetches it with those headers and passes the
bytes on unchanged, rewriting HLS playlists to point back at itself. That is for links only this computer can reach:
a company VPN, a site that needs the browser's login, Wi-Fi Direct. Local files always go through luchd.

Files are served on TCP port 47810 (any free port if it is taken), each under a random token and only to the receiver
it was cast to; open that port in a firewall. Nothing is served once the cast ends.

### `MediaCmd(s id, s cmd)`

Controls what the receiver plays: `cmd` is a `media_cmd` without `"t"`, as JSON: `{"do":"play"}`, `pause`, `toggle`,
`{"do":"seek","pos":90}`, `{"do":"skip","by":-10}`, `{"do":"rate","rate":1.5}`, `{"do":"audio","id":"a2"}`,
`{"do":"sub","id":"s0"}` (`null`: off), `{"do":"state"}` (report now).

### `MediaState(s id) → s`

The last `media_state` (JSON): `state` (`loading`, `playing`, `paused`, `ended`, `error`), `pos`, `dur` (null when
live), `rate`, `w`/`h`, `tracks` (`audio`, `sub` with `id`, `label`), `sel`, `by` (`receiver` when changed on the TV).
`{}` when nothing plays.

### `StopCast(s id)`

Ends what was cast (media or photos) and stops serving its files.

### `Sleep(s id)`

Puts the receiver to standby, as its remote's power key (PROTOCOL.md 4c). Fails with what is missing: the receiver's
accessibility switch for Luch is off, or its Android is older than 9. `Wake` turns it on again.

## Signals

### `MediaStateChanged(s id, s state)`

Every `media_state` from a receiver, and `{"state":"stopped","reason":...}` when it ends.

### `ReceiversChanged()`

Emitted when a receiver appears, disappears or changes its metadata, or a connection or mirroring session starts or ends.
Call `Receivers()` again to get the new state.

## Properties

### `LocalAudio` (`b`, read/write, default `true`)

Whether the sender keeps playing sound itself while mirroring. Set to `false`: while a session runs, the
sender's sound goes to a null sink (`luch_cast`), `luchd` records its monitor, the receiver plays it and the
sender stays silent (no echo with the receiver, no Bluetooth delay or A2DP artefacts). The default sink and the
streams that were on it are put back when the session ends, and after a crash on the next `luchd` start. It
applies at once to a running session. Saved in `~/.config/luch/local-audio`. Needs `pactl`; without it nothing changes.

### `SenderId` (`s`, read)

This machine's id (UUID kept in `~/.config/luch/id`). It is also announced as `_luchsrc._tcp`, so
receivers can show which senders are nearby.

### `WebUrl` (`s`, read)

`https://<lan-ip>:47801/` — the built-in web receiver, or an empty string with no default route.
Open it in any browser with WebCodecs (a TV browser will do); the page connects back over a
secure websocket, appears in `Receivers()` like any other receiver and starts mirroring after pressing Watch
(add `?mode=none` to stay manual, `?mode=monitor|window|virtual` to pick the source).

HTTPS runs locally without a domain or internet access. On first opening, the browser warns about
the self-signed certificate; proceed if the browser allows it. Some browsers require importing the
certificate as trusted before enabling WebCodecs; if the TV cannot do that, use the Android receiver.
Plain LAN HTTP cannot enable WebCodecs and redirects to HTTPS.
The certificate and private key live in `~/.config/luch/web-cert.pem` and `web-key.pem`
(or `$XDG_CONFIG_HOME/luch/`). The key is owner-only. The certificate survives restarts; connecting
through a new IP reissues it with the same key, so the browser may ask for trust again.
To check the certificate fingerprint on the laptop:

```sh
openssl x509 -in "${XDG_CONFIG_HOME:-$HOME/.config}/luch/web-cert.pem" -noout -fingerprint -sha256
```

## Examples

```sh
# list
gdbus call --session --dest dev.luch.Luch1 --object-path /dev/luch/Luch1 --method dev.luch.Luch1.Receivers

# mirror the whole screen with the lowest latency, then stop
gdbus call --session --dest dev.luch.Luch1 --object-path /dev/luch/Luch1 --method dev.luch.Luch1.Mirror "$ID" fast
gdbus call --session --dest dev.luch.Luch1 --object-path /dev/luch/Luch1 --method dev.luch.Luch1.StopMirror "$ID"

# watch for changes
gdbus monitor --session --dest dev.luch.Luch1 --object-path /dev/luch/Luch1
```

From GJS (as the GNOME extension does):

```js
const proxy = Gio.DBusProxy.new_for_bus_sync(Gio.BusType.SESSION, Gio.DBusProxyFlags.NONE, null,
    'dev.luch.Luch1', '/dev/luch/Luch1', 'dev.luch.Luch1', null);
const [rows] = proxy.call_sync('Receivers', null, Gio.DBusCallFlags.NONE, -1, null).deepUnpack();
proxy.call('Mirror', new GLib.Variant('(ss)', [rows[0][0], 'fast']), Gio.DBusCallFlags.NONE, -1, null, null);
proxy.connectSignal('ReceiversChanged', () => { /* refresh */ });
```

## Files and environment

| Path / variable | Purpose |
|-----------------|---------|
| `~/.config/luch/id` | sender id |
| `~/.config/luch/known.json` | receivers luchd has connected to, with the ways to wake them |
| `~/.config/luch/restore-token` | screencast portal restore token (no picker after the first time) |
| `LUCH_KMS_SOCKET` | a privileged `luch-kms --serve SOCKET [--uid N]` that `luchd` uses for the `fast` mode instead of running `luch-kms` itself (default `$XDG_RUNTIME_DIR/luch/kms.sock`, used when it exists): for a `luchd` in a sandbox (Flatpak), where file capabilities do not work. The socket is owned by `--uid`, mode 0600 |
| `LUCH_KMS_CARD`, `LUCH_KMS_RENDER` | DRM card and render node for `luch-kms` (default: the card with an active plane, and the render node of the same GPU) |
| `LUCH_PROBE=1` | per-stage latency probes in the `luchd` log |
