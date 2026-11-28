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

Where mDNS does not get through (hotel and guest Wi-Fi, client isolation, some VPNs) the daemon climbs a ladder
(PROTOCOL.md section 1): together with mDNS it asks every address of each LAN interface's subnet (a /24, at most
a /22 around this computer, never through a VPN) for the control port with a probe `hello`; after 4 s with nothing
it listens for receivers' cards over Bluetooth LE (BlueZ) while advertising that it is looking. Only on request
(`Find("all")`) does it also listen on the microphone for the TV saying its card by sound, and look through the
camera for the TV's QR code. Every receiver found any way is checked with a real hello and joins the same list;
`Discovered()` says how each was found. Receivers heard over Bluetooth are listed nearest (strongest signal) first,
the others by name. One found other than by mDNS leaves the list when it no longer answers at the next climb.

Where no network joins the computer and the TV at all (client isolation, no common LAN), a TV whose Bluetooth card
offers it can be reached **directly** (`Direct`): over a Bluetooth LE channel to the TV (L2CAP, the PSM in its card)
the daemon runs the TV's usual TLS (pinned; pairing with the code if not paired yet) and asks for its Wi-Fi Direct
network; the TV asks its person, makes the network and hands over its name and passphrase; the computer joins it as
a plain Wi-Fi network through NetworkManager (a volatile connection, never the default route, so Ethernet keeps the
internet), connects to the TV there like to any receiver, and goes back to its own Wi-Fi when that session ends or
on `DirectOff` (or at the next start, after a crash). Never automatic: it takes the computer off its Wi-Fi. The
ladder's `direct` way (part of `all`) only says which TVs offer it.

### TVs without Luch

TVs that do not run Luch are in the same list and play what `Cast` gives them ([other-tvs.md](other-tvs.md)):
Chromecasts and Google TVs (mDNS `_googlecast._tcp` through Avahi, ids `cast:<id>`) and DLNA/UPnP renderers (SSDP
M-SEARCH for `MediaRenderer:1` at start, every minute and on `Refresh`, ids `dlna:<UDN>`). A TV is one row whatever
it speaks: the ways that share an address are put together, and the best one names the row and is used, Luch first,
then Cast, then DLNA. `Devices` tells the kind and every way; the `transport` option of `Cast` picks another one.

Such a TV plays media and photos (the first picture), with subtitles where it reads them; it cannot mirror the screen,
show documents or open its YouTube app from here (`Mirror` and `Connect` fail with `NotSupported`). Its state comes
as `media_state` in `MediaState`/`MediaStateChanged` like a Luch receiver's, polled once a second over DLNA. Its
volume can be set with `SetVolume` while it plays for us.

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
| 6 | `b` | a control channel is open (a TV without Luch: it plays something for us) |
| 7 | `b` | mirroring is running |

### `Devices() → s`

Every row of `Receivers` as JSON, with how luchd reaches it: `id`, `name`, `model`, `address`, `kind` (`luch`,
`cast`, `dlna`: the way used), `can` (what that way does: `mirror`, `media`, `photo`, `doc`, `apps`, `volume`, `subs`,
`rate`), `control_ms` (about how long a command takes to show on the TV) and `transports` (every way to this TV, best
first, as `{"kind","id"}`).

```json
[{"id": "cast:1c4f…", "name": "Living room TV", "model": "Chromecast", "address": "192.168.1.41", "kind": "cast",
  "can": ["media", "photo", "volume", "subs", "rate"], "control_ms": 150,
  "transports": [{"kind": "cast", "id": "cast:1c4f…"}, {"kind": "dlna", "id": "dlna:5f1c…"}]}]
```

### `Refresh()`

Requests an immediate Avahi rescan and resolution of advertised services, and an SSDP search. Returns immediately;
read `Receivers()` for the current cache and listen for `ReceiversChanged()` for updates.
Overlapping requests share in-flight lookups. This does not connect to receivers or interrupt
active streams. The GNOME menu calls it when opened; the TUI calls it on entry and on `r`/Enter.
The CLI also exposes `luchctl refresh`. It also starts the automatic discovery ladder (`Find("auto")`), at most
once every 30 s; luchd climbs it once by itself a second after it starts.

