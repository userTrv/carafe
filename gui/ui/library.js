"use strict";

/* Библиотеки, установка, перенос игр и очередь загрузок. Пользуется состоянием и помощниками
   app.js (S, $, esc, I, size, toast, invoke, task, running, start, go); app.js зовёт отсюда
   хуки: installButton, libraryChips, wireLibraryChips, renderLibs, renderDownloads. */

Object.assign(I, {
  disk: '<svg viewBox="0 0 24 24"><rect x="3" y="6" width="18" height="12" rx="2.5"/><path d="M7 14h.01M11 14h6"/></svg>',
  plus: '<svg viewBox="0 0 24 24"><path d="M12 5v14M5 12h14"/></svg>',
  pause: '<svg viewBox="0 0 24 24"><path d="M8 5v14M16 5v14"/></svg>',
  resume: '<svg viewBox="0 0 24 24"><path d="M8 5.5v13l10-6.5z"/></svg>',
  move: '<svg viewBox="0 0 24 24"><path d="M4 12h14M13 7l5 5-5 5M4 5v14"/></svg>',
  star: '<svg viewBox="0 0 24 24"><path d="M12 4l2.4 5 5.6.6-4.2 3.8 1.2 5.6L12 16.2 7 19l1.2-5.6L4 9.6 9.6 9z"/></svg>',
});

S.libs = null;
S.downloads = [];

const DL_STAGE = {
  preparing: "Подготовка",
  allocating: "Место на диске",
  downloading: "Загрузка",
  verifying: "Проверка",
  committing: "Установка",
};
const DL_KIND = { install: "", update: "обновление", verify: "проверка" };

function libName(path) {
  const l = (S.libs || []).find((x) => x.path === path);
  return l ? l.name : String(path || "").split("/").filter(Boolean).pop() || path;
}

function eta(s) {
  if (s == null) return null;
  if (s < 60) return `${s} с`;
  if (s < 3600) return `${Math.ceil(s / 60)} мин`;
  return `${Math.floor(s / 3600)} ч ${Math.round((s % 3600) / 60)} мин`;
}

const gb = (b) => size(b) || "0 МБ";

/* ---------- данные ---------- */

async function loadLibs() {
  try {
    S.libs = await invoke("libraries");
  } catch (e) {
    S.libs = S.libs || [];
    toast(e);
  }
  renderLibs();
  if (S.view === "game") renderGameChips();
}

let dlTimer = null;
async function pollDownloads() {
  clearTimeout(dlTimer);
  const before = S.downloads.map((d) => d.appid).join();
  try {
    S.downloads = await invoke("downloads");
  } catch (e) {
    S.downloads = [];
  }
  const after = S.downloads.map((d) => d.appid).join();
  // Загрузка закончилась или отменена — игры и место на дисках поменялись.
  if (before && before !== after) {
    loadGames();
    loadLibs();
  }
  renderDlBadge();
  if (S.view === "downloads") renderDownloads();
  if (S.view === "game" && $("#chips")) {
    renderGameChips();
    renderPlay();
  }
  const busy = S.downloads.some((d) => d.state === "active");
  dlTimer = setTimeout(pollDownloads, busy || S.view === "downloads" ? 1000 : 4000);
}

function renderDlBadge() {
  const el = $("#dl-count");
  if (!el) return;
  const n = S.downloads.length;
  el.hidden = !n;
  el.textContent = n;
}

const queued = (appid) => S.downloads.find((d) => d.appid === appid);

/* ---------- меню выбора библиотеки ---------- */

function closeMenu() {
  document.querySelector(".menu")?.remove();
}

document.addEventListener("click", (e) => {
  if (!e.target.closest(".menu, [data-menu]")) closeMenu();
});
document.addEventListener("keydown", (e) => e.key === "Escape" && closeMenu());

