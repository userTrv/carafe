//! Запуск внешних программ: wine, steamcmd, soju-bridge, soju-steamd.

use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::error::{Error, Result};

/// Сообщения ядра. CLI печатает `Step` как `==> текст` (так делал `log` в bash), `Output` — как есть.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Log {
    /// Шаг carafe: «качаю движок», «Steam Cloud: pull».
    Step(String),
    /// Строка вывода внешней программы (stderr soju-bridge, soju-steamd, wine).
    Output(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stream {
    Out,
    Err,
}

pub(crate) fn program_name(cmd: &Command) -> String {
    let p = std::path::Path::new(cmd.get_program());
    p.file_name().unwrap_or(p.as_os_str()).to_string_lossy().into_owned()
}

/// Код выхода как у шелла: по сигналу — 128 + номер.
pub fn exit_code(status: ExitStatus) -> i32 {
    status.code().or_else(|| status.signal().map(|s| 128 + s)).unwrap_or(1)
}

pub(crate) fn check(cmd: &Command, status: ExitStatus) -> Result<()> {
    if status.success() { Ok(()) } else { Err(Error::Failed { program: program_name(cmd), status }) }
}

pub(crate) fn spawn_err(cmd: &Command) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::Spawn { program: program_name(cmd), source }
}

/// Запуск с терминалом как есть: steamcmd спрашивает пароль, curl рисует прогресс.
pub(crate) fn run_inherit(cmd: &mut Command) -> Result<()> {
    let status = cmd.status().map_err(spawn_err(cmd))?;
    check(cmd, status)
}

/// Запуск с построчным разбором stderr (и stdout, если `capture_stdout`). Строки приходят в
/// `on_line` в этом же потоке, пока программа работает.
///
/// Ждём выхода самой программы, а не конца потоков: wineserver и прочие процессы Wine наследуют
/// stderr и живут дольше лаунчера. После выхода дочитываем то, что успело прийти.
pub(crate) fn run_lines(
    cmd: &mut Command,
    capture_stdout: bool,
    on_line: &mut dyn FnMut(Stream, &str),
) -> Result<ExitStatus> {
    cmd.stderr(Stdio::piped());
    if capture_stdout {
        cmd.stdout(Stdio::piped());
    }
    let mut child = cmd.spawn().map_err(spawn_err(cmd))?;
    let (tx, rx) = mpsc::channel::<(Stream, String)>();
    if let Some(out) = child.stdout.take() {
        pump(out, Stream::Out, tx.clone());
    }
    if let Some(err) = child.stderr.take() {
        pump(err, Stream::Err, tx.clone());
    }
    drop(tx);

    let status = loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok((s, line)) => on_line(s, &line),
            Err(mpsc::RecvTimeoutError::Disconnected) => break child.wait(),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some(st) = child.try_wait().transpose() {
            break st;
        }
    };
    let status = status.map_err(spawn_err(cmd))?;
    // Хвост вывода: пока потоки не закроются или не замолчат, но не дольше пары секунд —
    // оставшийся процесс Wine может писать и дальше.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while let Ok((s, line)) = rx.recv_timeout(Duration::from_millis(300)) {
        on_line(s, &line);
        if std::time::Instant::now() > deadline {
            break;
        }
    }
    Ok(status)
}

fn pump(r: impl Read + Send + 'static, s: Stream, tx: mpsc::Sender<(Stream, String)>) {
    thread::spawn(move || {
        let mut r = BufReader::new(r);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match r.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => return,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf);
                    let line = line.trim_end_matches(['\n', '\r']);
                    if tx.send((s, line.to_string())).is_err() {
                        return;
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_and_status() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "echo out1; echo err1 >&2; printf 'err2\\r\\n' >&2; exit 3"]);
        let mut got = vec![];
        let st = run_lines(&mut cmd, true, &mut |s, l| got.push((s, l.to_string()))).unwrap();
        assert_eq!(exit_code(st), 3);
        got.sort_by_key(|(s, _)| *s == Stream::Err);
        assert_eq!(got, [(Stream::Out, "out1".into()), (Stream::Err, "err1".into()), (Stream::Err, "err2".into())]);
        assert!(matches!(check(&cmd, st), Err(Error::Failed { program, .. }) if program == "sh"));
    }

    #[test]
    fn does_not_wait_for_grandchildren() {
        // Внук держит stderr открытым дольше родителя — как wineserver после лаунчера wine.
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "(sleep 5 >&2 &) ; echo done >&2"]);
        let t = std::time::Instant::now();
        let mut got = vec![];
        run_lines(&mut cmd, false, &mut |_, l| got.push(l.to_string())).unwrap();
        assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
        assert_eq!(got, ["done"]);
    }
}
