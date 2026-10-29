"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

/* ---------- состояние ---------- */

const S = {
  view: "library",
  appid: null,
  info: null,
  games: null,
  env: null,
  settings: null,
  filter: "all",
  q: "",
  cloud: {}, // appid → { loading, data, error }
  tasks: {}, // task → { running, stages, progress, ok, error }
  logs: {}, // appid → строки
};

const $ = (sel, root = document) => root.querySelector(sel);
const main = $("#main");

const esc = (s) =>
  String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

/* ---------- иконки ---------- */

const I = {
  play: '<svg viewBox="0 0 24 24"><path d="M7 4.5v15a1 1 0 0 0 1.5.86l12.5-7.5a1 1 0 0 0 0-1.72L8.5 3.64A1 1 0 0 0 7 4.5z"/></svg>',
  trophy: '<svg viewBox="0 0 24 24"><path d="M8 4h8v6a4 4 0 0 1-8 0zM8 6H5a3 3 0 0 0 3 4M16 6h3a3 3 0 0 1-3 4M12 14v4M8 20h8"/></svg>',
  cloud: '<svg viewBox="0 0 24 24"><path d="M7 18a4 4 0 0 1-.6-7.96A6 6 0 0 1 18 9a4.5 4.5 0 0 1-.5 9z"/></svg>',
  cloudOff: '<svg viewBox="0 0 24 24"><path d="M7 18a4 4 0 0 1-.6-7.96M9.5 6.6A6 6 0 0 1 18 9a4.5 4.5 0 0 1 2 8.2M3 3l18 18"/></svg>',
  down: '<svg viewBox="0 0 24 24"><path d="M12 4v14M6 12l6 6 6-6"/></svg>',
  up: '<svg viewBox="0 0 24 24"><path d="M12 20V6M6 12l6-6 6 6"/></svg>',
  check: '<svg viewBox="0 0 24 24"><path d="M5 12.5l4.5 4.5L19 7.5"/></svg>',
  alert: '<svg viewBox="0 0 24 24"><path d="M12 8v5M12 16.5v.5"/><circle cx="12" cy="12" r="9"/></svg>',
  x: '<svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6L6 18"/></svg>',
  conflict: '<svg viewBox="0 0 24 24"><path d="M7 4v10M4 11l3 3 3-3M17 20V10M14 13l3-3 3 3"/></svg>',
  bridge: '<svg viewBox="0 0 24 24"><path d="M3 17h18M5 17V9M19 17V9M5 9c3 4 11 4 14 0M9 17v-4.5M15 17v-4.5"/></svg>',
  game: '<svg viewBox="0 0 24 24"><path d="M7 8h10a4 4 0 0 1 4 4v1a3 3 0 0 1-5.4 1.8L15 14H9l-.6.8A3 3 0 0 1 3 13v-1a4 4 0 0 1 4-4zM8 11v3M6.5 12.5h3"/><circle cx="16" cy="12" r=".6"/></svg>',
  back: '<svg viewBox="0 0 24 24"><path d="M15 5l-7 7 7 7"/></svg>',
  search: '<svg viewBox="0 0 24 24"><circle cx="11" cy="11" r="6.5"/><path d="M16 16l4 4"/></svg>',
  refresh: '<svg viewBox="0 0 24 24"><path d="M20 11a8 8 0 0 0-14.5-4.5L4 8M4 4v4h4M4 13a8 8 0 0 0 14.5 4.5L20 16M20 20v-4h-4"/></svg>',
  wrench: '<svg viewBox="0 0 24 24"><path d="M14.5 6.5a4 4 0 0 0 5 5L13 18a2.1 2.1 0 0 1-3-3zM14.5 6.5L17 4a4 4 0 0 0-5.5 5.5"/></svg>',
  chip: '<svg viewBox="0 0 24 24"><rect x="6" y="6" width="12" height="12" rx="2"/><path d="M10 3v3M14 3v3M10 18v3M14 18v3M3 10h3M3 14h3M18 10h3M18 14h3"/></svg>',
  box: '<svg viewBox="0 0 24 24"><path d="M4 8l8-4 8 4v8l-8 4-8-4zM4 8l8 4 8-4M12 12v8"/></svg>',
  steam: '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="9"/><circle cx="15" cy="10" r="2.5"/><path d="M3.5 14l5 2M13 11.5l-4.5 4.5"/></svg>',
  cpu: '<svg viewBox="0 0 24 24"><path d="M4 12a8 8 0 1 0 16 0 8 8 0 1 0-16 0M12 8v4l3 2"/></svg>',
  metal: '<svg viewBox="0 0 24 24"><path d="M12 3l8 4.5v9L12 21l-8-4.5v-9zM12 12l8-4.5M12 12v9M12 12L4 7.5"/></svg>',
};

