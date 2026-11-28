// SPDX-License-Identifier: GPL-3.0-or-later
// The extension's middle: watches which media links each tab loads, talks to luchd through the native host
// (`luchctl`, see luchctl/src/native.rs) and passes the TV's playback state to the tab that cast.
"use strict";
if (typeof importScripts === "function" && typeof LuchCore === "undefined") importScripts("core.js");
const B = typeof browser !== "undefined" ? browser : chrome;
const HOST = "dev.luch.host";

// ---- media links seen per tab (a blob: player's real source) ----------------------------------------------

const seen = new Map(); // tabId -> [{url, kind, type, frameId}], oldest first, at most 40
const subsSeen = new Map(); // tabId -> subtitle links the page loaded (VTT, SRT, ASS)
function remember(tabId, url, type, frameId) {
  if (tabId < 0) return;
  if (LuchCore.isSubtitle(url, type)) {
    const subs = subsSeen.get(tabId) || [];
    if (!subs.includes(url)) subs.push(url);
    subsSeen.set(tabId, subs.slice(-8));
    return;
  }
  const kind = LuchCore.kindOf(url, type);
  if (!kind) return;
  if (kind === "file" && LuchCore.isSegment(url)) return; // a piece of a stream, not a file to cast
  const list = seen.get(tabId) || [];
  if (list.some((s) => s.url === url)) return;
  list.push({ url, kind, type: type || "", frameId });
  if (list.length > 40) list.shift();
  seen.set(tabId, list);
}
B.webRequest.onHeadersReceived.addListener(
  (d) => {
    const type = ((d.responseHeaders || []).find((h) => h.name.toLowerCase() === "content-type") || {}).value;
    remember(d.tabId, d.url, type, d.frameId);
  },
  { urls: ["<all_urls>"], types: ["media", "xmlhttprequest", "other"] },
  ["responseHeaders"]
);
B.webNavigation &&
  B.webNavigation.onCommitted.addListener((d) => {
    if (d.frameId === 0) {
      seen.delete(d.tabId);
      subsSeen.delete(d.tabId);
    }
  });
B.tabs.onRemoved.addListener((tabId) => {
  seen.delete(tabId);
  subsSeen.delete(tabId);
});

/** Which of a file's sibling qualities exist: a 1-byte request each, 3 s at most for all. */
async function probeVariants(url) {
  const tries = LuchCore.variantLinks(url).map(async (v) => {
    const ctl = new AbortController();
    const t = setTimeout(() => ctl.abort(), 3000);
    try {
      const r = await fetch(v.url, { headers: { Range: "bytes=0-0" }, credentials: "include", signal: ctl.signal });
      const type = r.headers.get("content-type") || "";
      return (r.status === 206 || r.status === 200) && !/text\/html/i.test(type) ? v : null;
    } catch (e) {
      return null;
    } finally {
      clearTimeout(t);
      ctl.abort();
    }
  });
  return (await Promise.all(tries)).filter(Boolean);
}

// ---- the native host -----------------------------------------------------------------------------------

let port = null;
let nextId = 1;
const waiting = new Map();
/** receiver id -> {tabId, via, last state, the cast request (for a relayed retry)} */
const casts = new Map();

function connect() {
  if (port) return port;
  port = B.runtime.connectNative(HOST);
  port.onMessage.addListener(onHostMessage);
  port.onDisconnect.addListener(() => {
    const why = (B.runtime.lastError && B.runtime.lastError.message) || "the Luch host went away";
    port = null;
    for (const [, w] of waiting) w.reject(new Error(hostHint(why)));
    waiting.clear();
  });
  return port;
}

function hostHint(why) {
  if (/not found|forbidden|specified native messaging host/i.test(why)) {
    return "Luch is not set up for this browser: run `luchctl browser install` and restart the browser";
  }
  if (/host has exited/i.test(why)) {
    // A Flatpak browser whose wrapper may not leave the sandbox, or luchd not running.
    return "The Luch host stopped at once: is luchd running? A Flatpak browser also needs "
      + "`flatpak override --user --talk-name=org.freedesktop.Flatpak <its id>` (see the extension's README)";
  }
  return why;
}

function ask(op, fields, ms = 15000) {
  return new Promise((resolve, reject) => {
    const id = nextId++;
    const t = setTimeout(() => {
      waiting.delete(id);
      reject(new Error("luchd did not answer"));
    }, ms);
    waiting.set(id, {
      resolve: (v) => (clearTimeout(t), resolve(v)),
      reject: (e) => (clearTimeout(t), reject(e)),
    });
    try {
      connect().postMessage(Object.assign({ id, op }, fields));
    } catch (e) {
      waiting.delete(id);
      clearTimeout(t);
      reject(new Error(hostHint(String(e.message || e))));
    }
  });
}

