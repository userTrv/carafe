"use strict";

/* Аккаунт Steam: вход по QR (carafe-steam), полная библиотека купленных игр.
   Хуки для app.js: withOwned, loadAccount, renderSteamHead, renderSteamGroup.
   QR приходит готовым SVG из Rust — URL входа в окно не попадает. */

S.account = undefined; // undefined — ещё не знаем, null — не вошёл
S.owned = null;
S.login = null; // { svg, confirmed }

/* ---------- данные ---------- */

// Купленные игры, которых нет среди установленных, — в общий список, неустановленными.
function withOwned(games) {
  if (!games || !S.owned) return games;
  const have = new Set(games.map((g) => g.appid));
  const extra = S.owned
    .filter((o) => !have.has(o.appid))
    .map((o) => ({
      appid: o.appid,
      name: o.name,
      cover: o.cover,
      installed: false,
      windows: o.windows,
      sizeBytes: o.sizeBytes,
      library: null,
      available: true,
      bridge: "unknown",
      cloud: { state: "unknown", lastSync: null },
    }));
  return [...games, ...extra].sort((a, b) => a.name.localeCompare(b.name, "ru"));
}

async function loadAccount() {
  try {
    S.account = await invoke("steam_account");
  } catch (e) {
    S.account = null;
  }
  renderAccount();
  renderSteamHead();
  renderSteamGroup();
  if (S.account) loadOwned();
  else if (S.owned) {
    S.owned = null;
    remerge();
  }
}

async function loadOwned() {
  S.ownedLoading = true;
  renderSteamHead();
  try {
    S.owned = await invoke("owned_games");
  } catch (e) {
    toast(e);
  }
  S.ownedLoading = false;
  remerge();
}

function remerge() {
  if (S.gamesRaw) S.games = withOwned(S.gamesRaw);
  renderSteamHead();
  renderSteamGroup();
  if (S.view === "library") renderLibraryBody();
  if (S.view === "game" && $("#chips")) {
    renderGameChips();
    renderPlay();
  }
}

/* ---------- вход ---------- */

async function steamLogin() {
  S.login = { svg: null, confirmed: false };
  renderLogin();
  try {
    await invoke("steam_login");
  } catch (e) {
    S.login = null;
    renderLogin();
    toast(e);
  }
}

function cancelLogin() {
  invoke("cancel_task", { task: "login" });
  S.login = null;
  renderLogin();
}

async function steamLogout() {
  try {
    await invoke("steam_logout");
    toast("Вышел из Steam", "ok");
  } catch (e) {
    toast(e);
  }
  loadAccount();
}

listen("task", ({ payload: { task: name, event } }) => {
  if (name !== "login" || !S.login || event.kind !== "qr") return;
  S.login.svg = event.svg;
  renderLogin();
});

listen("task-end", ({ payload }) => {
  if (payload.task !== "login") return;
  const was = S.login;
  S.login = null;
  renderLogin();
  // code 1 — отменили сами, ошибку показал app.js.
  if (payload.ok && payload.code === 0 && was) {
    loadAccount().then(() => S.account && toast(`Вошёл: ${S.account.accountName}`, "ok"));
  }
});

/* ---------- отрисовка ---------- */

function renderLogin() {
  let el = $("#login");
  if (!S.login) {
    el?.remove();
    return;
  }
  if (!el) {
    el = document.createElement("div");
    el.id = "login";
    el.className = "login-back";
    el.addEventListener("click", (e) => {
      if (e.target === el || e.target.closest("[data-login-cancel]")) cancelLogin();
    });
    document.body.append(el);
  }
  el.innerHTML = `
    <div class="login-card" role="dialog" aria-label="Вход через Steam">
      <div class="login-title">${I.steam}Вход через Steam</div>
      <div class="qr">${S.login.svg || '<span class="spin"></span>'}</div>
      <div class="login-hint">Steam на телефоне → Steam Guard → значок QR</div>
      <button class="btn btn-ghost" data-login-cancel>Отмена</button>
    </div>`;
}

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && S.login) cancelLogin();
});

function renderAccount() {
  const el = $("#account");
  if (!el) return;
  if (S.account === undefined) return void (el.innerHTML = "");
  if (!S.account) {
    el.innerHTML = `<button class="btn btn-sm steam-btn" id="side-login">${I.steam}Войти через Steam</button>`;
    $("#side-login").onclick = steamLogin;
    return;
  }
  const n = S.account.accountName;
  el.innerHTML = `<button class="account" id="side-account" title="Аккаунт Steam">
    <span class="account-ava">${esc(n.slice(0, 1).toUpperCase())}</span><span class="account-name">${esc(n)}</span></button>`;
  $("#side-account").onclick = () => go("settings");
}

function renderSteamHead() {
  const el = $("#steam-head");
  if (!el) return;
  if (S.account === null) {
    el.innerHTML = `<button class="btn btn-sm steam-btn" id="head-login">${I.steam}Войти через Steam</button>`;
    $("#head-login").onclick = steamLogin;
  } else if (S.ownedLoading) {
    el.innerHTML = `<span class="chip chip-quiet"><span class="spin" style="width:12px;height:12px"></span>Библиотека Steam</span>`;
  } else el.innerHTML = "";
}

function renderSteamGroup() {
  const el = $("#steam-group");
  if (!el) return;
  const a = S.account;
  const owned = S.owned ? `${S.owned.length} игр в библиотеке` : "";
  el.innerHTML = `
    <h2>Steam</h2>
    <div class="field"><div class="field-label">${a ? esc(a.accountName) : "Не вошёл"}
      ${a ? `<div class="field-sub">${esc([a.steamId && `Steam ID ${a.steamId}`, owned].filter(Boolean).join(" · "))}</div>` : ""}</div>
      ${a ? `<button class="btn btn-sm" id="steam-logout">Выйти</button>` : `<button class="btn btn-sm steam-btn" id="steam-login">${I.steam}Войти через Steam</button>`}
    </div>`;
  const out = $("#steam-logout");
  if (out) out.onclick = steamLogout;
  const inn = $("#steam-login");
  if (inn) inn.onclick = steamLogin;
}

// Только для разработки: CARAFE_GUI_ROUTE=login — сразу окно входа.
(async function devLoginRoute() {
  const info = await invoke("info").catch(() => null);
  if (info?.route === "login") steamLogin();
})();
