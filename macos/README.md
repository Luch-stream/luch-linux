# Luch — macOS sender

A second sender for the Luch protocol: the same receiver, the same UDP format, a different platform
underneath. It speaks v0 exactly as [PROTOCOL.md](https://github.com/Toxblh/luch-android/blob/main/PROTOCOL.md)
describes, so no change is needed on the TV side.

| Part | What it uses |
|---|---|
| Capture | ScreenCaptureKit: display, NV12 video range, cursor drawn in; the size follows the client |
| Audio | ScreenCaptureKit system audio → libopus, 48 kHz stereo, CBR 128 kbit/s, 20 ms frames |
| Video | VideoToolbox H.264 or HEVC, real time, no B-frames, IDR on request, Annex-B with SPS/PPS before every IDR |
| Discovery | DNS-SD (Bonjour), a sweep of the LAN, Bluetooth LE, the TV's sound and its QR code; every socket is pinned to a LAN interface, never a VPN's |
| Control | Unix socket + JSON, the stand-in for the Linux sender's D-Bus API |

## Requirements

- macOS 14 or newer
- Swift 5.9+ (Xcode command line tools)
- libopus: `brew install opus`

## Build and run

```sh
swift build -c release
./Tools/make-app.sh          # builds Luch.app next to the package
open .build/Luch.app
```

macOS asks for **Screen Recording** and **Local Network** access on first use. The Screen Recording
grant is tied to the code signature, so an ad-hoc build needs a fresh grant after every rebuild. Pass a
stable identity to keep it:

```sh
SIGN_ID="<your code-signing identity>" ./Tools/make-app.sh
```

`swift test` runs 27 tests: header layout, shard reassembly, OpusHead, Opus frame cutting, AVCC→Annex-B,
the keepalive bounds, the codec weights and the quality presets.

## The codec

The receiver's `welcome` says which decoders it has, how fast each one measured, which size it accepts
and how its screen is shaped. The sender grades the codecs both can do and picks the best trade-off:

- **HEVC** for its bitrate at the same quality (a quarter less than H.264), **H.264** for reach;
- a codec the receiver names as its fastest gets a small time credit — not a flat bonus, so at equal
  decode times HEVC still wins on bitrate;
- a decode time **measured in a real session** outranks the receiver's own benchmark (that one drains
  into an `ImageReader` and reports the decoder core, not the path to the screen), and it is remembered
  per receiver and per codec;
- the decoder's own maximum size is the last word.

`luch caps "<receiver>"` prints the grading and the plan:

```
weights: preferms=3.0,bytes=25.0,compat=15.0,ms=2.0,base=20.0,hd60=5.0,nohd60=-25.0,lowlat=5.0
  HEVC   65.0  (5.0 ms measured)  bytes+25.0 ms+30.0 1080p60+5.0 lowlat+5.0
  H.264  42.0  (11.5 ms bench)    compat+15.0 ms+17.0 1080p60+5.0 lowlat+5.0
plan: HEVC 1662x1080@60 audio=true
```

## Size and quality

Two independent knobs:

- **Size** — whose geometry wins: `source` (the Mac's own aspect), `client` (the client's screen, the
  desktop cropped), `fit` (the Mac's aspect inside the client's pixels) or `average` (half-way). On a
  20:9 phone `fit` keeps the whole desktop, without stretching and without cutting it.
- **Quality** — a preset: `auto` (exactly what the client reports), `720p`, `1080p`, `2K`, `4K`. The
  preset picks the height, the size mode picks the aspect; a decoder that cannot do 1080p60 caps the
  stream at 30 fps, and one that cannot do 4K clamps the size proportionally.

## The client's state

`welcome.hw.device` and the per-second `stats` carry the client's battery, charging state, thermal
status and thermal headroom (`PowerManager.getThermalHeadroom`: 1.0 cool, below 0.3 throttling soon).
The sender **shows** them and never lowers the quality behind the user's back: the menu line says
`🔋 on battery` or `🌡0.62` / `🔥0.21`, and the presets above 720p get a `drains the battery faster`
note while the client runs on its own battery. The receiver reports these on change only, never by
polling.

## Use