function libMenu(anchor, { exclude, need, onPick }) {
  closeMenu();
  const libs = (S.libs || []).filter((l) => l.path !== exclude);
  const el = document.createElement("div");
  el.className = "menu";
  el.innerHTML = libs.length
    ? libs
        .map((l) => {
          const short = need && l.freeBytes != null && l.freeBytes < need;
          const off = !l.available || short;
          return `<button class="menu-item" data-path="${esc(l.path)}" ${off ? "disabled" : ""}>
            ${I.disk}<span class="grow"><b>${esc(l.name)}</b>${l.default ? `<i class="menu-tag">по умолчанию</i>` : ""}
            <small>${!l.available ? "отключена" : `свободно ${gb(l.freeBytes)}${short ? " — мало" : ""}`}</small></span></button>`;
        })
        .join("")
    : `<div class="menu-empty">Других библиотек нет</div>`;
  el.insertAdjacentHTML("beforeend", `<button class="menu-item menu-add" data-add>${I.plus}<span class="grow">Добавить папку…</span></button>`);
  document.body.append(el);
  const r = anchor.getBoundingClientRect();
  const w = el.offsetWidth;
  el.style.left = `${Math.max(8, Math.min(r.right - w, innerWidth - w - 8))}px`;
  el.style.top = `${Math.min(r.bottom + 6, innerHeight - el.offsetHeight - 8)}px`;
  el.addEventListener("click", async (e) => {
    const b = e.target.closest(".menu-item");
    if (!b || b.disabled) return;
    closeMenu();
    if (b.dataset.add !== undefined) {
      const path = await addLibrary();
      if (path) onPick(path);
    } else onPick(b.dataset.path);
  });
}

/* ---------- установка ---------- */

function installButton(g) {
  const d = queued(g.appid);
  if (d) {
    const pct = d.totalBytes ? Math.round((d.doneBytes / d.totalBytes) * 100) : 0;
    const label = d.state === "active" ? `${DL_STAGE[d.stage] || "Загрузка"} · ${pct}%` : d.state === "paused" ? "Пауза" : d.state === "failed" ? "Ошибка" : "В очереди";
    return `<button class="play busy" data-goto-downloads>${d.state === "active" ? '<span class="spin"></span>' : I.down}${esc(label)}</button>`;
  }
  return `<button class="play" data-install="${g.appid}" data-menu>${I.down}Установить</button>`;
}

async function install(appid, library) {
  try {
    await invoke("install", { appid, library });
    toast(`В очереди: ${libName(library)}`, "ok");
  } catch (e) {
    toast(e);
  }
  pollDownloads();
}

/* ---------- перенос ---------- */

function moveGame(appid, library) {
  start("move_game", { appid, library }, `move:${appid}`);
}

function libraryChips(g) {
  if (!g.library) return "";
  const t = task(`move:${g.appid}`);
  if (t?.running) {
    const p = t.progress.move;
    return `<span class="chip info">${I.move}Переношу${p !== undefined ? ` · ${Math.round(p * 100)}%` : ""}</span>
      <button class="btn btn-sm btn-ghost" id="move-cancel">Отмена</button>`;
  }
  const off = g.available === false;
  return `<span class="chip ${off ? "warn" : "chip-quiet"}" title="${esc(g.library)}">${I.disk}${esc(libName(g.library))}${off ? " · отключена" : ""}</span>
    ${!off && !running(`launch:${g.appid}`) && !queued(g.appid) && (S.libs || []).length > 0 ? `<button class="btn btn-sm" id="move" data-menu>Переместить</button>` : ""}`;
}

function wireLibraryChips(g) {
  const m = $("#move");
  if (m)
    m.onclick = (e) => {
      e.stopPropagation();
      libMenu(m, { exclude: g.library, need: g.sizeBytes, onPick: (p) => moveGame(g.appid, p) });
    };
  const c = $("#move-cancel");
  if (c) c.onclick = () => invoke("cancel_task", { task: `move:${g.appid}` });
}

main.addEventListener("click", (e) => {
  const b = e.target.closest("[data-install]");
  if (b) {
    e.stopPropagation();
    const appid = Number(b.dataset.install);
    const g = game(appid);
    if (!S.libs) loadLibs();
    libMenu(b, { need: g?.sizeBytes, onPick: (p) => install(appid, p) });
    return;
  }
  if (e.target.closest("[data-goto-downloads]")) {
    e.stopPropagation();
    go("downloads");
  }
});