/* ---------- тексты ---------- */

const CHECKS = {
  rosetta: { name: "Rosetta 2", missing: "Нет Rosetta 2", icon: I.cpu },
  engine: { name: "Движок", missing: "Движок не установлен", icon: I.chip },
  d3dMetal: { name: "D3DMetal", missing: "Нет D3DMetal", icon: I.metal },
  bottle: { name: "Бутылка", missing: "Нет бутылки", icon: I.box },
  bridge: { name: "Мост soju", missing: "Нет soju", icon: I.bridge },
  steam: { name: "Steam для Mac", missing: "Steam для Mac не запущен", icon: I.steam },
};
const FIX_ORDER = ["engine", "d3dMetal", "bottle", "steam"];
const FIX_LABEL = { steam: "Открыть Steam" };

const STAGES = [
  { id: "cloudPull", name: "Из облака", icon: I.down },
  { id: "bridge", name: "Мост", icon: I.bridge },
  { id: "game", name: "Игра", icon: I.game },
  { id: "cloudPush", name: "В облако", icon: I.up },
];

const CLOUD = {
  synced: { label: "Облако в порядке", cls: "ok", icon: I.cloud },
  pending: { label: "Есть изменения", cls: "info", icon: I.cloud },
  conflict: { label: "Конфликт", cls: "bad", icon: I.conflict },
  never: { label: "Не синхронизировано", cls: "warn", icon: I.cloud },
  unavailable: { label: "Без облака", cls: "", icon: I.cloudOff },
  unknown: { label: "Облако —", cls: "", icon: I.cloud },
};

const BRIDGE = {
  ready: { label: "Мост готов", cls: "ok" },
  notBuilt: { label: "Мост не собран", cls: "warn" },
  notNeeded: { label: "Без Steamworks", cls: "" },
  unknown: { label: "Мост —", cls: "" },
};

const rtf = new Intl.RelativeTimeFormat("ru", { numeric: "auto" });
function ago(ts) {
  if (!ts) return "никогда";
  const s = Math.round(Date.now() / 1000 - ts);
  if (s < 60) return "только что";
  if (s < 3600) return rtf.format(-Math.round(s / 60), "minute");
  if (s < 86400) return rtf.format(-Math.round(s / 3600), "hour");
  if (s < 86400 * 30) return rtf.format(-Math.round(s / 86400), "day");
  return new Date(ts * 1000).toLocaleDateString("ru");
}

function size(b) {
  if (!b) return null;
  if (b >= 1e9) return `${(b / 1e9).toFixed(b >= 1e10 ? 0 : 1)} ГБ`;
  return `${Math.max(1, Math.round(b / 1e6))} МБ`;
}

/* ---------- уведомления ---------- */

function toast(msg, kind = "bad") {
  const el = document.createElement("div");
  el.className = `toast ${kind}`;
  el.innerHTML = `${kind === "ok" ? I.check : I.alert}<span>${esc(msg)}</span>`;
  $("#toasts").append(el);
  setTimeout(() => {
    el.classList.add("out");
    setTimeout(() => el.remove(), 300);
  }, 4200);
}

/* ---------- данные ---------- */

async function loadGames() {
  try {
    S.games = await invoke("games");
  } catch (e) {
    S.games = S.games || [];
    toast(e);
  }
  if (S.view === "library") renderLibraryBody();
  // Карточку не пересобираем целиком: лог и прокрутка остаются на месте.
  if (S.view === "game") $("#chips") ? (renderGameChips(), renderPlay()) : renderGame();
}

