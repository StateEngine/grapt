//! The TUI in a real pseudo-terminal: keystrokes in, the rebuilt screen out.
//! Startup terminal queries get a minimal answer, as a terminal emulator gives.
use super::screen::Screen;
use super::*;
use tokio::time::Instant;

const COLUMNS: u16 = 120;
const ROWS: u16 = 40;

struct Output {
    screen: Screen,
    closed: bool,
}

pub(super) struct Terminal {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    output: Arc<(Mutex<Output>, Condvar)>,
    child: Process,
    reader: Option<thread::JoinHandle<()>>,
}

#[cfg(unix)]
struct Process {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    master: Option<Box<dyn portable_pty::MasterPty + Send>>,
}

#[cfg(windows)]
struct Process {
    child: conpty_oxide::blocking::Child,
    controller: Option<conpty_oxide::PtyController>,
}

impl Terminal {
    /// Every output byte is also kept in `log`, so a failure leaves the screen behind.
    pub(super) fn spawn(
        program: &Path,
        arguments: &[&str],
        environment: &[(&str, &Path)],
        log: &Path,
    ) -> Self {
        let log = fs::File::create(log)
            .unwrap_or_else(|error| panic!("{} terminal log: {error}", log.display()));
        let (child, reader, writer) = open(program, arguments, environment);
        let writer = Arc::new(Mutex::new(writer));
        let output = Arc::new((
            Mutex::new(Output {
                screen: Screen::new(ROWS, COLUMNS),
                closed: false,
            }),
            Condvar::new(),
        ));
        let reader = {
            let (writer, output) = (writer.clone(), output.clone());
            thread::Builder::new()
                .name("busy-seat-terminal".into())
                .spawn(move || pump(reader, &writer, &output, log))
                .unwrap_or_else(|error| panic!("terminal reader failed to start: {error}"))
        };
        Self {
            writer,
            output,
            child,
            reader: Some(reader),
        }
    }

    pub(super) fn send(&self, bytes: &[u8]) {
        let mut writer = locked(&self.writer);
        writer
            .write_all(bytes)
            .and_then(|()| writer.flush())
            .unwrap_or_else(|error| panic!("terminal input failed: {error}"));
    }

    /// When the screen first satisfies `shown`, waiting at most `within`.
    pub(super) fn wait_until(
        &self,
        within: Duration,
        shown: impl Fn(&str) -> bool,
    ) -> Option<Instant> {
        let deadline = Instant::now() + within;
        let (lock, wake) = &*self.output;
        let mut output = locked(lock);
        loop {
            if shown(&output.screen.text()) {
                return Some(Instant::now());
            }
            let now = Instant::now();
            if output.closed || now >= deadline {
                return None;
            }
            output = match wake.wait_timeout(output, deadline - now) {
                Ok((output, _)) => output,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
    }

    /// The characters on screen now, row by row.
    pub(super) fn screen(&self) -> String {
        locked(&self.output.0).screen.text()
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.child.kill();
        if let Some(reader) = self.reader.take() {
            if reader.join().is_err() && !thread::panicking() {
                panic!("terminal reader panicked");
            }
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Startup queries a TUI may block on, with the reply a plain terminal gives.
const QUERIES: [(&[u8], &[u8]); 3] = [
    (b"\x1b[6n", b"\x1b[1;1R"),
    (b"\x1b[c", b"\x1b[?1;2c"),
    (b"\x1b[?u", b"\x1b[?0u"),
];

fn pump(
    mut reader: Box<dyn Read + Send>,
    writer: &Mutex<Box<dyn Write + Send>>,
    output: &(Mutex<Output>, Condvar),
    mut log: fs::File,
) {
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let count = match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => count,
        };
        let bytes = &chunk[..count];
        log.write_all(bytes)
            .unwrap_or_else(|error| panic!("terminal log write failed: {error}"));
        for (query, reply) in QUERIES {
            if find(bytes, query) {
                let mut writer = locked(writer);
                // A closed terminal makes the reply moot; the reader ends next.
                let _ = writer.write_all(reply).and_then(|()| writer.flush());
            }
        }
        let (lock, wake) = output;
        locked(lock).screen.feed(bytes);
        wake.notify_all();
    }
    let (lock, wake) = output;
    locked(lock).closed = true;
    wake.notify_all();
}

type Opened = (Process, Box<dyn Read + Send>, Box<dyn Write + Send>);

#[cfg(unix)]
fn open(program: &Path, arguments: &[&str], environment: &[(&str, &Path)]) -> Opened {
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            rows: ROWS,
            cols: COLUMNS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap_or_else(|error| panic!("terminal allocation failed: {error}"));
    let mut command = portable_pty::CommandBuilder::new(program);
    command.args(arguments);
    command.env("TERM", "xterm-256color");
    for (name, value) in environment {
        command.env(name, value);
    }
    let reader = pair
        .master
        .try_clone_reader()
        .unwrap_or_else(|error| panic!("terminal reader failed: {error}"));
    let writer = pair
        .master
        .take_writer()
        .unwrap_or_else(|error| panic!("terminal writer failed: {error}"));
    let child = pair
        .slave
        .spawn_command(command)
        .unwrap_or_else(|error| panic!("TUI spawn failed: {error}"));
    (
        Process {
            child,
            master: Some(pair.master),
        },
        reader,
        writer,
    )
}

#[cfg(windows)]
fn open(program: &Path, arguments: &[&str], environment: &[(&str, &Path)]) -> Opened {
    use conpty_oxide::{blocking, ConPtyBackend, SessionOptions, Size};
    let mut command = blocking::Command::new(program);
    command.args(arguments);
    for (name, value) in environment {
        command.env(name, value);
    }
    let backend = ConPtyBackend::system()
        .unwrap_or_else(|error| panic!("system ConPTY unavailable: {error}"));
    let size = Size::try_new(COLUMNS, ROWS).unwrap_or_else(|error| panic!("console size: {error}"));
    let parts = command
        .spawn_with(SessionOptions::new().backend(backend).size(size))
        .unwrap_or_else(|error| panic!("TUI spawn failed: {error}"))
        .into_parts();
    (
        Process {
            child: parts.child,
            controller: Some(parts.controller),
        },
        Box::new(parts.output),
        Box::new(parts.input),
    )
}

impl Process {
    /// Ends the TUI, then closes the terminal so the reader sees end of output.
    fn kill(&mut self) {
        // The TUI may already have exited; a failed kill then changes nothing.
        let _ = self.child.kill();
        let _ = self.child.wait();
        #[cfg(unix)]
        drop(self.master.take());
        #[cfg(windows)]
        drop(self.controller.take());
    }
}
