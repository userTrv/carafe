"use strict";

/* Библиотеки, установка, перенос игр и очередь загрузок. Пользуется состоянием и помощниками
   app.js (S, $, esc, I, size, toast, invoke, task, running, start, go, refreshGame); app.js зовёт
   отсюда хуки: installButton, libraryActions, wireLibraryActions, libName, queued, renderLibs,
   renderDownloads. */

S.libs = null;
S.downloads = [];

const DL_STAGE = {
  preparing: "Подготовка",
  allocating: "Место на диске",
  downloading: "Загрузка",
  verifying: "Проверка",
  committing: "Установка",
};
const DL_KIND = { install: "", update: "ОБНОВЛЕНИЕ", verify: "ПРОВЕРКА" };

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

const gb = (b) => size(b) || "0 MB";
const pctOf = (d) => (d.totalBytes ? (d.doneBytes / d.totalBytes) * 100 : 0);

/* ---------- данные ---------- */

async function loadLibs() {
  try {
    S.libs = await invoke("libraries");
  } catch (e) {
    S.libs = S.libs || [];
    toast(e);
  }
  renderLibs();
  if (S.view === "game") refreshGame();
}

let dlTimer = null;
// Сколько держать в панели только что скачанную игру, если на «Загрузки» так и не зашли.
const DL_RECENT_MS = 10 * 60 * 1000;
S.dlRecent = []; // { appid, name, cover, at } — докачались, пока окно открыто
S.dlCancelled = new Set();

async function pollDownloads() {
  clearTimeout(dlTimer);
  const prev = S.downloads;
  const was = JSON.stringify(prev.map((d) => [d.appid, d.state]));
  try {
    S.downloads = await invoke("downloads");
  } catch (e) {
    S.downloads = [];
  }
  const now = new Set(S.downloads.map((d) => d.appid));
  const gone = prev.filter((d) => !now.has(d.appid));
  for (const d of gone) {
    // Убрали из очереди сами — не «готово».
    if (S.dlCancelled.delete(d.appid)) continue;
    S.dlRecent = [{ appid: d.appid, name: d.name, cover: d.cover, at: Date.now() }, ...S.dlRecent.filter((r) => r.appid !== d.appid)];
  }
  S.dlRecent = S.dlRecent.filter((r) => Date.now() - r.at < DL_RECENT_MS && !now.has(r.appid));
  // Загрузка закончилась или отменена — игры и место на дисках поменялись.
  if (gone.length) {
    loadGames();
    loadLibs();
  }
  renderDlBadge();
  if (S.view === "downloads") renderDownloads();
  if (S.view === "game" && $("#actions")) renderActions();
  // Библиотека: сменилось состояние — пересобрать, иначе только проценты на месте.
  const states = JSON.stringify(S.downloads.map((d) => [d.appid, d.state]));
  if (S.view === "library" && states !== was && !document.querySelector(".menu")) renderLibraryBody();
  else if (S.view === "library") updateOwnedProgress();
  const busy = S.downloads.some((d) => d.state === "active");
  dlTimer = setTimeout(pollDownloads, busy || S.view === "downloads" ? 1000 : 4000);
}

// Ушли с «Загрузок» — готовые увидены, кнопка в панели больше не нужна.
const seenDownloads = () => {
  S.dlRecent = [];
  renderDlBadge();
};

/* Кнопка загрузок в панели — только когда есть что показать: очередь, ошибка или только что
   скачанная игра. Без загрузок её нет совсем. */
function renderDlBadge() {
  const tab = $("#dl-tab");
  const n = S.downloads.length;
  const active = S.downloads.find((d) => d.state === "active");
  const failed = S.downloads.some((d) => d.state === "failed");
  const done = !n && S.dlRecent.length > 0;
  const show = n > 0 || done || S.view === "downloads";
  if (tab.hidden === show) tab.hidden = !show;
  const ico = failed ? "alert" : done ? "check" : "download";
  const icoEl = $("#dl-ico");
  if (icoEl.dataset.ico !== ico) {
    icoEl.dataset.ico = ico;
    icoEl.innerHTML = I[ico];
    icoEl.className = `dl-ico ${failed ? "warn" : done ? "ok" : ""}`;
  }
  const meter = $("#dl-meter");
  meter.hidden = !active;
  if (active) {
    const t = `scaleX(${pctOf(active) / 100})`;
    if (meter.firstElementChild.style.transform !== t) meter.firstElementChild.style.transform = t;
  }
  const cnt = $("#dl-count");
  const text = active ? `${Math.floor(pctOf(active))}%` : n ? String(n) : "";
  cnt.hidden = !text;
  if (cnt.textContent !== text) cnt.textContent = text;
  const label = n
    ? `Загрузки: ${n}${active ? `, ${Math.floor(pctOf(active))}%` : ""}${failed ? ", есть ошибка" : ""}`
    : done
      ? "Загрузки: готово"
      : "Загрузки";
  if (tab.getAttribute("aria-label") !== label) tab.setAttribute("aria-label", label);
}

