//! Очередь установки и обновления: по одной загрузке за раз, порядок меняется, пауза, отмена.
//!
//! Состояние — в `downloads.vdf` в каталоге carafe и переживает перезапуск: прерванная загрузка
//! снова встаёт в очередь и докачивается. Файл — единственный источник правды для порядка и
//! состояний, поэтому очередью можно управлять и из другого процесса (CLI при открытом GUI):
//! обработчик очереди раз в долю секунды сверяется с файлом и останавливает загрузку, которую
//! поставили на паузу или отменили. Обработчик в каждый момент один — его держит блокировка
//! `downloads.worker.lock`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use crate::download::{DownloadEvent, DownloadRequest, DownloadStage, Downloader, StopFlag};
use crate::error::{Error, Result};
use crate::fsutil::{self, FileLock};
use crate::vdf::{self, Obj, Value};
use crate::{Carafe, Log};

pub const QUEUE_FILE: &str = "downloads.vdf";
/// Как часто обработчик сверяется с файлом очереди.
const POLL: Duration = Duration::from_millis(300);
/// Как часто прогресс активной загрузки сохраняется в файл.
const SAVE_EVERY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Install,
    Update,
    /// Проверка файлов.
    Verify,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::Install => "install",
            JobKind::Update => "update",
            JobKind::Verify => "verify",
        }
    }

    pub fn parse(s: &str) -> Option<JobKind> {
        [JobKind::Install, JobKind::Update, JobKind::Verify].into_iter().find(|k| k.as_str() == s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Active,
    Paused,
    /// Загрузка упала; `resume` — попробовать снова.
    Failed,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Active => "active",
            JobState::Paused => "paused",
            JobState::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<JobState> {
        [JobState::Queued, JobState::Active, JobState::Paused, JobState::Failed].into_iter().find(|k| k.as_str() == s)
    }
}

/// Загрузка в очереди.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub appid: u32,
    pub name: Option<String>,
    /// Папка игры: `<библиотека>/<appid>`.
    pub dir: PathBuf,
    pub kind: JobKind,
    pub branch: Option<String>,
    pub depots: Vec<u32>,
    pub state: JobState,
    pub stage: Option<DownloadStage>,
    /// Байты этапа.
    pub done: u64,
    pub total: u64,
    /// Байт в секунду на этапе загрузки; только у активной в этом процессе.
    pub speed: Option<u64>,
    /// Секунд до конца этапа.
    pub eta: Option<u64>,
    pub error: Option<String>,
    /// Папку игры создала эта загрузка: при отмене установки её можно удалить целиком.
    pub created_dir: bool,
}

impl Job {
    pub fn new(appid: u32, dir: PathBuf, kind: JobKind) -> Job {
        Job {
            appid,
            name: None,
            dir,
            kind,
            branch: None,
            depots: vec![],
            state: JobState::Queued,
            stage: None,
            done: 0,
            total: 0,
            speed: None,
            eta: None,
            error: None,
            created_dir: false,
        }
    }

    pub fn request(&self) -> DownloadRequest {
        DownloadRequest {
            appid: self.appid,
            dir: self.dir.clone(),
            branch: self.branch.clone(),
            depots: self.depots.clone(),
            verify: self.kind != JobKind::Update,
        }
    }
}

/// Событие очереди для GUI: что-то поменялось (перечитай `jobs`) или загрузка закончилась.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueEvent {
    Changed,
    Finished { appid: u32, error: Option<String> },
}

type Listener = Arc<dyn Fn(QueueEvent) + Send + Sync>;

/// Живое состояние активной загрузки этого процесса.
struct Live {
    appid: u32,
    stop: Arc<StopFlag>,
    stage: Option<DownloadStage>,
    done: u64,
    total: u64,
    speed: Option<u64>,
    eta: Option<u64>,
}

pub struct Queue {
    carafe: Carafe,
    live: Mutex<Option<Live>>,
    wake: (Mutex<bool>, Condvar),
    listener: RwLock<Option<Listener>>,
}

impl Carafe {
    pub fn queue(&self) -> Queue {
        Queue::new(self.clone())
    }
}

impl Queue {
    pub fn new(carafe: Carafe) -> Queue {
        Queue { carafe, live: Mutex::new(None), wake: (Mutex::new(false), Condvar::new()), listener: RwLock::new(None) }
    }

    fn file(&self) -> PathBuf {
        self.carafe.config.home.join(QUEUE_FILE)
    }

