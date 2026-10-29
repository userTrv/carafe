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
  q: "",
  cloud: {}, // appid → { loading, data, error }
  tasks: {}, // task → { running, stages, progress, ok, error }
  logs: {}, // appid → строки
};

const $ = (sel, root = document) => root.querySelector(sel);
const main = $("#main");

const esc = (s) =>
  String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

/* ---------- тексты ---------- */

const CHECKS = {
  rosetta: { name: "Rosetta 2", missing: "Нет Rosetta 2", icon: I.cpu },
  engine: { name: "Движок", missing: "Движок не установлен", icon: I.wrench },
  d3dMetal: { name: "D3DMetal", missing: "Нет D3DMetal", icon: I.metal },
  bottle: { name: "Бутылка", missing: "Нет бутылки", icon: I.box },
  bridge: { name: "Мост soju", missing: "Нет soju", icon: I.bridge },
  steam: { name: "Steam для Mac", missing: "Steam для Mac не запущен", icon: I.steam },
};
const FIX_ORDER = ["engine", "d3dMetal", "bottle", "steam"];

// Этапы запуска — те, что шлёт бэкенд (`Stage` в backend.rs), по порядку.
const STAGES = [
  { id: "cloudPull", name: "Облако: скачиваю сохранения", short: "Облако" },
  { id: "bridge", name: "Мост Steam", short: "Мост" },
  { id: "game", name: "В игре", short: "Игра" },
  { id: "cloudPush", name: "Облако: выгружаю сохранения", short: "Облако" },
];

const CLOUD = {
  synced: { label: "синхронизировано", dot: "ok" },
  pending: { label: "есть изменения", dot: "warn" },
  conflict: { label: "конфликт", dot: "warn" },
  never: { label: "не синхронизировано", dot: "warn" },
  unavailable: { label: "без облака", dot: "" },
  unknown: { label: "нет данных", dot: "" },
};

const BRIDGE = {
  ready: { label: "мост готов", tag: "МОСТ ГОТОВ", dot: "ok" },
  notBuilt: { label: "мост не собран", tag: "МОСТ НЕ СОБРАН", dot: "warn" },
  notNeeded: { label: "без Steamworks", tag: null, dot: "" },
  unknown: { label: "нет данных", tag: null, dot: "" },
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
  if (b >= 1e9) return `${(b / 1e9).toFixed(b >= 1e10 ? 0 : 1)} GB`;
  return `${Math.max(1, Math.round(b / 1e6))} MB`;
}

const dot = (cls) => `<span class="dot ${cls || ""}" aria-hidden="true"></span>`;

/* ---------- уведомления ---------- */

function toast(msg, kind = "bad") {
  const el = document.createElement("div");
  el.className = `toast ${kind}`;
  el.innerHTML = `${kind === "ok" ? I.check : I.alert}<span>${esc(msg)}</span>`;
  $("#toasts").append(el);
  setTimeout(() => {
    el.classList.add("out");
    setTimeout(() => el.remove(), 500);
  }, 4200);
}

/* ---------- данные ---------- */

async function loadGames() {
  try {
    S.gamesRaw = await invoke("games");
    // Купленные, но не установленные — из аккаунта Steam (steam.js).
    S.games = withOwned(S.gamesRaw);
  } catch (e) {
    S.games = S.games || [];
    toast(e);
  }
  if (S.view === "library") renderLibraryBody();
  // Карточку не пересобираем целиком: лог и прокрутка остаются на месте.
  if (S.view === "game") $("#actions") ? refreshGame() : renderGame();
}

async function loadEnv() {
  try {
    S.env = await invoke("environment");
  } catch (e) {
    toast(e);
    return;
  }
  const bad = S.env.env.checks.filter((c) => !c.ok).length;
  const cnt = $("#env-count");
  cnt.hidden = !bad;
  cnt.textContent = bad;
  $('[data-view="setup"]').setAttribute("aria-label", bad ? `Окружение, проблем: ${bad}` : "Окружение");
  if (S.view === "setup") renderSetup();
  if (S.view === "library") renderNotice();
  if (S.view === "settings") renderSettings();
  if (S.view === "game") refreshGame();
}