const queued = (appid) => S.downloads.find((d) => d.appid === appid);

function updateOwnedProgress() {
  for (const d of S.downloads) {
    if (d.state !== "active") continue;
    const pct = Math.floor(pctOf(d));
    const tag = document.querySelector(`[data-dl-tag="${d.appid}"]`);
    if (tag) tag.textContent = `${pct}%`;
    const fill = document.querySelector(`[data-dl-fill="${d.appid}"]`);
    if (fill) {
      fill.style.transform = `translateY(${100 - pct}%)`;
      fill.firstElementChild.style.transform = `translateY(${pct - 100}%)`;
    }
    const bar = document.querySelector(`[data-dl-bar="${d.appid}"]`);
    if (bar) {
      bar.setAttribute("aria-valuenow", pct);
      bar.firstElementChild.style.transform = `scaleX(${pct / 100})`;
    }
  }
}

/* ---------- меню выбора библиотеки ---------- */

function closeMenu() {
  const m = document.querySelector(".menu");
  if (!m) return;
  m.remove();
  closeMenu.anchor?.focus();
  closeMenu.anchor = null;
}

document.addEventListener("click", (e) => {
  if (!e.target.closest(".menu, [data-menu]")) closeMenu();
});
document.addEventListener("keydown", (e) => e.key === "Escape" && closeMenu());

// choice — выбор сборки над списком папок: { options: [{ id, label, tag, need }], value }.
// Выбранная сборка уходит в onPick вторым аргументом.
function libMenu(anchor, { exclude, need, onPick, choice }) {
  closeMenu();
  const libs = (S.libs || []).filter((l) => l.path !== exclude);
  const el = document.createElement("div");
  el.className = "menu";
  el.setAttribute("role", "menu");
  let picked = choice?.value;
  if (choice) need = choice.options.find((o) => o.id === picked)?.need ?? need;
  const choiceHtml = choice
    ? `<div class="menu-choice" role="group" aria-label="Как установить">${choice.options
        .map(
          (o) => `<button class="menu-item" role="menuitemradio" data-platform="${o.id}" aria-checked="${o.id === picked}">
            <span class="radio" aria-hidden="true"></span><span class="grow"><b>${esc(o.label)}${o.tag ? `<i class="menu-tag">${o.tag}</i>` : ""}</b>
            ${o.need ? `<small>${gb(o.need)}</small>` : ""}</span></button>`,
        )
        .join("")}</div>`
    : "";
  const libsHtml = () => libs.length
    ? libs
        .map((l) => {
          const short = need && l.freeBytes != null && l.freeBytes < need;
          const off = !l.available || short;
          return `<button class="menu-item" role="menuitem" data-path="${esc(l.path)}" ${off ? "disabled" : ""}>
            ${I.disk}<span class="grow"><b>${esc(l.name)}${l.default ? `<i class="menu-tag">ПО УМОЛЧАНИЮ</i>` : ""}</b>
            <small>${!l.available ? "отключена" : `свободно ${gb(l.freeBytes)}${short ? " · мало места" : ""}`}</small></span></button>`;
        })
        .join("")
    : `<div class="menu-empty">Других библиотек нет</div>`;
  el.innerHTML = `${choiceHtml}<div class="menu-libs">${libsHtml()}</div>`;
  el.insertAdjacentHTML("beforeend", `<button class="menu-item menu-add" role="menuitem" data-add>${I.plus}<span class="grow"><b>Добавить папку</b></span></button>`);
  document.body.append(el);
  closeMenu.anchor = anchor;
  const r = anchor.getBoundingClientRect();
  const w = el.offsetWidth;
  el.style.left = `${Math.max(8, Math.min(r.left, innerWidth - w - 8))}px`;
  el.style.top = `${Math.min(r.bottom + 8, innerHeight - el.offsetHeight - 8)}px`;
  el.querySelector(".menu-item:not(:disabled)")?.focus();
  el.addEventListener("click", async (e) => {
    const b = e.target.closest(".menu-item");
    if (!b || b.disabled) return;
    // Выбор сборки меню не закрывает: место на дисках пересчитывается под её размер.
    if (b.dataset.platform) {
      e.stopPropagation();
      picked = b.dataset.platform;
      need = choice.options.find((o) => o.id === picked)?.need ?? need;
      el.querySelectorAll("[data-platform]").forEach((x) => x.setAttribute("aria-checked", String(x === b)));
      el.querySelector(".menu-libs").innerHTML = libsHtml();
      return;
    }
    closeMenu();
    if (b.dataset.add !== undefined) {
      const path = await addLibrary();
      if (path) onPick(path, picked);
    } else onPick(b.dataset.path, picked);
  });
}