`luchctl` in a terminal: the app links it into Homebrew's `bin` and registers its Tab completion for zsh, bash and
fish at every launch (commands, each one's words, receivers' names from the app, files where a file goes; the
shells ask `luchctl __complete`). A new terminal picks it up; `luchctl completion zsh|bash|fish` prints the shell's
side by hand.

Menu bar: pick a receiver → **Mirror Screen**. While streaming the menu shows the estimated latency
(capture + encode + one-way network + the receiver's decode), host time, RTT, the decoder's own number,
the receiver's battery and thermal headroom.

Submenus: **Warm picture** (a slider: pulls the blue chroma down and the red up in the NV12 plane before
the encoder, weighted by luma so dark UI stays neutral — for night viewing, applied live), **Quality**
(`auto/720p/1080p/2K/4K`), **Size** (`source/fit/average/client`), **Codec** (`auto/H.264/HEVC`),
**Bitrate**, **Display**, and the login item.

Without the app:

```sh
.build/debug/luchctl list
.build/debug/luchctl mirror "XGIMI MoGo 4 Laser"
.build/debug/luchctl mirror "S24+" --codec hevc --preset 1080p --size-mode fit
.build/debug/luchctl warmth 0.35      # 0...1, applied live
.build/debug/luchctl stats
.build/debug/luchctl stop
```

`luch` is the engine without the app, useful for testing and for receivers found by address:

```sh
.build/debug/luch list
.build/debug/luch caps "XGIMI MoGo 4 Laser"      # graders, plan, receiver state
.build/debug/luch hw                            # what this Mac's encoders can do
.build/debug/luch mirror "XGIMI MoGo 4 Laser" --preset 720p --size-mode source --warm 0.3
.build/debug/luch mirror --synthetic --ip 127.0.0.1   # no capture permission needed
.build/debug/luch sound "Living room TV"               # the TV as a speaker: this Mac's sound, no picture
```

`luchctl` talks to `Luch.app` over `~/Library/Application Support/Luch/control.sock`; if the app is not
running it says so instead of opening a second session.

## Test without a TV

`Tools/fakerecv.py` is the receiver's `testsend.py` in reverse: it answers hello/welcome and start/ready,
reassembles the shards, checks the header, Annex-B framing and the SPS/PPS-before-IDR rule, writes the
elementary stream and asks ffmpeg whether it decodes.

```sh
python3 Tools/fakerecv.py --seconds 20 --out .tmp/capture.h264 &
.build/debug/luch mirror --synthetic --ip 127.0.0.1 --seconds 8
```

## What works

- display mirroring with the cursor, H.264 and HEVC, a size that follows the client;
- codec choice by weighted grading, remembering what a real session measured;
- system audio (Opus, 20 ms frames), `OpusHead` before every IDR;
- sound only (`luch sound`, "Play Sound Only" in the menu): the TV plays this Mac's sound and shows what plays;
- what plays (title, artist, album, cover, position) goes to the TV as `now_playing` during any session (below);
- VPN-safe: the control channel and both media sockets are bound to the interface the receiver was
  found on, never to the default route;
- keepalive: repeats the last access unit when the screen is still — P-frames at 2 fps, an IDR more
  slowly — so the receiver's 15 s silence timeout never fires (ScreenCaptureKit is change-driven and
  delivers ~1 fps on a still desktop);