async function loadCloud(appid) {
  S.cloud[appid] = { ...(S.cloud[appid] || {}), loading: true, error: null };
  if (S.view === "game" && S.appid === appid) renderCloud();
  try {
    S.cloud[appid] = { loading: false, data: await invoke("cloud_status", { appid }) };
  } catch (e) {
    S.cloud[appid] = { loading: false, error: String(e) };
  }
  if (S.view === "game" && S.appid === appid) {
    renderCloud();
    renderLabel();
  }
}

const game = (appid) => (S.games || []).find((g) => g.appid === appid);
const task = (name) => S.tasks[name];
const running = (name) => !!task(name)?.running;

// Движок из профиля; без выбора — первый найденный.
function engine() {
  const list = S.env?.env.engines || [];
  const id = S.settings?.profile.engine;
  return list.find((e) => e.id === id) || list[0] || null;
}

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
    if (!el) return renderLog();
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
    renderActions();
    if (type === "bridge") renderLabel();
    if (type === "launch" && !$("#log")) renderLog();
  }
  if (S.view === "library" && type === "launch") renderLibraryBody();
  if (S.view === "setup" && type === "fix") renderSetup();
}

/* ---------- навигация ---------- */

function go(view, appid = null) {
  if (typeof closeMenu === "function") closeMenu();
  S.view = view;
  S.appid = appid;
  document.querySelectorAll(".tab[data-view]").forEach((b) => {
    const on = b.dataset.view === view || (view === "game" && b.dataset.view === "library");
    if (on) b.setAttribute("aria-current", "page");
    else b.removeAttribute("aria-current");
  });
  main.scrollTop = 0;
  if (view === "library") renderLibrary();
  if (view === "game") {
    renderGame();
    if (!S.cloud[appid] && game(appid)?.installed) loadCloud(appid);
  }
  if (view === "setup") {
    renderSetup();
    loadEnv();
  }
  if (view === "settings") renderSettings();
  if (view === "downloads") {
    renderDownloads();
    pollDownloads();
  }
}

document.querySelectorAll(".tab[data-view]").forEach((b) => b.addEventListener("click", () => go(b.dataset.view)));

/* ---------- поиск в панели ---------- */

const q = $("#q");
const searchBtn = $("#search-btn");
searchBtn.innerHTML = I.search;

function openSearch() {
  q.hidden = false;
  searchBtn.hidden = true;
  q.focus();
}

function closeSearch() {
  q.value = "";
  q.hidden = true;
  searchBtn.hidden = false;
  if (S.q) {
    S.q = "";
    if (S.view === "library") renderLibraryBody();
  }
}

searchBtn.addEventListener("click", openSearch);
q.addEventListener("input", () => {
  S.q = q.value;
  if (S.view !== "library") go("library");
  else renderLibraryBody();
});
q.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    closeSearch();
    searchBtn.focus();
  }
});
q.addEventListener("blur", () => {
  if (!q.value) closeSearch();
});
document.addEventListener("keydown", (e) => {
  if ((e.metaKey || e.ctrlKey) && e.key === "f") {
    e.preventDefault();
    openSearch();
  }
});

/* ---------- обложки ---------- */

function cover(g, src = g.cover.portrait) {
  return `<img src="${esc(src)}" alt="" loading="lazy" data-fallback="${esc(g.cover.header)}" data-name="${esc(g.name)}">`;
}

// Нет вертикальной обложки — горизонтальная, нет и её — плоская плашка с названием.
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
      const cls = img.classList.contains("art") ? "art-name" : "cover-fallback";
      img.insertAdjacentHTML("afterend", `<div class="${cls}" aria-hidden="true">${esc(img.dataset.name)}</div>`);
    } else {
      img.style.opacity = "0";
    }
  },
  true,
);

/* ---------- библиотека ---------- */

function renderLibrary() {
  main.innerHTML = `
    <div class="view">
      <div id="notice" hidden></div>
      <div id="lib-body" class="section" style="gap:48px"></div>
    </div>`;
  renderNotice();
  renderLibraryBody();
}

function renderNotice() {
  const el = $("#notice");
  if (!el) return;
  const bad = S.env?.env.checks.find((c) => !c.ok);
  el.hidden = !bad;
  if (!bad) return void (el.innerHTML = "");
  const open = bad.id === "steam" && bad.fixable;
  el.innerHTML = `<div class="notice rise">${I.alert}<span class="grow">${esc(CHECKS[bad.id].missing)}</span>
    <button class="btn btn-sm" id="notice-fix" ${S.fixing ? "disabled" : ""}>${open ? "Открыть Steam" : "Починить"}</button></div>`;
  $("#notice-fix").onclick = () => {
    if (open) start("fix", { check: "steam" }, "fix:steam").then(() => setTimeout(loadEnv, 1500));
    else go("setup");
  };
}