/* ---------- установка ---------- */

function installButton(g) {
  const d = queued(g.appid);
  if (d) {
    const pct = Math.floor(pctOf(d));
    const label =
      d.state === "active" ? `${DL_STAGE[d.stage] || "Загрузка"} · ${pct}%` : d.state === "paused" ? "Пауза" : d.state === "failed" ? "Ошибка загрузки" : "В очереди";
    return `<button class="btn btn-ghost launch" data-goto-downloads>${I.download}${esc(label)}</button>`;
  }
  return `<button class="btn btn-primary launch" data-install="${g.appid}" data-menu aria-haspopup="menu">${I.download}Установить</button>`;
}

async function install(appid, library, platform = "windows") {
  try {
    await invoke("install", { appid, library, platform });
    toast(`В очереди: ${libName(library)}${platform === "macos" ? " · macOS" : ""}`, "ok");
  } catch (e) {
    toast(e);
  }
  pollDownloads();
}

// Есть нативная сборка — выбор, как ставить; по умолчанию нативно. Только macOS — без выбора.
function installChoice(g) {
  if (!g?.macos) return null;
  const mac = { id: "macos", label: "Нативно для macOS", tag: "РЕКОМЕНДУЕТСЯ", need: g.macosSizeBytes };
  if (g.windows === false) return { options: [{ ...mac, tag: null }], value: "macos" };
  return { options: [mac, { id: "windows", label: "Через carafe (Windows)", need: g.sizeBytes }], value: "macos" };
}

/* ---------- перенос ---------- */

function moveGame(appid, library) {
  start("move_game", { appid, library }, `move:${appid}`);
}

function libraryActions(g) {
  if (!g.library) return "";
  const t = task(`move:${g.appid}`);
  if (t?.running) {
    const p = t.progress.move;
    return `<span class="tag">${I.move}ПЕРЕНОС${p !== undefined ? ` ${Math.round(p * 100)}%` : ""}</span>
      <button class="btn btn-ghost" id="move-cancel">Отмена</button>`;
  }
  const off = g.available === false;
  return !off && !running(`launch:${g.appid}`) && !queued(g.appid) && (S.libs || []).length > 0
    ? `<button class="btn btn-ghost" id="move" data-menu aria-haspopup="menu">${I.move}Переместить</button>`
    : "";
}

function wireLibraryActions(g) {
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
    libMenu(b, { need: g?.sizeBytes, choice: installChoice(g), onPick: (p, platform) => install(appid, p, platform) });
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
  if (name.startsWith("move:") && S.view === "game" && S.appid === Number(name.split(":")[1])) refreshGame();
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
          const used = `игр ${l.games}${l.usedBytes ? ` · ${gb(l.usedBytes)}` : ""}`;
          return `<div class="row lib ${l.available ? "" : "lib-off"}">
            <span class="lib-ico">${I.disk}</span>
            <div class="row-label">
              <div class="lib-name">${esc(l.name)}${l.default ? `<span class="caption" style="color:var(--accent)">ПО УМОЛЧАНИЮ</span>` : ""}${l.available ? "" : `<span class="caption">ОТКЛЮЧЕНА</span>`}</div>
              <div class="row-sub" title="${esc(l.path)}">${esc(l.path)}</div>
              ${
                l.available && total
                  ? `<div class="lib-bar" aria-hidden="true"><i class="g" style="width:${games}%"></i><i class="o" style="width:${Math.max(0, other)}%"></i></div>
                     <div class="row-sub">свободно ${gb(l.freeBytes)} из ${gb(total)} · ${used}</div>`
                  : `<div class="row-sub">${used}</div>`
              }
            </div>
            ${!l.default && l.available ? `<button class="icon-btn" data-lib="default" data-path="${esc(l.path)}" aria-label="Ставить сюда новые игры: ${esc(l.name)}" title="Ставить сюда новые игры">${I.star}</button>` : ""}
            ${!l.builtin ? `<button class="icon-btn" data-lib="remove" data-path="${esc(l.path)}" aria-label="Убрать из списка: ${esc(l.name)}" title="Убрать из списка, файлы останутся">${I.x}</button>` : ""}
          </div>`;
        })
        .join("")
    : `<div class="row skel" style="height:96px"></div>`;
  el.innerHTML = `<h2>Библиотеки</h2><div class="rows">${rows}</div>
    <div class="group-foot"><button class="btn btn-sm" id="lib-add">${I.plus}Добавить папку</button></div>`;
  el.querySelectorAll("[data-lib]").forEach((b) => (b.onclick = () => libAction(b.dataset.lib, b.dataset.path)));
  $("#lib-add").onclick = addLibrary;
}