    fn worker_lock(&self) -> PathBuf {
        self.carafe.config.home.join("downloads.worker.lock")
    }

    pub fn set_listener(&self, f: impl Fn(QueueEvent) + Send + Sync + 'static) {
        *self.listener.write().expect("listener") = Some(Arc::new(f));
    }

    fn notify(&self, e: QueueEvent) {
        let l = self.listener.read().expect("listener").clone();
        if let Some(l) = l {
            l(e)
        }
    }

    fn wake(&self) {
        *self.wake.0.lock().expect("wake") = true;
        self.wake.1.notify_all();
    }

    /// Прочитать и, если `f` что-то поменяла, записать файл очереди — под блокировкой.
    fn edit<T>(&self, f: impl FnOnce(&mut Vec<Job>) -> Result<T>) -> Result<T> {
        let home = &self.carafe.config.home;
        fsutil::create_dir_all(home)?;
        let _lock = FileLock::new(&home.join("downloads.lock"))?;
        let mut jobs = read_jobs(&self.file())?;
        let before = jobs.clone();
        let r = f(&mut jobs)?;
        if jobs != before {
            fsutil::write_atomic(&self.file(), format_jobs(&jobs).as_bytes())?;
        }
        Ok(r)
    }

    /// Очередь по порядку; у активной загрузки этого процесса — живой прогресс.
    pub fn jobs(&self) -> Result<Vec<Job>> {
        let mut jobs = self.edit(|j| Ok(j.clone()))?;
        if let Some(l) = &*self.live.lock().expect("live")
            && let Some(j) = jobs.iter_mut().find(|j| j.appid == l.appid && j.state == JobState::Active)
        {
            (j.stage, j.done, j.total, j.speed, j.eta) = (l.stage, l.done, l.total, l.speed, l.eta);
        }
        Ok(jobs)
    }

    /// В конец очереди. Одна игра — одна загрузка.
    pub fn add(&self, job: Job) -> Result<()> {
        self.edit(|jobs| {
            if jobs.iter().any(|j| j.appid == job.appid) {
                return Err(Error::AlreadyQueued(job.appid));
            }
            jobs.push(job);
            Ok(())
        })?;
        self.wake();
        self.notify(QueueEvent::Changed);
        Ok(())
    }

    fn set_state(&self, appid: u32, f: impl FnOnce(&mut Job)) -> Result<()> {
        self.edit(|jobs| {
            let j = jobs.iter_mut().find(|j| j.appid == appid).ok_or(Error::NotQueued(appid))?;
            f(j);
            Ok(())
        })?;
        self.wake();
        self.notify(QueueEvent::Changed);
        Ok(())
    }

    /// Пауза: активная останавливается (скачанное остаётся), ждущая пропускается.
    pub fn pause(&self, appid: u32) -> Result<()> {
        self.set_state(appid, |j| j.state = JobState::Paused)?;
        self.stop_live(appid);
        Ok(())
    }

    /// Снять с паузы или повторить упавшую.
    pub fn resume(&self, appid: u32) -> Result<()> {
        self.set_state(appid, |j| {
            if matches!(j.state, JobState::Paused | JobState::Failed) {
                j.state = JobState::Queued;
                j.error = None;
            }
        })
    }

    /// Убрать из очереди. Недокачанная новая установка удаляется с диска; у обновления и проверки
    /// папка игры остаётся как есть.
    pub fn cancel(&self, appid: u32) -> Result<()> {
        let job = self.edit(|jobs| {
            let i = jobs.iter().position(|j| j.appid == appid).ok_or(Error::NotQueued(appid))?;
            Ok(jobs.remove(i))
        })?;
        if !self.stop_live(appid) && job.state != JobState::Active {
            // Активную в другом процессе подчистит её обработчик.
            cleanup_cancelled(&job)?;
        }
        self.notify(QueueEvent::Changed);
        Ok(())
    }

    /// Переставить на место `index` (0 — первой). Активная загрузка не прерывается: новая первая
    /// пойдёт следующей.
    pub fn move_to(&self, appid: u32, index: usize) -> Result<()> {
        self.edit(|jobs| {
            let i = jobs.iter().position(|j| j.appid == appid).ok_or(Error::NotQueued(appid))?;
            let j = jobs.remove(i);
            jobs.insert(index.min(jobs.len()), j);
            Ok(())
        })?;
        self.notify(QueueEvent::Changed);
        Ok(())
    }

