// Copyright 2023-2026 SpaceXAI. Licensed under Apache-2.0.
// Native macOS command choice. Modified by Grapt:
// bound reads before allocation and supervise acquisition through completion.
//! Clipboard acquisition is a bounded stream owned by one short-lived process.
use std::{
    io::Read,
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

#[cfg(unix)]
const LIMIT: usize = crate::MAX_DRAFT;
#[cfg(unix)]
const TOO_LARGE: &str = "Clipboard exceeds 64 KiB; paste refused";
const UNAVAILABLE: &str = "Clipboard has no readable text";
const TIMEOUT: &str = "Clipboard read timed out";

#[cfg(unix)]
pub(super) fn read_text() -> Result<String, &'static str> {
    let mut command = Command::new(std::env::current_exe().map_err(|_| UNAVAILABLE)?);
    command.arg("--clipboard-read-text");
    // The helper owns its shorter deadline even if this frontend disappears.
    read_command(command, Duration::from_secs(4))
}

#[cfg(unix)]
pub(crate) fn helper() -> std::process::ExitCode {
    use std::io::Write;
    #[cfg(target_os = "linux")]
    let result = bounded_source(super::linux::read_text, Duration::from_secs(1));
    #[cfg(target_os = "macos")]
    let result = {
        let mut command = Command::new("/usr/bin/pbpaste");
        command.args(["-Prefer", "txt"]);
        read_command(command, Duration::from_secs(1))
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let result: Result<String, &'static str> = Err(UNAVAILABLE);
    match result.and_then(|text| {
        std::io::stdout()
            .write_all(text.as_bytes())
            .map_err(|_| UNAVAILABLE)
    }) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(TOO_LARGE) => std::process::ExitCode::from(2),
        Err(_) => std::process::ExitCode::FAILURE,
    }
}

fn bounded_source<T: Send + 'static>(
    source: impl FnOnce() -> Result<T, &'static str> + Send + 'static,
    timeout: Duration,
) -> Result<T, &'static str> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let _worker = std::thread::Builder::new()
        .name("clipboard-source".into())
        .spawn(move || {
            let _ = sender.send(source());
        })
        .map_err(|_| UNAVAILABLE)?;
    // On timeout helper main returns, terminating its stalled X11 thread.
    receiver
        .recv_timeout(timeout)
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => TIMEOUT,
            mpsc::RecvTimeoutError::Disconnected => UNAVAILABLE,
        })?
}

#[cfg(unix)]
fn read_command(command: Command, timeout: Duration) -> Result<String, &'static str> {
    String::from_utf8(read_bytes(command, timeout, LIMIT, TOO_LARGE)?).map_err(|_| UNAVAILABLE)
}
fn read_bytes(
    mut command: Command,
    timeout: Duration,
    limit: usize,
    too_large: &'static str,
) -> Result<Vec<u8>, &'static str> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| UNAVAILABLE)?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(UNAVAILABLE);
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    let reader = std::thread::Builder::new()
        .name("clipboard-read".into())
        .spawn(move || {
            let mut bytes = Vec::with_capacity(limit.min(64 * 1024) + 1);
            let result = stdout
                .take((limit + 1) as u64)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
                .map_err(|_| UNAVAILABLE);
            let _ = sender.send(result);
        });
    let Ok(reader) = reader else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(UNAVAILABLE);
    };
    let result = (|| {
        let bytes = receiver.recv_timeout(timeout).map_err(|_| TIMEOUT)??;
        if bytes.len() > limit {
            return Err(too_large);
        }
        // EOF is not process completion. An owner that closes stdout and hangs
        // must not leave the worker waiting for it indefinitely.
        for _ in 0..100 {
            if let Some(status) = child.try_wait().map_err(|_| UNAVAILABLE)? {
                return if status.success() {
                    Ok(bytes)
                } else if status.code() == Some(2) {
                    Err(too_large)
                } else {
                    Err(UNAVAILABLE)
                };
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(TIMEOUT)
    })();
    let _ = child.kill();
    let _ = child.wait();
    reader.join().map_err(|_| UNAVAILABLE)?;
    result
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn oversized_and_nonfinishing_clipboards_are_stopped_at_acquisition() {
        let mut oversized = Command::new("python3");
        // The source deliberately keeps stdout open after crossing the limit.
        // A read-to-EOF implementation times out instead of detecting overflow.
        oversized.args(["-c", "import os,time;os.write(1,b'x'*65537);time.sleep(30)"]);
        assert_eq!(
            read_command(oversized, Duration::from_secs(1)),
            Err(TOO_LARGE)
        );
        let mut stalled = Command::new("python3");
        stalled.args(["-c", "import time;time.sleep(30)"]);
        assert_eq!(read_command(stalled, Duration::from_secs(1)), Err(TIMEOUT));
    }
}

pub(super) fn read_image() -> Result<Option<Vec<u8>>, &'static str> {
    let mut command = Command::new(std::env::current_exe().map_err(|_| UNAVAILABLE)?);
    command.arg("--clipboard-read-image");
    let bytes = read_bytes(
        command,
        Duration::from_secs(7),
        grapt_image::PREPARED_BYTES,
        "Clipboard image exceeds 700 KiB; paste refused",
    )?;
    Ok((!bytes.is_empty()).then_some(bytes))
}

pub(crate) fn image_helper() -> std::process::ExitCode {
    use std::io::Write;
    let result = bounded_source(
        || {
            let mut clipboard = arboard::Clipboard::new().map_err(|_| UNAVAILABLE)?;
            let pixels = match clipboard.get_image() {
                Ok(pixels) => pixels,
                Err(arboard::Error::ContentNotAvailable) => return Ok(Vec::new()),
                Err(_) => return Err("Clipboard image could not be read"),
            };
            let width = u32::try_from(pixels.width).map_err(|_| "Clipboard image is too wide")?;
            let height = u32::try_from(pixels.height).map_err(|_| "Clipboard image is too tall")?;
            grapt_image::prepare_rgba(width, height, pixels.bytes.into_owned())
                .map(|image| image.bytes)
                .map_err(|_| "Clipboard image is invalid or too large")
        },
        Duration::from_secs(5),
    );
    match result.and_then(|bytes| std::io::stdout().write_all(&bytes).map_err(|_| UNAVAILABLE)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(_) => std::process::ExitCode::FAILURE,
    }
}