function pickHero() {
  const gs = (S.games || []).filter((g) => g.installed);
  return gs.find((g) => running(`launch:${g.appid}`)) || [...gs].sort((a, b) => (b.cloud.lastSync || 0) - (a.cloud.lastSync || 0))[0];
}

// Слово рядом с точкой: статус не только цветом.
function gameStatus(g) {
  if (running(`launch:${g.appid}`)) return ["ok", "ИДЁТ"];
  if (g.available === false) return ["", "ДИСК ОТКЛЮЧЁН"];
  if (running(`move:${g.appid}`)) return ["warn", "ПЕРЕНОС"];
  if (g.cloud.state === "conflict") return ["warn", "КОНФЛИКТ ОБЛАКА"];
  if (g.bridge === "notBuilt") return ["warn", "МОСТ НЕ СОБРАН"];
  if (g.bridge === "ready") return ["ok", "МОСТ ГОТОВ"];
  return ["ok", "ГОТОВА"];
}

function renderLibraryBody() {
  const el = $("#lib-body");
  if (!el) return;
  if (!S.games) {
    el.innerHTML = `<div class="hero skel"></div>
      <section class="section"><div class="skel-line" style="width:160px;height:32px"></div>${skelGrid(6)}</section>`;
    return;
  }
  const q = S.q.trim().toLowerCase();
  const match = (g) => !q || g.name.toLowerCase().includes(q);
  const installed = S.games.filter((g) => g.installed && match(g));
  const owned = S.games.filter((g) => !g.installed && match(g));
  if (q && !installed.length && !owned.length) {
    el.innerHTML = `<div class="empty rise">${I.search}<h3>Ничего не нашлось</h3>
      <button class="btn btn-sm" id="clear-q">Сбросить поиск</button></div>`;
    $("#clear-q").onclick = closeSearch;
    return;
  }
  const hero = q ? null : pickHero();
  el.innerHTML = `
    ${hero ? heroHtml(hero) : ""}
    <section class="section" aria-labelledby="h-installed">
      <div class="section-head"><h2 id="h-installed">Установлено</h2><span class="caption">${installed.length}</span></div>
      ${
        installed.length
          ? `<div class="grid">${installed.map(installedCard).join("")}</div>`
          : `<div class="empty">${I.game}<h3>Пока ничего не установлено</h3></div>`
      }
    </section>
    ${ownedSection(owned, q)}`;
}

function heroHtml(g) {
  const live = running(`launch:${g.appid}`);
  const e = engine();
  const tags = [
    e?.d3dmetal ? "D3DMETAL" : null,
    BRIDGE[g.bridge].tag,
    g.cloud.lastSync ? `ОБЛАКО · ${ago(g.cloud.lastSync)}` : null,
    size(g.sizeBytes),
  ].filter(Boolean);
  return `
    <section class="hero rise" aria-label="${live ? "Сейчас играет" : "Продолжить"}">
      <img class="art" src="${esc(g.cover.hero)}" alt="" data-fallback="${esc(g.cover.header)}" data-name="${esc(g.name)}">
      <div class="scrim"></div>
      <div class="hero-body">
        <span class="eyebrow">${live ? "СЕЙЧАС ИГРАЕТ" : "ПРОДОЛЖИТЬ"}</span>
        <h1 class="hero-title display">${esc(g.name)}</h1>
        ${tags.length ? `<div class="tags">${tags.map((t) => `<span class="tag">${esc(t)}</span>`).join("")}</div>` : ""}
        <div class="actions">
          ${live ? `<button class="btn btn-ghost" data-open="${g.appid}">${I.game}Идёт</button>` : `<button class="btn btn-primary" data-play="${g.appid}">${I.play}Играть</button>`}
          <button class="btn btn-ghost" data-open="${g.appid}">Об игре</button>
        </div>
      </div>
    </section>`;
}