- menu bar with receiver list, live latency, warmth, quality, size, codec, bitrate, display and login item;
- the client's battery, charging, thermal state and headroom, shown but never acted on silently;
- `luchctl` over a Unix socket;
- what the TV shows, per TV in its **Screen** menu or `luchctl mirror [r] <source> [which]`, with luchd's
  words and the Mac's: `fast`/`asis` (this screen as it is), `virtual`/`extend` (a second screen),
  `speed`/`quality`/`mac` (drawn at one of the TV's modes), `monitor [n|name]` (one of several displays),
  `window [app or title]` (one window alone, fitted into the TV's shape, followed as it moves), `audio`;
- input from the TV (PROTOCOL.md 4i): a keyboard, mouse or touch screen on the receiver drives this Mac
  (keys by position, the Mac's own layout makes the characters; a finger is the pointer). "Allow TV to
  Control This Mac" in the TV's menu; the first use asks (once, always, no), then macOS asks once for
  Accessibility. "Always" is kept per TV; "Take Back Control" ends it during a session;
- pairing and encryption (below): TLS to the receiver, its certificate pinned, every frame sealed.

## Pairing and encryption

As luchd (PROTOCOL.md 3a, 4): a receiver that announces a TLS port (`tls=` in its TXT) is reached over TLS with
this Mac's own certificate (`sender-cert.pem`, `sender-key.pem` readable only by you, in
`~/Library/Application Support/Luch`, made once; no keychain). The first time, the TV shows a code; the app asks
for it in a small window (or `luchctl pair`, or any `luchctl` command that connects asks on the terminal); both
sides then pin each other's certificate (`paired.json`) and later connections need nothing. A paired receiver is
never reached without TLS again. Over TLS each session's frames are sealed with AES-128-GCM before they are cut
into datagrams (a 200 KB IDR takes about 25 µs on an M-series Mac), and the receiver hands over its crash and error
reports (`luchctl reports`, a notification that opens the issue to file). Older receivers keep the plain port and
are asked about on the TV each time.

```bash
luchctl pair [r] [code]      # pair now; the code may be given
luchctl paired               # receivers this Mac is paired with
luchctl forget <r>           # pair again next time (a receiver reset or reinstalled)
luchctl reports              # what receivers reported; report <n> [open|dismiss]
```

## Report a problem

"Report a Problem…" in the menu (or `luchctl report [--keep-names] [what happened…]`) asks what happened and makes
one zip in Downloads, shown in Finder: this Mac's log (the app keeps it in `~/Library/Logs/Luch/luch.log`, two
files of 1 MB, where Console.app shows it too), the logs of the paired receivers on now that can hand theirs over
(`{"t":"logs"}`; one not paired is not asked, as that would ask on its TV), `about.txt` (versions, this Mac, each
receiver's welcome, the last session's plan and stats, what you wrote) and the receivers' crash reports. "Hide
addresses and names" (on unless `--keep-names`) turns IPv4 addresses into 192.0.2.N, IPv6 into 2001:db8::N, MACs
into 02:00:00:00:00:NN and the names of this Mac, its user and the receivers into Computer, User, Receiver 1….
Nothing is sent by itself: Copy Description gives a few lines for a chat, Open an Issue a filled-in issue to attach
the file to, and in a build with a report address **Send to the Developers** (or `luchctl report --send`) sends it,
with no account, to the developers' GlitchTip as one event (what happened, `about.txt` and the last 64 KB of each
log, as hidden as the file) plus the zip, then shows its number (`LUCH-` and 8 hex digits) with Copy Number. The
address is a DSN set when building, `REPORT_DSN="https://<key>@<host>/<project>" ./Tools/make-app.sh` (Info.plist
`LuchReportDSN`; `LUCH_REPORT_DSN` in the app's environment overrides it); without one the button is not there.

## Not yet

- gamepads from the TV: a virtual one needs a DriverKit HID driver (`com.apple.developer.hid.virtual.device`);
- a stylus's pressure: the stylus is the pointer, as a finger.

## Numbers

Apple M4 Max → Samsung S24+ over Wi-Fi, HEVC 1662×1080@60: capture + encode + send **2–5 ms**,
56–58 fps, RTT 7–10 ms, the receiver decodes in **5–11 ms**. H.264 on the same phone decodes in
**75–85 ms** live — its own benchmark says 12.7 ms, because the benchmark measures the decoder core
rather than the path to the screen. That is why the sender prefers HEVC here and why a measured decode
time outranks the benchmark.

The display adds up to a frame period on top, and is not counted: a 60 Hz panel adds about 8 ms on
average.

## Notes

- 5 ms Opus frames crackle on Android: the receiver's `AudioTrack` buffer is about four frames (20 ms),
  so Wi-Fi jitter cuts them up. 20 ms frames (50 packets per second) measured best; `--frame-ms` switches
  it at runtime.
- VideoToolbox does not always attach `kCMSampleAttachmentKey_NotSync`, so the IDR flag is taken from
  the bitstream (NAL type 5), and the parameter sets it puts in the format description are stamped in
  front of every IDR — the receiver configures its decoder without `csd-0`.
- The warm filter walks the chroma plane once per frame: well under a millisecond at 1080p, no Core
  Image and no extra buffer.

## Casting

Besides mirroring, the Mac can hand a receiver things to play or show by itself, in their own quality
(luch-android PROTOCOL.md 4b-4g), exactly as the Linux sender does:

```sh
luchctl cast film.mkv film.en.srt          # the TV plays the file itself; a 4K file in 4K whatever this screen is
luchctl cast ~/Pictures/*.jpg              # photos, left/right on the remote
luchctl cast report.pdf                    # a PDF page by page; text too (cupsfilter), office files with LibreOffice
luchctl cast https://youtu.be/<id>         # opens in the TV's own YouTube app
luchctl cast https://example.com/v.m3u8    # the TV fetches the link; --relay passes it through this Mac
luchctl media pause                        # play | pause | toggle | seek 1:30 | skip -10 | rate 1.5 | audio a2 | sub s0
luchctl volume 3 | page next | sleep | stop-cast
luchctl browser install                    # lets the Luch browser extension (luch-linux browser-extension/) reach the app
```

Luch.app serves cast files on TCP port 47810 to that receiver only (a random token per file); macOS asks to let the
app accept incoming connections the first time. While mirroring, the TV remote's media keys (play/pause, next,
previous) are passed on as the Mac's own media keys, so Music, Spotify or a browser's video obey them; macOS asks
for Accessibility access for that once.