### `Find(s mode)`

Climbs the discovery ladder; returns at once, progress comes as `FindProgress`. `mode`: `auto` (mDNS and the
sweep of the LAN, then Bluetooth), `all` (then also sound and the camera, which need the person: the TV says its
address when asked, the code is held up to the camera; and which TVs nearby offer a direct connection), or a
comma-separated list of `mdns`, `scan`, `ble`, `sound`, `qr`, `direct` to try at once. A climb already running is left to finish. Invalid modes fail with `InvalidArgs`.
`luchctl find [mode]` (default `all`) prints what it tries.

### `FindStatus() → s`

The last `FindProgress` as JSON (see below); `"running": false` with no rungs before the first climb.

### `Discovered() → s`

How each listed receiver was found: `{"<id>":{"via":["mdns","scan"],"rssi":-48}}`. `via` holds `mdns`, `scan`,
`ble`, `sound`, `qr` (its QR link through `AddReceiver`), `manual` or `direct` (over its own Wi-Fi network);
`rssi` (dBm) is the Bluetooth signal when it was heard, else `null`.

### `Direct(s key) → s`

Connects to a TV over its own Wi-Fi Direct network (see above). `key`: `""` for the nearest TV offering it, else
the start of its id (as its Bluetooth card says it, 8 hex digits) or its name; a TV not heard lately is listened
for first (12 s). Returns at once with `{"leaves": "<the Wi-Fi network this computer leaves>"|null}`; the steps
come as `DirectChanged`. A TV not paired yet asks for pairing: `PairChanged` and `Pair` then use `key`'s 8 hex
digits as the id. Fails at once without NetworkManager or Wi-Fi, while another one is under way, or while on a TV's
network already. `luchctl direct [r]`.

### `DirectOff() → s`

Ends the session over the TV's network and goes back to the Wi-Fi network before; returns what it did ("back on
'Home'"). `luchctl direct off`.

### `DirectStatus() → s`

Where it stands, as `DirectChanged` says it.

### `DirectOffers() → s`

`{"offers": [{"key": "0b6f1c2a", "name": "Living room TV", "rssi": -52}], "leaves": "Home"|null}`: the TVs heard
over Bluetooth in the last 2 minutes that offer a direct connection, nearest first (named when this computer knows
them), and the Wi-Fi network a direct connection would leave, for the question to ask first.

### `AddReceiver(s link) → s`

Lists a receiver from what a person has: its QR link (`luch://connect?host=…&port=…&t=…`) or its address
(`host` or `host:port`). It is checked with a probe hello from the interface that reaches it (never a VPN); a
pairing token in the link goes with the next `Connect`. Returns the receiver's id. `luchctl add <link|address>`.

### `Connect(s id) → s`

Opens the control channel (JSON lines; TLS to a receiver that offers it, PROTOCOL.md 3a) and keeps it alive with a
ping every second.
Returns the receiver's `welcome` message as a JSON string: decoders, maximum size, hardware
passport (`hw`) and decoder benchmark (`bench`). `Mirror` connects by itself, so calling this is
optional.

Before connecting, resolves the selected receiver again to obtain its current address and port,
sharing an existing lookup if one is running. A resolution error fails the connection attempt
but keeps the last known entry in the list.

A receiver this computer has not paired with yet shows a code on its screen (maybe after its person allows the
connection there): `Connect`, and everything that connects by itself (`Mirror`, `Cast`, `Sleep`, `SetVolume`), then
fails with `AccessDenied` saying "… asks for pairing … (`luchctl pair <id>`)" while the connection waits for the code
(`Pair`); `PairChanged` tells front-ends to ask for it. A paired receiver is checked against its pinned certificate:
another one under its id, or no TLS where there was TLS, is refused with `AccessDenied`.