    /// Остановить живую загрузку этого процесса; true — она была.
    fn stop_live(&self, appid: u32) -> bool {
        match &*self.live.lock().expect("live") {
            Some(l) if l.appid == appid => {
                l.stop.stop();
                true
            }
            _ => false,
        }
    }

    /// Обрабатывать очередь, пока в ней есть что качать (CLI).
    pub fn run_until_empty(&self, dl: &dyn Downloader) -> Result<()> {
        self.run(dl, &AtomicBool::new(false), true)
    }

    /// Обрабатывать очередь, пока не поднят `shutdown` (GUI: в своём потоке). Ждёт новых загрузок.
    pub fn run_forever(&self, dl: &dyn Downloader, shutdown: &AtomicBool) -> Result<()> {
        self.run(dl, shutdown, false)
    }

    fn run(&self, dl: &dyn Downloader, shutdown: &AtomicBool, until_empty: bool) -> Result<()> {
        let _worker = FileLock::try_new(&self.worker_lock())?.ok_or(Error::QueueBusy)?;
        // Обработчик один, значит «активные» в файле — прерванные прошлым запуском: докачать.
        self.edit(|jobs| {
            jobs.iter_mut().filter(|j| j.state == JobState::Active).for_each(|j| j.state = JobState::Queued);
            Ok(())
        })?;
        while !shutdown.load(Ordering::Relaxed) {
            let next = self.edit(|jobs| {
                Ok(jobs.iter_mut().find(|j| j.state == JobState::Queued).map(|j| {
                    j.state = JobState::Active;
                    j.error = None;
                    j.clone()
                }))
            })?;
            match next {
                Some(job) => {
                    self.notify(QueueEvent::Changed);
                    self.run_job(dl, job, shutdown)?;
                }
                None if until_empty => break,
                None => {
                    // Новые загрузки будят сразу; из другого процесса — заметим на следующем круге.
                    let mut woken = self.wake.0.lock().expect("wake");
                    if !*woken {
                        woken = self.wake.1.wait_timeout(woken, Duration::from_secs(1)).expect("wake").0;
                    }
                    *woken = false;
                }
            }
        }
        Ok(())
    }

    fn run_job(&self, dl: &dyn Downloader, mut job: Job, shutdown: &AtomicBool) -> Result<()> {
        let appid = job.appid;
        let stop = Arc::new(StopFlag::default());
        *self.live.lock().expect("live") = Some(Live {
            appid,
            stop: stop.clone(),
            stage: job.stage,
            done: job.done,
            total: job.total,
            speed: None,
            eta: None,
        });
        self.carafe.log(Log::Step(format!("{}: {} {appid} → {}", dl.name(), job.kind.as_str(), job.dir.display())));

        let result = self.prepare_dir(&mut job).and_then(|()| {
            let finished = AtomicBool::new(false);
            std::thread::scope(|s| {
                // Сверка с файлом: пауза и отмена из другого процесса, выход из GUI.
                s.spawn(|| {
                    while !finished.load(Ordering::Relaxed) {
                        std::thread::sleep(POLL);
                        if shutdown.load(Ordering::Relaxed) {
                            stop.stop();
                        }
                        let state = self.edit(|jobs| Ok(jobs.iter().find(|j| j.appid == appid).map(|j| j.state)));
                        if matches!(state, Ok(None) | Ok(Some(JobState::Paused))) {
                            stop.stop();
                        }
                    }
                });
                let mut meter = Meter::default();
                let mut saved = Instant::now();
                let r = dl.download(&job.request(), &stop, &mut |ev| {
                    self.on_event(&mut meter, ev);
                    if saved.elapsed() > SAVE_EVERY {
                        saved = Instant::now();
                        let _ = self.save_live(appid);
                    }
                });
                finished.store(true, Ordering::Relaxed);
                r
            })
        });
        let _ = self.save_live(appid);
        *self.live.lock().expect("live") = None;

        let state = self.edit(|jobs| Ok(jobs.iter().find(|j| j.appid == appid).map(|j| j.state)))?;
        match (result, state) {
            (Ok(()), _) => {
                crate::steam::write_appid(&job.dir, appid)?;
                self.edit(|jobs| {
                    jobs.retain(|j| j.appid != appid);
                    Ok(())
                })?;
                let size = crate::library::tree_size(&job.dir);
                let name = job.name.clone().or_else(|| self.carafe.cached_app_info(appid).and_then(|i| i.name));
                self.carafe.remember_app(appid, Some(size), name)?;
                self.carafe.log(Log::Step(format!("{appid}: готово")));
                self.notify(QueueEvent::Finished { appid, error: None });
            }
            // Отменили, пока качалась.
            (Err(_), None) => {
                cleanup_cancelled(&job)?;
                self.notify(QueueEvent::Changed);
            }
            (Err(Error::Interrupted), Some(JobState::Paused)) => self.notify(QueueEvent::Changed),
            (Err(Error::Interrupted), Some(_)) if shutdown.load(Ordering::Relaxed) => {
                self.set_state(appid, |j| j.state = JobState::Queued)?;
            }
            (Err(e), Some(_)) => {
                let msg = e.to_string();
                self.carafe.log(Log::Step(format!("{appid}: {msg}")));
                self.set_state(appid, |j| {
                    j.state = JobState::Failed;
                    j.error = Some(msg.clone());
                })?;
                self.notify(QueueEvent::Finished { appid, error: Some(msg) });
            }
        }
        Ok(())
    }