function onHostMessage(m) {
  if (m.event === "state") return onState(m.receiver, m.state);
  const w = waiting.get(m.id);
  if (!w) return;
  waiting.delete(m.id);
  if ("error" in m) w.reject(new Error(m.error));
  else w.resolve(m.ok);
}

/** To the frame that cast (every frame would show the same note otherwise). */
function toTab(tabId, msg, frameId) {
  if (tabId == null) return;
  B.tabs.sendMessage(tabId, msg, frameId == null ? {} : { frameId }).catch(() => {});
}

async function onState(receiver, state) {
  const c = casts.get(receiver);
  if (!c) return;
  c.state = state;
  if (LuchCore.shouldRetryRelayed(c.via, state) && !c.retried) {
    // The TV could not fetch it (a VPN, a login): this computer passes it on instead, from the same moment.
    c.retried = true;
    c.via = "sender";
    toTab(c.tabId, { luch: "note", text: "relay" }, c.frameId);
    const options = LuchCore.castOptions(Object.assign({}, c.request, { via: "sender", cookie: await cookieFor(c.request.url) }));
    try {
      await ask("cast", { receiver, items: [c.request.url], options });
    } catch (e) {
      toTab(c.tabId, { luch: "error", text: e.message }, c.frameId);
    }
    return;
  }
  toTab(c.tabId, { luch: "state", receiver, state, at: Date.now() }, c.frameId);
  if (state.state === "stopped") casts.delete(receiver);
}

async function cookieFor(url) {
  try {
    const all = await B.cookies.getAll({ url });
    return all.map((c) => `${c.name}=${c.value}`).join("; ");
  } catch (e) {
    return "";
  }
}

async function receivers() {
  const list = await ask("receivers", {});
  // Receivers that said they cannot play media (an older Luch) are left out; unknown ones stay.
  return list.filter((r) => !/^web/i.test(r.model || "") && r.media !== false);
}

async function chosenReceiver(list) {
  const { receiver } = await B.storage.local.get("receiver");
  if (list.some((r) => r.id === receiver)) return receiver;
  return list.length === 1 ? list[0].id : null;
}

/** Casts the video a tab's frame describes ({video, page}) to `receiver` (or the chosen one). */
async function castFromTab(tabId, frameId, frameUrl, req) {
  const list = await receivers();
  const receiver = req.receiver || (await chosenReceiver(list));
  if (!receiver) return { choose: list };
  const yt = LuchCore.youtubeLink(frameUrl || (req.page && req.page.pageUrl));
  if (yt) {
    // The TV's own YouTube app, from the same moment; the page's player stays as it is.
    const start = Math.floor((req.page && req.page.currentTime) || 0);
    await ask("cast", { receiver, items: [yt], options: { start, title: (req.page && req.page.title) || "" } });
    await B.storage.local.set({ receiver });
    const name = (list.find((r) => r.id === receiver) || {}).name || "TV";
    return { receiver, name, app: "youtube" };
  }
  const src = LuchCore.pickSource(req.video, seen.get(tabId), frameId);
  if (!src) {
    // A player that has not started yet loads nothing (most iframe players wait for their first click, some
    // show an ad first, from a blob of its own).
    if (!req.video.currentSrc || !(seen.get(tabId) || []).length) throw new Error(req.page && /^ru/i.test(req.page.lang || "")
      ? "Видео ещё не загрузилось (или идёт реклама): запустите его на странице и нажмите снова"
      : "The video has not loaded yet (or an ad plays first): start it on the page, then send it again");
    throw new Error("no link to this video (it may be protected)");
  }
  const { relay, trusted = [] } = await B.storage.local.get(["relay", "trusted"]);
  const via = LuchCore.chooseVia(src.url, relay);
  const trust = trusted.includes(receiver);
  // A page's player with one file per quality: its siblings go too, so the TV plays the best its screen shows.
  const sources = src.kind === "file" ? LuchCore.sourcesOf(src.url, src.mime, await probeVariants(src.url)) : [];
  const subs = [...new Set([...(req.page.subs || []), ...(subsSeen.get(tabId) || [])])];
  const request = Object.assign({}, req.page, { url: src.url, mime: src.mime, via, trusted: trust, subs });
  const cookie = via === "sender" || trust ? await cookieFor(src.url) : "";
  const options = LuchCore.castOptions(Object.assign({}, request, { cookie }));
  if (sources.length > 1) options.sources = sources;
  casts.set(receiver, { tabId, frameId, frameUrl, via, request, sources, state: null, retried: false });
  try {
    await ask("cast", { receiver, items: [src.url], options });
  } catch (e) {
    casts.delete(receiver);
    // The Luch receiver plays HLS and files; a DASH-only site (VK Video, ok.ru, Dzen, Vimeo) is said as such.
    if (src.kind === "dash" && /media_failed|no source/i.test(e.message)) {
      throw new Error(/^ru/i.test((req.page && req.page.lang) || "")
        ? "Сайт отдаёт видео в DASH, а этот ТВ его пока не играет"
        : "This site streams in DASH, which this TV cannot play yet");
    }
    throw e;
  }
  await B.storage.local.set({ receiver });
  const name = (list.find((r) => r.id === receiver) || {}).name || "TV";
  return { receiver, name, via, kind: src.kind, url: src.url, qualities: sources.map((s) => ({ url: s.url, h: s.h })) };
}