Errors: `org.freedesktop.DBus.Error.InvalidArgs` for an unknown id, `org.freedesktop.DBus.Error.Failed`
if already connected or the handshake fails, `org.freedesktop.DBus.Error.AccessDenied` for pairing (above).

### `Pair(s id, s code) → s`

The code shown on a receiver that asked for pairing, as typed (six digits, spaces allowed). luchd and the receiver
check it digit by digit and pin each other's certificate; returns the `welcome`, and the receiver is connected. A
wrong code ends this pairing (the TV shows a new code on the next `Connect`). Errors read as they are meant to be
shown: "wrong code …", "Living room TV refused the connection", "… ignores this computer for 59 min".

### `Pairing(s id) → s`

Where pairing stands, as JSON: `step` `none`, `allow` (the TV asks its person first), `code` (call `Pair`), `paired`
or `failed` (with `reason` in words); `name`, `digits`.

### `Paired() → s`, `Forget(s id)`

The receivers this computer is paired with (`id`, `name`, `fp`: the SHA-256 of their certificate, `paired`: unix
time), and forgetting one: the next `Connect` pairs again.

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

Mirrors to several receivers in step (PROTOCOL.md 4k): `ids[0]` shows the picture, every receiver presents what its
role says (`video`, `audio` or `both`; the first one's is `video` or `both`). `mode` `interactive` keeps the picture
as fast as alone and lets the sound wait for it (a TV and a speaker); `watch` gives every receiver the same playout
delay, at least 150 ms (speakers in several rooms, a wall of TVs). Every receiver must be connected (it is connected
when it is not) and announce `sync` in its welcome; a busy or older one is refused. `StopMirror` on any member ends the
group; a member that leaves by itself (the first one too) is dropped and the others go on; the last one out ends it.
The picture is H.264 when a member that shows it cannot decode HEVC. `Stats` of any member then has a `group` part:
`id`, `mode`, `sound_only`, `d_video_ms`, `d_audio_ms` and the members playing now, each with `id`, `name`, `role`,
`need_ms`, `video_pts_ms`, `route`, `offset_ms`.

### `AddToGroup(s member, s id, s role)`

Puts receiver `id` into the running group that `member` (any receiver in it) plays in. It is connected when it is not
(and asks for pairing as any connection does), must announce `sync` and not be playing anything; it gets the group's
`timeline` and current D, and its picture starts at the group's next keyframe (one is asked for at once, merged with
any other request) and its sound at the next OpusHead: never a P-frame of a picture it did not have. When it needs a
larger D, D rises once for everyone at the same frame. `role` is `video`, `audio` or `both`; `""` is `both`, or
`audio` in a group of sound only (where a picture role is refused). A group playing HEVC refuses a receiver without an
HEVC decoder for a picture role. Answered once it plays, or with why not (10 s at most).

### `RemoveFromGroup(s id)`

Takes receiver `id` out of the group it plays in: it gets `stop`, the others play on untouched; the last one out ends
the group. `InvalidArgs` when it is in none.

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
been opened once (`Connect` or `Mirror`). It never connects by itself. For a TV without Luch:
`{"kind","modes","name","model"}`, `modes` being `media` and `photo`.

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
- `idr` — keyframe requests: `asked`, `merged` (into one already pending: a group's members that lost the same
  frame) and `sent` (IDRs forced). At most one IDR per 500 ms for the whole session; a request within that wait is
  kept and served at its end.

The same sender numbers are sent to the receiver (`sender_stats`) for its on-screen overlay.

### `Info(s id) → s`

