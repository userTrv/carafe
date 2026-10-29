#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backend;
mod cli;
mod mock;
mod redact;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State};
use tauri_plugin_dialog::DialogExt;

use backend::{AppId, Backend, CheckId, DownloadAction, Profile, TaskEvent};

/// Настройки GUI: профиль запуска и путь к checkout carafe для `CliBackend`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Settings {
    profile: Profile,
    checkout: Option<String>,
}

struct App {
    backend: RwLock<Arc<dyn Backend>>,
    settings: Mutex<Settings>,
    settings_path: PathBuf,
    busy: Mutex<HashSet<String>>,
    /// Флаги отмены задач, которые её поддерживают (перенос игры).
    cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl App {
    fn backend(&self) -> Arc<dyn Backend> {
        self.backend.read().expect("backend").clone()
    }

    fn profile(&self) -> Profile {
        self.settings.lock().expect("settings").profile.clone()
    }
}

/// `CARAFE_GUI_BACKEND=mock|cli`; без неё — CLI, если известен checkout carafe.
fn pick_backend(settings: &Settings) -> Arc<dyn Backend> {
    let forced = std::env::var("CARAFE_GUI_BACKEND").unwrap_or_default();
    if forced == "mock" {
        return Arc::new(mock::MockBackend::new());
    }
    let checkout = std::env::var("CARAFE_CHECKOUT")
        .ok()
        .or_else(|| settings.checkout.clone());
    match checkout.map(|c| cli::CliBackend::new(&expand_home(&c))) {
        Some(Ok(b)) => Arc::new(b),
        Some(Err(e)) => {
            eprintln!("carafe-gui: {e}; беру демо-данные");
            Arc::new(mock::MockBackend::new())
        }
        None => Arc::new(mock::MockBackend::new()),
    }
}

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(rest),
        None => PathBuf::from(path),
    }
}

/// Всё, что уходит в UI, проходит через вырезание Steam ID.
fn clean<T: Serialize>(v: T) -> Value {
    fn walk(v: Value) -> Value {
        match v {
            Value::String(s) => Value::String(redact::redact(&s)),
            Value::Array(a) => Value::Array(a.into_iter().map(walk).collect()),
            Value::Object(o) => Value::Object(o.into_iter().map(|(k, v)| (k, walk(v))).collect()),
            v => v,
        }
    }
    walk(serde_json::to_value(v).unwrap_or(Value::Null))
}

fn err(e: impl std::fmt::Display) -> String {
    redact::redact(&e.to_string())
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> backend::Result<T> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(err)?
        .map_err(err)
}

#[tauri::command]
fn info(app: State<'_, App>) -> Value {
    let s = app.settings.lock().expect("settings");
    json!({
        "backend": app.backend().label(),
        "checkout": s.checkout,
        // Только для разработки: открыть экран сразу (`game/<appid>`, `play/<appid>`, `setup`, `settings`).
        "route": std::env::var("CARAFE_GUI_ROUTE").ok(),
    })
}

#[tauri::command]
async fn environment(app: State<'_, App>) -> Result<Value, String> {
    let (b, p) = (app.backend(), app.profile());
    let env = blocking(move || b.environment(&p)).await?;
    Ok(clean(json!({ "ready": env.ready(), "env": env })))
}

#[tauri::command]
async fn games(app: State<'_, App>) -> Result<Value, String> {
    let (b, p) = (app.backend(), app.profile());
    blocking(move || b.games(&p)).await.map(clean)
}

#[tauri::command]
async fn cloud_status(app: State<'_, App>, appid: AppId) -> Result<Value, String> {
    let (b, p) = (app.backend(), app.profile());
    let st = blocking(move || b.cloud_status(appid, &p)).await?;
    Ok(clean(json!({ "summary": st.summary(), "status": st })))
}

#[tauri::command]
fn get_settings(app: State<'_, App>) -> Value {
    clean(&*app.settings.lock().expect("settings"))
}

#[tauri::command]
fn save_settings(app: State<'_, App>, settings: Settings) -> Result<Value, String> {
    let backend_changed = {
        let mut cur = app.settings.lock().expect("settings");
        let changed = cur.checkout != settings.checkout;
        *cur = settings.clone();
        changed
    };
    if let Some(dir) = app.settings_path.parent() {
        fs::create_dir_all(dir).map_err(err)?;
    }
    let text = serde_json::to_string_pretty(&settings).map_err(err)?;
    fs::write(&app.settings_path, text).map_err(err)?;
    if backend_changed {
        *app.backend.write().expect("backend") = pick_backend(&settings);
    }
    Ok(json!({ "backend": app.backend().label() }))
}

