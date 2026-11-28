// SPDX-License-Identifier: GPL-3.0-or-later
// On every page: a "To TV" button over each video, and once cast, the TV's controls over the page's player:
// play/pause, position, speed, sound track and subtitles, as the TV reports them. The page's own video waits.
(() => {
  "use strict";
  const B = typeof browser !== "undefined" ? browser : chrome;
  const RU = (navigator.language || "").toLowerCase().startsWith("ru");
  const T = RU
    ? { cast: "На ТВ", on: "Играет на", stop: "Остановить", speed: "Скорость", sound: "Звук", subs: "Субтитры",
        off: "Выкл.", choose: "Куда показать?", relay: "ТВ не смог открыть ссылку сам — идёт через этот компьютер",
        loading: "Загрузка…", paused: "Пауза", ended: "Закончилось", quality: "Качество", best: "лучшее для ТВ",
        failed: "ТВ не смог это воспроизвести", app: "Открыто в приложении YouTube на" }
    : { cast: "To TV", on: "Playing on", stop: "Stop", speed: "Speed", sound: "Sound", subs: "Subtitles",
        off: "Off", choose: "Show it where?", relay: "The TV could not open the link itself: it goes through this computer",
        loading: "Loading…", paused: "Paused", ended: "Ended", quality: "Quality", best: "best for the TV",
        failed: "The TV could not play it", app: "Opened in the YouTube app on" };
  const RATES = [0.5, 0.75, 1, 1.25, 1.5, 1.75, 2];

  const send = (msg) => new Promise((res) => B.runtime.sendMessage(msg, (r) => res(r || { ok: false, error: "no answer" })));

  // ---- the layer everything is drawn in -------------------------------------------------------------------
  const host = document.createElement("luch-layer");
  host.style.cssText = "all:initial;position:fixed;inset:0;pointer-events:none;z-index:2147483647";
  const shadow = host.attachShadow({ mode: "closed" });
  shadow.innerHTML = `<style>
    :host{all:initial}
    *{box-sizing:border-box;font:13px/1.3 system-ui,sans-serif}
    .btn{position:fixed;pointer-events:auto;display:none;align-items:center;gap:6px;padding:6px 10px;border-radius:16px;
      border:0;background:#121318e6;color:#ffba5f;cursor:pointer;box-shadow:0 2px 8px #0008}
    .btn:hover{background:#292a2f}
    .panel{position:fixed;pointer-events:auto;display:none;flex-direction:column;gap:8px;
      padding:10px 14px 18px;background:linear-gradient(#000d 60%,#0000);color:#e3e2e8}
    .panel.small .more{display:none}
    .panel.small{padding-bottom:10px;background:#000b;border-radius:0 0 12px 0}
    button.fold{margin-left:auto}
    .row{display:flex;align-items:center;gap:8px;flex-wrap:wrap}
    .title{font-weight:600;color:#ffba5f}
    button.c,select{background:#292a2f;color:#e3e2e8;border:1px solid #47484d;border-radius:8px;padding:4px 8px;cursor:pointer}
    input[type=range]{flex:1;accent-color:#ffba5f;min-width:120px}
    .time{font-variant-numeric:tabular-nums;color:#c4c6d0}
    .note{color:#c4c6d0;font-size:12px}
    .menu{position:fixed;pointer-events:auto;display:none;flex-direction:column;gap:4px;padding:8px;border-radius:12px;
      background:#1e1f24;color:#e3e2e8;box-shadow:0 4px 16px #000a;min-width:200px}
    .menu b{padding:4px 6px;color:#c4c6d0;font-weight:500}
    .menu button{text-align:left;background:none;border:0;color:#e3e2e8;padding:6px;border-radius:6px;cursor:pointer}
    .menu button:hover{background:#292a2f}
  </style>
  <button class="btn" part="btn"><svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor"><path d="M21 3H3a2 2 0 0 0-2 2v3h2V5h18v14h-7v2h7a2 2 0 0 0 2-2V5a2 2 0 0 0-2-2ZM1 18v3h3a3 3 0 0 0-3-3Zm0-4v2a5 5 0 0 1 5 5h2a7 7 0 0 0-7-7Zm0-4v2a9 9 0 0 1 9 9h2A11 11 0 0 0 1 10Z"/></svg><span></span></button>
  <div class="panel">
    <div class="row"><span class="title"></span><span class="note"></span><button class="c fold">▴</button></div>
    <div class="row more">
      <button class="c play">⏸</button>
      <button class="c back">−10</button>
      <input type="range" class="seek" min="0" max="1000" value="0">
      <button class="c fwd">+10</button>
      <span class="time">0:00</span>
    </div>
    <div class="row more">
      <select class="rate"></select>
      <select class="audio"></select>
      <select class="sub"></select>
      <select class="quality"></select>
      <button class="c stop"></button>
    </div>
  </div>
  <div class="menu"></div>`;
  const $ = (s) => shadow.querySelector(s);
  const btn = $(".btn"), panel = $(".panel"), menu = $(".menu");
  btn.querySelector("span").textContent = T.cast;
  $(".stop").textContent = T.stop;
  for (const r of RATES) $(".rate").add(new Option(`${T.speed} ${r}×`, String(r)));
  (document.body || document.documentElement).appendChild(host);

  let hovered = null; // the video under the pointer
  let cast = null; // {receiver, name, video, state, at}

  // ---- the button over a video -----------------------------------------------------------------------------
  // Closed shadow roots too, where the browser lets an extension in (Chrome's dom API, Firefox's own property).
  // Chrome's throws for elements that cannot have one (SVG's).
  const shadowOf = (el) => {
    try {
      return (B.dom && B.dom.openOrClosedShadowRoot ? B.dom.openOrClosedShadowRoot(el) : el.openOrClosedShadowRoot) || null;
    } catch (e) {
      return null;
    }
  };
  let listed = { at: 0, list: [] };
  /** The page's videos, in shadow roots too; looked up again at most once a second (the pointer moves often). */
  function videos() {
    if (Date.now() - listed.at > 1000) {
      listed = { at: Date.now(), list: LuchCore.mediaElements(document, shadowOf).filter((v) => v.tagName === "VIDEO") };
    }
    return listed.list;
  }
  /** The video the person most likely means here: one playing with sound, else the biggest; null when none. */
  function best() {
    const list = videos().filter((v) => v.isConnected);
    const i = LuchCore.chooseVideo(list.map(about));
    return i < 0 ? null : list[i];
  }
  function about(v) {
    const r = v.getBoundingClientRect();
    return { area: r.width * r.height, playing: !v.paused && !v.ended && v.readyState > 2, muted: v.muted || v.volume === 0 };
  }
  // A YouTube player embedded in a page shows its picture before its video element has a place: the whole frame
  // is its video, and the TV's YouTube app plays it.
  const youtubeEmbed = window !== window.top && !!LuchCore.youtubeLink(location.href);
  function rectOf(v) {
    return youtubeEmbed ? { top: 0, left: 0, right: innerWidth, bottom: innerHeight, width: innerWidth, height: innerHeight }
      : v.getBoundingClientRect();
  }
  function videoAt(x, y) {
    if (youtubeEmbed) return videos()[0] || null;
    for (const v of videos()) {
      const r = v.getBoundingClientRect();
      if (r.width > 160 && r.height > 90 && x >= r.left && x <= r.right && y >= r.top && y <= r.bottom) return v;
    }
    return null;
  }
  document.addEventListener("mousemove", (e) => {
    if (cast) return;
    const v = videoAt(e.clientX, e.clientY);
    if (v !== hovered) {
      hovered = v;
      place();
    }
  }, { passive: true });
  function place() {
    if (!hovered || cast) {
      btn.style.display = "none";
      return;
    }
    const r = rectOf(hovered);
    btn.style.display = "flex";
    btn.style.top = `${Math.max(4, r.top + 10)}px`;
    btn.style.left = `${Math.max(4, r.right - btn.offsetWidth - 10)}px`;
  }
  addEventListener("scroll", () => (cast ? follow() : place()), { passive: true, capture: true });
  addEventListener("resize", () => (cast ? follow() : place()));

  function describe(v) {
    return {
      video: { currentSrc: v.currentSrc, src: v.src, sources: [...v.querySelectorAll("source")].map((s) => ({ src: s.src, type: s.type })),
        audio: v.tagName === "AUDIO" },
      page: {
        title: document.title, pageUrl: location.href, userAgent: navigator.userAgent, currentTime: v.currentTime,
        lang: navigator.language,
        // A video the page never started (an iframe player waiting for its first click) plays on the TV; one paused
        // part way stays paused there too.
        paused: v.paused && v.played.length > 0 && v.currentTime > 0, playbackRate: v.playbackRate, poster: v.poster,
        subs: [...v.querySelectorAll("track")].filter((t) => /subtitles|captions/.test(t.kind || "subtitles")).map((t) => t.src),
      },
    };
  }

  btn.addEventListener("click", (e) => {
    e.stopPropagation();
    if (hovered) castVideo(hovered);
  });

  async function castVideo(v, receiver) {
    const r = await send(Object.assign({ op: "cast", receiver }, describe(v)));
    if (!r.ok) return toast(r.error);
    if (r.choose) return chooseThen(v, r.choose);
    if (r.app) return toast(`${T.app} ${r.name}`);
    start(v, r.receiver, r.name);
    cast.src = r.url;
    qualities(r.qualities || []);
  }

  function chooseThen(v, list) {
    if (!list.length) return toast(RU ? "Не найдено ни одного приёмника Luch" : "No Luch receiver found");
    menu.innerHTML = `<b>${T.choose}</b>`;
    for (const r of list) {
      const b = document.createElement("button");
      b.textContent = r.name + (r.model ? ` · ${r.model}` : "");
      b.onclick = () => {
        menu.style.display = "none";
        castVideo(v, r.id);
      };
      menu.appendChild(b);
    }
    const rect = btn.getBoundingClientRect();
    menu.style.display = "flex";
    menu.style.top = `${rect.bottom + 6}px`;
    menu.style.left = `${Math.max(4, rect.right - 220)}px`;
  }
  document.addEventListener("click", () => (menu.style.display = "none"));

  // ---- while it plays on the TV ------------------------------------------------------------------------------
  function start(v, receiver, name) {
    cast = { receiver, name, video: v, state: null, at: Date.now() };
    holdPage(v);
    btn.style.display = "none";
    $(".title").textContent = `${T.on} ${name}`;
    $(".note").textContent = T.loading;
    panel.style.display = "flex";
    follow();
    send({ op: "state", receiver });
  }

  /** The page's own video stays paused and silent; its play button asks the TV instead. */
  /** The panel's quality list: "best for the TV" (the receiver picks) and each file the page has. */
  function qualities(list) {
    const sel = $(".quality");
    sel.innerHTML = "";
    sel.add(new Option(`${T.quality}: ${T.best}`, ""));
    for (const q of list) if (q.h) sel.add(new Option(`${T.quality}: ${q.h}p`, q.url));
    sel.style.display = list.length > 1 ? "" : "none";
  }

  /** The page's player switched files (another quality, the next episode): the TV follows, from where it is. */
  function onPageSource(e) {
    if (!cast || e.target !== cast.video) return;
    const url = cast.video.currentSrc;
    if (!/^https?:/.test(url) || url === cast.src) return;
    cast.src = url;
    send({ op: "recast", receiver: cast.receiver, url, start: LuchCore_position() });
  }

  function holdPage(v) {
    v.pause();
    // loadstart: a new source, even one this browser cannot play itself (HLS in a <video> outside Safari).
    v.addEventListener("loadstart", onPageSource, true);
    v.addEventListener("loadedmetadata", onPageSource, true);
    v.dataset.luchMuted = v.muted ? "1" : "0";
    v.muted = true;
    v.addEventListener("play", onPagePlay, true);
  }
  function onPagePlay(e) {
    if (!cast || e.target !== cast.video) return;
    cast.video.pause();
    cmd({ do: "play" });
  }
  function releasePage() {
    if (!cast) return;
    const v = cast.video;
    v.removeEventListener("play", onPagePlay, true);
    v.removeEventListener("loadstart", onPageSource, true);
    v.removeEventListener("loadedmetadata", onPageSource, true);
    v.muted = v.dataset.luchMuted === "1";
    // Take up where the TV left off.
    const pos = LuchCore_position();
    if (pos > 0 && Number.isFinite(pos)) v.currentTime = pos;
    cast = null;
    panel.style.display = "none";
  }

  /**
   * The panel sits along the video's top edge, never over its bottom, where sites keep their own controls (quality,
   * episodes): those stay usable, and they drive the TV (play asks the TV, a new quality is recast). Folded, it is
   * one line in the corner.
   */
  function follow() {
    if (!cast) return;
    const r = cast.video.isConnected ? cast.video.getBoundingClientRect() : { left: 0, top: 0, width: innerWidth, height: 0 };
    const small = panel.classList.contains("small");
    panel.style.left = `${Math.max(0, r.left)}px`;
    panel.style.width = small ? "auto" : `${Math.min(innerWidth, r.width)}px`;
    panel.style.top = `${Math.max(0, r.top)}px`;
    panel.style.height = "auto";
  }

  function LuchCore_position() {
    const s = cast && cast.state;
    if (!s || typeof s.pos !== "number") return 0;
    return s.state === "playing" ? Math.min(s.pos + ((Date.now() - cast.at) / 1000) * (s.rate || 1), s.dur || Infinity) : s.pos;
  }

  const clock = (s) => {
    s = Math.max(0, Math.floor(s || 0));
    const two = (n) => String(n).padStart(2, "0");
    return s >= 3600 ? `${Math.floor(s / 3600)}:${two(Math.floor(s / 60) % 60)}:${two(s % 60)}` : `${Math.floor(s / 60)}:${two(s % 60)}`;
  };

  let dragging = false;
  function draw() {
    if (!cast || !cast.state) return;
    const s = cast.state;
    const pos = LuchCore_position();
    $(".play").textContent = s.state === "playing" ? "⏸" : "▶";
    if (!dragging && s.dur) $(".seek").value = String(Math.round((pos / s.dur) * 1000));
    $(".seek").disabled = !s.dur;
    $(".time").textContent = s.dur ? `${clock(pos)} / ${clock(s.dur)}` : clock(pos);
    $(".note").textContent = { loading: T.loading, paused: T.paused, ended: T.ended }[s.state] || (s.w ? `${s.w}×${s.h}` : "");
    $(".rate").value = String(s.rate || 1);
    fill($(".audio"), (s.tracks && s.tracks.audio) || [], s.sel && s.sel.audio, T.sound, false);
    fill($(".sub"), (s.tracks && s.tracks.sub) || [], s.sel && s.sel.sub, T.subs, true);
  }
  setInterval(draw, 500);

  function fill(sel, tracks, chosen, label, offItem) {
    const key = JSON.stringify(tracks) + chosen;
    if (sel.dataset.key === key) return;
    sel.dataset.key = key;
    sel.innerHTML = "";
    if (offItem) sel.add(new Option(`${label}: ${T.off}`, ""));
    for (const t of tracks) sel.add(new Option(`${label}: ${t.label || t.lang || t.id}`, t.id));
    sel.value = chosen || "";
    sel.style.display = tracks.length > (offItem ? 0 : 1) ? "" : "none";
  }

  function cmd(c) {
    if (cast) send({ op: "cmd", receiver: cast.receiver, cmd: c });
  }
  $(".play").onclick = () => cmd({ do: cast && cast.state && cast.state.state === "playing" ? "pause" : "play" });
  $(".back").onclick = () => cmd({ do: "skip", by: -10 });
  $(".fwd").onclick = () => cmd({ do: "skip", by: 10 });
  $(".seek").oninput = () => (dragging = true);
  $(".seek").onchange = () => {
    dragging = false;
    const s = cast && cast.state;
    if (s && s.dur) cmd({ do: "seek", pos: (Number($(".seek").value) / 1000) * s.dur });
  };
  $(".rate").onchange = () => cmd({ do: "rate", rate: Number($(".rate").value) });
  $(".audio").onchange = () => cmd({ do: "audio", id: $(".audio").value });
  $(".sub").onchange = () => cmd({ do: "sub", id: $(".sub").value || null });
  $(".quality").onchange = () => {
    if (!cast) return;
    const url = $(".quality").value || null;
    if (url) cast.src = url;
    send({ op: "recast", receiver: cast.receiver, url, start: LuchCore_position() });
  };
  const fold = () => {
    panel.classList.toggle("small");
    $(".fold").textContent = panel.classList.contains("small") ? "▾" : "▴";
    // The extension's own storage, not the site's.
    B.storage.local.set({ panelSmall: panel.classList.contains("small") }).catch(() => {});
    follow();
  };
  $(".fold").onclick = fold;
  B.storage.local.get("panelSmall").then(({ panelSmall }) => {
    if (panelSmall && !panel.classList.contains("small")) fold();
  }).catch(() => {});
  $(".stop").onclick = () => {
    if (!cast) return;
    send({ op: "stop", receiver: cast.receiver });
    releasePage();
  };

  // Space, k, j, l and the arrows over the panel work as on a player.
  panel.addEventListener("keydown", (e) => {
    const map = { " ": { do: "toggle" }, k: { do: "toggle" }, j: { do: "skip", by: -10 }, l: { do: "skip", by: 10 },
      ArrowLeft: { do: "skip", by: -5 }, ArrowRight: { do: "skip", by: 5 } };
    if (map[e.key]) {
      e.preventDefault();
      cmd(map[e.key]);
    }
  });

  B.runtime.onMessage.addListener((m, sender, reply) => {
    if (m.luch === "best") {
      // The popup asks every frame; the background picks the frame whose video casts.
      const v = best();
      reply(v ? about(v) : null);
      return;
    }
    if (m.luch === "state" && cast && m.receiver === cast.receiver) {
      if (m.state.state === "stopped") return releasePage();
      if (m.state.state === "error" && (!cast.state || cast.state.state !== "error")) {
        toast(`${T.failed}${m.state.detail ? ` (${m.state.detail})` : ""}`);
      }
      cast.state = m.state;
      cast.at = m.at || Date.now();
      draw();
    } else if (m.luch === "note") {
      toast(T.relay);
    } else if (m.luch === "error") {
      toast(m.text);
    } else if (m.luch === "castBiggest") {
      const v = best();
      if (v) castVideo(v, m.receiver);
      else toast(RU ? "На странице нет видео" : "No video on this page");
    }
  });

  let toastTimer = 0;
  function toast(text) {
    let t = shadow.querySelector(".toast");
    if (!t) {
      t = document.createElement("div");
      t.className = "toast";
      t.style.cssText = "position:fixed;left:50%;bottom:24px;transform:translateX(-50%);pointer-events:auto;padding:10px 14px;border-radius:12px;background:#1e1f24;color:#e3e2e8;box-shadow:0 4px 16px #000a;max-width:80vw";
      shadow.appendChild(t);
    }
    t.textContent = text;
    t.style.display = "block";
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (t.style.display = "none"), 5000);
  }

  // A page reloaded while casting picks the panel up again.
  send({ op: "active" }).then((r) => {
    if (r.ok && r.receiver) {
      const v = best();
      if (v) {
        start(v, r.receiver, "TV");
        cast.state = r.state;
      }
    }
  });
})();