What a person should know about a receiver, as JSON: `paired` (its certificate is pinned), `encrypted` (the
channel open now is TLS), `android` (e.g. `Android 5.1`; empty until it has been
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

### `SetInputChoice(s id, s choice)` / `InputChoice(s id) → s`

Whether a receiver's touch screen, keyboard, mouse and gamepads may control this computer without asking
(PROTOCOL.md 4i, [input.md](input.md)): `ask` (default: a notification asks at its first use in a session, with
allow once / always / don't allow) or `always` (what "Always allow" saves). Kept in `~/.config/luch/input`;
`ask` takes an `always` back from the next session.
### `LatencyLab(s id, b on) → s`

The latency lab ([latency-lab.md](latency-lab.md)). `on`: serves a page on `127.0.0.1` (a free port) and returns its
address; opened full screen, it counts this screen's refreshes in Gray code under a QR code of the stream's setup
(version, capture mode, codec, bitrate, size, ping, luchd's own figures). The receiver is told
`{"t":"latency","on":true}` (PROTOCOL.md 4j, now or when mirroring to it starts) and draws its own pattern in the TV's
bottom right corner. Off: the page's server stops, the receiver takes its pattern away, `""` is returned. `id` may be
empty while exactly one receiver is mirroring.

### `Reports() → s`, `ReportUrl(s id) → s`, `DismissReport(s id)`

Crash and error reports the receivers handed over (luchd asks for them with `"want": ["reports"]`), as a JSON array,
oldest first; each as the receiver sent it plus `receiver_model`. Kept in `~/.config/luch/reports` and announced
with a desktop notification that opens the issue. `ReportUrl` is that link (the receiver project's new-issue page
with title and body filled in); `DismissReport` forgets one.

### `Report(s text, b keep_names) → s`

"Report a problem": one file for a person to hand over (a chat, an issue), `Luch-report-YYYY-MM-DD-HHMM.zip` in the
XDG Downloads folder. Nothing is sent anywhere. Inside: `about.txt` (`text`, the versions of Luch, the OS, the desktop,
the machine; each receiver's model, Android, Luch version, decoders and caps from its welcome, the numbers of its
session now or of the last one), `computer/luchd.log` (the last ~1 MB of `journalctl --user -u luchd`),
`computer/gnome-shell.log` (the GNOME extension's lines), `receivers/<name>.log` and `crash-reports/` (the reports
of `Reports`). Each receiver connected now, and each known one on the network that is paired (a short TLS channel of
its own, so the TV shows nothing), is asked for its log (`{"t":"logs"}`, luch-android PROTOCOL.md) at once, 10 s at
most each; one that does not answer, an older one and an unpaired one are noted instead. Unless `keep_names`, IPv4
addresses become `192.0.2.N`, IPv6 `2001:db8::N`, MACs `02:00:00:00:00:NN`, this computer's name and id
`Computer`, the receivers' names and ids `Receiver N`, the login name `user` and the home folder `~`, the same
stand-in each time within the file. Returns JSON: `path`, `description` (a few lines for a chat: what happened,
versions, receivers, the file's name), `issue` (luch-linux's new-issue page with title and body filled in, asking for
the file), `notes` (who gave no log, and why) and `can_send` (this build has a report address: `SendReport` works).
Takes up to ~12 s.

### `SendReport(s path) → s`

"Send to the developers", only when the person presses it after seeing the file: the report at `path` (one `Report`
made since luchd started; no other file) goes to the developers' GlitchTip (Sentry-compatible), no account needed.
The address is a DSN (`https://<key>@<host>/<project>`) built in with `LUCH_REPORT_DSN=... cargo build`, or the same
variable in luchd's environment; without one this fails and `can_send` is false. Two Sentry envelopes are POSTed to
`https://<host>/api/<project>/envelope/` (`X-Sentry-Auth: Sentry sentry_version=7, sentry_key=<key>,
sentry_client=luch/<version>`): an `event` (level `info`, "Problem report: <first line of what happened>", release
`luch-linux@<version>`, tags `app`, `os`, `receivers` (models), `codec`, contexts `os` and `device`, and in `extra`
what happened, `about.txt` and each log of the file by its name, the last 64 KB of each, under 800 KB in all: as
redacted as the file), then the zip as an `attachment` of the same event (best effort: a server that refuses it does
not fail the send). Returns JSON: `number` (`LUCH-` and the event id's first 8 hex digits, for the person to tell in
a chat), `event_id`, `attached` and `attach_error` when the zip did not go. Errors say why (unreachable, refused key,
too many reports now).

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
without an extension), `poster`, `via`, `headers` and `transport` (`luch`, `cast` or `dlna`: another way to the same TV
than the one luchd picks; `InvalidArgs` when the TV has none such). A link is fetched by the receiver itself (`"via":"direct"`, the
default; the computer may go to sleep meanwhile), with `headers` (only `Referer`, `User-Agent`, `Origin`, `Cookie`,
`Authorization`). `"via":"sender"` relays it through luchd: luchd fetches it with those headers and passes the
bytes on unchanged, rewriting HLS playlists to point back at itself. That is for links only this computer can reach:
a company VPN, a site that needs the browser's login, Wi-Fi Direct. Local files always go through luchd.

Files are served on TCP port 47810 (any free port if it is taken), each under a random token and only to the receiver
it was cast to; open that port in a firewall. Nothing is served once the cast ends. For TVs without Luch the server
also answers DLNA's `getcontentFeatures.dlna.org` and `transferMode.dlna.org`, Samsung's `CaptionInfo.sec`, CORS for
a Chromecast's page (with its `OPTIONS` preflight), and serves SubRip files as WebVTT to a Chromecast.

A TV without Luch cannot send a page's headers, so a link with `headers` goes through luchd there unless `via` says
otherwise.

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

### `Present(s id, s file) → s`

Shows a PDF or a presentation (`.odp`, `.pptx` and the other documents `Cast` turns into PDF) on the receiver and
returns the address of its presenter view, `http://127.0.0.1:<port>/<token>/`, to open in a browser on this computer:
current and next slide, speaker notes, timer, slide counter, laser pointer and ink. See
[presenting.md](presenting.md). Counts as presenting for `Quiet`.

### `Annotate(s id, s msg)`

Points or draws on what the receiver shows for us: `msg` is a `pointer`, `ink` or `blank` message of PROTOCOL.md 4h
as JSON (coordinates are fractions of the picture). Cleaned (known fields only, coordinates clamped) and passed on
as it comes: a caller sending pointer moves keeps them to one a frame itself.

### `AnswerQuiet(b hide)`

The answer to `QuietAsk`: hide notification banners while presenting or mirroring, or not. Kept until `luchd` stops.

### `Sleep(s id)`

Puts the receiver to standby, as its remote's power key (PROTOCOL.md 4c). Fails with what is missing: the receiver's
accessibility switch for Luch is off, or its Android is older than 9. `Wake` turns it on again.

### `WebClients() → s`, `ApproveWeb(s request, b allow)`, `ForgetWeb(s id)`

The browsers allowed to watch this screen through the web receiver (`id`, `name`, `address`, `allowed`, `used`),
and the one waiting to be (`waiting`, `code`); letting it in or not (the notification's buttons do the same);
forgetting an allowed one (it is asked about again next time).

## Signals

### `PairChanged(s id, s state)`

Pairing moved on; `state` as in `Pairing`. The GNOME extension asks for the code on `code`.

### `WebApproval(s request, s name, s address, s code)`

A browser opened the web receiver and waits: its page shows `code`. luchd also asks with a desktop notification
(Allow / Deny).

### `MediaStateChanged(s id, s state)`

Every `media_state` from a receiver, and `{"state":"stopped","reason":...}` when it ends.

### `FindProgress(s status)`

What the discovery ladder is trying, on every change:

```json
{"running": true, "mode": "all", "found": 0, "elapsed": 4.3,
 "rungs": [{"rung": "mdns", "state": "done", "note": "0 found"},
           {"rung": "scan", "state": "done", "note": "asked 253 addresses on wlan0: no receiver"},
           {"rung": "ble", "state": "running", "note": "listening for receivers over Bluetooth"},
           {"rung": "sound", "state": "waiting", "note": ""}, {"rung": "qr", "state": "waiting", "note": ""}]}
```

`state`: `waiting`, `running`, `done`, `failed` (the note says why: "Bluetooth is off", "no Bluetooth adapter"),
`skipped` (a receiver was found before it was needed). The last one has `"running": false`. The GNOME menu shows
the running rungs instead of "Searching…", and offers "Search harder" (`Find("all")`) when the list is empty.

### `DirectChanged(s status)`

A direct connection's step: `{"step": "...", "name": "Living room TV", ...}`. `step`: `idle`, `looking` (listening
over Bluetooth), `opening` (the TV's Bluetooth channel), `pairing` (with `key`, for `Pair`), `asking` (the TV asks
its person), `joining` (with `ssid`, `band`), `connecting`, `on` (with `id`, `ssid`, `previous`: the network to go
back to), `back` (with `note`) or `failed` (with `reason`). The GNOME menu follows it, offers "Connect directly
(Wi-Fi Direct)…" (asking first) and then "Leave the TV's network".
### `QuietAsk()`

A presentation or a mirroring session began, `Quiet` is `ask` and nobody has answered yet: ask the person (the GNOME
extension does, once a login) and call `AnswerQuiet`.

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

### `Quiet` (`s`, read/write, default `ask`)

Do not disturb while presenting (`Present`) or mirroring (not sound only): `on` turns GNOME's notification banners off
(`org.gnome.desktop.notifications show-banners`) for the session and back on afterwards, `off` leaves them, `ask`
signals `QuietAsk`. Banners the person had turned off stay off. A crash mid-session is undone at the next `luchd`
start (`~/.config/luch/restore-banners`). Saved in `~/.config/luch/quiet`.

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

The first time a browser connects, its page shows a 4-digit code and this computer asks (a notification with Allow
and Deny, or `luchctl web allow <code>`) whether to let it watch: allow it only if the page in front of you shows
the same code. An allowed browser keeps a cookie (a year, HttpOnly) and is let straight in afterwards; at most 20
are remembered (`luchctl web`, `luchctl web forget <n>`). Three refusals from an address and it is not asked about
for an hour; one question at a time.
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
| `~/.config/luch/sender-cert.pem`, `sender-key.pem` | this computer's certificate for TLS receivers (the key owner-only) |
| `~/.config/luch/paired.json` | receivers paired with: their certificate's SHA-256, pinned |
| `~/.config/luch/web-browsers.json` | browsers allowed to watch through the web receiver (a hash of each one's cookie) |
| `~/.config/luch/known.json` | receivers luchd has connected to, with the ways to wake them |
| `~/.config/luch/restore-token` | screencast portal restore token (no picker after the first time) |
| `LUCH_KMS_SOCKET` | a privileged `luch-kms --serve SOCKET [--uid N]` that `luchd` uses for the `fast` mode instead of running `luch-kms` itself (default `$XDG_RUNTIME_DIR/luch/kms.sock`, used when it exists): for a `luchd` in a sandbox (Flatpak), where file capabilities do not work. The socket is owned by `--uid`, mode 0600 |
| `LUCH_KMS` | the `luch-kms` to run (default: the one next to `luchd`, then `PATH`): one installed with `setcap` outside a `nosuid` mount such as many `/home` setups, where file capabilities are ignored |
| `LUCH_DRM_FORMAT` | the one NV12 DRM format the screen capture asks `vapostproc` for (e.g. `NV12:0x0100000000000009`); by default the NV12 modifiers `vapostproc` offers on this GPU, from its caps |
| `LUCH_KMS_CARD`, `LUCH_KMS_RENDER` | DRM card and render node for `luch-kms` (default: the card with an active plane, and the render node of the same GPU) |
| `LUCH_PROBE=1` | per-stage latency probes in the `luchd` log |