/* ---------- загрузки ---------- */

async function dlAction(appid, action) {
  if (action === "cancel") S.dlCancelled.add(appid);
  try {
    await invoke("download_action", { appid, action });
  } catch (e) {
    S.dlCancelled.delete(appid);
    toast(e);
  }
  pollDownloads();
}

/* Список обновляется раз в секунду. Пересобирать его целиком нельзя: картинки создаются
   заново и моргают, у пустого состояния каждый раз заново проигрывается появление. Поэтому
   строка пересобирается, только когда меняется её устройство (состояние, место в очереди),
   а проценты, байты и скорость меняются на месте. */
function renderDownloads() {
  if (S.view !== "downloads") return;
  if (!$("#dl-list")) {
    main.innerHTML = `<div class="view dl"><h1 class="page-title display">Загрузки</h1>
      <div id="dl-list" class="dl-list"></div><div id="dl-done" class="dl-list"></div></div>`;
    $("#dl-list").addEventListener("click", (e) => {
      const b = e.target.closest("[data-dl]");
      if (b) dlAction(Number(b.closest("[data-appid]").dataset.appid), b.dataset.dl);
      if (e.target.closest("[data-go-lib]")) go("library");
    });
  }
  const list = S.downloads;
  const el = $("#dl-list");
  const empty = !list.length && !S.dlRecent.length;
  if (empty) {
    if (!el.querySelector(".empty"))
      el.innerHTML = `<div class="empty rise">${I.download}<h3>Очередь пуста</h3><button class="btn btn-sm" data-go-lib>В библиотеку</button></div>`;
  } else {
    el.querySelector(".empty")?.remove();
    syncRows(el, list, dlRow, dlUpdate, (d) => d.appid);
  }
  const done = $("#dl-done");
  syncRows(done, S.dlRecent, doneRow, () => {}, (r) => r.appid);
}

// Строки по ключу: лишние убрать, новые и изменившиеся по устройству — пересобрать, остальные
// обновить на месте. Обложка переезжает в новую строку тем же узлом.
function syncRows(el, items, html, update, key) {
  const want = new Set(items.map((x) => String(key(x))));
  el.querySelectorAll(":scope > [data-appid]").forEach((r) => !want.has(r.dataset.appid) && r.remove());
  items.forEach((x, i) => {
    const id = String(key(x));
    const sig = html(x, i, items, true);
    let row = el.querySelector(`:scope > [data-appid="${id}"]`);
    if (!row || row.dataset.sig !== sig) {
      const tpl = document.createElement("template");
      tpl.innerHTML = html(x, i, items).trim();
      const fresh = tpl.content.firstElementChild;
      fresh.dataset.sig = sig;
      const img = row?.querySelector(".dl-cover img");
      if (img) fresh.querySelector(".dl-cover").replaceChildren(...row.querySelector(".dl-cover").childNodes);
      const focus = row?.contains(document.activeElement) && document.activeElement.dataset.dl;
      if (row) row.replaceWith(fresh);
      else el.append(fresh);
      row = fresh;
      if (focus) row.querySelector(`[data-dl="${focus}"]`)?.focus();
    }
    if (el.children[i] !== row) el.insertBefore(row, el.children[i] || null);
    update(row, x);
  });
}

