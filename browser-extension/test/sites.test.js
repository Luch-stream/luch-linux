// SPDX-License-Identifier: GPL-3.0-or-later
// The player structures met on real sites in a field test, as small fixtures: what the page loaded
// and which frame did, and how its videos sit in the DOM. Links are made up; only their shapes are the sites'.
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const c = require("../core.js");

const blob = (host) => ({ currentSrc: `blob:https://${host}/4f0c`, src: `blob:https://${host}/4f0c`, sources: [] });

test("an iframe player (Kodik) casts what its own frame loaded, not an ad's stream in another frame", () => {
  // A sticky "video news" player (muted autoplay) in frame 12 loaded its playlist first; the episode's player in
  // frame 4 plays from a blob.
  const seen = [
    { url: "https://ad-video.example/route/77.m3u8?guids=1", kind: "hls", frameId: 12 },
    { url: "https://ad-cdn.example/hls-vod/a/77.mp4.m3u8?i=256x144", kind: "hls", frameId: 12 },
    { url: "https://p1.player-cdn.example/s/m/aHR0cHM6/87de:202", kind: "hls", frameId: 4 },
  ];
  assert.equal(c.pickSource(blob("player.example"), seen, 4).url, "https://p1.player-cdn.example/s/m/aHR0cHM6/87de:202");
  // A frame that loaded nothing itself (its player fetches from a worker): the whole tab's links, as before.
  assert.equal(c.pickSource(blob("player.example"), seen, 7).url, "https://ad-video.example/route/77.m3u8?guids=1");
});

test("a Playerjs player with its playlist as the video's own src (doram-ru's) casts that playlist", () => {
  const v = { currentSrc: "https://hls.db.example/file/videos/125514/p_1.m3u8", src: "https://hls.db.example/file/videos/125514/p_1.m3u8", sources: [] };
  assert.deepEqual(c.pickSource(v, [{ url: "https://ad-video.example/route/77.m3u8", kind: "hls", frameId: 9 }], 4),
    { url: "https://hls.db.example/file/videos/125514/p_1.m3u8", kind: "hls", mime: c.MIME.hls });
});

test("an mp4 player behind a blob (animevost's) casts its file, not the banner video next to it", () => {
  const seen = [
    { url: "https://site.example/html5banner/vide/Comp%201_1.mp4", kind: "file", type: "video/mp4", frameId: 3 },
    { url: "https://hd.mirror.example/720/2102606427.mp4?md5=x&time=1&ip=203.0.113.5", kind: "file", type: "video/mp4", frameId: 5 },
    { url: "https://res.mirror.example/720/2102606427.mp4?md5=x&time=1&ip=203.0.113.5&wsn=0", kind: "file", type: "video/mp4", frameId: 5 },
  ];
  const s = c.pickSource(blob("site.example"), seen, 5);
  assert.equal(s.url, "https://res.mirror.example/720/2102606427.mp4?md5=x&time=1&ip=203.0.113.5&wsn=0");
  assert.equal(s.mime, "video/mp4");
});

test("pieces of streams are not files: DASH segments, byte ranges in the link (ok.ru, Vimeo)", () => {
  for (const u of ["https://cdn.example/v/seg-1-v1-a1.ts", "https://cdn.example/x/remux/avf/3e8c/segment.mp4?sid=0&st=video",
    "https://cdn.example/x/init.mp4", "https://vd1.cdn.example/?expires=1&type=4&id=1&bytes=0-6938",
    "https://cdn.example/v/chunk_3.mp4", "https://cdn.example/v/frag5.m4s", "https://cdn.example/a/seg-12-v1.mp4",
    "https://cdn.example/a.mp4?range=100-2000"]) {
    assert.equal(c.isSegment(u), true, u);
  }
  for (const u of ["https://cdn.example/film.mp4", "https://cdn.example/v/episode-1.mp4", "https://cdn.example/720/2102606427.mp4?ip=1",
    "https://cdn.example/a.mp4?range=hd"]) {
    assert.equal(c.isSegment(u), false, u);
  }
  // ok.ru: a DASH manifest and its byte ranges; only the manifest is left to cast (a Chromecast can play it).
  const seen = [
    { url: "https://vd1.cdn.example/?expires=1&type=1&id=1", kind: "dash", type: "application/dash+xml" },
    { url: "https://vd1.cdn.example/?expires=1&type=4&id=1&bytes=0-6938", kind: "file", type: "video/mp4" },
  ];
  assert.equal(c.pickSource(blob("ok.example"), seen).kind, "dash");
});

test("a plain file beats a DASH manifest the Luch receiver cannot play; HLS beats both", () => {
  const seen = [
    { url: "https://cdn.example/m.mpd", kind: "dash" },
    { url: "https://cdn.example/720/a.mp4", kind: "file", type: "video/mp4" },
  ];
  assert.equal(c.pickSource(blob("x.example"), seen).kind, "file");
  assert.equal(c.pickSource(blob("x.example"), seen.concat([{ url: "https://cdn.example/a.m3u8", kind: "hls" }])).kind, "hls");
});

test("a site's interface sounds are no video (VK's beeps, a notification)", () => {
  const seen = [
    { url: "https://vk.example/mp3/bb1.mp3", kind: "file", type: "audio/mpeg" },
    { url: "https://site.example/js/notification.mp3", kind: "file" },
  ];
  assert.equal(c.pickSource(blob("vk.example"), seen), null);
  // An <audio> player may well cast a sound.
  assert.equal(c.pickSource(Object.assign(blob("radio.example"), { audio: true }), seen).url, "https://site.example/js/notification.mp3");
});

/** A tiny stand-in for the DOM: elements with children and maybe a shadow root. */
function el(tagName, children = [], shadow = null, closed = false) {
  const e = { tagName, children };
  if (shadow) {
    const root = frag(shadow);
    if (closed) e.closedRoot = root;
    else e.shadowRoot = root;
  }
  return e;
}
function frag(children) {
  const all = () => children.flatMap((ch) => [ch, ...frag(ch.children).querySelectorAll("*")]);
  return { querySelectorAll: () => all() };
}

test("videos inside players built as web components are found (VK's open shadow root, a closed one)", () => {
  const vk = el("VIDEO");
  const closed = el("VIDEO");
  const doc = frag([
    el("DIV", [el("DIV", [], [el("DIV", [vk])])]),
    el("VK-PLAYER", [], [closed], true),
    el("AUDIO"),
  ]);
  const found = c.mediaElements(doc, (e) => e.closedRoot || null);
  assert.ok(found.includes(vk));
  assert.ok(found.includes(closed));
  assert.equal(found.filter((e) => e.tagName === "VIDEO").length, 2);
  // Without the browser's help, the closed one stays hidden.
  assert.ok(!c.mediaElements(doc).includes(closed));
});

test("'send this page's video' means the video with its sound on, else the biggest; never a thumbnail", () => {
  // doram-ru: a muted autoplaying teaser (674x300) next to the episode's player, idle but bigger.
  const frames = [
    { area: 533 * 300, playing: false, muted: false }, // an empty ad slot in the page
    { area: 652 * 366, playing: false, muted: false }, // the episode
    { area: 674 * 300, playing: true, muted: true }, // the teaser
  ];
  assert.equal(c.chooseVideo(frames), 1);
  // The person started a video: that one, even if smaller.
  assert.equal(c.chooseVideo(frames.concat([{ area: 400 * 225, playing: true, muted: false }])), 3);
  assert.equal(c.chooseVideo([{ area: 120 * 68, playing: true, muted: false }, null]), -1);
  assert.equal(c.chooseVideo([]), -1);
});
