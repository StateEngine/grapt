//! Bounded installer feedback; disk work keeps the existing verification path.
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::sync::{
    mpsc::{sync_channel, RecvTimeoutError, SyncSender},
    Arc, Mutex,
};
use std::thread::JoinHandle;
use std::time::Duration;

const INTERVAL: Duration = Duration::from_secs(2);
const LINE_BYTES: usize = 76;
pub(super) const VERIFY_PACKAGE: &str = "Verifying package files";

#[derive(Default)]
struct FileProgress {
    name: String,
    index: usize,
    total: usize,
    bytes: u64,
    expected: u64,
}
impl FileProgress {
    fn row(&self, phase: &str) -> String {
        if self.name.is_empty() {
            return phase.to_owned();
        }
        let count = if self.total == 0 {
            format!("file {}", self.index)
        } else {
            format!("file {}/{}", self.index, self.total)
        };
        format!(
            "{count} · {} · {}",
            if self.expected == 0 {
                format!("{} bytes", self.bytes)
            } else {
                format!("{}/{} bytes", self.bytes, self.expected)
            },
            self.name
        )
        .chars()
        .scan(0, |bytes, ch| {
            *bytes += ch.len_utf8();
            (*bytes <= LINE_BYTES).then_some(ch)
        })
        .collect()
    }
}

// Clear the whole byte-bounded row before repainting: UTF-8 bytes are not terminal
// cells, especially for wide or combining characters. CR and spaces also work on
// ordinary ttys without requiring ANSI erase-line support.
fn terminal_row(row: &str, finished: bool) -> String {
    format!(
        "\r{}\r{row}{}",
        " ".repeat(LINE_BYTES),
        if finished { "\n" } else { "" }
    )
}