async function loadEnv() {
  try {
    S.env = await invoke("environment");
  } catch (e) {
    toast(e);
    return;
  }
  $("#env-dot").hidden = S.env.ready;
  if (S.view === "setup") renderSetup();
  if (S.view === "library") renderBanner();
  if (S.view === "settings") renderSettings();
}

async function loadCloud(appid) {
  S.cloud[appid] = { ...(S.cloud[appid] || {}), loading: true, error: null };
  if (S.view === "game" && S.appid === appid) renderCloud();
  try {
    S.cloud[appid] = { loading: false, data: await invoke("cloud_status", { appid }) };
  } catch (e) {
    S.cloud[appid] = { loading: false, error: String(e) };
  }
  if (S.view === "game" && S.appid === appid) renderCloud();
}

const game = (appid) => (S.games || []).find((g) => g.appid === appid);
const task = (name) => S.tasks[name];
const running = (name) => !!task(name)?.running;

/* ---------- задачи ---------- */

async function start(cmd, args, name) {
  S.tasks[name] = { running: true, stages: {}, progress: {}, ok: null, error: null };
  const appid = args.appid;
  if (appid && cmd === "launch") S.logs[appid] = [];
  refreshTaskViews(name);
  try {
    await invoke(cmd, args);
  } catch (e) {
    S.tasks[name].running = false;
    toast(e);
    refreshTaskViews(name);
  }
}

const play = (appid) => start("launch", { appid }, `launch:${appid}`);
const buildBridge = (appid) => start("build_bridge", { appid }, `bridge:${appid}`);

async function fixAll() {
  const failing = S.env.env.checks.filter((c) => !c.ok && c.fixable).map((c) => c.id);
  let order = FIX_ORDER.filter((id) => failing.includes(id));
  // D3DMetal приходит вместе с движком.
  if (order.includes("engine")) order = order.filter((id) => id !== "d3dMetal");
  S.fixing = true;
  renderSetup();
  for (const id of order) {
    const name = `fix:${id}`;
    await new Promise((resolve) => {
      S.waiters = S.waiters || {};
      S.waiters[name] = resolve;
      start("fix", { check: id }, name).then(() => {
        if (!running(name)) resolve();
      });
    });
    if (task(name)?.ok === false) break;
    await loadEnv();
  }
  S.fixing = false;
  await loadEnv();
  loadGames();
}

function appendLog(appid, line) {
  const log = (S.logs[appid] = S.logs[appid] || []);
  log.push(line);
  if (log.length > 2000) log.splice(0, log.length - 2000);
  if (S.view === "game" && S.appid === appid) {
    const el = $("#log");
    if (!el) return;
    const stick = el.scrollTop + el.clientHeight >= el.scrollHeight - 30;
    el.insertAdjacentHTML("beforeend", logLine(line));
    if (stick) el.scrollTop = el.scrollHeight;
  }
}

function logLine(l) {
  const cls = l.startsWith("==>") ? "mark" : /^(carafe|soju cloud):|error|ошибк/i.test(l) ? "err" : "";
  return `<div class="${cls}">${esc(l)}</div>`;
}

listen("task", ({ payload: { task: name, event } }) => {
  const t = S.tasks[name] || (S.tasks[name] = { running: true, stages: {}, progress: {} });
  const appid = Number(name.split(":")[1]);
  if (event.kind === "stage") t.stages[event.stage] = event.status;
  if (event.kind === "progress") t.progress[event.stage] = event.fraction;
  if (event.kind === "log" && appid) appendLog(appid, event.line);
  refreshTaskViews(name, event.kind);
});

listen("task-end", ({ payload }) => {
  const t = S.tasks[payload.task] || (S.tasks[payload.task] = { stages: {}, progress: {} });
  t.running = false;
  t.ok = payload.ok;
  t.error = payload.error;
  const [kind, id] = payload.task.split(":");
  if (!payload.ok) toast(payload.error);
  else if (kind === "bridge") toast("Мост собран", "ok");
  if (kind === "launch" || kind === "bridge") {
    loadGames();
    if (S.view === "game" && S.appid === Number(id)) loadCloud(Number(id));
  }
  S.waiters?.[payload.task]?.();
  refreshTaskViews(payload.task);
});

