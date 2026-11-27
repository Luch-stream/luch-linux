# TVs without the Luch app

A hotel TV, a friend's Chromecast, a TV whose shop has no Luch: "cast this video" and, later, "show this screen"
should work there too. This page compares the ways such TVs can be reached, says which luchd uses and in what
order, what is built, how it is tested without hardware, and the plan for mirroring.

## The ways, side by side

| | Luch | Google Cast | AirPlay 2 | DLNA / UPnP AV | Miracast | Roku ECP / DIAL |
|---|---|---|---|---|---|---|
| Found by | mDNS `_luch._tcp` | mDNS `_googlecast._tcp` | mDNS `_airplay._tcp`, `_raop._tcp` | SSDP `MediaRenderer:1` | Wi-Fi Direct (P2P IE with WFD); MS-MICE: mDNS `_display._tcp` | SSDP `roku:ecp`, DIAL `urn:dial-multiscreen-org:service:dial:1` |
| Who has it | TVs with the app | Chromecast, Google TV, Android TV, some Sony/Philips/TCL/Vizio | Apple TV; Samsung, LG, Sony, Vizio, Roku TVs since ~2019 | nearly every smart TV since ~2012, Kodi, VLC (as renderer), gmrender | most Android, Samsung, LG TVs and sticks; Windows | Roku players and TVs; DIAL: many TVs (YouTube, Netflix) |
| Media (the TV fetches a link) | yes | yes (MP4, WebM, HLS, DASH) | yes (HLS, MP4) | yes, formats per TV, strict MIME | no | Play on Roku (MP4, HLS) |
| Mirroring | yes, 15-40 ms est. | Cast Streaming, ~150-300 ms | yes, FairPlay-encrypted | no (a live MPEG-TS "video": 2-5 s) | yes, ~100-250 ms | Roku's mirroring is Miracast |
| State | pushed | pushed (MEDIA_STATUS) | pushed (/playback-info polled by most) | polled (GENA events exist, often buggy) | none | polled (`/query/media-player`) |
| Volume | yes | yes | yes | yes (RenderingControl) | the TV's own | key presses |
| Subtitles | SRT, VTT, ASS | WebVTT, TTML (CORS needed) | in the HLS only | vendor extensions: Samsung `CaptionInfo.sec`, `res text/srt`, `pv:subtitleFileUri`; many TVs ignore all | burned in | no |
| Start of a video | ~1 s | 2-4 s (launching the receiver app) | 1-2 s | 1-4 s | 2-6 s (P2P group, RTSP) | 2-4 s |
| A command shows after | ~30 ms | ~150 ms | ~150 ms | 0.2-1 s, state up to a poll later | - | ~0.5 s |
| Prompts on the TV | none after pairing | none on a Chromecast; Google TV may need "let others control cast media" | PIN on first pairing (or "allow" on Samsung/LG) | Samsung/LG/Sony ask "allow this device" once; Sony needs "renderer access" on | "allow connection from ..." every time or once (PIN/push button) | "Control by mobile apps" must allow network access (Roku OS 10+) |
| Open to implement | ours | yes: CASTV2 is TLS + protobuf + JSON, documented by many open clients; device auth optional for senders | media: pair-setup/pair-verify (SRP, Ed25519, ChaCha20) is reverse-engineered (pyatv); mirroring needs FairPlay, which is Apple's: no clean way | yes, open standards (UPnP Forum, DLNA guidelines are paid but widely known) | yes: Wi-Fi Display spec (Wi-Fi Alliance), RTSP + MPEG-TS; gnome-network-displays is the Linux reference; MS-MICE is published by Microsoft | ECP is documented by Roku; DIAL by Netflix/YouTube |

Notes:

- **Google Cast.** TLS on 8009, each message a 4-byte length and a protobuf `CastMessage` (`cast_channel.proto`)
  carrying JSON in a namespace: `tp.connection` (CONNECT/CLOSE of a virtual connection), `tp.heartbeat`
  (PING/PONG, a silent sender is dropped after about 10 s), `receiver` (LAUNCH, STOP, GET_STATUS, SET_VOLUME on
  `receiver-0`), `media` (LOAD, PLAY, PAUSE, SEEK, SET_PLAYBACK_RATE, EDIT_TRACKS_INFO, MEDIA_STATUS on the app's
  `transportId`). The Default Media Receiver (app id `CC1AD845`) is a web page on the TV: it fetches with an
  `Origin`, so HLS/DASH and text tracks need CORS, and it reads WebVTT only. The TV's TLS certificate is self-signed;
  senders that care check it through the `deviceauth` namespace against Google's device CA.
