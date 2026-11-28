//! Stderr custody for a service that creates its own profile. Such a profile
//! has no log directory at launch, so the child's stderr is read through a
//! pipe: a bounded tail until readiness, then the profile's server log. A
//! launch that fails names that tail.
//!
//! The pipe reader never waits on storage. It keeps the tail in memory, or
//! hands each chunk to a log writer thread through a bounded channel and
//! counts what a full channel could not take, so the service never blocks
//! on stderr however slow the log is.

use std::collections::VecDeque;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::mpsc::{channel, error::TrySendError, Receiver, Sender};

/// The most startup output kept for a refusal or the log.
const TAIL_BYTES: usize = 4096;
/// Chunks waiting for the log writer; more are dropped and counted.
const LOG_BACKLOG: usize = 64;
/// How long a refusal waits for the child's stderr to close after it exited
/// or was killed; a descendant can keep the pipe open past that.
const TAIL_WAIT: Duration = Duration::from_millis(500);

/// One write for the log: a note on what was not written, then bytes.
type Chunk = (Option<String>, Vec<u8>);

enum Sink {
    Tail { kept: VecDeque<u8>, dropped: u64 },
    Log { chunks: Sender<Chunk>, lost: u64 },
    Discard,
}

impl Sink {
    /// Never blocks: no file I/O happens while the sink is locked.
    fn accept(&mut self, bytes: &[u8]) {
        match self {
            Self::Tail { kept, dropped } => {
                kept.extend(bytes);
                let excess = kept.len().saturating_sub(TAIL_BYTES);
                kept.drain(..excess);
                *dropped = dropped.saturating_add(u64::try_from(excess).unwrap_or(u64::MAX));
            }
            Self::Log { chunks, lost } => {
                let note = (*lost > 0)
                    .then(|| format!("[{lost} bytes of service output were not written]\n"));
                match chunks.try_send((note, bytes.to_vec())) {
                    Ok(()) => *lost = 0,
                    Err(TrySendError::Full(_)) => {
                        *lost = lost.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
                    }
                    Err(TrySendError::Closed(_)) => *self = Self::Discard,
                }
            }
            Self::Discard => {}
        }
    }
}

pub(crate) struct StartupStderr {
    sink: Arc<Mutex<Sink>>,
    reader: tokio::task::JoinHandle<()>,
}

/// A ready service's stderr on its way to the log; finish it after the
/// child exits so its last output is written before this command exits.
/// The writer ends once the pipe has closed and every sender is gone.
pub(crate) struct Forward {
    reader: tokio::task::JoinHandle<()>,
    writer: Option<std::thread::JoinHandle<()>>,
}

impl Forward {
    /// Waits at most `within` for the pipe to close and the log to be
    /// written; past that, what is still unwritten is left behind.
    pub(crate) async fn finish(self, within: Duration) {
        let deadline = tokio::time::Instant::now() + within;
        let _ = tokio::time::timeout_at(deadline, self.reader).await;
        if let Some(writer) = self.writer {
            let joined = tokio::task::spawn_blocking(move || writer.join());
            let _ = tokio::time::timeout_at(deadline, joined).await;
        }
    }
}

impl StartupStderr {
    pub(crate) fn capture(mut stderr: impl AsyncRead + Unpin + Send + 'static) -> Self {
        let sink = Arc::new(Mutex::new(Sink::Tail {
            kept: VecDeque::new(),
            dropped: 0,
        }));
        let shared = Arc::clone(&sink);
        let reader = tokio::spawn(async move {
            let mut chunk = [0u8; 4096];
            while let Ok(read @ 1..) = stderr.read(&mut chunk).await {
                shared
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .accept(&chunk[..read]);
            }
        });
        Self { sink, reader }
    }

    /// After readiness the profile exists: the captured output, then all
    /// later output, goes to its server log.
    pub(crate) fn into_log(self, profile: &Path) -> Forward {
        let writer = crate::server_log::open(profile).ok().and_then(|file| {
            let (chunks, mut pending) = channel(LOG_BACKLOG);
            std::thread::Builder::new()
                .name("service-stderr-log".into())
                .spawn(move || write_log(file, &mut pending))
                .ok()
                .map(|writer| (chunks, writer))
        });
        let mut sink = self.sink.lock().unwrap_or_else(PoisonError::into_inner);
        let writer = match (std::mem::replace(&mut *sink, Sink::Discard), writer) {
            (Sink::Tail { kept, dropped }, Some((chunks, writer))) => {
                let note = (dropped > 0).then(|| {
                    format!("[{dropped} earlier bytes of startup output were not kept]\n")
                });
                // The channel is empty, so the captured output always fits.
                let _ = chunks.try_send((note, kept.into()));
                *sink = Sink::Log { chunks, lost: 0 };
                Some(writer)
            }
            // No log: dropping its sender lets the writer, if any, finish.
            (_, writer) => writer.map(|(_, writer)| writer),
        };
        drop(sink);
        Forward {
            reader: self.reader,
            writer,
        }
    }

    /// A failed launch's refusal, followed by what the child wrote to stderr.
    pub(crate) async fn refusal(self, message: String) -> String {
        // The reader ends at end of input; one still running keeps draining.
        let _ = tokio::time::timeout(TAIL_WAIT, self.reader).await;
        let sink = self.sink.lock().unwrap_or_else(PoisonError::into_inner);
        let (kept, dropped) = match &*sink {
            Sink::Tail { kept, dropped } => (kept.iter().copied().collect::<Vec<_>>(), *dropped),
            _ => return message,
        };
        let text = String::from_utf8_lossy(&kept).trim().to_owned();
        match (text.is_empty(), dropped) {
            (true, 0) => message,
            (true, dropped) => format!(
                "{message}\nservice stderr: {dropped} earlier bytes were not kept, and the last {} are blank",
                kept.len()
            ),
            (false, 0) => format!("{message}\nservice stderr:\n{text}"),
            (false, dropped) => {
                format!("{message}\nservice stderr (after {dropped} earlier bytes):\n{text}")
            }
        }
    }
}

/// The log writer: stops at the first failed write, which disconnects the
/// channel so the reader discards the rest instead of queueing it.
fn write_log(mut file: std::fs::File, chunks: &mut Receiver<Chunk>) {
    while let Some((note, bytes)) = chunks.blocking_recv() {
        let written = note
            .map_or(Ok(()), |note| file.write_all(note.as_bytes()))
            .and_then(|()| file.write_all(&bytes));
        if written.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests;