function installedCard(g, i) {
  const [cls, word] = gameStatus(g);
  const can = g.available !== false && !running(`launch:${g.appid}`);
  return `
    <div class="card-wrap rise" style="animation-delay:${Math.min(i, 12) * 30}ms">
      <button class="card" data-open="${g.appid}" aria-label="${esc(g.name)}">
        <span class="cover">${cover(g)}</span>
        <span class="card-meta">
          <span class="card-name">${esc(g.name)}</span>
          <span class="status">${dot(cls)}<span>${word}</span></span>
        </span>
      </button>
      ${can ? `<button class="cover-play" data-play="${g.appid}" aria-label="Играть: ${esc(g.name)}">${I.play}</button>` : ""}
    </div>`;
}

function ownedSection(list, q) {
  const head = `<div class="section-head"><h2 id="h-owned">В библиотеке Steam</h2><span class="caption">не установлены</span></div>`;
  if (S.account === null && !list.length) {
    if (q) return "";
    return `<section class="section" aria-labelledby="h-owned">${head}
      <div class="empty">${I.steam}<h3>Вся библиотека Steam</h3>
        <button class="btn btn-primary" data-login>${I.steam}Войти через Steam</button></div></section>`;
  }
  if (S.ownedLoading && !list.length) return `<section class="section" aria-labelledby="h-owned">${head}${skelGrid(6)}</section>`;
  if (!list.length) return "";
  return `<section class="section" aria-labelledby="h-owned">${head}
    <div class="grid">${list.map(ownedCard).join("")}</div></section>`;
}

function ownedCard(g, i) {
  const d = queued(g.appid);
  const pct = d && d.totalBytes ? Math.floor((d.doneBytes / d.totalBytes) * 100) : 0;
  const badge = d
    ? d.state === "active"
      ? `${pct}%`
      : { queued: "В ОЧЕРЕДИ", paused: "ПАУЗА", failed: "ОШИБКА" }[d.state]
    : g.windows === false
      ? "НЕТ WINDOWS"
      : null;
  return `
    <button class="card owned rise ${g.windows === false ? "faded" : ""}" data-open="${g.appid}" style="animation-delay:${Math.min(i, 12) * 30}ms">
      <span class="cover">${cover(g)}${badge ? `<span class="tag" data-dl-tag="${g.appid}">${badge}</span>` : ""}</span>
      <span class="card-meta">
        <span class="card-row"><span class="card-name">${esc(g.name)}</span><span class="card-size">${esc(size(g.sizeBytes) || "")}</span></span>
        ${d ? `<span class="bar" data-dl-bar="${g.appid}" role="progressbar" aria-label="Загрузка" aria-valuenow="${pct}" aria-valuemin="0" aria-valuemax="100"><i style="transform:scaleX(${pct / 100})"></i></span>` : ""}
      </span>
    </button>`;
}

function skelGrid(n) {
  return `<div class="grid" aria-hidden="true">${Array.from(
    { length: n },
    () => `<div class="card"><div class="cover skel"></div><div class="skel-line" style="width:70%"></div></div>`,
  ).join("")}</div>`;
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
  if (e.target.closest("[data-login]")) return void steamLogin();
  const o = e.target.closest("[data-open]");
  if (o) go("game", Number(o.dataset.open));
});

/* ---------- карточка игры ---------- */

function renderGame() {
  const g = game(S.appid);
  if (!g) {
    main.innerHTML = S.games
      ? `<div class="view"><div class="empty">${I.game}<h3>Игра не найдена</h3><button class="btn btn-sm" data-go-lib>В библиотеку</button></div></div>`
      : `<div class="game-hero skel"></div>`;
    const b = main.querySelector("[data-go-lib]");
    if (b) b.onclick = () => go("library");
    return;
  }
  main.innerHTML = `
    <header class="game-hero">
      <img class="art" src="${esc(g.cover.hero)}" alt="" data-fallback="${esc(g.cover.header)}" data-name="${esc(g.name)}">
      <div class="scrim"></div>
      <button class="btn btn-sm back" id="back">${I.back}Библиотека</button>
      <div class="game-head">
        <h1 class="game-title display">${esc(g.name)}</h1>
        <div class="actions" id="actions"></div>
        <div id="launch-err"></div>
      </div>
    </header>
    <div class="game-body">
      <section class="panel wide rise" aria-labelledby="h-label">
        <div class="panel-head"><h2 id="h-label">Этикетка</h2><span class="caption">APPID ${g.appid}</span></div>
        <dl class="label" id="label"></dl>
      </section>
      <section class="panel rise" aria-labelledby="h-profile" id="profile"></section>
      ${
        g.installed
          ? `<section class="panel wide rise" aria-labelledby="h-cloud">
              <div class="panel-head"><h2 id="h-cloud">Steam Cloud</h2>
                <button class="icon-btn" id="cloud-refresh" aria-label="Обновить статус облака">${I.refresh}</button></div>
              <div id="cloud" class="section" style="gap:16px"></div>
            </section>`
          : ""
      }
      <div id="log-slot" class="panel full" hidden></div>
    </div>`;
  $("#back").onclick = () => go("library");
  const r = $("#cloud-refresh");
  if (r) r.onclick = () => loadCloud(g.appid);
  refreshGame();
  renderProfile();
  renderCloud();
  renderLog();
}