    /// Папка игры перед загрузкой. Создаём только сам `<appid>/`: если библиотеки нет (диск
    /// отключён), не плодим её на системном диске.
    fn prepare_dir(&self, job: &mut Job) -> Result<()> {
        if job.dir.is_dir() {
            return Ok(());
        }
        let lib = job.dir.parent().unwrap_or(Path::new("/"));
        if !lib.is_dir() {
            return Err(Error::LibraryUnavailable(lib.into()));
        }
        std::fs::create_dir(&job.dir).map_err(|source| Error::Io { path: job.dir.clone(), source })?;
        job.created_dir = job.kind == JobKind::Install;
        let created = job.created_dir;
        self.edit(|jobs| {
            if let Some(j) = jobs.iter_mut().find(|j| j.appid == job.appid) {
                j.created_dir = created;
            }
            Ok(())
        })
    }

    fn on_event(&self, meter: &mut Meter, ev: DownloadEvent) {
        let mut live = self.live.lock().expect("live");
        let Some(l) = live.as_mut() else { return };
        match ev {
            DownloadEvent::Stage(s) => {
                l.stage = Some(s);
                (l.done, l.total, l.speed, l.eta) = (0, 0, None, None);
                *meter = Meter::default();
            }
            DownloadEvent::Progress { done, total } => {
                (l.done, l.total) = (done, total);
                meter.sample(done);
                l.speed = meter.speed.map(|s| s as u64).filter(|&s| s > 0);
                l.eta = l.speed.map(|s| total.saturating_sub(done) / s);
            }
            DownloadEvent::Log(line) => {
                drop(live);
                self.carafe.log(Log::Output(line));
                return;
            }
        }
        drop(live);
        self.notify(QueueEvent::Changed);
    }

    /// Прогресс активной — в файл: после перезапуска видно, сколько уже было.
    fn save_live(&self, appid: u32) -> Result<()> {
        let Some((stage, done, total)) =
            self.live.lock().expect("live").as_ref().filter(|l| l.appid == appid).map(|l| (l.stage, l.done, l.total))
        else {
            return Ok(());
        };
        self.edit(|jobs| {
            if let Some(j) = jobs.iter_mut().find(|j| j.appid == appid) {
                (j.stage, j.done, j.total) = (stage, done, total);
            }
            Ok(())
        })
    }
}

/// Отменённая установка: удалить то, что она успела скачать, — только если папку создала она.
fn cleanup_cancelled(job: &Job) -> Result<()> {
    if job.kind == JobKind::Install && job.created_dir {
        fsutil::remove_all(&job.dir)?;
    }
    Ok(())
}

/// Скорость по байтам: скользящее среднее по отсчётам не чаще раза в секунду.
#[derive(Debug, Default)]
struct Meter {
    last: Option<(Instant, u64)>,
    speed: Option<f64>,
}

impl Meter {
    fn sample(&mut self, done: u64) {
        self.sample_at(Instant::now(), done)
    }

    fn sample_at(&mut self, now: Instant, done: u64) {
        let Some((t, d)) = self.last else {
            self.last = Some((now, done));
            return;
        };
        let dt = now.duration_since(t).as_secs_f64();
        if dt < 1.0 {
            return;
        }
        let inst = done.saturating_sub(d) as f64 / dt;
        self.speed = Some(match self.speed {
            Some(s) => 0.3 * inst + 0.7 * s,
            None => inst,
        });
        self.last = Some((now, done));
    }
}

