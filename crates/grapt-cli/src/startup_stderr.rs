//! Stderr custody for a service that creates its own profile. Such a profile
//! has no log directory at launch, so the child's stderr is read through a
//! pipe: a bounded tail until readiness, then the profile's server log. A
//! launch that fails names that tail. The pipe is drained until it closes,
//! whatever happens to the log, so the service never blocks on stderr.

use std::collections::VecDeque;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};

/// The most startup output kept for a refusal or the log.
const TAIL_BYTES: usize = 4096;
/// How long a refusal waits for the child's stderr to close after it exited
/// or was killed; a descendant can keep the pipe open past that.
const TAIL_WAIT: Duration = Duration::from_millis(500);

enum Sink {
    Tail { kept: VecDeque<u8>, dropped: u64 },
    Log(std::fs::File),
    Discard,
}

impl Sink {
    fn accept(&mut self, bytes: &[u8]) {
        match self {
            Self::Tail { kept, dropped } => {
                kept.extend(bytes);
                let excess = kept.len().saturating_sub(TAIL_BYTES);
                kept.drain(..excess);
                *dropped = dropped.saturating_add(u64::try_from(excess).unwrap_or(u64::MAX));
            }
            Self::Log(file) => {
                if file.write_all(bytes).is_err() {
                    *self = Self::Discard;
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
    pub(crate) fn into_log(self, profile: &Path) {
        let mut sink = self.sink.lock().unwrap_or_else(PoisonError::into_inner);
        let Sink::Tail { kept, dropped } = &*sink else {
            return;
        };
        let mut next = match crate::server_log::open(profile) {
            Ok(file) => Sink::Log(file),
            Err(_) => Sink::Discard,
        };
        if *dropped > 0 {
            next.accept(
                format!("[{dropped} earlier bytes of startup output were not kept]\n").as_bytes(),
            );
        }
        let (front, back) = kept.as_slices();
        next.accept(front);
        next.accept(back);
        *sink = next;
    }

    /// A failed launch's refusal, followed by what the child wrote to stderr.
    pub(crate) async fn refusal(self, message: String) -> String {
        // The reader ends at end of input; one still running keeps draining.
        let _ = tokio::time::timeout(TAIL_WAIT, self.reader).await;
        let sink = self.sink.lock().unwrap_or_else(PoisonError::into_inner);
        let Sink::Tail { kept, dropped } = &*sink else {
            return message;
        };
        let text = String::from_utf8_lossy(&kept.iter().copied().collect::<Vec<_>>())
            .trim()
            .to_owned();
        match (text.is_empty(), *dropped) {
            (true, _) => message,
            (false, 0) => format!("{message}\nservice stderr:\n{text}"),
            (false, dropped) => {
                format!("{message}\nservice stderr (after {dropped} earlier bytes):\n{text}")
            }
        }
    }
}

#[cfg(test)]
mod tests;
