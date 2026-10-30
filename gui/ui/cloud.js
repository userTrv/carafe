"use strict";

/* Steam Cloud: окно конфликта сейвов перед запуском и выгрузка в фоне после игры. Пользуется
   состоянием и помощниками app.js (S, $, esc, I, toast, play, task, game, ago); app.js зовёт
   отсюда хуки: cloudTaskEvent, cloudTaskEnd, pushLabel. */

// События задач `launch:` и `push:`: сколько файлов едет, нашёлся ли конфликт.
function cloudTaskEvent(t, ev) {
  if (ev.kind === "cloudFile" && ev.file.action === "download") t.downloads = (t.downloads || 0) + 1;
  if (ev.kind === "cloudFile" && ev.file.action === "upload") t.uploads = (t.uploads || 0) + 1;
  if (ev.kind === "cloudConflict") t.conflict = ev.files;
}

function cloudTaskEnd(kind, appid, t) {
  if (!t.conflict) return;
  const files = t.conflict;
  t.conflict = null;
  if (kind === "launch") openConflict(appid, files);
  // Пока шла игра, облако поменялось с другого устройства: выбор — при следующем запуске.
  if (kind === "push") toast(`${game(appid)?.name || "Игра"}: сейвы не выгружены, в облаке другая версия`);
}

// Строка «Облако» на этикетке, пока идёт выгрузка.
function pushLabel(appid) {
  const n = task(`push:${appid}`)?.uploads;
  return n ? `выгружаю сохранения · ${n}` : "выгружаю сохранения";
}

/* ---------- окно конфликта ---------- */

function kb(b) {
  if (b < 1024) return `${b} B`;
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(b < 10240 ? 1 : 0)} KB`;
  return `${(b / 1048576).toFixed(1)} MB`;
}

const when = (iso) =>
  new Date(iso).toLocaleString("ru", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });

// Сторона целиком: самое позднее изменение и общий размер.
function side(files, key) {
  const xs = files.map((f) => f[key]).filter(Boolean);
  const t = Math.max(...xs.map((x) => Date.parse(x.modified)));
  return { t, size: xs.reduce((n, x) => n + x.size, 0) };
}

function openConflict(appid, files) {
  files = files.filter((f) => f.local && f.cloud);
  if (!files.length) return toast("Сейвы разошлись с облаком");
  $("#cloud-dlg")?.remove();
  const g = game(appid);
  const local = side(files, "local");
  const cloud = side(files, "cloud");
  const card = (prefer, icon, where, s, newer, action) => `
    <section class="cd-side${newer ? " newer" : ""}" aria-label="${where}">
      <div class="cd-where">${icon}<span>${where}</span>${newer ? '<span class="tag">НОВЕЕ</span>' : ""}</div>
      <div class="cd-when">${esc(when(s.t))}</div>
      <div class="caption">${esc(ago(s.t / 1000)).toUpperCase()} · ${kb(s.size)}</div>
      <button class="btn ${newer ? "btn-primary" : ""}" data-prefer="${prefer}">${action}</button>
    </section>`;
  const rows =
    files.length > 1
      ? `<ul class="cd-files">${files
          .map(
            (f) => `<li><span class="file-name">${esc(f.name)}</span>
              <span class="mono">${esc(when(f.local.modified))} · ${kb(f.local.size)}</span>
              <span class="mono">${esc(when(f.cloud.modified))} · ${kb(f.cloud.size)}</span></li>`,
          )
          .join("")}</ul>`
      : "";
  const dlg = document.createElement("dialog");
  dlg.id = "cloud-dlg";
  dlg.className = "cloud-dlg";
  dlg.setAttribute("aria-labelledby", "cd-title");
  dlg.innerHTML = `
    <div class="cd-head">
      <h2 id="cd-title">Сейвы разошлись</h2>
      <span class="caption">${esc((g?.name || `APPID ${appid}`).toUpperCase())}</span>
    </div>
    <div class="cd-sides">
      ${card("local", I.disk, "Этот Mac", local, local.t >= cloud.t, "Оставить локальные")}
      ${card("cloud", I.cloud, "Steam Cloud", cloud, cloud.t > local.t, "Взять из облака")}
    </div>
    ${rows}
    <div class="cd-foot">
      <span class="caption">ВТОРАЯ ВЕРСИЯ ОСТАНЕТСЯ В РЕЗЕРВНОЙ КОПИИ</span>
      <button class="btn btn-sm btn-quiet" data-close>Не сейчас</button>
    </div>`;
  document.body.append(dlg);
  dlg.addEventListener("close", () => dlg.remove());
  dlg.querySelector("[data-close]").onclick = () => dlg.close();
  dlg.querySelectorAll("[data-prefer]").forEach((b) => {
    b.onclick = () => {
      dlg.close();
      play(appid, b.dataset.prefer);
    };
  });
  dlg.showModal();
  dlg.querySelector(".newer [data-prefer]")?.focus();
}