function refreshTaskViews(name, kind) {
  if (kind === "log") return;
  const [type, id] = name.split(":");
  if (S.view === "game" && S.appid === Number(id)) {
    renderPlay();
    renderStages();
    renderGameChips();
  }
  if (S.view === "library" && type === "launch") renderLibraryBody();
  if (S.view === "setup" && type === "fix") renderSetup();
}

/* ---------- навигация ---------- */

function go(view, appid = null) {
  S.view = view;
  S.appid = appid;
  document.querySelectorAll(".nav-item").forEach((b) => b.classList.toggle("active", b.dataset.view === view || (view === "game" && b.dataset.view === "library")));
  main.scrollTop = 0;
  if (view === "library") renderLibrary();
  if (view === "game") {
    renderGame();
    if (!S.cloud[appid]) loadCloud(appid);
  }
  if (view === "setup") {
    renderSetup();
    loadEnv();
  }
  if (view === "settings") renderSettings();
}

document.querySelectorAll(".nav-item").forEach((b) => b.addEventListener("click", () => go(b.dataset.view)));

/* ---------- обложки ---------- */

function cover(g, cls = "") {
  return `<img class="${cls}" src="${esc(g.cover.portrait)}" alt="" loading="lazy" data-fallback="${esc(g.cover.header)}" data-name="${esc(g.name)}">`;
}

// Нет вертикальной обложки — горизонтальная, нет и её — плашка с названием.
document.addEventListener(
  "error",
  (e) => {
    const img = e.target;
    if (img.tagName !== "IMG") return;
    if (img.dataset.fallback) {
      img.src = img.dataset.fallback;
      delete img.dataset.fallback;
    } else if (img.dataset.name !== undefined && !img.dataset.dead) {
      img.dataset.dead = "1";
      img.style.opacity = "0";
      img.insertAdjacentHTML("afterend", `<div class="cover-fallback">${esc(img.dataset.name)}</div>`);
    } else {
      img.style.opacity = "0";
    }
  },
  true,
);

/* ---------- библиотека ---------- */

function renderLibrary() {
  main.innerHTML = `
    <section class="view">
      <div id="banner"></div>
      <div id="hero"></div>
      <div class="head">
        <h1>Библиотека</h1>
        <span class="grow"></span>
        <div class="seg" id="seg">
          <button data-f="all">Все</button>
          <button data-f="installed">Установлены</button>
        </div>
        <label class="search">${I.search}<input id="q" placeholder="Поиск" value="${esc(S.q)}" spellcheck="false"></label>
      </div>
      <div class="grid" id="grid"></div>
    </section>`;
  $("#q").addEventListener("input", (e) => {
    S.q = e.target.value;
    renderGrid();
  });
  $("#seg").addEventListener("click", (e) => {
    if (!e.target.dataset.f) return;
    S.filter = e.target.dataset.f;
    renderGrid();
  });
  renderBanner();
  renderLibraryBody();
}

function renderBanner() {
  const el = $("#banner");
  if (!el) return;
  const bad = S.env?.env.checks.find((c) => !c.ok);
  if (!bad) return void (el.innerHTML = "");
  const fixing = S.fixing;
  el.innerHTML = `<div class="banner">${I.alert}<span class="grow">${esc(CHECKS[bad.id].missing)}</span>
    <button class="btn btn-sm" id="banner-fix" ${fixing ? "disabled" : ""}>${bad.id === "steam" && bad.fixable ? "Открыть" : "Починить"}</button></div>`;
  $("#banner-fix").onclick = () => {
    if (bad.id === "steam" && bad.fixable) {
      start("fix", { check: "steam" }, "fix:steam").then(() => setTimeout(loadEnv, 1500));
    } else go("setup");
  };
}

function renderLibraryBody() {
  renderHero();
  renderGrid();
}

function pickHero() {
  const gs = (S.games || []).filter((g) => g.installed);
  return gs.find((g) => running(`launch:${g.appid}`)) || [...gs].sort((a, b) => (b.cloud.lastSync || 0) - (a.cloud.lastSync || 0))[0];
}