listen("task-end", ({ payload }) => {
  const [kind, id] = payload.task.split(":");
  if (kind !== "move") return;
  if (payload.ok) toast(`Перенесено: ${game(Number(id))?.name || id}`, "ok");
  loadGames();
  loadLibs();
});

listen("task", ({ payload: { task: name } }) => {
  if (name.startsWith("move:") && S.view === "game" && S.appid === Number(name.split(":")[1])) renderGameChips();
});

/* ---------- настройки: библиотеки ---------- */

async function addLibrary() {
  let path;
  try {
    path = await invoke("pick_folder");
  } catch (e) {
    toast(e);
    return null;
  }
  if (!path) return null;
  try {
    await invoke("library_action", { action: "add", path });
    await loadLibs();
    return path.replace(/\/+$/, "");
  } catch (e) {
    toast(e);
    return null;
  }
}

async function libAction(action, path) {
  try {
    await invoke("library_action", { action, path });
  } catch (e) {
    toast(e);
  }
  loadLibs();
  if (action === "remove") loadGames();
}

function renderLibs() {
  const el = $("#libs");
  if (!el) return;
  const libs = S.libs;
  const rows = libs
    ? libs
        .map((l) => {
          const total = l.totalBytes || 0;
          const games = total ? (l.usedBytes / total) * 100 : 0;
          const other = total ? ((total - l.freeBytes - l.usedBytes) / total) * 100 : 0;
          return `<div class="field lib ${l.available ? "" : "lib-off"}">
            <span class="lib-ico">${I.disk}</span>
            <div class="field-label">
              <div class="lib-name">${esc(l.name)}${l.default ? `<span class="chip ok lib-tag">по умолчанию</span>` : ""}${l.available ? "" : `<span class="chip warn lib-tag">отключена</span>`}</div>
              <div class="field-sub lib-path" title="${esc(l.path)}">${esc(l.path)}</div>
              ${
                l.available && total
                  ? `<div class="lib-bar" title="игры ${gb(l.usedBytes)}"><i class="g" style="width:${games}%"></i><i class="o" style="width:${Math.max(0, other)}%"></i></div>
                     <div class="field-sub">свободно ${gb(l.freeBytes)} из ${gb(total)} · игр ${l.games}${l.usedBytes ? `, ${gb(l.usedBytes)}` : ""}</div>`
                  : `<div class="field-sub">игр ${l.games}${l.usedBytes ? `, ${gb(l.usedBytes)}` : ""}</div>`
              }
            </div>
            ${!l.default && l.available ? `<button class="btn btn-sm btn-ghost" data-lib="default" data-path="${esc(l.path)}" title="Ставить сюда новые игры">${I.star}</button>` : ""}
            ${!l.builtin ? `<button class="btn btn-sm btn-ghost" data-lib="remove" data-path="${esc(l.path)}" title="Убрать из списка (файлы останутся)">${I.x}</button>` : ""}
          </div>`;
        })
        .join("")
    : `<div class="field skeleton" style="height:76px"></div>`;
  el.innerHTML = `<h2>Библиотеки</h2>${rows}
    <button class="btn btn-sm lib-add" id="lib-add">${I.plus}Добавить папку</button>`;
  el.querySelectorAll("[data-lib]").forEach((b) => (b.onclick = () => libAction(b.dataset.lib, b.dataset.path)));
  $("#lib-add").onclick = addLibrary;
}

/* ---------- загрузки ---------- */

async function dlAction(appid, action) {
  try {
    await invoke("download_action", { appid, action });
  } catch (e) {
    toast(e);
  }
  pollDownloads();
}