// Всё, что зависит от игры и задач, без пересборки лога и облака.
function refreshGame() {
  if (S.view !== "game" || !$("#actions")) return;
  renderActions();
  renderLabel();
}

function renderActions() {
  const el = $("#actions");
  const g = game(S.appid);
  if (!el || !g) return;
  const t = task(`launch:${g.appid}`);
  let main_;
  if (!g.installed) {
    main_ = g.windows === false ? `<button class="btn btn-primary launch" disabled>Нет Windows-сборки</button>` : installButton(g);
  } else if (g.available === false) {
    main_ = `<button class="btn btn-primary launch" disabled>${I.disk}Диск отключён</button>`;
  } else if (t?.running) {
    // Полоска уже на месте — только обновить, иначе каждое событие перезапускает её появление.
    const cur = el.querySelector(".stages-btn");
    if (cur) return void updateStages(cur, t);
    main_ = stagesBlock(t);
  } else {
    main_ = `<button class="btn btn-primary launch" data-play="${g.appid}">${I.play}Играть</button>`;
  }
  const building = running(`bridge:${g.appid}`);
  const bridge =
    g.installed && g.bridge === "notBuilt"
      ? `<button class="btn btn-ghost" id="build" ${building ? "disabled" : ""}>${I.bridge}${building ? "Собираю мост" : "Собрать мост"}</button>`
      : "";
  el.innerHTML = main_ + bridge + libraryActions(g);
  wireLibraryActions(g);
  const b = $("#build");
  if (b) b.onclick = () => buildBridge(g.appid);
  const err = $("#launch-err");
  if (err) {
    const fail = t && !t.running && t.ok === false && t.error;
    err.innerHTML = fail ? `<div class="notice" style="max-width:560px">${I.alert}<span class="grow">${esc(t.error)}</span></div>` : "";
  }
}

// «Играть» превращается в полоску этапов: каждый заполняется по порядку.
function stagesState(t) {
  const st = (s) => t.stages[s.id]?.status;
  const failed = STAGES.find((s) => st(s) === "failed");
  let cur = STAGES.findIndex((s) => st(s) === "active");
  if (cur < 0) cur = STAGES.reduce((n, s, i) => (st(s) ? i : n), -1);
  const label = failed ? `${failed.short}: ${t.stages[failed.id].message || "ошибка"}` : cur >= 0 ? STAGES[cur].name : "Запуск";
  return { label, num: Math.max(1, cur + 1), cls: STAGES.map((s) => `seg ${st(s) || ""}`) };
}

function stagesBlock(t) {
  const { label, num, cls } = stagesState(t);
  return `<div class="stages-btn" role="status" aria-live="polite">
    <span class="stages-top"><span class="stage-label">${esc(label)}</span><span class="mono stage-num" aria-label="этап ${num} из 4">${num}/4</span></span>
    <span class="segs" aria-hidden="true">${cls.map((c) => `<span class="${c}"><i></i></span>`).join("")}</span></div>`;
}

function updateStages(el, t) {
  const { label, num, cls } = stagesState(t);
  const l = el.querySelector(".stage-label");
  if (l.textContent !== label) l.textContent = label;
  const n = el.querySelector(".stage-num");
  n.textContent = `${num}/4`;
  n.setAttribute("aria-label", `этап ${num} из 4`);
  // Класс меняем только при смене этапа: анимация набора не начинается заново.
  el.querySelectorAll(".seg").forEach((s, i) => s.className !== cls[i] && (s.className = cls[i]));
}