/// Долгая операция в отдельном потоке: события — `task`, итог — `task-end`.
fn spawn_task(
    handle: AppHandle,
    app: &App,
    task: String,
    work: impl FnOnce(&dyn Backend, &Profile, backend::Events) -> backend::Result<i32> + Send + 'static,
) -> Result<(), String> {
    if !app.busy.lock().expect("busy").insert(task.clone()) {
        return Err("уже идёт".into());
    }
    let (b, p) = (app.backend(), app.profile());
    std::thread::spawn(move || {
        let h = handle.clone();
        let t = task.clone();
        let sink = move |e: TaskEvent| {
            let event = match e {
                // URL входа в окно не уходит — только картинка.
                TaskEvent::Qr { url } => json!({ "kind": "qr", "svg": qr_svg(&url) }),
                e => clean(e),
            };
            let _ = h.emit("task", json!({ "task": t, "event": event }));
        };
        let res = work(b.as_ref(), &p, &sink);
        handle
            .state::<App>()
            .busy
            .lock()
            .expect("busy")
            .remove(&task);
        let end = match res {
            Ok(code) => json!({ "task": task, "ok": true, "code": code }),
            Err(e) => json!({ "task": task, "ok": false, "error": err(e) }),
        };
        let _ = handle.emit("task-end", end);
    });
    Ok(())
}

#[tauri::command]
fn launch(handle: AppHandle, app: State<'_, App>, appid: AppId) -> Result<(), String> {
    spawn_task(handle, &app, format!("launch:{appid}"), move |b, p, ev| {
        b.launch(appid, p, ev)
    })
}

#[tauri::command]
fn build_bridge(handle: AppHandle, app: State<'_, App>, appid: AppId) -> Result<(), String> {
    spawn_task(handle, &app, format!("bridge:{appid}"), move |b, p, ev| {
        b.build_bridge(appid, p, ev).map(|_| 0)
    })
}

#[tauri::command]
fn fix(handle: AppHandle, app: State<'_, App>, check: CheckId) -> Result<(), String> {
    let name = serde_json::to_value(check)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    spawn_task(handle, &app, format!("fix:{name}"), move |b, p, ev| {
        b.fix(check, p, ev).map(|_| 0)
    })
}

#[tauri::command]
async fn libraries(app: State<'_, App>) -> Result<Value, String> {
    let b = app.backend();
    blocking(move || b.libraries()).await.map(clean)
}

/// Системный диалог выбора папки. Зовётся из Rust — JS-доступ к плагину не открываем.
#[tauri::command]
async fn pick_folder(handle: AppHandle) -> Result<Option<String>, String> {
    let picked = tauri::async_runtime::spawn_blocking(move || {
        handle
            .dialog()
            .file()
            .set_title("Папка библиотеки")
            .set_can_create_directories(true)
            .blocking_pick_folder()
    })
    .await
    .map_err(err)?;
    Ok(picked
        .and_then(|p| p.into_path().ok())
        .map(|p| p.display().to_string()))
}

#[tauri::command]
async fn library_action(app: State<'_, App>, action: String, path: String) -> Result<(), String> {
    let b = app.backend();
    blocking(move || {
        let p = PathBuf::from(&path);
        match action.as_str() {
            "add" => b.add_library(&p),
            "remove" => b.remove_library(&p),
            "default" => b.set_default_library(&p),
            _ => Err(backend::BackendError::new("неизвестное действие")),
        }
    })
    .await
}

#[tauri::command]
async fn install(app: State<'_, App>, appid: AppId, library: Option<String>) -> Result<(), String> {
    let b = app.backend();
    blocking(move || b.install(appid, library.as_deref().map(std::path::Path::new))).await
}