/// ```text
/// "downloads"
/// {
///     "0" { "appid" "3405340" "kind" "install" "state" "queued" "dir" "…/games/3405340" … }
/// }
/// ```
fn read_jobs(file: &Path) -> Result<Vec<Job>> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(source) => return Err(Error::Io { path: file.into(), source }),
    };
    if text.trim().is_empty() {
        return Ok(vec![]);
    }
    let doc = vdf::parse(&text)?;
    let Some(root) = doc.obj("downloads") else { return Ok(vec![]) };
    let mut out: Vec<Job> = vec![];
    for (_, v) in root.iter() {
        let Some(o) = v.as_obj() else { continue };
        let (Some(appid), Some(dir)) = (o.str("appid").and_then(|s| s.parse().ok()), o.str("dir")) else { continue };
        if out.iter().any(|j| j.appid == appid) {
            continue;
        }
        let opt = |k: &str| o.str(k).filter(|s| !s.is_empty()).map(String::from);
        let num = |k: &str| o.str(k).and_then(|s| s.parse().ok()).unwrap_or(0);
        out.push(Job {
            appid,
            name: opt("name"),
            dir: dir.into(),
            kind: o.str("kind").and_then(JobKind::parse).unwrap_or(JobKind::Install),
            branch: opt("branch"),
            depots: o.str("depots").unwrap_or("").split_whitespace().filter_map(|d| d.parse().ok()).collect(),
            state: o.str("state").and_then(JobState::parse).unwrap_or(JobState::Queued),
            stage: o.str("stage").and_then(DownloadStage::parse),
            done: num("done"),
            total: num("total"),
            speed: None,
            eta: None,
            error: opt("error"),
            created_dir: o.str("created") == Some("1"),
        });
    }
    Ok(out)
}