function renderLabel() {
  const el = $("#label");
  const g = game(S.appid);
  if (!el || !g) return;
  const e = engine();
  const p = S.settings?.profile;
  const rows = [];
  if (g.installed) {
    const checks = S.env?.env.checks || [];
    const okCheck = (id) => checks.find((c) => c.id === id)?.ok;
    const c = CLOUD[S.cloud[g.appid]?.data?.summary || g.cloud.state];
    const last = S.cloud[g.appid]?.data?.status.lastSync ?? g.cloud.lastSync;
    const building = running(`bridge:${g.appid}`);
    rows.push(
      ["Движок", e ? e.name : S.env ? "не установлен" : "…", e ? "ok" : "warn"],
      ["Графика", e ? (e.d3dmetal ? "D3DMetal" : "без D3DMetal") : "…", e?.d3dmetal && okCheck("d3dMetal") !== false ? "ok" : "warn"],
      ["Steam", building ? "собираю мост" : BRIDGE[g.bridge].label, building ? "warn" : BRIDGE[g.bridge].dot],
      ["Облако", `${c.label}${last ? ` · ${ago(last)}` : ""}`, c.dot],
      ["Папка", g.library ? `${libName(g.library)}${g.available === false ? " · отключена" : ""}` : "—", g.available === false ? "warn" : ""],
      ["Бутылка", p ? p.bottle : "…", ""],
    );
  } else {
    rows.push(["Windows", g.windows === false ? "нет сборки" : "есть", g.windows === false ? "warn" : "ok"], ["Папка", "не установлена", ""]);
  }
  if (g.sizeBytes) rows.push(["Размер", size(g.sizeBytes), ""]);
  el.innerHTML = rows.map(([k, v, d]) => `<dt>${k}</dt><dd>${dot(d)}${esc(v)}</dd>`).join("");
}

const PROFILE = [
  ["bridge", "Мост в Steam"],
  ["cloud", "Steam Cloud"],
  ["hud", "Счётчик FPS"],
];

function switchHtml(key, label, on) {
  return `<button class="switch" role="switch" aria-checked="${on ? "true" : "false"}" aria-label="${esc(label)}" data-switch="${key}"><i></i></button>`;
}

function renderProfile() {
  const el = $("#profile");
  if (!el) return;
  const p = S.settings?.profile;
  el.innerHTML = `
    <div class="panel-head"><h2 id="h-profile">Профиль</h2><span class="caption">ДЛЯ ВСЕХ ИГР</span></div>
    <div class="tray">
      ${PROFILE.map(([k, name]) => `<label class="toggle"><span>${name}</span>${switchHtml(k, name, p?.[k])}</label>`).join("")}
    </div>`;
  wireSwitches(el);
}

function wireSwitches(root) {
  root.querySelectorAll("[data-switch]").forEach((b) => {
    b.onclick = (e) => {
      e.preventDefault();
      const on = b.getAttribute("aria-checked") !== "true";
      b.setAttribute("aria-checked", on);
      saveSettings((s) => (s.profile[b.dataset.switch] = on));
    };
  });
}

function renderCloud() {
  const el = $("#cloud");
  const g = game(S.appid);
  if (!el || !g) return;
  const c = S.cloud[g.appid] || {};
  const last = c.data?.status.lastSync ?? g.cloud.lastSync;
  const btn = $("#cloud-refresh");
  if (btn) btn.disabled = !!c.loading;
  const meta = `<span class="caption">СИНХРОНИЗАЦИЯ · ${esc(ago(last))}</span>`;
  if (c.error) {
    el.innerHTML = `${meta}<div class="notice">${I.alert}<span class="grow">${esc(c.error)}</span>
      <button class="btn btn-sm" id="cloud-retry">Повторить</button></div>`;
    $("#cloud-retry").onclick = () => loadCloud(g.appid);
    return;
  }
  if (!c.data) {
    el.innerHTML = `${meta}<ul class="files" aria-hidden="true">${[1, 2, 3].map(() => '<li class="file skel"></li>').join("")}</ul>`;
    return;
  }
  const files = c.data.status.files;
  if (!files.length) {
    el.innerHTML = `${meta}<div class="row"><span class="row-label" style="color:var(--text-2)">${esc(c.data.status.note || "Файлов нет")}</span></div>`;
    return;
  }
  const ico = { synced: I.check, download: I.cloudDown, upload: I.cloudUp };
  const word = { synced: "=", download: "↓", upload: "↑" };
  const tip = { synced: "Синхронизирован", download: "Скачается", upload: "Выгрузится" };
  el.innerHTML =
    meta +
    `<ul class="files">${files
      .map(
        (f) => `<li class="file ${f.action}" title="${tip[f.action]}">${ico[f.action]}
          <span class="sr">${tip[f.action]}</span>
          <span class="file-name">${esc(f.name)}</span>
          ${f.conflict ? `<span class="tag plain">КОНФЛИКТ</span>` : `<span class="caption" aria-hidden="true">${word[f.action]}</span>`}</li>`,
      )
      .join("")}</ul>`;
}