- **AirPlay.** AirPlay 1's `POST /play` with a link still works on older Apple TVs; AirPlay 2 receivers want
  HomeKit-style pairing first, with a PIN shown on the TV. That is doable (pyatv does it). Mirroring, and some
  receivers' streams, need FairPlay, which is not ours to reimplement: no mirroring over AirPlay.
- **DLNA.** The TV fetches the link itself and is picky: it asks `getcontentFeatures.dlna.org: 1` and wants
  `contentFeatures.dlna.org` back (DLNA.ORG_OP for seeking, DLNA.ORG_FLAGS), checks the MIME type against the
  DIDL-Lite `protocolInfo`, seeks with `Range`. `ConnectionManager.GetProtocolInfo` lists what it plays.
- **Miracast** needs the Wi-Fi radio itself (a P2P group next to, or instead of, the hotel Wi-Fi), so it is for
  mirroring when nothing else is there. MS-MICE runs the same RTSP over the normal network, but few TVs have it.
- **Roku**: `POST /launch/<app>` and `/input/15985?t=v&u=<link>` (Play on Roku) on port 8060; DIAL's
  `POST /apps/YouTube` opens YouTube on most TVs, which is the way to "open in the TV's YouTube" without Luch.

## The ranking luchd uses

A TV is one row whatever it speaks: everything at one address is put together (two Chromecasts at one address are
a TV and a speaker group: two rows). The best way names the row and is used:

