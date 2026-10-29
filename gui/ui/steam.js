"use strict";

/* Аккаунт Steam: вход по QR (carafe-steam), полная библиотека купленных игр.
   Хуки для app.js: withOwned, loadAccount, steamLogin, renderSteamGroup.
   QR приходит готовым SVG из Rust — URL входа в окно не попадает. */

S.account = undefined; // undefined — ещё не знаем, null — не вошёл
S.owned = null;
S.login = null; // { svg }

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

// Steam ID приходит маской («••••4242»); в панели — как в макете: 7656…4242.
function maskedId(a) {
  const tail = String(a?.steamId || "").replace(/\D/g, "").slice(-4);
  return tail ? `7656…${tail}` : "";
}

async function loadAccount() {
  try {
    S.account = await invoke("steam_account");
  } catch (e) {
    S.account = null;
  }
  renderAccount();
  renderSteamGroup();
  if (S.account) loadOwned();
  else {
    if (S.owned) S.owned = null;
    remerge();
  }
}

async function loadOwned() {
  S.ownedLoading = true;
  if (S.view === "library") renderLibraryBody();
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
  renderSteamGroup();
  if (S.view === "library") renderLibraryBody();
  if (S.view === "game") refreshGame();
}

/* ---------- вход ---------- */

async function steamLogin() {
  S.login = { svg: null };
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
  renderQr();
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

const LOGIN_STEPS = ["Откройте Steam на телефоне", "Steam Guard, значок сканера", "Наведите камеру на код"];

function renderLogin() {
  let el = $("#login");
  if (!S.login) {
    if (el) {
      el.remove();
      renderLogin.opener?.focus();
    }
    return;
  }
  if (el) return renderQr();
  renderLogin.opener = document.activeElement;
  el = document.createElement("div");
  el.id = "login";
  el.className = "login-back";
  el.addEventListener("click", (e) => {
    if (e.target === el || e.target.closest("[data-login-cancel]")) cancelLogin();
  });
  el.innerHTML = `
    <div class="login-card" role="dialog" aria-modal="true" aria-labelledby="login-title" tabindex="-1">
      <div class="qr" id="qr"></div>
      <div class="login-side">
        <header>
          <span class="caption">STEAM GUARD</span>
          <h1 class="display" id="login-title">Войти через Steam</h1>
        </header>
        <ol class="steps">${LOGIN_STEPS.map((t, i) => `<li><span class="n">${i + 1}</span>${t}</li>`).join("")}</ol>
        <div class="login-wait"><span class="dots" aria-hidden="true"><i></i><i></i><i></i></span><span>Жду подтверждения</span></div>
        <div class="login-foot"><span>Пароль не нужен</span><button class="btn btn-sm btn-quiet" data-login-cancel>Без входа</button></div>
      </div>
      <button class="icon-btn close" data-login-cancel aria-label="Закрыть">${I.x}</button>
    </div>`;
  document.body.append(el);
  renderQr();
  // Фокус на само окно: кнопки получают его с Tab, без рамки сразу при открытии.
  el.querySelector(".login-card").focus();
}

function renderQr() {
  const el = $("#qr");
  if (!el) return;
  el.innerHTML = S.login?.svg
    ? `<div role="img" aria-label="QR-код для входа в Steam">${S.login.svg}</div>`
    : `<div class="qr-wait" aria-label="Готовлю QR-код"></div>`;
  const svg = el.querySelector("svg");
  if (svg) svg.setAttribute("aria-hidden", "true");
}

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && S.login) cancelLogin();
  // Фокус не уходит из окна входа.
  if (e.key === "Tab" && S.login) {
    const items = [...document.querySelectorAll("#login button")];
    if (!items.length) return;
    const i = items.indexOf(document.activeElement);
    const next = e.shiftKey ? (i <= 0 ? items.length - 1 : i - 1) : (i + 1) % items.length;
    e.preventDefault();
    items[next].focus();
  }
});

function renderAccount() {
  const el = $("#account");
  if (!el) return;
  if (S.account === undefined) return void (el.innerHTML = "");
  if (!S.account) {
    el.innerHTML = `<button class="tab" id="nav-login">${I.steam}Войти</button>`;
    $("#nav-login").onclick = steamLogin;
    return;
  }
  const n = S.account.accountName;
  el.innerHTML = `<button class="account" id="nav-account" aria-label="Аккаунт Steam: ${esc(n)}">
    <span class="ava" aria-hidden="true">${esc(n.slice(0, 1).toUpperCase())}</span><span class="account-id">${esc(maskedId(S.account) || n)}</span></button>`;
  $("#nav-account").onclick = () => go("settings");
}

function renderSteamGroup() {
  const el = $("#steam-group");
  if (!el) return;
  const a = S.account;
  const sub = a ? [maskedId(a), S.owned && `${S.owned.length} игр`].filter(Boolean).join(" · ") : "";
  el.innerHTML = `
    <h2>Steam</h2>
    <div class="rows"><div class="row">
      ${a ? `<span class="ava" aria-hidden="true">${esc(a.accountName.slice(0, 1).toUpperCase())}</span>` : ""}
      <div class="row-label">${a ? esc(a.accountName) : "Не вошёл"}${sub ? `<span class="row-sub">${esc(sub)}</span>` : ""}</div>
      ${
        a
          ? `<button class="btn btn-sm" id="steam-logout">${I.signOut}Выйти</button>`
          : `<button class="btn btn-sm btn-primary" id="steam-login">${I.steam}Войти через Steam</button>`
      }
    </div></div>`;
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