function renderLog() {
  const slot = $("#log-slot");
  if (!slot) return;
  const log = S.logs[S.appid] || [];
  if (!log.length && !running(`launch:${S.appid}`)) {
    slot.hidden = true;
    return;
  }
  slot.hidden = false;
  slot.innerHTML = `<div class="panel-head"><h2>Лог запуска</h2></div><div class="log" id="log" tabindex="0" aria-label="Лог запуска">${log.map(logLine).join("")}</div>`;
  $("#log").scrollTop = 1e9;
}

/* ---------- окружение ---------- */

function renderSetup() {
  if (S.view !== "setup") return;
  const env = S.env?.env;
  const checks = env?.checks || [];
  const ok = checks.filter((c) => c.ok).length;
  const fixable = checks.some((c) => !c.ok && c.fixable);
  const ready = S.env?.ready;
  const working = (id) => running(`fix:${id}`) || (id === "d3dMetal" && running("fix:engine"));
  const title = !S.env ? "Проверяю" : ready ? "Всё готово" : `Осталось ${checks.length - ok}`;
  const segs = checks.map((c) => `<span class="seg ${c.ok ? "done" : working(c.id) ? "active" : ""}"><i></i></span>`).join("");
  main.innerHTML = `
    <div class="view setup">
      <div class="setup-head">
        <div class="grow">
          <span class="caption">ОКРУЖЕНИЕ · ${ok}/${checks.length || "–"}</span>
          <h1 class="page-title display">${title}</h1>
          ${checks.length ? `<div class="segs" style="--n:${checks.length}" aria-hidden="true">${segs}</div>` : ""}
        </div>
        ${
          fixable
            ? `<button class="btn btn-primary" id="fix" ${S.fixing ? "disabled" : ""}>${I.wrench}${S.fixing ? "Чиню" : "Починить"}</button>`
            : ready
              ? `<button class="btn btn-primary" data-go="library">${I.play}В библиотеку</button>`
              : ""
        }
      </div>
      <div class="checks">
        ${
          checks.length
            ? checks.map((c, i) => checkRow(c, i, working)).join("")
            : Array.from({ length: 6 }, () => '<div class="check skel"></div>').join("")
        }
      </div>
    </div>`;
  const f = $("#fix");
  if (f) f.onclick = fixAll;
  const lib = main.querySelector("[data-go]");
  if (lib) lib.onclick = () => go("library");
}

function checkRow(c, i, working) {
  const m = CHECKS[c.id];
  const w = working(c.id);
  const t = task(`fix:${c.id === "d3dMetal" ? "engine" : c.id}`);
  const prog = t?.running ? Object.values(t.progress)[0] : undefined;
  const stage = t?.running ? Object.entries(t.stages).find(([, s]) => s.status === "active")?.[0] : null;
  const stageName = { download: "СКАЧИВАЮ", unpack: "РАСПАКОВЫВАЮ", bottle: "СОЗДАЮ БУТЫЛКУ" }[stage] || "ЧИНЮ";
  const failed = t && t.ok === false && !c.ok;
  const status = c.ok
    ? ["ok", "ОК"]
    : w
      ? ["warn", `${stageName}${prog !== undefined ? ` ${Math.round(prog * 100)}%` : ""}`]
      : c.fixable
        ? ["warn", "НЕТ"]
        : ["warn", "ВРУЧНУЮ"];
  const hint = failed ? t.error : !c.ok && !c.fixable ? c.hint : null;
  return `<div class="check rise ${w ? "work" : c.ok ? "ok" : "no"}" style="animation-delay:${i * 40}ms">
    <span class="check-ico">${c.ok ? I.check : m.icon}</span>
    <div class="check-body">
      <div class="check-name">${esc(m.name)}</div>
      ${hint ? `<div class="check-hint">${esc(hint)}</div>` : ""}
      ${w && prog !== undefined ? `<div class="bar"><i style="transform:scaleX(${prog})"></i></div>` : ""}
    </div>
    <span class="status">${dot(status[0])}<span>${status[1]}</span></span>
  </div>`;
}