function playButton(g) {
  const t = task(`launch:${g.appid}`);
  const cls = "play";
  if (!g.installed) return `<button class="${cls}" disabled style="opacity:.5">Не установлена</button>`;
  if (t?.running) {
    if (t.stages.game?.status === "active") return `<button class="${cls} running" disabled>${I.game}Играет</button>`;
    const st = STAGES.find((s) => t.stages[s.id]?.status === "active");
    return `<button class="${cls} busy" disabled><span class="spin"></span>${esc(st ? st.name : "Запуск")}</button>`;
  }
  return `<button class="${cls}" data-play="${g.appid}">${I.play}Играть</button>`;
}

function renderHero() {
  const el = $("#hero");
  if (!el) return;
  if (!S.games) {
    el.innerHTML = `<div class="hero skeleton"></div>`;
    return;
  }
  const g = pickHero();
  if (!g) return void (el.innerHTML = "");
  const c = CLOUD[g.cloud.state];
  el.innerHTML = `
    <div class="hero">
      <img class="hero-img" src="${esc(g.cover.hero)}" alt="" data-fallback="${esc(g.cover.header)}">
      <div class="hero-body">
        <span class="eyebrow">${running(`launch:${g.appid}`) ? "Сейчас" : "Продолжить"}</span>
        <div class="hero-title">${esc(g.name)}</div>
        <div class="row">
          ${playButton(g)}
          <button class="btn btn-ghost" data-open="${g.appid}">Подробнее</button>
        </div>
        <div class="row">
          <span class="chip ${c.cls}">${c.icon}${g.cloud.lastSync ? esc(ago(g.cloud.lastSync)) : esc(c.label)}</span>
          <span class="chip ${BRIDGE[g.bridge].cls}">${I.trophy}${esc(BRIDGE[g.bridge].label)}</span>
        </div>
      </div>
    </div>`;
}

function renderGrid() {
  const el = $("#grid");
  if (!el) return;
  document.querySelectorAll("#seg button").forEach((b) => b.classList.toggle("on", b.dataset.f === S.filter));
  if (!S.games) {
    el.innerHTML = Array.from({ length: 8 }, () => `<div class="card skeleton"></div>`).join("");
    return;
  }
  const q = S.q.trim().toLowerCase();
  const list = S.games.filter((g) => (S.filter === "all" || g.installed) && (!q || g.name.toLowerCase().includes(q)));
  if (!list.length) {
    el.innerHTML = `<div class="empty" style="grid-column:1/-1">${S.games.length ? "Ничего не нашлось" : "Пока пусто"}</div>`;
    return;
  }
  el.innerHTML = list
    .map((g, i) => {
      const c = CLOUD[g.cloud.state];
      const b = BRIDGE[g.bridge];
      const live = running(`launch:${g.appid}`);
      const badges = g.installed
        ? `${g.bridge === "ready" || g.bridge === "notBuilt" ? `<span class="badge ${b.cls}" title="${esc(b.label)}">${I.trophy}</span>` : ""}
           ${g.cloud.state !== "unknown" ? `<span class="badge ${c.cls}" title="${esc(c.label)}">${c.icon}</span>` : ""}`
        : "";
      return `
      <button class="card ${g.installed ? "" : "off"}" data-open="${g.appid}" style="animation-delay:${Math.min(i, 16) * 25}ms">
        ${cover(g)}
        ${live ? `<span class="chip ok live">${I.game}Играет</span>` : ""}
        <span class="badges">${badges}</span>
        ${g.installed && !live ? `<span class="card-play" data-play="${g.appid}" title="Играть">${I.play}</span>` : ""}
        <span class="card-foot"><span class="card-name">${esc(g.name)}</span>${g.installed ? "" : `<span class="chip chip-quiet">не установлена</span>`}</span>
      </button>`;
    })
    .join("");
}

main.addEventListener("click", (e) => {
  const p = e.target.closest("[data-play]");
  if (p) {
    e.stopPropagation();
    const appid = Number(p.dataset.play);
    play(appid);
    if (S.view === "library") go("game", appid);
    return;
  }
  const o = e.target.closest("[data-open]");
  if (o) go("game", Number(o.dataset.open));
});