What plays comes from MediaRemote, the source of Control Center's player. Since macOS 15.4 it answers only Apple's
own processes and returns nothing to other apps, so a small library (`libLuchNowPlaying.dylib`, in the app's
Frameworks) is loaded into the system `/usr/bin/perl`, which is one of them, and reports changes as JSON lines; the
open mediaremote-adapter does the same. If Apple closes that too, the TV simply shows no title.

**From Finder.** Select photos, videos, music, a PDF or an office file, then right-click → *Quick Actions* (or
*Services*) → **Show on TV with Luch**; a PDF or a presentation also has **Present on TV with Luch**, which opens the
presenter view. Several photos are one slideshow. With one TV it goes there at once; with several, a short menu
under the pointer asks which. Files dropped on the menu bar icon do the same (a presentation dropped there can be
shown or presented). These are the app's Services (`NSServices` in Info.plist, written by `Tools/make-app.sh`); macOS
lists them once Luch.app has run from its place, and `/System/Library/CoreServices/pbs -update` makes it look again.
If they are not in the menu, turn them on in System Settings → Keyboard → Keyboard Shortcuts → Services → Files and
Folders.

## Presenting

`luchctl present [r] talk.pptx` (or *Present…* in a TV's menu) shows a PDF or a presentation on the TV and opens the
presenter view in the browser: the current and next slide, the speaker notes, a timer. The mouse over the slide is
the TV's laser pointer, dragging draws, a clicker or the arrow keys turn the slides; the TV draws pointer and ink
itself (PROTOCOL.md 4h). It is luchd's own page (`luchd/src/present.html`, `present.js`; `Tools/sync-present.sh`
copies them), served on 127.0.0.1 only; slides are drawn with PDFKit. Notes come from the `.pptx`/`.odp` itself, or a
PDF's `.pdfpc` file or sticky notes. `luchctl blank [r] on|off` (*Black Screen* in the menu) blacks out the TV, also
while mirroring.

`luchctl quiet ask|on|off` (*Do Not Disturb While Presenting*): macOS has no API for Focus, so the app makes two
shortcuts with Shortcuts' Set Focus action, *Luch Quiet On* and *Luch Quiet Off*, and Shortcuts asks to add them the
first time; `ask` asks once while the app runs. A Focus already on is left alone when macOS lets the app read
`~/Library/DoNotDisturb/DB/Assertions.json` (Full Disk Access on some versions). While mirroring, macOS itself hides
notifications unless *Allow notifications when mirroring or sharing the display* is on.

## In step, sound modes, the latency lab

Several receivers can play one capture in step (PROTOCOL.md 4k), as luchd's groups: "Play Together…" in the menu,
or the Linux luchctl's words. One capture and one encode; every member gets the streams of its role on its own
sockets, with the playout delay D in each packet, chosen from what the members report by the same policy as luchd's
(the same test vectors, `SyncTests`). The receivers' clock requests are answered on the media sockets.

