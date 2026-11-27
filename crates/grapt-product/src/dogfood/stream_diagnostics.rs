//! Why the product ended its live event streams.
//!
//! Every client of `/api/event` (the TUI, ACP, the CLI) treats an ended stream
//! as "rebind and hydrate from history". When the product keeps ending them,
//! a client sees nothing live, and until now nothing said why. Each end the
//! hub reports (reconcile, invalidate or overflow, with its reason) becomes
//! one JSON line in `diagnostics/event-stream.jsonl` under the service root.
//! It is a bounded diagnostic file, not durable product history: nothing
//! reads it back.
//!
//! The hub's caller never waits on this: a line is handed to one writer
//! thread over a bounded channel, and when the channel is full the line is
//! dropped and counted; the next line that gets through says how many were
//! lost before it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};

use grapt_api::DogfoodEventHub;

/// The file, relative to the service root.
pub(crate) const LOG: &str = "diagnostics/event-stream.jsonl";

/// Every line's `schema`.
const SCHEMA: &str = "grapt.stream-end.v1";

/// Lines waiting for the writer before new ones are dropped.
const QUEUE: usize = 256;

/// The largest a log file may grow, counting what earlier processes left in
/// it; one final line says when lines were dropped for it, if it fits.
pub(crate) const CAP: u64 = 1024 * 1024;

/// The longest reason kept in one line.
const REASON_CAP: usize = 1024;

/// Tell `hub` to record why it ends streams, under `service_root`. Without a
/// writer thread nothing is recorded, and the hub is unaffected.
pub(crate) fn install(hub: &DogfoodEventHub, service_root: &Path) {
    let mut sink = FileSink::new(service_root.join(LOG), SCHEMA);
    if let Some(recorder) = Recorder::spawn(SCHEMA, QUEUE, move |line| sink.write(line)) {
        let _installed = hub.observe_stream_ends(Box::new(move |end| {
            recorder.offer(stream_end_fields(end));
        }));
    }
}

fn stream_end_fields(end: &grapt_api::StreamEnd<'_>) -> serde_json::Value {
    let reason: String = end.reason.chars().take(REASON_CAP).collect();
    serde_json::json!({
        "cause": end.cause.as_str(),
        "subscribers": end.subscribers,
        "reason": reason,
        "reconcile_reason": end.reconcile_reason.map(|reason| reason.as_str()),
    })
}

/// Hands lines to one writer thread without ever waiting for it.
pub(crate) struct Recorder {
    schema: &'static str,
    lines: SyncSender<String>,
    sequence: AtomicU64,
    dropped: AtomicU64,
}

impl Recorder {
    /// Start the writer. It ends when the recorder (and so the sender) is
    /// dropped and the queue is drained.
    pub(crate) fn spawn(
        schema: &'static str,
        queue: usize,
        mut sink: impl FnMut(&str) + Send + 'static,
    ) -> Option<Self> {
        let (lines, receiver) = sync_channel::<String>(queue);
        std::thread::Builder::new()
            .name("grapt-diagnostics".into())
            .spawn(move || {
                for line in receiver {
                    sink(&line);
                }
            })
            .ok()?;
        Some(Self {
            schema,
            lines,
            sequence: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        })
    }

    /// Queue one line: `fields`, an object, with the schema, a sequence
    /// number and the count of lines lost since the last one that got through.
    pub(crate) fn offer(&self, fields: serde_json::Value) {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let dropped_before = self.dropped.swap(0, Ordering::Relaxed);
        let mut line = match fields {
            serde_json::Value::Object(fields) => fields,
            _ => serde_json::Map::new(),
        };
        line.insert("schema".to_owned(), self.schema.into());
        line.insert("sequence".to_owned(), sequence.into());
        line.insert("dropped_before".to_owned(), dropped_before.into());
        let line = serde_json::Value::Object(line).to_string();
        match self.lines.try_send(line) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                // This line and the uncounted ones before it are lost.
                self.dropped
                    .fetch_add(dropped_before + 1, Ordering::Relaxed);
            }
        }
    }
}

/// The capped, regular-file-only diagnostic log, owned by the writer thread.
pub(crate) struct FileSink {
    path: PathBuf,
    schema: &'static str,
    file: Option<std::fs::File>,
    written: u64,
    capped: bool,
}

impl FileSink {
    pub(crate) fn new(path: PathBuf, schema: &'static str) -> Self {
        Self {
            path,
            schema,
            file: None,
            written: 0,
            capped: false,
        }
    }