// Меняющиеся каждую секунду части строки.
function dlNums(d) {
  const pct = pctOf(d);
  const active = d.state === "active";
  const stage = active || d.state === "paused" ? DL_STAGE[d.stage] || "Подготовка" : null;
  const status =
    d.state === "failed"
      ? d.error || "Ошибка"
      : d.state === "paused"
        ? `Пауза${stage && d.totalBytes ? ` · ${Math.floor(pct)}%` : ""}`
        : active
          ? `${stage}${d.totalBytes ? ` · ${Math.floor(pct)}%` : ""}`
          : "В очереди";
  const bytes = d.totalBytes && d.state !== "queued" ? `${gb(d.doneBytes)} из ${gb(d.totalBytes)}` : d.totalBytes ? gb(d.totalBytes) : "";
  const speed = active && d.speed ? `${size(d.speed)}/с` : "";
  const left = active ? eta(d.etaSecs) : null;
  return {
    pct,
    status,
    extra: [bytes, speed, left].filter(Boolean),
    scale: d.state === "queued" ? 0 : pct / 100,
  };
}

function dlUpdate(row, d) {
  const n = dlNums(d);
  const bar = row.querySelector(".bar");
  const v = String(Math.floor(n.pct));
  if (bar.getAttribute("aria-valuenow") !== v) bar.setAttribute("aria-valuenow", v);
  const t = `scaleX(${n.scale})`;
  if (bar.firstElementChild.style.transform !== t) bar.firstElementChild.style.transform = t;
  const st = row.querySelector(".state");
  if (st.textContent !== n.status) st.textContent = n.status;
  const nums = row.querySelector(".dl-nums");
  const html = n.extra.map((x) => `<span>${esc(x)}</span>`).join("");
  if (nums.dataset.html !== html) {
    nums.dataset.html = html;
    nums.innerHTML = html;
  }
}

// `sig` — только устройство строки, без цифр: по нему решаем, пересобирать ли её.
function dlRow(d, i, list, sig) {
  const kind = [DL_KIND[d.kind], d.platform === "macos" ? "MACOS" : ""].filter(Boolean).join(" · ");
  const paused = d.state === "paused" || d.state === "failed";
  if (sig) return [d.state, d.kind, d.platform, d.name, d.library, i === 0, i === list.length - 1].join("|");
  const g = { name: d.name, cover: d.cover };
  const n = dlNums(d);
  return `<div class="dl-row ${d.state}" data-appid="${d.appid}">
    <span class="dl-cover">${cover(g, d.cover.header)}</span>
    <div class="dl-body">
      <div class="dl-title"><b>${esc(d.name)}</b>${kind ? `<span class="tag plain">${kind}</span>` : ""}
        <span class="caption">${esc(libName(d.library))}</span></div>
      <div class="bar" role="progressbar" aria-label="${esc(d.name)}" aria-valuenow="${Math.floor(n.pct)}" aria-valuemin="0" aria-valuemax="100"><i style="transform:scaleX(${n.scale})"></i></div>
      <div class="dl-meta"><span class="state">${esc(n.status)}</span><span class="grow"></span><span class="dl-nums"></span></div>
    </div>
    <div class="dl-actions">
      <button class="icon-btn" data-dl="up" aria-label="Выше в очереди" ${i === 0 ? "disabled" : ""}>${I.up}</button>
      <button class="icon-btn" data-dl="down" aria-label="Ниже в очереди" ${i === list.length - 1 ? "disabled" : ""}>${I.down}</button>
      ${
        paused
          ? `<button class="icon-btn" data-dl="resume" aria-label="${d.state === "failed" ? "Повторить" : "Продолжить"}">${d.state === "failed" ? I.refresh : I.resume}</button>`
          : `<button class="icon-btn" data-dl="pause" aria-label="Пауза">${I.pause}</button>`
      }
      <button class="icon-btn" data-dl="cancel" aria-label="Убрать из очереди">${I.x}</button>
    </div>
  </div>`;
}

// Докачалась, пока окно было открыто.
function doneRow(r, i, list, sig) {
  if (sig) return r.name;
  return `<div class="dl-row done rise" data-appid="${r.appid}">
    <span class="dl-cover">${cover({ name: r.name, cover: r.cover }, r.cover.header)}</span>
    <div class="dl-body">
      <div class="dl-title"><b>${esc(r.name)}</b></div>
      <div class="dl-meta"><span class="status">${dot("ok")}<span>УСТАНОВЛЕНА</span></span></div>
    </div>
    <div class="dl-actions"><button class="btn btn-sm" data-open="${r.appid}">${I.play}Открыть</button></div>
  </div>`;
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