function renderDownloads() {
  if (S.view !== "downloads") return;
  if (!$("#dl-list")) {
    main.innerHTML = `<section class="view dl"><div class="head"><h1>Загрузки</h1></div><div id="dl-list"></div></section>`;
    $("#dl-list").addEventListener("click", (e) => {
      const b = e.target.closest("[data-dl]");
      if (b) dlAction(Number(b.closest("[data-appid]").dataset.appid), b.dataset.dl);
    });
  }
  const list = S.downloads;
  const el = $("#dl-list");
  if (!list.length) {
    el.innerHTML = `<div class="empty">Очередь пуста</div>`;
    return;
  }
  el.innerHTML = list
    .map((d, i) => {
      const pct = d.totalBytes ? (d.doneBytes / d.totalBytes) * 100 : 0;
      const active = d.state === "active";
      const stage = active || d.state === "paused" ? DL_STAGE[d.stage] || "Подготовка" : null;
      const status =
        d.state === "failed"
          ? `<span class="dl-err" title="${esc(d.error)}">${esc(d.error || "Ошибка")}</span>`
          : d.state === "paused"
            ? `Пауза${stage && d.totalBytes ? ` · ${stage} ${Math.floor(pct)}%` : ""}`
            : active
              ? `${stage}${d.totalBytes ? ` · ${Math.floor(pct)}%` : ""}`
              : "В очереди";
      const bytes = d.totalBytes && d.state !== "queued" ? `${gb(d.doneBytes)} из ${gb(d.totalBytes)}` : d.totalBytes ? gb(d.totalBytes) : "";
      const speed = active && d.speed ? `${size(d.speed)}/с` : "";
      const left = active ? eta(d.etaSecs) : null;
      const kind = DL_KIND[d.kind];
      return `<div class="dl-row ${d.state}" data-appid="${d.appid}">
        <img class="dl-cover" src="${esc(d.cover.header)}" alt="">
        <div class="dl-body">
          <div class="dl-title"><b>${esc(d.name)}</b>${kind ? `<span class="chip chip-quiet">${kind}</span>` : ""}
            <span class="dl-lib">${I.disk}${esc(libName(d.library))}</span></div>
          <div class="bar dl-bar"><i style="width:${d.state === "queued" ? 0 : pct}%"></i></div>
          <div class="dl-meta"><span>${status}</span><span class="grow"></span>
            ${[bytes, speed, left].filter(Boolean).map((x) => `<span>${esc(x)}</span>`).join("<span>·</span>")}</div>
        </div>
        <div class="dl-actions">
          <button class="btn btn-sm btn-ghost" data-dl="up" title="Выше" ${i === 0 ? "disabled" : ""}>${I.up}</button>
          <button class="btn btn-sm btn-ghost" data-dl="down" title="Ниже" ${i === list.length - 1 ? "disabled" : ""}>${I.down}</button>
          ${
            d.state === "paused" || d.state === "failed"
              ? `<button class="btn btn-sm" data-dl="resume" title="${d.state === "failed" ? "Повторить" : "Продолжить"}">${d.state === "failed" ? I.refresh : I.resume}</button>`
              : `<button class="btn btn-sm" data-dl="pause" title="Пауза">${I.pause}</button>`
          }
          <button class="btn btn-sm btn-ghost" data-dl="cancel" title="Убрать из очереди">${I.x}</button>
        </div>
      </div>`;
    })
    .join("");
}

/* ---------- старт ---------- */

loadLibs();
pollDownloads();

// Только для разработки: CARAFE_GUI_ROUTE=install/<appid> или move/<appid> — карточка игры с
// открытым меню библиотек, move/<appid>/go — сразу перенос в первую подходящую.
(async function devRoute() {
  for (let i = 0; i < 100 && (!S.info || !S.games || !S.libs); i++) await new Promise((r) => setTimeout(r, 100));
  const [r, id, now] = (S.info?.route || "").split("/");
  if (r !== "install" && r !== "move") return;
  go("game", Number(id));
  await new Promise((res) => setTimeout(res, 300));
  const g = game(Number(id));
  if (r === "move" && now === "go") {
    const to = S.libs.find((l) => l.available && l.path !== g.library);
    if (to) moveGame(g.appid, to.path);
  } else (r === "move" ? $("#move") : main.querySelector("[data-install]"))?.click();
})();