    /// Best effort: a line that cannot be written is lost, never retried.
    /// The file never grows past `CAP`, however many processes append to it:
    /// its size is read when it is opened, so a restart gets no fresh budget.
    pub(crate) fn write(&mut self, line: &str) {
        if self.capped {
            return;
        }
        if self.file.is_none() {
            match self.open() {
                Ok((file, size)) => {
                    self.file = Some(file);
                    self.written = size;
                }
                Err(_) => return,
            }
        }
        let line = if self.written + (line.len() as u64) < CAP {
            line.to_owned()
        } else {
            // Said once per process, and only if it fits: a full file stays
            // exactly as it is.
            self.capped = true;
            let marker = serde_json::json!({
                "schema": self.schema,
                "capped": "later lines were not recorded",
            })
            .to_string();
            if self.written + marker.len() as u64 + 1 > CAP {
                return;
            }
            marker
        };
        if let Some(file) = &mut self.file {
            if file.write_all(format!("{line}\n").as_bytes()).is_ok() {
                self.written += line.len() as u64 + 1;
            } else {
                self.file = None;
            }
        }
    }

    /// The opened log and the bytes already in it.
    fn open(&self) -> std::io::Result<(std::fs::File, u64)> {
        if let Some(parent) = self.path.parent() {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            builder.create(parent)?;
        }
        // Only ever a regular file: anything else is refused before it is
        // opened, and the open does not wait (a FIFO without a reader fails at
        // once instead of blocking the writer).
        let refused = || std::io::Error::other("diagnostic log is not a regular file");
        match std::fs::symlink_metadata(&self.path) {
            Ok(metadata) if !metadata.is_file() => return Err(refused()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600).custom_flags(NONBLOCK);
        }
        let file = options.open(&self.path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(refused());
        }
        Ok((file, metadata.len()))
    }
}

