//! Read-only launch observations. They never authorize or retry a process.
use serde_json::{json, Value};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static ACTIVE: AtomicUsize = AtomicUsize::new(0);
const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;
const RECENT_MS: u64 = 30_000;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const SHARE_READ_WRITE_DELETE: u32 = 0x7;

#[derive(Debug)]
struct FileObservation {
    handle: same_file::Handle,
    created: Option<u64>,
    written: Option<u64>,
    size: u64,
    attributes: u32,
}

#[derive(Debug)]
struct Snapshot(Result<FileObservation, (String, Option<i32>)>);

impl Snapshot {
    fn read(path: &Path) -> Self {
        Self(
            (|| {
                // Query identity/metadata without reading the executable or
                // restricting writes, replacement or deletion during launch.
                let file = std::fs::OpenOptions::new()
                    .read(true)
                    .access_mode(0)
                    .share_mode(SHARE_READ_WRITE_DELETE)
                    .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                    .open(path)?;
                let handle = same_file::Handle::from_file(file)?;
                let metadata = handle.as_file().metadata()?;
                Ok(FileObservation {
                    handle,
                    created: nonzero(metadata.creation_time()),
                    written: nonzero(metadata.last_write_time()),
                    size: metadata.file_size(),
                    attributes: metadata.file_attributes(),
                })
            })()
            .map_err(|error: std::io::Error| (format!("{:?}", error.kind()), error.raw_os_error())),
        )
    }

    fn wire(&self, observed_ms: Option<u64>) -> Value {
        match &self.0 {
            Ok(file) => json!({
                "created_filetime":file.created,"written_filetime":file.written,
                "recently_created":recent(file.created, observed_ms),
                "recently_written":recent(file.written, observed_ms),
                "size":file.size,"attributes":file.attributes,
            }),
            Err((kind, code)) => json!({"unreadable":kind,"win32_error":code}),
        }
    }

    fn compare(&self, after: &Self, observed_ms: Option<u64>) -> Value {
        let change = match (&self.0, &after.0) {
            (Ok(before), Ok(after)) => Some((
                before.handle != after.handle,
                before.written.zip(after.written).map(|(a, b)| a != b),
            )),
            _ => None,
        };
        json!({
            "before":self.wire(observed_ms),"after":after.wire(observed_ms),
            "replaced_during_spawn":change.map(|(replaced, _)| replaced),
            "written_during_spawn":change.and_then(|(_, written)| written),
        })
    }
}

fn nonzero(value: u64) -> Option<u64> {
    (value != 0).then_some(value)
}

fn recent(filetime: Option<u64>, observed_ms: Option<u64>) -> Option<bool> {
    let written_ms = filetime?.checked_sub(FILETIME_UNIX_EPOCH)? / 10_000;
    Some(observed_ms?.checked_sub(written_ms)? <= RECENT_MS)
}

fn path(path: &Path) -> Value {
    let text = path.to_string_lossy();
    let mut end = text.len().min(2048);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    json!({"path":&text[..end],"truncated":end < text.len()})
}

#[derive(Debug)]
pub(in crate::dogfood::tools) struct Context {
    executable: PathBuf,
    requested_shell: Option<PathBuf>,
    cwd: PathBuf,
    executable_before: Snapshot,
    cwd_before: Snapshot,
    observed_ms: Option<u64>,
    mode: &'static str,
    concurrent: usize,
}

impl Context {
    pub fn new(
        executable: &Path,
        requested_shell: Option<&Path>,
        cwd: &Path,
        observed_ms: Option<u64>,
        mode: &'static str,
    ) -> Self {
        Self {
            executable: executable.to_path_buf(),
            requested_shell: requested_shell.map(Path::to_path_buf),
            cwd: cwd.to_path_buf(),
            executable_before: Snapshot::read(executable),
            cwd_before: Snapshot::read(cwd),
            observed_ms,
            mode,
            concurrent: ACTIVE.fetch_add(1, Ordering::Relaxed) + 1,
        }
    }

    pub fn concurrent(&self) -> usize {
        self.concurrent
    }

    pub fn io_failure(&self, call: &'static str, error: &std::io::Error, job: Value) -> Value {
        let point = error
            .raw_os_error()
            .map(|win32_error| windows_spawn::diagnostics::Failure {
                call,
                win32_error,
                launch: None,
            });
        self.failure(point, error.raw_os_error(), job)
    }

    pub fn failure(
        &self,
        failure: Option<windows_spawn::diagnostics::Failure>,
        raw_error: Option<i32>,
        job: Value,
    ) -> Value {
        let launch = failure.and_then(|failure| failure.launch).map(|launch| {
            json!({
                "requested_creation_flags":launch.requested_creation_flags,
                "effective_creation_flags":launch.effective_creation_flags,
                "suspended":launch.suspended,"jobs":launch.jobs,
                "inherited_handles":launch.inherited_handles,"pseudoconsole":launch.pseudoconsole,
            })
        });
        json!({
            "schema":"grapt.windows-spawn-denied.v1",
            "call":failure.map(|failure| failure.call),
            "win32_error":failure.map(|failure| failure.win32_error).or(raw_error),
            "process_id":std::process::id(),"observed_unix_ms":self.observed_ms,
            "mode":self.mode,"concurrent_spawns":self.concurrent,"launch":launch,"job":job,
            "executable":path(&self.executable),
            "requested_shell":self.requested_shell.as_deref().map(path),
            "cwd":path(&self.cwd),
            "executable_observation":self.executable_before.compare(&Snapshot::read(&self.executable),self.observed_ms),
            "cwd_observation":self.cwd_before.compare(&Snapshot::read(&self.cwd),self.observed_ms),
        })
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::Relaxed);
    }
}

pub(in crate::dogfood::tools) fn log(record: &Value) {
    // A lost diagnostic stream must not replace the native launch failure.
    crate::diagnostics::report(format!("terminal_spawn_denied {record}"));
}

pub(in crate::dogfood::tools) fn log_failure(failure: super::process::SpawnFailure) -> String {
    if let Some(record) = &failure.diagnostic {
        log(record);
    }
    failure.detail
}

/// A shim uses its parent's injected launch timestamp, never an ambient clock.
pub(super) fn received(mut record: Value, observed_ms: Option<u64>, concurrent: usize) {
    if let Some(object) = record.as_object_mut() {
        object.insert("observed_unix_ms".into(), json!(observed_ms));
        object.insert("observer_process_id".into(), json!(std::process::id()));
        object.insert("parent_concurrent_spawns".into(), json!(concurrent));
        object.insert("observation_time_source".into(), json!("parent_spawn"));
    }
    for owner in ["executable_observation", "cwd_observation"] {
        for phase in ["before", "after"] {
            let Some(snapshot) = record
                .get_mut(owner)
                .and_then(|value| value.get_mut(phase))
                .and_then(Value::as_object_mut)
            else {
                continue;
            };
            let created = snapshot.get("created_filetime").and_then(Value::as_u64);
            let written = snapshot.get("written_filetime").and_then(Value::as_u64);
            snapshot.insert(
                "recently_created".into(),
                json!(recent(created, observed_ms)),
            );
            snapshot.insert(
                "recently_written".into(),
                json!(recent(written, observed_ms)),
            );
        }
    }
    log(&record);
}