```sh
luchctl group watch "Living room TV" Kitchen Den --source audio   # this Mac's sound on three speakers, one delay
luchctl group interactive "Living room TV" Kitchen=audio          # the picture as fast as alone, the sound waits
luchctl offset Kitchen 40                  # its own correction (positive = later), kept per output route
luchctl stats                              # also D and each member's need, route and correction
luchctl audio "Living room TV" sync        # auto | on | off | sync: the sound of the next session
luchctl latency on                         # the lab's page opens here, the TV draws its pattern (docs/latency-lab.md)
luchctl latency analyze clip.mov           # runs luch-latency (cargo install --path luch-latency; brew install ffmpeg)
```

The TV's menu has the same: its sound mode, a slider for its correction, and the latency lab.

## Finding and waking receivers

When Bonjour finds nothing (hotel Wi-Fi, a VPN, a mesh that drops multicast), the Mac climbs the same ladder as luchd
(luch-android PROTOCOL.md 1): a sweep of every address of each LAN interface (never utun, ipsec or a VM bridge), then
Bluetooth LE; on request also the TV saying its address by sound (microphone) and its QR code (camera). Microphone,
camera and Bluetooth are asked for only when that way is used. The menu has Find Receivers and Add Receiver; TVs
connected to before stay in it while they are off, with Wake.

```sh
luchctl find                                # all ways, what is tried as it goes; `find scan,ble` just those
luchctl add 'luch://connect?host=…&t=…'     # the TV's QR link, or its address: luchctl add 192.168.1.20
luchctl known                               # receivers talked to before, on or off, and how each wakes
luchctl wake 'Living room TV'               # Wake-on-LAN; the remote's Bluetooth signal through another Luch device
luchctl wake 'Living room TV' learn         # with the TV off, press its remote's power key near the Mac
luchctl info | id | refresh
```

macOS lets no app advertise manufacturer data (CoreBluetooth sends only a name and service UUIDs, and IOBluetooth's
HCI calls do nothing since the stack moved into bluetoothd), so the Mac can learn the remote's signal but not send
it: a Luch receiver or phone that is on and lists `ble_wake` in its welcome sends it for the Mac.

### Connecting directly

When the Mac and the TV share no network that lets them talk (a hotel's Wi-Fi that keeps devices apart), a receiver
heard over Bluetooth with a PSM in its card is asked for its Wi-Fi Direct network: the Mac opens an L2CAP channel to
it, runs the receiver's TLS control protocol over it (pin, pairing code, the person's Allow on the TV), and joins
the network it answers with as an ordinary Wi-Fi network; the receiver is then listed at its address there, and
mirror, cast and the rest work as on any LAN. It is the last way of `find all` and `find direct`, never automatic:
the Mac asks first, as it leaves its Wi-Fi ("Connect directly to <TV>? This Mac leaves '<network>' while showing,
and comes back after."), and goes back when showing ends, after 2.5 minutes of nothing shown, or on `direct off`.
Ethernet keeps its internet meanwhile. Wi-Fi names are Location data on macOS 14+: Location is asked for the first
time it is used; without it the Mac cannot tell which network it left, and macOS's auto-join picks where to go back.

```sh
luchctl direct                              # the nearest receiver heard offering it; asks on the terminal first
luchctl direct 'Living room TV'             # or by name or card id; `luchctl find` lists them ("direct possible")
luchctl direct off                          # leave its network now and go back
```

## TVs without Luch and the web receiver

Chromecasts and Google TVs (mDNS `_googlecast._tcp`, Google Cast on port 8009) and DLNA/UPnP renderers (SSDP, on
nearly every smart TV) are listed next to the Luch receivers, one row per TV (a TV with Luch keeps its Luch row), and
`luchctl list` shows how each is reached (luch, cast, dlna). `cast`, `media`, `volume` and `stop-cast` work for them
as in luch-linux's docs/other-tvs.md; they play files and links, not this screen. The file server answers what they
ask for: DLNA's content features, CORS for a Chromecast's page, SubRip as WebVTT.

Any browser with WebCodecs (a TV's will do) can watch this screen: open `https://<this Mac>:47801/` (Web Receiver in
the menu has the address; the certificate is this Mac's own, so the browser asks once). A new browser shows a 4-digit
code and the Mac asks whether to let it in, showing the same code; an allowed one keeps a cookie and is not asked
again. `luchctl web` lists the allowed browsers and the one waiting, `luchctl web allow|deny <code>` answers it,
`luchctl web forget <n>` forgets one.
