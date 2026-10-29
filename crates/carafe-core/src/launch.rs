//! `game run`: облако pull → мост → игра → облако push, с событиями для GUI.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

use crate::Carafe;
use crate::cloud::{CloudEntry, CloudMode, CloudReport};
use crate::error::{Error, Result};
use crate::game;
use crate::process::{self, Log};

/// Ход запуска игры. Приходят по порядку; облачных нет, если облако выключено или нет моста.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchEvent {
    /// exe найден, обходы применены, мост поставлен (или нет).
    Prepared {
        exe: PathBuf,
        bridge: Option<PathBuf>,
    },
    CloudPullStarted,
    /// Файл скачан из облака (или уже совпадал — в pull такие не печатаются).
    CloudEntry(CloudEntry),
    CloudPulled(CloudReport),
    /// Не скачалось: игра идёт с локальными сейвами, выгрузки после неё не будет.
    CloudPullFailed {
        error: String,
    },
    /// Хелпер моста поднялся и ждёт игру на этом порту.
    BridgeStarted {
        port: u16,
    },
    /// wine с игрой запущен.
    GameStarted,
    /// Игра закрыта и отключилась от Steam (хелпер моста вышел).
    GameExited {
        code: i32,
    },
    CloudPushStarted,
    CloudPushed(CloudReport),
    CloudPushFailed {
        error: String,
    },
}

/// Строка soju-bridge о готовности хелпера: `==> мост в Steam: порт 12345` (с жирным ANSI).
fn bridge_port(line: &str) -> Option<u16> {
    let rest = line.split("мост в Steam: порт ").nth(1)?;
    rest.trim_start().chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok()
}

impl Carafe {
    /// Запустить игру и дождаться её выхода. Возвращает код выхода wine.
    ///
    /// С мостом: Steam Cloud pull (ошибка не мешает играть), хелпер моста, игра; хелпер живёт,
    /// пока игра не отключится от Steam, — лаунчер wine может выйти раньше; затем push.
    /// Без моста облака нет: хелпер облака — часть моста.
    pub fn run_game(&self, appid: u32, args: &[OsString], on_event: &mut dyn FnMut(LaunchEvent)) -> Result<i32> {
        let dir = self.installed_dir(appid)?;
        let exe =
            game::find_game_exe(&dir, self.config.exe.as_deref()).ok_or_else(|| Error::ExeNotFound(dir.clone()))?;
        let wine = self.wine()?;
        game::apply_quirks(&dir, &wine.windows_dlls(), &|l| self.log(l))?;
        let bridge = self.apply_bridge(&dir)?;
        on_event(LaunchEvent::Prepared { exe: exe.clone(), bridge: bridge.clone() });
        // Префикс — до облака: сейвам нужен пользователь в drive_c/users.
        let mut cmd = self.exe_command(&exe, args)?;

        let Some(bridge) = bridge else {
            self.log(Log::Step(format!("запускаю {}", exe.display())));
            let mut child = cmd.spawn().map_err(process::spawn_err(&cmd))?;
            on_event(LaunchEvent::GameStarted);
            let status = child.wait().map_err(process::spawn_err(&cmd))?;
            let code = process::exit_code(status);
            on_event(LaunchEvent::GameExited { code });
            return Ok(code);
        };

        // Облако до запуска: игра должна увидеть свежие сейвы.
        let mut cloud = self.config.cloud;
        if cloud {
            on_event(LaunchEvent::CloudPullStarted);
            match self.cloud_sync(appid, CloudMode::Pull, &mut |e| on_event(LaunchEvent::CloudEntry(e.clone()))) {
                Ok(r) => on_event(LaunchEvent::CloudPulled(r)),
                Err(e) => {
                    self.log(Log::Output(format!("carafe: {e}")));
                    self.log(Log::Step(
                        "Steam Cloud: не удалось скачать сейвы — играю с локальными, выгружать после игры не буду"
                            .into(),
                    ));
                    on_event(LaunchEvent::CloudPullFailed { error: e.to_string() });
                    cloud = false;
                }
            }
        }

        self.log(Log::Step(format!("запускаю {}", exe.display())));
        // soju-bridge exec поднимает хелпер, запускает wine и ждёт обоих.
        let mut b = Command::new(self.config.soju_dir()?.join("bin/soju-bridge"));
        b.arg("exec").arg(&bridge).arg(appid.to_string()).arg(cmd.get_program()).args(cmd.get_args());
        if let Some(d) = cmd.get_current_dir() {
            b.current_dir(d);
        }
        for (k, v) in cmd.get_envs() {
            match v {
                Some(v) => b.env(k, v),
                None => b.env_remove(k),
            };
        }
        b.env("SOJU_BRIDGE_LOG", self.config.bridge_log(appid));
        let status = process::run_lines(&mut b, false, &mut |_, line| {
            if let Some(port) = bridge_port(line) {
                on_event(LaunchEvent::BridgeStarted { port });
                self.log(Log::Output(line.into()));
                on_event(LaunchEvent::GameStarted);
            } else {
                self.log(Log::Output(line.into()));
            }
        })?;
        let code = process::exit_code(status);
        on_event(LaunchEvent::GameExited { code });

        // Хелпер выходит, когда игра отключилась, — значит, сейвы уже на диске.
        if cloud {
            on_event(LaunchEvent::CloudPushStarted);
            match self.cloud_sync(appid, CloudMode::Push, &mut |e| on_event(LaunchEvent::CloudEntry(e.clone()))) {
                Ok(r) => on_event(LaunchEvent::CloudPushed(r)),
                Err(e) => {
                    self.log(Log::Output(format!("carafe: {e}")));
                    self.log(Log::Step(format!(
                        "Steam Cloud: выгрузка не удалась — повтори: carafe cloud push {appid}"
                    )));
                    on_event(LaunchEvent::CloudPushFailed { error: e.to_string() });
                }
            }
        }
        Ok(code)
    }

    /// Произвольный exe в префиксе; ждёт выхода wine.
    pub fn run_exe(&self, exe: &std::path::Path, args: &[OsString]) -> Result<i32> {
        let mut cmd = self.exe_command(exe, args)?;
        let status = cmd.status().map_err(process::spawn_err(&cmd))?;
        Ok(process::exit_code(status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_line() {
        assert_eq!(bridge_port("\u{1b}[1m==> мост в Steam: порт 52341\u{1b}[0m"), Some(52341));
        assert_eq!(bridge_port("==> мост в Steam: порт 7"), Some(7));
        assert_eq!(bridge_port("==> мост готов: /x"), None);
        assert_eq!(bridge_port("мост в Steam: порт abc"), None);
    }
}