/**
 * The same video again from where the TV is: another quality chosen in the panel (`url`, or null for the best),
 * or the page's player switched to another file.
 */
async function recast(receiver, url, start) {
  const c = casts.get(receiver);
  if (!c) throw new Error("nothing cast to that receiver");
  const target = url || c.request.url;
  const request = Object.assign({}, c.request, { url: target, currentTime: start, paused: false });
  const cookie = c.via === "sender" || request.trusted ? await cookieFor(target) : "";
  const options = LuchCore.castOptions(Object.assign({}, request, { via: c.via, cookie }));
  if (!url && c.sources.length > 1) options.sources = c.sources;
  c.request = request;
  await ask("cast", { receiver, items: [target], options });
  return {};
}

/**
 * The popup's "send this page's video": each frame says what its best video is, and only the frame with the one
 * the person most likely watches casts it (not every frame with an ad's player in it).
 */
async function castPage(tabId, receiver) {
  let frames = [{ frameId: 0 }];
  try {
    frames = (await B.webNavigation.getAllFrames({ tabId })) || frames;
  } catch (e) {
    // only the top frame then
  }
  const asked = await Promise.all(frames.map((f) =>
    B.tabs.sendMessage(tabId, { luch: "best" }, { frameId: f.frameId }).then((r) => r && Object.assign({ frameId: f.frameId }, r), () => null)));
  const found = asked.filter(Boolean);
  const i = LuchCore.chooseVideo(found);
  const frameId = i < 0 ? 0 : found[i].frameId;
  await B.tabs.sendMessage(tabId, { luch: "castBiggest", receiver }, { frameId }).catch(() => {});
  return { frameId };
}

B.runtime.onMessage.addListener((msg, sender, reply) => {
  const tabId = sender.tab ? sender.tab.id : msg.tabId;
  const run = async () => {
    switch (msg.op) {
      case "receivers":
        return Object.assign({ list: await receivers() }, await B.storage.local.get(["receiver", "trusted"]));
      case "choose":
        await B.storage.local.set({ receiver: msg.receiver });
        return {};
      case "trust": {
        const { trusted = [] } = await B.storage.local.get("trusted");
        const next = trusted.filter((id) => id !== msg.receiver);
        if (msg.on) next.push(msg.receiver);
        await B.storage.local.set({ trusted: next });
        return {};
      }
      case "cast":
        return castFromTab(tabId, sender.frameId, sender.url, msg);
      case "castPage":
        return castPage(msg.tabId, msg.receiver);
      case "recast":
        return recast(msg.receiver, msg.url || null, msg.start || 0);
      case "cmd":
        await ask("cmd", { receiver: msg.receiver, cmd: msg.cmd });
        return {};
      case "state":
        return { state: await ask("state", { receiver: msg.receiver }) };
      case "stop":
        casts.delete(msg.receiver);
        await ask("stop", { receiver: msg.receiver });
        return {};
      case "active": {
        // Only the frame that cast, reloaded: another page in the tab (the next episode) starts afresh.
        for (const [receiver, c] of casts) {
          if (c.tabId !== tabId || c.frameUrl !== sender.url) continue;
          c.frameId = sender.frameId; // a reloaded iframe has a new one
          return { receiver, state: c.state };
        }
        return {};
      }
      case "seen":
        return { seen: seen.get(tabId) || [] };
      default:
        throw new Error("unknown op " + msg.op);
    }
  };
  run().then(
    (v) => reply(Object.assign({ ok: true }, v)),
    (e) => reply({ ok: false, error: String(e.message || e) })
  );
  return true; // answered later
});
