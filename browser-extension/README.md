# Luch for the browser

Sends a video from any web page to a TV with Luch. The TV plays the video itself, in the quality the site offers up
to what its screen can show. The page gets the TV's controls along the top of its own player (the site's own controls stay usable below and drive the TV too): play/pause, position, speed, quality,
sound track and subtitles. The TV's remote works too, and both stay in step.

## Setup

1. luchd from this repository is running (`luchctl list` shows the TV).
2. `luchctl browser install` lets the browser reach luchd. It writes a native messaging host manifest for every
   browser it finds, pointing at `luchctl`.
3. Add the extension:
   - **Firefox**: `browser-extension/make-firefox.sh` builds `dist/firefox` (Firefox needs its own manifest);
     then `about:debugging` → *This Firefox* → *Load Temporary Add-on…* → `dist/firefox/manifest.json`.
     Then in `about:addons` → Luch → *Permissions*, allow *Access your data for all websites*: Firefox asks for it
     separately, and without it the extension cannot see a page's videos.
   - **Chrome, Chromium, Brave, Edge**: `chrome://extensions` → *Developer mode* → *Load unpacked* →
     `browser-extension/`. The `key` in the manifest keeps the extension's id
     (`enoggfebgncdlpimolmheljkneecempd`), which the host manifest allows.
   - **A browser installed as a Flatpak** (Chrome from Flathub, say): `luchctl browser install` puts a small script
     into its data that starts `luchctl` outside the sandbox with `flatpak-spawn --host`. That works only once the
     browser may do so, which you allow yourself:
     `flatpak override --user --talk-name=org.freedesktop.Flatpak com.google.Chrome`. This lets the browser run
     programs outside its sandbox in general, so decide whether you want it; Firefox from the system needs nothing.
     A Flatpak browser also sees only some folders: load the extension from a copy in *Downloads*.

## Use

Move the pointer over a video: a **To TV** button appears in its corner. The toolbar button picks the TV and sends
the page's biggest video.

How the video gets to the TV:

- **Direct.** The TV fetches the video itself, with the page's address as `Referer`. This needs nothing from this
  computer, which may even go to sleep.
- **Through this computer** (*relay*). luchd fetches the video and passes it on, for links the TV cannot open by
  itself: a company site behind a VPN, a local address, a login. This happens by itself for local addresses, and
  again whenever a direct cast fails. The toolbar popup can force either way.
- **Cookies** (the site's login) never leave this computer, unless you mark a TV as your own in the popup: then they
  go to that TV for direct fetching. Otherwise a link that needs a login is relayed, and the cookies stay here.

## What works

- A `<video>` with its own link: MP4, WebM and the like.
- Players that stream with HLS: the playlist the page loaded is cast, so the TV picks its own quality.
- Players in an iframe (Kodik, Playerjs and the like on film and anime sites): what that frame loaded is cast,
  not the teaser or ad playing in another one. Most of them load nothing before their first click: start the video
  on the page, then send it.
- Players built as web components (VK Video): their video is found inside the component.
- YouTube, on its own pages and embedded in others: the TV's own YouTube app opens the video, from the same moment.
- DASH (`.mpd`: VK Video, ok.ru, Dzen, CNN, BBC) plays on a Luch receiver whose welcome lists it (built with
  Media3) and on a Chromecast; an older Luch receiver says so instead. The same receivers play HLS in fragmented MP4
  (PeerTube) and with separate sound tracks. Relayed, a manifest's pieces come through this computer too.
- Sites with DRM (Netflix and the like) cannot be cast this way.

## Tests

`node --test browser-extension/test/*.test.js` checks the decisions (`core.js`) and the two manifests;
`test/sites.test.js` holds the player structures met on real sites (which frame loaded what, shadow roots, segment
names), as small made-up fixtures.
`browser-extension/test/host_e2e.py <receiver id>` runs the native host the way a browser does, against a running
luchd and a real TV.