/* ---------- карточка игры ---------- */

function renderGame() {
  const g = game(S.appid);
  if (!g) {
    main.innerHTML = `<section class="view"><div class="empty">${S.games ? "Игра не найдена" : ""}</div></section>`;
    return;
  }
  main.innerHTML = `
    <section class="game view" style="padding-top:0">
      <button class="btn btn-sm back" id="back">${I.back}Библиотека</button>
      <div class="game-hero">
        <img class="hero-img" src="${esc(g.cover.hero)}" alt="" data-fallback="${esc(g.cover.header)}">
        <div class="game-head">
          ${cover(g, "game-cover")}
          <div class="grow" style="min-width:240px">
            <div class="game-title">${esc(g.name)}</div>
            <div class="row" id="chips"></div>
          </div>
          <div id="play"></div>
        </div>
      </div>
      <div class="game-body">
        <div class="panel">
          <div class="panel-head"><h2>Запуск</h2></div>
          <div class="stages" id="stages"></div>
          <div class="log" id="log"></div>
        </div>
        <div class="panel">
          <div class="panel-head"><h2>Steam Cloud</h2><span class="grow"></span>
            <button class="btn btn-sm btn-ghost" id="cloud-refresh" title="Обновить">${I.refresh}</button></div>
          <div id="cloud"></div>
        </div>
      </div>
    </section>`;
  $("#back").onclick = () => go("library");
  $("#cloud-refresh").onclick = () => loadCloud(g.appid);
  const log = S.logs[g.appid] || [];
  $("#log").innerHTML = log.length ? log.map(logLine).join("") : "";
  $("#log").scrollTop = 1e9;
  renderGameChips();
  renderPlay();
  renderStages();
  renderCloud();
}

function renderGameChips() {
  const el = $("#chips");
  const g = game(S.appid);
  if (!el || !g) return;
  const b = BRIDGE[g.bridge];
  const building = running(`bridge:${g.appid}`);
  const c = CLOUD[S.cloud[g.appid]?.data?.summary || g.cloud.state];
  const sz = size(g.sizeBytes);
  el.innerHTML = `
    <span class="chip ${b.cls}">${building ? '<span class="spin" style="width:12px;height:12px"></span>' : I.trophy}${building ? "Собираю мост" : esc(b.label)}</span>
    ${g.bridge === "notBuilt" && !building && g.installed ? `<button class="btn btn-sm" id="build">Собрать</button>` : ""}
    <span class="chip ${c.cls}">${c.icon}${esc(c.label)}</span>
    ${sz ? `<span class="chip chip-quiet">${sz}</span>` : ""}`;
  const btn = $("#build");
  if (btn) btn.onclick = () => buildBridge(g.appid);
}

function renderPlay() {
  const el = $("#play");
  const g = game(S.appid);
  if (el && g) el.innerHTML = playButton(g);
}

function renderStages() {
  const el = $("#stages");
  if (!el) return;
  const t = task(`launch:${S.appid}`);
  el.innerHTML = STAGES.map((s) => {
    const st = t?.stages[s.id];
    const cls = st ? st.status : "";
    const ico = st?.status === "done" ? I.check : st?.status === "failed" ? I.x : s.icon;
    return `<div class="stage ${cls}"><span class="stage-ico">${ico}</span><span class="stage-name">${s.name}</span>
      ${st?.status === "failed" ? `<span class="stage-msg">${esc(st.message)}</span>` : ""}</div>`;
  }).join("");
}