fn format_jobs(jobs: &[Job]) -> String {
    let s = |v: &str| Value::Str(v.into());
    let list = jobs
        .iter()
        .enumerate()
        .map(|(i, j)| {
            let mut o = vec![
                ("appid".to_string(), s(&j.appid.to_string())),
                ("kind".into(), s(j.kind.as_str())),
                ("state".into(), s(j.state.as_str())),
                ("dir".into(), s(&j.dir.to_string_lossy())),
            ];
            let mut opt = |k: &str, v: Option<String>| {
                if let Some(v) = v {
                    o.push((k.into(), s(&v)));
                }
            };
            opt("name", j.name.clone());
            opt("branch", j.branch.clone());
            opt(
                "depots",
                (!j.depots.is_empty()).then(|| j.depots.iter().map(u32::to_string).collect::<Vec<_>>().join(" ")),
            );
            opt("stage", j.stage.map(|st| st.as_str().into()));
            opt("done", (j.done > 0).then(|| j.done.to_string()));
            opt("total", (j.total > 0).then(|| j.total.to_string()));
            opt("error", j.error.clone());
            opt("created", j.created_dir.then(|| "1".into()));
            (i.to_string(), Value::Obj(Obj(o)))
        })
        .collect();
    Obj(vec![("downloads".into(), Value::Obj(Obj(list)))]).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::{MOCK_EXE, MockDownloader};
    use crate::fsutil::testdir::TestDir;
    use crate::library::tests::carafe;

    fn ids(q: &Queue) -> Vec<(u32, JobState)> {
        q.jobs().unwrap().iter().map(|j| (j.appid, j.state)).collect()
    }

    fn slow() -> MockDownloader {
        MockDownloader { total: 100, step: 1, delay: Duration::from_millis(10), fail_at: None }
    }

    /// Дождаться условия (обработчик работает в другом потоке).
    fn wait_for(what: &str, f: impl Fn() -> bool) {
        let t = Instant::now();
        while !f() {
            assert!(t.elapsed() < Duration::from_secs(10), "не дождался: {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn runs_in_order_and_installs() {
        let d = TestDir::new();
        let c = carafe(&d.path().join("carafe"));
        let q = c.queue();
        for id in [10, 20, 30] {
            q.add(Job::new(id, c.install_dir(id, None).unwrap(), JobKind::Install)).unwrap();
        }
        assert!(matches!(q.add(Job::new(10, "/x".into(), JobKind::Install)), Err(Error::AlreadyQueued(10))));
        q.move_to(30, 0).unwrap();
        q.move_to(10, 99).unwrap();
        assert_eq!(ids(&q), [(30, JobState::Queued), (20, JobState::Queued), (10, JobState::Queued)]);

        let order = Arc::new(Mutex::new(vec![]));
        let o = order.clone();
        q.set_listener(move |e| {
            if let QueueEvent::Finished { appid, error: None } = e {
                o.lock().unwrap().push(appid)
            }
        });
        q.run_until_empty(&MockDownloader::default()).unwrap();
        assert_eq!(*order.lock().unwrap(), [30, 20, 10]);
        assert!(q.jobs().unwrap().is_empty());
        let games = c.games().unwrap();
        assert_eq!(games.iter().map(|g| g.appid).collect::<Vec<_>>(), [10, 20, 30]);
        assert!(games.iter().all(|g| g.dir.join(MOCK_EXE).is_file() && g.dir.join("steam_appid.txt").is_file()));
        // Размер запомнен.
        assert!(c.game_locations().unwrap().iter().all(|g| g.size == Some(2 + 3)));
    }

    #[test]
    fn failure_stays_in_queue() {
        let d = TestDir::new();
        let c = carafe(&d.path().join("carafe"));
        let q = c.queue();
        q.add(Job::new(1, c.install_dir(1, None).unwrap(), JobKind::Install)).unwrap();
        q.add(Job::new(2, c.install_dir(2, None).unwrap(), JobKind::Install)).unwrap();
        let bad = MockDownloader { total: 10, step: 2, fail_at: Some(4), ..Default::default() };
        q.run_until_empty(&bad).unwrap();
        let jobs = q.jobs().unwrap();
        assert_eq!(jobs.iter().map(|j| j.state).collect::<Vec<_>>(), [JobState::Failed, JobState::Failed]);
        assert_eq!(jobs[0].error.as_deref(), Some("steamcmd: No connection"));
        assert_eq!(jobs[0].done, 4);
        // Повтор докачивает с того же места.
        q.resume(1).unwrap();
        q.cancel(2).unwrap();
        q.run_until_empty(&MockDownloader { total: 10, step: 2, ..Default::default() }).unwrap();
        assert!(q.jobs().unwrap().is_empty());
        assert!(c.install_dir(1, None).unwrap().join(MOCK_EXE).is_file());
        // Отменённая новая установка удалена целиком.
        assert!(!d.path().join("carafe/games/2").exists());
    }

    #[test]
    fn restored_after_restart() {
        let d = TestDir::new();
        let c = carafe(&d.path().join("carafe"));
        {
            let q = c.queue();
            let mut j = Job::new(5, c.install_dir(5, None).unwrap(), JobKind::Install);
            j.name = Some("Spacewar \"x\"".into());
            j.branch = Some("beta".into());
            j.depots = vec![6, 7];
            q.add(j).unwrap();
            q.add(Job::new(6, c.install_dir(6, None).unwrap(), JobKind::Update)).unwrap();
            q.add(Job::new(7, c.install_dir(7, None).unwrap(), JobKind::Verify)).unwrap();
            q.pause(7).unwrap();
            // Как будто процесс упал посреди загрузки 5.
            q.edit(|jobs| {
                jobs[0].state = JobState::Active;
                jobs[0].created_dir = true;
                (jobs[0].done, jobs[0].total, jobs[0].stage) = (40, 100, Some(DownloadStage::Downloading));
                Ok(())
            })
            .unwrap();
        }
        let q = c.queue();
        let jobs = q.jobs().unwrap();
        assert_eq!(jobs.len(), 3);
        let j = &jobs[0];
        assert_eq!((j.appid, j.state, j.kind), (5, JobState::Active, JobKind::Install));
        assert_eq!(
            (j.name.as_deref(), j.branch.as_deref(), &j.depots[..]),
            (Some("Spacewar \"x\""), Some("beta"), &[6, 7][..])
        );
        assert_eq!((j.done, j.total, j.stage, j.created_dir), (40, 100, Some(DownloadStage::Downloading), true));
        assert_eq!((jobs[1].kind, jobs[2].state), (JobKind::Update, JobState::Paused));
        assert!(!jobs[1].request().verify && jobs[2].request().verify);

        // Новый обработчик докачивает прерванную, пропускает паузу.
        q.run_until_empty(&MockDownloader::default()).unwrap();
        assert_eq!(ids(&q), [(7, JobState::Paused)]);
    }

    #[test]
    fn pause_resume_cancel_while_running() {
        let d = TestDir::new();
        let c = carafe(&d.path().join("carafe"));
        let q = Arc::new(c.queue());
        q.add(Job::new(1, c.install_dir(1, None).unwrap(), JobKind::Install)).unwrap();
        q.add(Job::new(2, c.install_dir(2, None).unwrap(), JobKind::Install)).unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker = {
            let (q, shutdown) = (q.clone(), shutdown.clone());
            std::thread::spawn(move || q.run_forever(&slow(), &shutdown))
        };
        // Второй обработчик не нужен.
        wait_for("старт 1", || q.jobs().unwrap()[0].state == JobState::Active);
        assert!(matches!(c.queue().run_until_empty(&slow()), Err(Error::QueueBusy)));
        wait_for("прогресс 1", || q.jobs().unwrap()[0].done > 5);

        // Пауза активной: следующей идёт 2.
        q.pause(1).unwrap();
        wait_for("старт 2", || ids(&q) == [(1, JobState::Paused), (2, JobState::Active)]);
        let partial = q.jobs().unwrap()[0].done;
        assert!(partial > 0 && partial < 100);

        // Отмена активной из «другого процесса»: просто правка файла.
        wait_for("прогресс 2", || q.jobs().unwrap()[1].done > 2);
        c.queue().cancel(2).unwrap();
        wait_for("отмена 2", || ids(&q) == [(1, JobState::Paused)] && !d.path().join("carafe/games/2").exists());

        // Продолжение 1 — с того же места.
        q.resume(1).unwrap();
        wait_for("готово 1", || q.jobs().unwrap().is_empty());
        assert!(d.path().join("carafe/games/1").join(MOCK_EXE).is_file());

        shutdown.store(true, Ordering::Relaxed);
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn shutdown_requeues() {
        let d = TestDir::new();
        let c = carafe(&d.path().join("carafe"));
        let q = Arc::new(c.queue());
        q.add(Job::new(1, c.install_dir(1, None).unwrap(), JobKind::Install)).unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker = {
            let (q, shutdown) = (q.clone(), shutdown.clone());
            std::thread::spawn(move || q.run_forever(&slow(), &shutdown))
        };
        wait_for("прогресс", || q.jobs().unwrap()[0].done > 2);
        shutdown.store(true, Ordering::Relaxed);
        worker.join().unwrap().unwrap();
        let j = &c.queue().jobs().unwrap()[0];
        assert_eq!(j.state, JobState::Queued);
        assert!(j.done > 2);
        // Папка с недокачанным на месте — продолжим после запуска.
        assert!(d.path().join("carafe/games/1/.mock-download").is_file());
    }

    #[test]
    fn unavailable_library_fails_job() {
        let d = TestDir::new();
        let c = carafe(&d.path().join("carafe"));
        let q = c.queue();
        q.add(Job::new(1, d.path().join("Volumes/Gone/1"), JobKind::Install)).unwrap();
        q.run_until_empty(&MockDownloader::default()).unwrap();
        let j = &q.jobs().unwrap()[0];
        assert_eq!(j.state, JobState::Failed);
        assert!(j.error.as_deref().unwrap().contains("недоступна"));
        assert!(!d.path().join("Volumes").exists());
    }

    #[test]
    fn update_cancel_keeps_game() {
        let d = TestDir::new();
        let c = carafe(&d.path().join("carafe"));
        d.file("carafe/games/3/Game.exe", b"MZ");
        let q = c.queue();
        let mut j = Job::new(3, c.install_dir(3, None).unwrap(), JobKind::Update);
        j.created_dir = true; // даже если флаг почему-то стоит
        q.add(j).unwrap();
        q.cancel(3).unwrap();
        assert!(d.path().join("carafe/games/3/Game.exe").is_file());
    }

    #[test]
    fn meter() {
        let mut m = Meter::default();
        let t = Instant::now();
        m.sample_at(t, 0);
        m.sample_at(t + Duration::from_millis(500), 100);
        assert_eq!(m.speed, None);
        m.sample_at(t + Duration::from_secs(2), 2000);
        assert_eq!(m.speed, Some(1000.0));
        m.sample_at(t + Duration::from_secs(3), 4000);
        assert_eq!(m.speed, Some(0.3 * 2000.0 + 0.7 * 1000.0));
    }
}
