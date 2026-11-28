// SPDX-License-Identifier: GPL-3.0-or-later
// The extension's decisions, without the browser: what a link is, which one to cast, whether the TV can fetch it
// itself or this computer has to pass it on. Loaded by the background (importScripts / background.scripts) and by
// the tests under Node.
(function (root) {
  "use strict";

  const MIME = {
    hls: "application/vnd.apple.mpegurl",
    dash: "application/dash+xml",
  };

  const FILE_TYPES = {
    mp4: "video/mp4", m4v: "video/mp4", mov: "video/mp4", webm: "video/webm", mkv: "video/x-matroska",
    ts: "video/mp2t", mp3: "audio/mpeg", m4a: "audio/mp4", aac: "audio/aac", ogg: "audio/ogg", oga: "audio/ogg",
    opus: "audio/ogg", flac: "audio/flac", wav: "audio/wav",
  };

  function pathOf(url) {
    try {
      return new URL(url).pathname.toLowerCase();
    } catch (e) {
      return "";
    }
  }

  function extension(url) {
    const p = pathOf(url);
    const dot = p.lastIndexOf(".");
    return dot > p.lastIndexOf("/") ? p.slice(dot + 1) : "";
  }

  /** "hls", "dash", "file" or null: what a link (and the Content-Type it came with, if known) is. */
  function kindOf(url, contentType) {
    if (!/^https?:/i.test(url || "")) return null;
    const c = (contentType || "").toLowerCase().split(";")[0].trim();
    const ext = extension(url);
    // A link named like a playlist that answered with a page or a document is something else (Dailymotion's ad
    // schedule, VMAP XML, behind a ".m3u8").
    if (/^(text\/html|application\/(xml|json)|text\/xml)/.test(c)) return null;
    if (c.includes("mpegurl") || ext === "m3u8") return "hls";
    if (c === "application/dash+xml" || ext === "mpd") return "dash";
    if (FILE_TYPES[ext] || /^(video|audio)\//.test(c)) return "file";
    return null;
  }

  /** The MIME type to tell the receiver for a link, "" when the receiver had better sniff it. */
  function mimeOf(url, kind, contentType) {
    if (kind === "hls" || kind === "dash") return MIME[kind];
    const c = (contentType || "").toLowerCase().split(";")[0].trim();
    if (/^(video|audio)\//.test(c)) return c;
    return FILE_TYPES[extension(url)] || "";
  }

  /**
   * Hosts the TV cannot be expected to reach on its own: loopback, link-local, private and CGNAT addresses,
   * names without a dot and names under the local suffixes networks use (a company's intranet behind a VPN).
   */
  function isPrivateHost(host) {
    let h = (host || "").toLowerCase().replace(/^\[|\]$/g, "");
    if (!h) return true;
    if (h === "localhost" || !h.includes(".") && !h.includes(":")) return true;
    if (/\.(local|lan|home|internal|intranet|corp|home\.arpa|localdomain|test)$/.test(h)) return true;
    const v4 = h.match(/^(\d+)\.(\d+)\.(\d+)\.(\d+)$/);
    if (v4) {
      const [a, b] = [Number(v4[1]), Number(v4[2])];
      return a === 10 || a === 127 || a === 0 || (a === 169 && b === 254) || (a === 172 && b >= 16 && b <= 31)
        || (a === 192 && b === 168) || (a === 100 && b >= 64 && b <= 127);
    }
    if (h.includes(":")) return h === "::1" || /^f[cd]/.test(h) || /^fe[89ab]/.test(h);
    return false;
  }

  function hostOf(url) {
    try {
      return new URL(url).hostname;
    } catch (e) {
      return "";
    }
  }

  /**
   * "direct" when the TV can fetch the link itself, "sender" when this computer has to pass it on: a private
   * host, or a person's choice ("always" / "never" relay).
   */
  function chooseVia(mediaUrl, prefer) {
    if (prefer === "always") return "sender";
    if (prefer === "never") return "direct";
    return isPrivateHost(hostOf(mediaUrl)) ? "sender" : "direct";
  }

  /**
   * A piece of a stream rather than a whole file: an HLS or DASH segment (`.ts`, `.m4s`, `segment.mp4`, `init.mp4`,
   * `seg-12-v1.mp4`, `chunk_3.mp4`, `frag5.mp4`), or a byte range a player asks for in the link itself
   * (`?bytes=0-6938`, `&range=100-200`). Casting one would play a second or two.
   */
  function isSegment(url) {
    const p = pathOf(url);
    if (/\.(ts|m4s|aac|m4f|cmfv|cmfa)$/.test(p)) return true;
    const name = p.slice(p.lastIndexOf("/") + 1);
    if (/^(init|segment|seg|chunk|frag|fragment)([-_.]?\d+)?([-_.][^/]*)?\.(mp4|m4v|m4a|webm)$/.test(name)) return true;
    if (/(^|[-_])(seg|segment|chunk|frag|fragment)[-_]?\d+[^/]*\.(mp4|m4v|m4a|webm)$/.test(name)) return true;
    try {
      const q = new URL(url).searchParams;
      // CMCD: the data a DASH or HLS player adds to every piece it asks for (CTA-5004).
      return ["bytes", "range"].some((k) => /^\d+-\d*$/.test(q.get(k) || "")) || q.has("CMCD");
    } catch (e) {
      return false;
    }
  }

  /** The link's origin and folder: "https://cdn.example/a/b/" for "https://cdn.example/a/b/c.mp4?x". */
  function dirOf(url) {
    try {
      const u = new URL(url);
      return u.origin + u.pathname.slice(0, u.pathname.lastIndexOf("/") + 1);
    } catch (e) {
      return "";
    }
  }

  /**
   * A YouTube video's link in the form both senders open in the TV's own YouTube app, from a watch, shorts, embed
   * or youtu.be page; null for anything else. The site hides its streams from a page's video element.
   */
  function youtubeLink(pageUrl) {
    let u;
    try {
      u = new URL(pageUrl);
    } catch (e) {
      return null;
    }
    const host = u.hostname.replace(/^(www|m|music)\./, "");
    const parts = u.pathname.split("/").filter(Boolean);
    let id = null;
    if (host === "youtu.be") id = parts[0];
    else if (host === "youtube.com" || host === "youtube-nocookie.com") {
      if (parts[0] === "watch") id = u.searchParams.get("v");
      else if (["embed", "shorts", "live", "v"].includes(parts[0])) id = parts[1];
    }
    return id && /^[\w-]{6,20}$/.test(id) ? `https://www.youtube.com/watch?v=${id}` : null;
  }

  /**
   * What to cast for a <video>: its own link when it has one (not a blob: of a page's own player), else what the
   * page fetched for it: an HLS playlist (a master one first), then DASH when the receiver plays it (`dash`: a Luch
   * receiver with Media3, a Chromecast), then a plain file, and DASH last for a receiver that does not. `video` is {currentSrc, src, sources: [{src, type}], audio}; `seen` the
   * media links the page loaded, oldest first: [{url, kind, type, frameId}]. What the video's own frame loaded wins
   * over other frames' (an ad's player in another iframe); pieces of streams never count as files, nor sounds for a
   * video (a site's interface beeps). Returns {url, kind, mime} or null.
   */
  function pickSource(video, seen, frameId, dash) {
    const own = [video && video.currentSrc, video && video.src]
      .concat(((video && video.sources) || []).map((s) => s.src))
      .filter((u) => kindOf(u));
    if (own.length) {
      const url = own[0];
      const type = ((video.sources || []).find((s) => s.src === url) || {}).type;
      const kind = kindOf(url, type);
      return { url, kind, mime: mimeOf(url, kind, type) };
    }
    let list = (seen || []).filter((s) => s && s.kind);
    if (frameId != null && list.some((s) => s.frameId === frameId)) list = list.filter((s) => s.frameId === frameId);
    const hls = list.filter((s) => s.kind === "hls");
    if (hls.length) {
      // A subtitle or sound-only rendition's playlist is never the one to cast.
      const name = (s) => pathOf(s.url).slice(pathOf(s.url).lastIndexOf("/") + 1);
      const whole = hls.filter((s) => !/sub|caption|audio/i.test(name(s)));
      const master = hls.find((s) => /master|index|playlist|manifest/i.test(pathOf(s.url))) || whole[0] || hls[0];
      return { url: master.url, kind: "hls", mime: MIME.hls };
    }
    const isVideo = !(video && video.audio);
    // A file next to a playlist the page loaded, or in a folder under it, is one of its pieces, whatever its name (a
    // live DASH stream's "…_001236492.mp4", a DASH track's "v/3_0171f8/v2.mp4").
    const dirs = list.filter((s) => s.kind !== "file").map((s) => dirOf(s.url)).filter((d) => !/^[^/]+\/\/[^/]+\/$/.test(d));
    const files = list.filter((s) => s.kind === "file" && !isSegment(s.url) && !dirs.some((d) => dirOf(s.url).startsWith(d))
      && !(isVideo && /^audio\//.test(mimeOf(s.url, "file", s.type))));
    const manifests = list.filter((s) => s.kind === "dash");
    const manifest = manifests.length && { url: manifests[manifests.length - 1].url, kind: "dash", mime: MIME.dash };
    if (manifest && dash) return manifest;
    if (files.length) {
      const f = files[files.length - 1];
      return { url: f.url, kind: "file", mime: mimeOf(f.url, "file", f.type) };
    }
    return manifest || null;
  }

  /**
   * Every <video> and <audio> under `root`, also inside shadow roots: players built as web components keep theirs
   * there (VK's). `shadowOf(el)` reaches closed roots where the browser lets an extension do so.
   */
  function mediaElements(root, shadowOf) {
    const out = [];
    const walk = (r) => {
      for (const el of r.querySelectorAll("*")) {
        if (el.tagName === "VIDEO" || el.tagName === "AUDIO") out.push(el);
        const sr = el.shadowRoot || (shadowOf && shadowOf(el));
        if (sr) walk(sr);
      }
    };
    walk(root);
    return out;
  }

  /**
   * Which video "send this page's video" means, among [{area, playing, muted}] (one per frame, or per video): one
   * playing with its sound first (what the person watches; a muted one playing by itself is a site's teaser or an
   * ad), then the biggest. Videos smaller than a thumbnail never count. Returns the index, or -1.
   */
  function chooseVideo(list) {
    let best = -1;
    const score = (c) => [c.playing && !c.muted ? 1 : 0, c.area];
    (list || []).forEach((c, i) => {
      if (!c || !(c.area >= 160 * 90)) return;
      if (best < 0) return (best = i);
      const [a, b] = [score(c), score(list[best])];
      if (a[0] > b[0] || (a[0] === b[0] && a[1] > b[1])) best = i;
    });
    return best;
  }

  /**
   * The Cast options for luchd (docs/dbus.md). The page's cookies (the site's login) go when this computer relays
   * the link, where they stay on it, or to a TV the person trusts with them (`trusted`: their own TV at home);
   * never to any other TV.
   */
  function castOptions(o) {
    const out = {
      title: (o.title || "").slice(0, 200),
      start: Math.max(0, Math.floor((o.currentTime || 0) * 10) / 10),
      via: o.via,
    };
    if (o.mime) out.mime = o.mime;
    if (o.playbackRate && o.playbackRate !== 1) out.rate = o.playbackRate;
    if (o.paused) out.paused = true;
    if (o.poster && /^https?:/.test(o.poster)) out.poster = o.poster;
    const headers = {};
    if (o.pageUrl && /^https?:/.test(o.pageUrl)) {
      headers.Referer = o.pageUrl;
      try {
        headers.Origin = new URL(o.pageUrl).origin;
      } catch (e) {
        // no origin
      }
    }
    if (o.userAgent) headers["User-Agent"] = o.userAgent;
    if (o.cookie && (o.via === "sender" || o.trusted)) headers.Cookie = o.cookie;
    out.headers = headers;
    const subs = (o.subs || []).filter((s) => /^https?:/.test(s));
    if (subs.length) out.subs = subs;
    return out;
  }

  /**
   * Whether a receiver plays DASH, from the MIME types its welcome listed (`media_types`): true, false, or null when
   * its sender has not talked to it yet.
   */
  function playsDash(receiver) {
    const types = receiver && receiver.media_types;
    return Array.isArray(types) ? types.includes(MIME.dash) : null;
  }

  /** After a direct cast: relay instead when the receiver could not fetch the link itself. */
  function shouldRetryRelayed(via, state) {
    if (via !== "direct" || !state) return false;
    return state.state === "error" || state.code === "bad_link";
  }

  function clock(s) {
    s = Math.max(0, Math.floor(s || 0));
    const h = Math.floor(s / 3600), m = Math.floor(s / 60) % 60, sec = s % 60;
    const two = (n) => String(n).padStart(2, "0");
    return h ? `${h}:${two(m)}:${two(sec)}` : `${m}:${two(sec)}`;
  }

  /** Where playback is now: the receiver's last position moved on by the time since, at its speed. */
  function positionNow(state, receivedAt, now) {
    if (!state || typeof state.pos !== "number") return 0;
    if (state.state !== "playing") return state.pos;
    const p = state.pos + ((now - receivedAt) / 1000) * (state.rate || 1);
    return typeof state.dur === "number" ? Math.min(p, state.dur) : p;
  }

  const QUALITIES = [144, 240, 360, 480, 540, 576, 720, 1080, 1440, 2160, 4320];
  // A quality in a link: "720p", "/720/", "_720.", "-1080-", "=480&": a known height between separators.
  const Q_RE = /(^|[\/_\-.=])(144|240|360|480|540|576|720|1080|1440|2160|4320)(p?)(?=$|[\/_\-.?&=])/i;

  /** The video height a link names, or 0. */
  function qualityOf(url) {
    let path;
    try {
      const u = new URL(url);
      path = u.pathname + u.search;
    } catch (e) {
      return 0;
    }
    const m = path.match(Q_RE);
    return m ? Number(m[2]) : 0;
  }

  /** The same link with another quality in place of the one it names; [] when it names none. */
  function variantLinks(url) {
    const q = qualityOf(url);
    if (!q) return [];
    let u;
    try {
      u = new URL(url);
    } catch (e) {
      return [];
    }
    const path = u.pathname + u.search;
    const m = path.match(Q_RE);
    const at = m.index + m[1].length;
    const out = [];
    for (const other of QUALITIES) {
      if (other === q) continue;
      const replaced = path.slice(0, at) + other + m[3] + path.slice(at + String(q).length + m[3].length);
      out.push({ url: u.origin + replaced, h: other });
    }
    return out;
  }

  /**
   * The cast's sources, best first: the link itself and the variants that answered ([{url, h}]), each with its size
   * (16:9 assumed) so the receiver can pick what its screen shows.
   */
  function sourcesOf(url, mime, found) {
    const all = [{ url, h: qualityOf(url) }].concat(found || []);
    const seen = new Set();
    return all
      .filter((s) => !seen.has(s.url) && seen.add(s.url))
      .sort((a, b) => b.h - a.h)
      .map((s) => {
        const o = { url: s.url };
        if (mime) o.mime = mime;
        if (s.h) {
          o.h = s.h;
          o.w = Math.round((s.h * 16) / 9);
        }
        return o;
      });
  }

  /** "subtitles" for a link the page loaded: WebVTT, SubRip or ASS by its extension or type. */
  function isSubtitle(url, type) {
    const c = (type || "").toLowerCase();
    // A player's preview pictures and chapter marks come as WebVTT too (Mux's storyboard.vtt).
    if (/storyboard|thumbnail|sprite|chapters|preview/i.test(pathOf(url))) return false;
    return /\.(vtt|srt|ass|ssa)(\?|$)/i.test(pathOf(url)) || c.includes("text/vtt") || c.includes("subrip");
  }

  const api = { playsDash, youtubeLink, isSegment, mediaElements, chooseVideo, qualityOf, variantLinks, sourcesOf, isSubtitle, kindOf, mimeOf, isPrivateHost, chooseVia, pickSource, castOptions, shouldRetryRelayed, clock,
    positionNow, MIME };
  if (typeof module !== "undefined" && module.exports) module.exports = api;
  else root.LuchCore = api;
})(typeof self !== "undefined" ? self : this);