function renderCloud() {
  const el = $("#cloud");
  const g = game(S.appid);
  if (!el || !g) return;
  const c = S.cloud[g.appid] || {};
  const last = c.data?.status.lastSync ?? g.cloud.lastSync;
  const meta = `<dl class="meta"><dt>Синхронизация</dt><dd>${esc(ago(last))}</dd></dl>`;
  $("#cloud-refresh").innerHTML = c.loading ? '<span class="spin"></span>' : I.refresh;
  if (c.error) {
    el.innerHTML = `${meta}<div class="banner" style="margin:0">${I.alert}<span>${esc(c.error)}</span></div>`;
    return;
  }
  if (!c.data) {
    el.innerHTML = `${meta}<ul class="files">${[1, 2, 3].map(() => '<li class="file skeleton" style="height:38px"></li>').join("")}</ul>`;
    return;
  }
  const files = c.data.status.files;
  if (!files.length) {
    el.innerHTML = `${meta}<div class="empty">${esc(c.data.status.note || "Файлов нет")}</div>`;
    return;
  }
  const ico = { synced: I.check, download: I.down, upload: I.up };
  const tip = { synced: "Синхронизирован", download: "Скачается", upload: "Выгрузится" };
  el.innerHTML =
    meta +
    `<ul class="files">${files
      .map(
        (f, i) => `<li class="file" style="animation-delay:${i * 30}ms" title="${tip[f.action]}${f.conflict ? " · конфликт, старая версия уйдёт в backups" : ""}">
        <span class="file-ico ${f.action}">${ico[f.action]}</span>
        <span class="file-name">${esc(f.name)}</span>
        ${f.conflict ? `<span class="chip bad">${I.conflict}конфликт</span>` : ""}</li>`,
      )
      .join("")}</ul>`;
}

/* ---------- окружение ---------- */

function renderSetup() {
  if (S.view !== "setup") return;
  const env = S.env?.env;
  const checks = env?.checks || [];
  const ok = checks.filter((c) => c.ok).length;
  const pct = checks.length ? Math.round((ok / checks.length) * 100) : 0;
  const fixable = checks.some((c) => !c.ok && c.fixable);
  const ready = S.env?.ready;
  const working = (id) => running(`fix:${id}`) || (id === "d3dMetal" && running("fix:engine"));
  const title = !S.env ? "Проверяю…" : ready ? "Всё готово" : `Осталось ${checks.length - ok}`;
  main.innerHTML = `
    <section class="view setup">
      <div class="setup-hero">
        <div class="ring" style="--p:${pct}" data-label="${ok}/${checks.length || "–"}"></div>
        <div class="grow"><h1>${title}</h1></div>
        ${ready ? `<button class="play" data-go="library">${I.play}В библиотеку</button>` : ""}
      </div>
      <div class="checks">
        ${
          checks.length
            ? checks
                .map((c, i) => {
                  const m = CHECKS[c.id];
                  const w = working(c.id);
                  const t = task(`fix:${c.id === "d3dMetal" ? "engine" : c.id}`);
                  const prog = t?.running ? Object.values(t.progress)[0] : undefined;
                  const stage = t?.running ? Object.entries(t.stages).find(([, s]) => s.status === "active")?.[0] : null;
                  const stageName = { download: "Скачиваю", unpack: "Распаковываю", bottle: "Создаю бутылку" }[stage];
                  const failed = t && t.ok === false && !c.ok;
                  return `<div class="check ${w ? "work" : c.ok ? "ok" : "no"}" style="animation-delay:${i * 40}ms">
                    <span class="check-ico">${w ? '<span class="spin"></span>' : c.ok ? I.check : m.icon}</span>
                    <div class="check-body">
                      <div class="check-name">${esc(m.name)}</div>
                      ${w && stageName ? `<div class="check-hint">${stageName}${prog !== undefined ? ` · ${Math.round(prog * 100)}%` : ""}</div>` : ""}
                      ${w && prog !== undefined ? `<div class="bar"><i style="width:${prog * 100}%"></i></div>` : ""}
                      ${failed ? `<div class="check-hint" style="color:var(--bad)">${esc(t.error)}</div>` : ""}
                      ${!c.ok && !c.fixable && c.hint && !failed ? `<div class="check-hint" title="${esc(c.hint)}">${esc(c.hint)}</div>` : ""}
                    </div>
                    ${c.ok ? "" : c.fixable ? "" : `<span class="chip warn">вручную</span>`}
                  </div>`;
                })
                .join("")
            : Array.from({ length: 6 }, () => '<div class="check skeleton" style="height:60px"></div>').join("")
        }
      </div>
      ${fixable ? `<button class="play" id="fix" ${S.fixing ? "disabled" : ""}>${S.fixing ? '<span class="spin"></span>Чиню' : `${I.wrench}Починить`}</button>` : ""}
    </section>`;
  const f = $("#fix");
  if (f) f.onclick = fixAll;
  const lib = main.querySelector("[data-go]");
  if (lib) lib.onclick = () => go("library");
}

