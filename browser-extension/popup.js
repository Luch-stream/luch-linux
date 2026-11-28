// SPDX-License-Identifier: GPL-3.0-or-later
// The toolbar popup: which TV to cast to, how links travel, and "send this page's video".
"use strict";
const B = typeof browser !== "undefined" ? browser : chrome;
const RU = (navigator.language || "").toLowerCase().startsWith("ru");
const T = RU
  ? { wait: "Ищу приёмники…", none: "Приёмников Luch не найдено", cast: "Показать видео этой страницы",
      auto: "Ссылки: ТВ сам, если сможет", always: "Ссылки: всегда через этот компьютер", never: "Ссылки: всегда ТВ сам",
      trust: "мой ТВ: можно передавать вход на сайты (cookies)" }
  : { wait: "Looking for receivers…", none: "No Luch receivers found", cast: "Send this page's video",
      auto: "Links: the TV fetches them when it can", always: "Links: always through this computer", never: "Links: always the TV itself",
      trust: "my TV: it may get my site logins (cookies)" };
const $ = (id) => document.getElementById(id);
const send = (msg) => new Promise((res) => B.runtime.sendMessage(msg, (r) => res(r || { ok: false, error: "no answer" })));

$("wait").textContent = T.wait;
$("cast").textContent = T.cast;
for (const k of ["auto", "always", "never"]) $("relay").add(new Option(T[k], k));
B.storage.local.get("relay").then(({ relay }) => ($("relay").value = relay || "auto"));
$("relay").onchange = () => B.storage.local.set({ relay: $("relay").value });

let chosen = null;
send({ op: "receivers" }).then((r) => {
  const list = $("list");
  list.innerHTML = "";
  if (!r.ok) {
    $("err").textContent = r.error;
    return;
  }
  if (!r.list.length) {
    list.innerHTML = `<small>${T.none}</small>`;
    return;
  }
  chosen = r.list.some((x) => x.id === r.receiver) ? r.receiver : r.list[0].id;
  const trusted = r.trusted || [];
  for (const rec of r.list) {
    const l = document.createElement("label");
    const i = document.createElement("input");
    i.type = "radio";
    i.name = "rx";
    i.checked = rec.id === chosen;
    i.onchange = () => {
      chosen = rec.id;
      send({ op: "choose", receiver: rec.id });
    };
    const text = document.createElement("span");
    text.innerHTML = `${escape(rec.name)}<br><small>${escape(rec.model || "")}</small>`;
    l.append(i, text);
    list.appendChild(l);
    // Cookies go to a TV only when the person says it is theirs; otherwise a link that needs a login is relayed.
    const t = document.createElement("label");
    t.style.paddingLeft = "28px";
    const box = document.createElement("input");
    box.type = "checkbox";
    box.checked = trusted.includes(rec.id);
    box.onchange = () => send({ op: "trust", receiver: rec.id, on: box.checked });
    const small = document.createElement("small");
    small.textContent = T.trust;
    t.append(box, small);
    list.appendChild(t);
  }
  $("cast").disabled = false;
});

$("cast").onclick = async () => {
  const [tab] = await B.tabs.query({ active: true, currentWindow: true });
  await send({ op: "choose", receiver: chosen });
  // The background asks every frame of the page which video it has and casts from the right one.
  const r = await send({ op: "castPage", tabId: tab.id, receiver: chosen });
  if (!r.ok) return ($("err").textContent = r.error);
  window.close();
};

function escape(s) {
  return String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
}