pub(super) struct Reporter {
    state: Arc<Mutex<FileProgress>>,
    stop: Option<SyncSender<()>>,
    worker: Option<JoinHandle<()>>,
}
impl Reporter {
    fn start(
        enabled: bool,
        terminal: bool,
        phase: String,
        interval: Duration,
        mut emit: impl FnMut(&str) + Send + 'static,
    ) -> Result<Self, String> {
        let state = Arc::new(Mutex::new(FileProgress::default()));
        // Redirected Windows handles may be decoded through an OEM codepage.
        let phase = format!("{phase}{}", if terminal { "…" } else { "..." });
        if enabled {
            emit(&format!("{phase}\n"));
        }
        let (stop, worker) = if enabled && terminal {
            let (stop, receiver) = sync_channel(1);
            let shared = Arc::clone(&state);
            let worker = std::thread::Builder::new()
                .name("install-progress".into())
                .spawn(move || {
                    while let Err(RecvTimeoutError::Timeout) = receiver.recv_timeout(interval) {
                        let row = shared
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner())
                            .row(&phase);
                        emit(&terminal_row(&row, false));
                    }
                    let row = shared
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .row(&phase);
                    emit(&terminal_row(&row, true));
                })
                .map_err(|error| format!("installer progress cannot start: {:?}", error.kind()))?;
            (Some(stop), Some(worker))
        } else {
            (None, None)
        };
        Ok(Self {
            state,
            stop,
            worker,
        })
    }
    pub(super) fn file(&self, index: usize, total: usize, path: &Path, expected: u64) {
        // Manifest paths are bounded; escaped controls cannot alter terminal rows.
        let name = path
            .to_string_lossy()
            .chars()
            .flat_map(char::escape_debug)
            .take(48)
            .collect();
        *self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = FileProgress {
            name,
            index,
            total,
            bytes: 0,
            expected,
        };
    }
    pub(super) fn bytes(&self, bytes: u64) {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .bytes = bytes;
    }
}
impl Drop for Reporter {
    fn drop(&mut self) {
        if let Some(stop) = &self.stop {
            let _ = stop.try_send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
pub(super) fn during<T>(
    enabled: bool,
    text: impl Into<String>,
    work: impl FnOnce(&Reporter) -> Result<T, String>,
) -> Result<T, String> {
    let reporter = Reporter::start(
        enabled,
        std::io::stdout().is_terminal() && std::io::stderr().is_terminal(),
        text.into(),
        INTERVAL,
        |row| {
            let mut stderr = std::io::stderr().lock();
            let _ = stderr.write_all(row.as_bytes());
            let _ = stderr.flush();
        },
    )?;
    work(&reporter)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn observed(
        receiver: &std::sync::mpsc::Receiver<String>,
        wanted: &str,
    ) -> Result<String, String> {
        for _ in 0..50 {
            match receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(row) if row.contains(wanted) => return Ok(row),
                Ok(_) | Err(RecvTimeoutError::Timeout) => {}
                Err(error) => return Err(format!("progress closed before {wanted}: {error}")),
            }
        }
        Err(format!("progress did not report {wanted} within 50 polls"))
    }
    #[test]
    fn held_disk_work_reports_file_and_measured_bytes_then_stops() -> Result<(), String> {
        let (output, receiver) = sync_channel(16);
        let reporter = Reporter::start(
            true,
            true,
            VERIFY_PACKAGE.into(),
            Duration::from_millis(20),
            move |row| {
                let _ = output.try_send(row.to_owned());
            },
        )?;
        assert!(receiver
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "initial progress missing")?
            .contains(VERIFY_PACKAGE));
        reporter.file(2, 7, Path::new("grapt.exe"), 131072);
        reporter.bytes(65536);
        let first = observed(&receiver, "65536/131072 bytes")?;
        assert!(
            first.contains("file 2/7") && first.contains("grapt.exe"),
            "{first}"
        );
        reporter.bytes(131072);
        observed(&receiver, "131072/131072 bytes")?;
        drop(reporter);
        while receiver.try_recv().is_ok() {}
        assert!(matches!(
            receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ));
        Ok(())
    }
    #[test]
    fn non_terminal_emits_one_phase_line_despite_file_updates() -> Result<(), String> {
        let (output, receiver) = sync_channel(16);
        let reporter = Reporter::start(
            true,
            false,
            "Copying package files".into(),
            Duration::from_millis(1),
            move |row| {
                let _ = output.try_send(row.to_owned());
            },
        )?;
        for index in 1..=70 {
            reporter.file(index, 70, Path::new("payload"), 1);
            reporter.bytes(1);
        }
        drop(reporter);
        assert_eq!(
            receiver.into_iter().collect::<Vec<_>>(),
            vec!["Copying package files...\n"]
        );
        Ok(())
    }
    #[test]
    fn redirected_phase_is_ascii_and_interactive_phase_keeps_ellipsis() -> Result<(), String> {
        for terminal in [false, true] {
            let (output, receiver) = sync_channel(4);
            let reporter = Reporter::start(
                true,
                terminal,
                "Verifying current installation".into(),
                Duration::from_secs(60),
                move |row| {
                    let _ = output.try_send(row.to_owned());
                },
            )?;
            let row = receiver
                .recv_timeout(Duration::from_secs(5))
                .map_err(|error| format!("phase line missing: {error}"))?;
            assert_eq!(
                row,
                if terminal {
                    "Verifying current installation…\n"
                } else {
                    "Verifying current installation...\n"
                }
            );
            assert_eq!(row.is_ascii(), !terminal);
            drop(reporter);
        }
        Ok(())
    }
    #[test]
    fn paths_and_rows_are_bounded_and_controls_are_visible() {
        let reporter = Reporter::start(false, false, String::new(), INTERVAL, |_| {})
            .unwrap_or_else(|error| panic!("silent reporter: {error}"));
        reporter.file(
            1,
            70,
            Path::new(&format!("evil\n\r\x1b{}", "Ω".repeat(2000))),
            1024,
        );
        reporter.bytes(64);
        let row = reporter
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .row(VERIFY_PACKAGE);
        assert!(row.len() <= LINE_BYTES);
        assert!(!row.chars().any(char::is_control));
        assert!(row.contains("evil\\n\\r\\u{1b}"));
    }
    // Replay only this CR/LF/plain-text protocol and the explicit Unicode fixtures
    // below. Unknown characters fail rather than silently assuming a cell width.
    fn replay_row(screen: &mut [char; LINE_BYTES], input: &str) -> Result<(), String> {
        let mut column = 0;
        for ch in input.chars() {
            let width = match ch {
                '\r' => {
                    column = 0;
                    continue;
                }
                '\n' => continue,
                '\u{301}' => 0,
                '文' | '件' => 2,
                'é' | '·' => 1,
                ch if ch.is_ascii() && !ch.is_ascii_control() => 1,
                _ => return Err(format!("unmodeled replay character: {ch:?}")),
            };
            let end = column + width;
            let Some(cells) = screen.get_mut(column..end) else {
                return Err("progress replay would wrap beyond its bounded row".into());
            };
            cells.fill(ch);
            column = end;
        }
        Ok(())
    }

    #[test]
    fn unicode_refresh_clears_a_full_ascii_row_without_stale_cells() -> Result<(), String> {
        let reporter = Reporter::start(false, false, String::new(), INTERVAL, |_| {})?;
        for name in ["café.bin", "文件.bin", "cafe\u{301}.bin"] {
            reporter.file(1, 3, Path::new(name), 1024);
            reporter.bytes(64);
            let row = reporter
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .row(VERIFY_PACKAGE);
            assert!(row.len() <= LINE_BYTES);
            assert!(!row.chars().any(char::is_control));
            // Exercise both the actual escaped filename path and raw combining
            // characters at the renderer seam, even when escape_debug escapes them.
            for row in [row.as_str(), name] {
                for finished in [false, true] {
                    let mut screen = [' '; LINE_BYTES];
                    replay_row(&mut screen, &terminal_row(&"X".repeat(LINE_BYTES), false))?;
                    assert!(screen.iter().all(|cell| *cell == 'X'));
                    let refreshed = terminal_row(row, finished);
                    assert!(refreshed.len() <= 2 * LINE_BYTES + 3);
                    assert_eq!(refreshed.ends_with('\n'), finished);
                    replay_row(&mut screen, &refreshed)?;
                    assert!(
                        !screen.contains(&'X'),
                        "stale cells after {row:?}: {screen:?}"
                    );
                }
            }
        }
        Ok(())
    }

    #[test]
    fn progress_preserves_disk_refusal() {
        assert_eq!(
            during(false, VERIFY_PACKAGE, |_| Err::<(), _>(
                "disk refusal".into()
            )),
            Err("disk refusal".into())
        );
    }
}