/* ---------- настройки ---------- */

function renderSettings() {
  if (S.view !== "settings") return;
  const s = S.settings;
  if (!s) {
    main.innerHTML = `<section class="view settings"><h1>Настройки</h1></section>`;
    return;
  }
  const p = s.profile;
  const env = S.env?.env;
  const bottles = [...new Set([p.bottle, ...(env?.bottles || [])])];
  const engines = env?.engines || [];
  const sw = (key, on) => `<label class="switch"><input type="checkbox" data-key="${key}" ${on ? "checked" : ""}><i></i></label>`;
  main.innerHTML = `
    <section class="view settings">
      <div class="head"><h1>Настройки</h1></div>
      <div class="group">
        <h2>Запуск</h2>
        <div class="field"><div class="field-label">Бутылка</div>
          <select class="select" data-key="bottle">${bottles.map((b) => `<option ${b === p.bottle ? "selected" : ""}>${esc(b)}</option>`).join("")}</select></div>
        <div class="field"><div class="field-label">Движок</div>
          <select class="select" data-key="engine">
            ${engines.length ? "" : `<option value="">по умолчанию</option>`}
            ${engines
              .map((e) => `<option value="${esc(e.id)}" ${(p.engine || "") === e.id ? "selected" : ""}>${esc(e.name)}${e.d3dmetal ? "" : " · без D3DMetal"}</option>`)
              .join("")}
          </select></div>
        <div class="field"><div class="field-label">HUD с FPS</div>${sw("hud", p.hud)}</div>
        <div class="field"><div class="field-label">Мост в Steam<div class="field-sub">ачивки и лидерборды</div></div>${sw("bridge", p.bridge)}</div>
        <div class="field"><div class="field-label">Steam Cloud</div>${sw("cloud", p.cloud)}</div>
      </div>
      <div class="group">
        <h2>Ядро</h2>
        <div class="field"><div class="field-label">Checkout carafe<div class="field-sub" id="backend-now">${esc(S.info?.backend || "")}</div></div>
          <input class="text" data-key="checkout" placeholder="~/Projects/carafe" value="${esc(s.checkout || "")}" spellcheck="false"></div>
      </div>
    </section>`;
  main.querySelectorAll("[data-key]").forEach((el) => el.addEventListener("change", () => saveSetting(el)));
}

async function saveSetting(el) {
  const key = el.dataset.key;
  const s = structuredClone(S.settings);
  if (key === "checkout") {
    const v = el.value.trim();
    s.checkout = v || null;
  } else if (el.type === "checkbox") s.profile[key] = el.checked;
  else if (key === "engine") s.profile.engine = el.value || null;
  else s.profile[key] = el.value;
  try {
    const r = await invoke("save_settings", { settings: s });
    S.settings = s;
    if (S.info.backend !== r.backend) {
      S.info.backend = r.backend;
      $("#backend-label").textContent = r.backend;
      S.cloud = {};
      S.games = null;
      loadGames();
    }
    const now = $("#backend-now");
    if (now) now.textContent = r.backend;
    if (key === "engine" || key === "bottle" || key === "checkout") loadEnv();
  } catch (e) {
    toast(e);
  }
}

/* ---------- старт ---------- */

(async function boot() {
  try {
    [S.info, S.settings] = await Promise.all([invoke("info"), invoke("get_settings")]);
  } catch (e) {
    toast(e);
    S.info = { backend: "" };
  }
  $("#backend-label").textContent = S.info.backend;
  const route = S.info.route || "";
  const [r, id] = route.split("/");
  go(r === "game" || r === "play" ? "library" : r === "setup" || r === "settings" ? r : "library");
  await Promise.all([loadGames(), loadEnv()]);
  if (r === "game" || r === "play") {
    go("game", Number(id));
    if (r === "play") play(Number(id));
  } else if (!r && S.env && !S.env.ready && S.env.env.checks.some((c) => !c.ok && c.id !== "steam")) {
    // Первый запуск: сначала окружение.
    go("setup");
  }
})();
