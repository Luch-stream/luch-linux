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

## Signals

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
| `~/.config/luch/restore-token` | screencast portal restore token (no picker after the first time) |
| `LUCH_KMS_SOCKET` | a privileged `luch-kms --serve SOCKET [--uid N]` that `luchd` uses for the `fast` mode instead of running `luch-kms` itself (default `$XDG_RUNTIME_DIR/luch/kms.sock`, used when it exists): for a `luchd` in a sandbox (Flatpak), where file capabilities do not work. The socket is owned by `--uid`, mode 0600 |
| `LUCH_KMS_CARD`, `LUCH_KMS_RENDER` | DRM card and render node for `luch-kms` (default `/dev/dri/card1`, `/dev/dri/renderD128`) |
| `LUCH_PROBE=1` | per-stage latency probes in the `luchd` log |