#[tauri::command]
fn move_game(
    handle: AppHandle,
    app: State<'_, App>,
    appid: AppId,
    library: String,
) -> Result<(), String> {
    let task = format!("move:{appid}");
    let cancel = Arc::new(AtomicBool::new(false));
    app.cancels
        .lock()
        .expect("cancels")
        .insert(task.clone(), cancel.clone());
    let h = handle.clone();
    let t = task.clone();
    let r = spawn_task(handle, &app, task.clone(), move |b, _, ev| {
        let r = b
            .move_game(appid, std::path::Path::new(&library), &cancel, ev)
            .map(|_| 0);
        h.state::<App>().cancels.lock().expect("cancels").remove(&t);
        r
    });
    if r.is_err() {
        app.cancels.lock().expect("cancels").remove(&task);
    }
    r
}

#[tauri::command]
fn cancel_task(app: State<'_, App>, task: String) {
    if let Some(c) = app.cancels.lock().expect("cancels").get(&task) {
        c.store(true, Ordering::Relaxed);
    }
}

#[tauri::command]
async fn downloads(app: State<'_, App>) -> Result<Value, String> {
    let b = app.backend();
    blocking(move || b.downloads()).await.map(clean)
}

#[tauri::command]
async fn download_action(
    app: State<'_, App>,
    appid: AppId,
    action: DownloadAction,
) -> Result<(), String> {
    let b = app.backend();
    blocking(move || b.download_action(appid, action)).await
}

/// QR в SVG: тёмные модули на белом с полями — так его читает камера приложения Steam.
fn qr_svg(url: &str) -> Value {
    use qrcode::render::svg;
    match qrcode::QrCode::new(url.as_bytes()) {
        Ok(code) => code
            .render::<svg::Color<'_>>()
            .min_dimensions(240, 240)
            .dark_color(svg::Color("#0b0b10"))
            .light_color(svg::Color("#ffffff"))
            .build()
            .into(),
        Err(_) => Value::Null,
    }
}

#[tauri::command]
async fn steam_account(app: State<'_, App>) -> Result<Value, String> {
    let b = app.backend();
    blocking(move || b.steam_account()).await.map(clean)
}

/// Вход по QR — задача `login`: события `qr` с SVG, итог — `task-end`. Отмена —
/// `cancel_task("login")`.
#[tauri::command]
fn steam_login(handle: AppHandle, app: State<'_, App>) -> Result<(), String> {
    let task = "login".to_string();
    let cancel = Arc::new(AtomicBool::new(false));
    app.cancels
        .lock()
        .expect("cancels")
        .insert(task.clone(), cancel.clone());
    let h = handle.clone();
    let r = spawn_task(handle, &app, task.clone(), move |b, _, ev| {
        // Отмена — не ошибка: окно просто закрывает QR (код 1 вместо тоста).
        let r = match b.steam_login(&cancel, ev) {
            Ok(_) => Ok(0),
            Err(_) if cancel.load(Ordering::Relaxed) => Ok(1),
            Err(e) => Err(e),
        };
        h.state::<App>()
            .cancels
            .lock()
            .expect("cancels")
            .remove("login");
        r
    });
    if r.is_err() {
        app.cancels.lock().expect("cancels").remove(&task);
    }
    r
}

#[tauri::command]
async fn steam_logout(app: State<'_, App>) -> Result<(), String> {
    let b = app.backend();
    blocking(move || b.steam_logout()).await
}

#[tauri::command]
async fn owned_games(app: State<'_, App>) -> Result<Value, String> {
    let b = app.backend();
    blocking(move || b.owned_games()).await.map(clean)
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|tauri_app| {
            let path = tauri_app
                .path()
                .app_config_dir()
                .map(|d| d.join("settings.json"))
                .unwrap_or_else(|_| PathBuf::from("settings.json"));
            let settings: Settings = fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            let backend = pick_backend(&settings);
            tauri_app.manage(App {
                backend: RwLock::new(backend),
                settings: Mutex::new(settings),
                settings_path: path,
                busy: Mutex::new(HashSet::new()),
                cancels: Mutex::new(HashMap::new()),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            info,
            environment,
            games,
            cloud_status,
            get_settings,
            save_settings,
            launch,
            build_bridge,
            fix,
            libraries,
            pick_folder,
            library_action,
            install,
            move_game,
            cancel_task,
            downloads,
            download_action,
            steam_account,
            steam_login,
            steam_logout,
            owned_games,
        ])
        .build(tauri::generate_context!())
        .expect("carafe-gui")
        .run(|handle, ev| {
            if let RunEvent::Exit = ev {
                // Загрузка остановится и продолжится при следующем запуске.
                handle.state::<App>().backend().shutdown();
            }
        });
}