/* ---------- настройки ---------- */

function renderSettings() {
  if (S.view !== "settings") return;
  const s = S.settings;
  if (!s) {
    main.innerHTML = `<div class="view settings"><h1 class="page-title display">Настройки</h1><div class="group skel" style="height:240px"></div></div>`;
    return;
  }
  const p = s.profile;
  const env = S.env?.env;
  const bottles = [...new Set([p.bottle, ...(env?.bottles || [])])];
  const engines = env?.engines || [];
  main.innerHTML = `
    <div class="view settings">
      <h1 class="page-title display">Настройки</h1>
      <section class="group" aria-labelledby="h-run">
        <h2 id="h-run">Запуск</h2>
        <div class="rows">
          <div class="row"><label class="row-label" for="set-bottle">Бутылка</label>
            <select class="select" id="set-bottle" data-key="bottle">${bottles.map((b) => `<option ${b === p.bottle ? "selected" : ""}>${esc(b)}</option>`).join("")}</select></div>
          <div class="row"><label class="row-label" for="set-engine">Движок</label>
            <select class="select" id="set-engine" data-key="engine">
              ${engines.length ? "" : `<option value="">по умолчанию</option>`}
              ${engines
                .map((e) => `<option value="${esc(e.id)}" ${(p.engine || "") === e.id ? "selected" : ""}>${esc(e.name)}${e.d3dmetal ? "" : " · без D3DMetal"}</option>`)
                .join("")}
            </select></div>
          ${PROFILE.map(([k, name]) => `<label class="row toggle"><span class="row-label">${name}</span>${switchHtml(k, name, p[k])}</label>`).join("")}
        </div>
      </section>
      <section class="group" id="steam-group" aria-label="Steam"></section>
      <section class="group" id="libs" aria-label="Библиотеки"></section>
      <section class="group" aria-labelledby="h-core">
        <h2 id="h-core">Ядро</h2>
        <div class="rows">
          <div class="row"><label class="row-label" for="set-checkout">Checkout carafe<span class="row-sub" id="backend-now">${esc(S.info?.backend || "")}</span></label>
            <input class="text" id="set-checkout" data-key="checkout" placeholder="~/Projects/carafe" value="${esc(s.checkout || "")}" spellcheck="false"></div>
        </div>
      </section>
    </div>`;
  main.querySelectorAll("[data-key]").forEach((el) => el.addEventListener("change", () => saveSetting(el)));
  wireSwitches(main);
  renderLibs();
  loadLibs();
  renderSteamGroup();
}

function saveSetting(el) {
  const key = el.dataset.key;
  saveSettings((s) => {
    if (key === "checkout") s.checkout = el.value.trim() || null;
    else if (key === "engine") s.profile.engine = el.value || null;
    else s.profile[key] = el.value;
  }, key);
}

async function saveSettings(change, key) {
  const s = structuredClone(S.settings);
  change(s);
  try {
    const r = await invoke("save_settings", { settings: s });
    S.settings = s;
    if (S.info.backend !== r.backend) {
      S.info.backend = r.backend;
      S.cloud = {};
      S.games = null;
      loadGames();
    }
    const now = $("#backend-now");
    if (now) now.textContent = r.backend;
    if (key === "engine" || key === "bottle" || key === "checkout") loadEnv();
    if (S.view === "game") renderLabel();
  } catch (e) {
    toast(e);
    if (S.view === "settings") renderSettings();
    if (S.view === "game") renderProfile();
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
  const route = S.info.route || "";
  const [r, id] = route.split("/");
  go(r === "game" || r === "play" ? "library" : r === "setup" || r === "settings" || r === "downloads" ? r : "library");
  await Promise.all([loadGames(), loadEnv(), loadAccount()]);
  if (r === "game" || r === "play") {
    go("game", Number(id));
    if (r === "play") play(Number(id));
  } else if (!r && S.env && !S.env.ready && S.env.env.checks.some((c) => !c.ok && c.id !== "steam")) {
    // Первый запуск: сначала окружение.
    go("setup");
  }
})();