1. **Luch**: everything (mirroring, media, photos, documents, the TV's apps), the lowest latency.
2. **Google Cast**: state pushed to us at once, subtitles, speed; a slower start.
3. **DLNA**: on almost every TV, but state polled once a second, one speed, subtitles only on some TVs.

AirPlay (media) would come between Cast and DLNA, Roku after DLNA; neither is built yet. `Devices` gives each row's
kind, what it can do and `control_ms`; `Cast` takes `"transport":"luch"|"cast"|"dlna"` to choose another way by
hand (not shown in the front-ends on purpose).

For mirroring the order is Luch, then Cast Streaming, then Miracast; AirPlay mirroring is out (FairPlay) and DLNA's
live MPEG-TS is a last resort with seconds of delay.

## What is built

- Discovery: `_googlecast._tcp` through Avahi (only devices with video out), SSDP M-SEARCH at start, every minute
  and on `Refresh` (devices not seen for 200 s are dropped). Both join `Receivers` without changing its signature;
  `Devices` is new.
- `luchd/src/tv/`: `castv2.rs` (the protobuf framing by hand, TLS, virtual connections, heartbeat, LAUNCH, LOAD with
  WebVTT tracks, commands, volume, STOP), `dlna.rs` (SOAP, DIDL-Lite with the subtitle extensions, polling,
  commands, RenderingControl volume), `ssdp.rs`, `mdns.rs`, and `mod.rs`: grouping, ranking, and the session that
  tells `media_state` like a Luch receiver (on a change, a seek, every 5 s while playing; `by` sender or receiver).
- `cast::Server`, which every TV fetches from, now also answers `getcontentFeatures.dlna.org` and
  `transferMode.dlna.org`, adds Samsung's `CaptionInfo.sec`, sends CORS to a page that asks and answers its `OPTIONS`
  preflight, and serves SubRip as WebVTT to a Chromecast.
- Not yet: AirPlay, Roku, YouTube through DIAL, DLNA's `GetProtocolInfo` check before casting, GENA events, pinning a
  Chromecast's certificate.

## Tested without hardware

- `tv/fake_dlna.rs`: a renderer that answers M-SEARCH in the real group on port 1900 (the tests search with TTL 0,
  so nothing leaves the machine), serves a device description, and checks every SOAP call as a strict TV does:
  envelope and encoding style, the quoted SOAPAction against the body, arguments present and in order, InstanceID,
  the AVTransport state machine and its error codes (701, 710, 711, 714, 716, 717, 718), and it fetches the link with
  `getcontentFeatures.dlna.org` and `Range`.
- `tv/fake_cast.rs`: TLS with a self-signed certificate, length-prefixed CastMessages, virtual connections that must be
  opened first, PINGs to the sender and a 10 s drop for silence, LAUNCH/STOP/SET_VOLUME, LOAD with mediaSessionId
  checks, the video fetched with `Range`, text tracks fetched with an `Origin` and refused without CORS or WebVTT.
- `tv/tests.rs` drives luchd against both: found, played, paused, sought, volume, played to the end, stopped on the
  TV's remote, taken over by another app, a silent heartbeat for 11 s; and the whole D-Bus API with both fakes at one
  address (one row, two transports, the hidden choice).
- `contrib/tv-test/gmediarender.sh`: the same against **gmrender-resurrect**, a real UPnP renderer, in Docker with
  fakesinks that keep time (no sound): found over SSDP, playing 1 s after `Cast`, paused, sought, volume, played out.
- There is no open Cast receiver to run (the Default Media Receiver is Google's page on Google's runtime), so Cast
  is checked against the fake and the protobuf against bytes worked out by hand from `cast_channel.proto`.

## Plan: Cast mirroring (Cast Streaming)

The way Chrome mirrors a tab: an offer/answer over CASTV2, then RTP over UDP, encrypted. Reference: Open Screen's
`cast/streaming` (Chromium, BSD), the spec in `cast/streaming/README` there.

1. Launch the mirroring receiver (app `0F5096E8`), connect to its transport, send an `OFFER` on
   `urn:x-cast:com.google.cast.webrtc`: streams (H.264 or VP8 video at 90 kHz, Opus audio at 48 kHz), each with
   `ssrc`, `aesKey`, `aesIvMask`, `maxBitRate`, resolutions, `targetDelay`; read the `ANSWER` (UDP port, chosen
   streams). Test: the fake Cast receiver answers an OFFER.
2. Packetize encoded frames the Cast way (RTP header plus Cast's frame id, packet id and reference frame), encrypt
   each frame with AES-128-CTR (IV from the mask and the frame id). Test: the fake decrypts and reassembles.
3. RTCP: sender reports; the receiver's Cast feedback (ACKs, NACKs: resend), PLI: a keyframe. Test: loss injected
   on loopback.
4. Feed it from luchd's existing capture and VA-API H.264 encoder (no B-frames, short GOP, `targetDelay` 100 ms).
5. On a real Chromecast: the latency lab's flashing box, glass to glass. Expect 150-250 ms; Luch is 30-60 ms.

## Plan: Miracast

1. **Over Wi-Fi Direct**: NetworkManager's `WifiP2P` device (`StartFind`, peers with WFD IEs, our WFD IEs through
   `wifi-p2p.wfd-ies`), connect: the TV usually becomes group owner and hands us an address. Test: two
   `mac80211_hwsim` radios in a Linux VM (Docker Desktop's kernel has no hwsim), wpa_supplicant on both.
2. **RTSP** as the WFD source on TCP 7236: M1-M4 (OPTIONS, GET_PARAMETER of `wfd_video_formats`,
   `wfd_audio_codecs`, `wfd_client_rtp_ports`, SET_PARAMETER of our choice), M5 trigger, M6 SETUP, M7 PLAY, M16
   keep-alive. Test: a fake sink speaking the same, as for DLNA.
3. **Stream**: capture -> VA-API H.264 (CBP/CHP, level 4.1) -> `mpegtsmux` -> `rtpmp2tpay` -> UDP to the sink's
   RTP port; LPCM or AAC sound. gnome-network-displays does exactly this with GStreamer and is the reference.
4. **MS-MICE** (same RTSP over the normal network): mDNS `_display._tcp`, the MICE "source ready" on TCP 7250.
5. Risks: drivers that cannot keep the hotel Wi-Fi and a P2P group at once (the laptop goes offline for the
   session), TVs that insist on HDCP, the TV's prompt each time. Expect 100-250 ms.

## Real-device test plan

Needed: a Chromecast or Google TV, and a DLNA TV. VLC is no DLNA renderer (it only plays from media servers, and
casts to a Chromecast itself); for a second renderer use Kodi with "Allow remote control via UPnP" on, or
gmediarender on another Linux machine. Laptop and TVs on one network, TCP 47810 open on the laptop.

1. `luchctl list`: the Chromecast as `cast`, the TV as `dlna`, within 2 s of `luchctl refresh`. A TV that also runs
   Luch shows once, as `luch` (`gdbus call ... Devices` lists the other ways).
2. `luchctl cast <id> film.mp4 --sub film.en.srt`: the time to the first picture (expect Cast 2-4 s, DLNA 1-4 s); the
   subtitles shown (Cast: always; DLNA: Samsung and LG with SRT, others may not).
3. `luchctl media <id> pause`, `seek 1:30`, `skip 10`, `rate 1.5` (Cast only), `luchctl volume <id> +`, then pause
   with the TV's own remote; `luchctl media <id> watch` shows each `MediaStateChanged`, and `by` should be
   `receiver` for the remote. Expect Cast states within
   0.3 s, DLNA within 1.5 s.
4. A web video from the browser extension (an HLS page): Cast needs CORS (watch the luchd log for `OPTIONS`), DLNA TVs
   often do not play HLS at all: note which do.
5. Let it play out (`ended`, then `stopped` `ended`), stop on the TV (`stopped` `receiver`), start YouTube on the
   Chromecast from a phone (`stopped` `replaced`), `StopCast` (the Chromecast back to its ambient screen).
6. Leave a Cast session paused for 10 minutes: it must stay (heartbeat).
7. Note every prompt the TV shows and which setting it needed.