/// `O_NONBLOCK`, which std does not name (as in `tools/skill.rs`): 0o4000 on
/// the supported Linux targets, 0x4 on the Apple and BSD systems.
#[cfg(any(target_os = "linux", target_os = "android"))]
const NONBLOCK: i32 = 0o4000;
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
const NONBLOCK: i32 = 0x0004;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier, Mutex};
    use std::time::Duration;

    fn end(reason: &str) -> serde_json::Value {
        serde_json::json!({"cause": "invalidate", "subscribers": 1, "reason": reason})
    }

    #[test]
    fn every_global_reconcile_offers_one_structured_line_with_its_fixed_reason(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use grapt_api::u3a::{ServerConnectedEvent, WireId};
        use grapt_api::{ReconcileReason, SseLimits};
        let (send, receive) = sync_channel(6);
        let recorder = Recorder::spawn(SCHEMA, 6, move |line| {
            send.try_send(line.to_owned())
                .unwrap_or_else(|error| panic!("diagnostic fixture sink: {error}"));
        })
        .ok_or("diagnostic writer refused")?;
        let hub = DogfoodEventHub::try_new(
            SseLimits::production()?,
            ServerConnectedEvent::new(
                WireId::parse("evt_diag".to_owned()).ok_or("event identity")?,
            ),
        )?;
        assert!(hub.observe_stream_ends(Box::new(move |end| {
            recorder.offer(stream_end_fields(end));
        })));
        hub.reconcile_because(ReconcileReason::UnpublishedAdvisoryNotice, "notice");
        hub.reconcile_because(ReconcileReason::UnrepresentableFrame, "frame");
        hub.invalidate_because("projection");
        drop(hub); // The owned observer drops its recorder; the writer drains and ends.
        for (index, reason) in [
            "unpublished_advisory_notice",
            "unrepresentable_frame",
            "projection_failure",
        ]
        .into_iter()
        .enumerate()
        {
            let line = receive.recv_timeout(Duration::from_secs(5))?;
            let value: serde_json::Value = serde_json::from_str(&line)?;
            assert_eq!(value["schema"], SCHEMA);
            assert_eq!(value["sequence"], index + 1);
            assert_eq!(value["reconcile_reason"], reason);
            assert_eq!(value["subscribers"], 0);
            assert_eq!(value["dropped_before"], 0);
        }
        assert!(
            matches!(
                receive.recv_timeout(Duration::from_secs(5)),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
            ),
            "the writer must finish after exactly one line per close"
        );
        Ok(())
    }

    #[test]
    fn a_stalled_sink_never_holds_up_the_hub_and_its_losses_are_counted() {
        // The writer is stuck on its first line until released.
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let written = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let (sink_entered, sink_release, sink_written) = (
            Arc::clone(&entered),
            Arc::clone(&release),
            Arc::clone(&written),
        );
        let mut first = true;
        let recorder = Recorder::spawn(SCHEMA, 4, move |line| {
            if first {
                first = false;
                sink_entered.wait();
                sink_release.wait();
            }
            if let (Ok(mut written), Ok(value)) = (sink_written.lock(), serde_json::from_str(line))
            {
                written.push(value);
            }
        })
        .unwrap_or_else(|| panic!("writer thread"));
        recorder.offer(end("first"));
        entered.wait();
        // The queue holds 4; the rest must be dropped without waiting.
        let started = tokio::time::Instant::now();
        for _ in 0..1000 {
            recorder.offer(end("flood"));
        }
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "offering blocked for {:?}",
            started.elapsed()
        );
        release.wait();
        // Let the writer drain the queued lines, then offer one more.
        let drained = tokio::time::Instant::now() + Duration::from_secs(5);
        while written.lock().map(|lines| lines.len()).unwrap_or_default() < 5
            && tokio::time::Instant::now() < drained
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        recorder.offer(end("after"));
        drop(recorder);
        // The writer drains and ends once the recorder is gone.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let lines = loop {
            let lines = written
                .lock()
                .map(|lines| lines.clone())
                .unwrap_or_default();
            if lines.iter().any(|line| line["reason"] == "after")
                || tokio::time::Instant::now() > deadline
            {
                break lines;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let after = lines
            .iter()
            .find(|line| line["reason"] == "after")
            .unwrap_or_else(|| panic!("the line after the stall never arrived: {lines:?}"));
        // first + 4 queued reached the sink; 996 floods were lost and counted.
        assert_eq!(after["dropped_before"], 996, "{lines:?}");
        assert_eq!(lines.len(), 6, "{lines:?}");
    }

    #[test]
    fn a_full_or_reopened_log_never_grows_past_its_cap() {
        let size = |path: &Path| std::fs::metadata(path).map_or(0, |metadata| metadata.len());
        let fill = |path: &Path, bytes: u64| {
            let mut text = "x".repeat(bytes as usize - 1);
            text.push('\n');
            std::fs::write(path, text).unwrap_or_else(|error| panic!("{error}"));
        };
        for schema in [SCHEMA, super::super::provider_errors::SCHEMA] {
            let root = std::env::temp_dir().join(format!(
                "grapt-diag-cap-{}-{}",
                std::process::id(),
                schema.len()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap_or_else(|error| panic!("{error}"));
            let log = root.join("log.jsonl");

            // A full file stays exactly as it is, cap line included.
            fill(&log, CAP);
            FileSink::new(log.clone(), schema).write("{\"line\":1}");
            assert_eq!(size(&log), CAP, "{schema}: a full file grew");

            // A line that fits is kept; nothing after it may pass the cap,
            // and a cap line that would not fit is not written.
            fill(&log, CAP - 20);
            let mut sink = FileSink::new(log.clone(), schema);
            sink.write("{\"line\":12345}");
            assert_eq!(size(&log), CAP - 5, "{schema}");
            sink.write("{\"line\":12345}");
            assert_eq!(size(&log), CAP - 5, "{schema}: passed the cap");

            // Every restart reads the size first: ten processes that each
            // offer a line too large for the room left never pass the cap.
            fill(&log, CAP - 500);
            let large = format!("{{\"line\":\"{}\"}}", "y".repeat(600));
            for _ in 0..10 {
                FileSink::new(log.clone(), schema).write(&large);
                assert!(size(&log) <= CAP, "{schema}: {} > {CAP}", size(&log));
            }
            let settled = size(&log);
            FileSink::new(log.clone(), schema).write(&large);
            assert_eq!(size(&log), settled, "{schema}: a settled file kept growing");
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_in_place_of_the_log_is_refused_without_blocking() {
        let root =
            std::env::temp_dir().join(format!("grapt-stream-diag-fifo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("diagnostics")).unwrap_or_else(|error| panic!("{error}"));
        let made = std::process::Command::new("mkfifo")
            .arg(root.join(LOG))
            .status()
            .unwrap_or_else(|error| panic!("mkfifo: {error}"));
        assert!(made.success());
        let mut sink = FileSink::new(root.join(LOG), SCHEMA);
        let started = tokio::time::Instant::now();
        sink.write("{\"line\":1}");
        sink.write("{\"line\":2}");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(sink.file.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
