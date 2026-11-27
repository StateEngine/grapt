//! Generation-owned ready-time discovery under the caller-owned service root.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use grapt_types::service::OwnerReceipt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const READY_RECEIPT_FILE: &str = "serve-ready.json";
const READY_LOCK_FILE: &str = ".serve-ready.lock";
const MAX_READY_RECEIPT_BYTES: usize = 8_192;

#[derive(Debug, Error)]
pub enum ReadyReceiptRefusal {
    #[error("ready receipt encoding failed: {0}")]
    Encode(serde_json::Error),
    #[error("ready receipt exceeded {MAX_READY_RECEIPT_BYTES} bytes")]
    TooLarge,
    #[error("ready receipt custody is already held at {path}")]
    CustodyBusy { path: PathBuf },
    #[error("ready receipt belongs to another service generation at {path}")]
    ForeignCustody { path: PathBuf },
    #[error("ready receipt could not be read from {path}: {reason}")]
    Read { path: PathBuf, reason: String },
    #[error("ready receipt could not be written to {path}: {reason}")]
    Write { path: PathBuf, reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ReadyReceipt {
    schema: String,
    operation: String,
    service_slot: String,
    service_instance: String,
    environment: String,
    service_generation: u64,
    pid: u32,
    observed_endpoint: String,
    serve_window_ms: u64,
    build: String,
}

impl ReadyReceipt {
    fn from_owner(owner: &OwnerReceipt, serve_window_ms: u64, build: &str) -> Self {
        Self {
            schema: grapt_api::product_health::PRODUCT_READY_SCHEMA.to_owned(),
            operation: "serve".to_owned(),
            service_slot: owner.slot().as_str().to_owned(),
            service_instance: owner.instance().as_str().to_owned(),
            environment: owner.environment().as_str().to_owned(),
            service_generation: owner.generation().value(),
            pid: owner.pid().value(),
            observed_endpoint: owner.endpoint().as_str().to_owned(),
            serve_window_ms,
            build: build.to_owned(),
        }
    }
}

struct CustodyLock {
    _file: File,
}

impl CustodyLock {
    fn acquire(service_root: &Path) -> Result<Self, ReadyReceiptRefusal> {
        let path = service_root.join(READY_LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| ReadyReceiptRefusal::Write {
                path: path.clone(),
                reason: error.to_string(),
            })?;
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => ReadyReceiptRefusal::CustodyBusy { path },
            std::fs::TryLockError::Error(error) => ReadyReceiptRefusal::Write {
                path,
                reason: error.to_string(),
            },
        })?;
        // Closing releases the OS lock after crashes too. Never unlink a lock
        // inode: doing so would let concurrent holders lock different files.
        Ok(Self { _file: file })
    }
}

impl Drop for CustodyLock {
    fn drop(&mut self) {
        // Release before closing: a concurrent fork may briefly inherit the
        // open file description until exec. Closing just this descriptor can
        // otherwise leave the predecessor's lock held in that child.
        let _ = self._file.unlock();
    }
}

fn read_receipt(path: &Path) -> Result<Option<ReadyReceipt>, ReadyReceiptRefusal> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(ReadyReceiptRefusal::Read {
                path: path.to_owned(),
                reason: error.to_string(),
            })
        }
    };
    let mut bytes = Vec::new();
    file.take((MAX_READY_RECEIPT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| ReadyReceiptRefusal::Read {
            path: path.to_owned(),
            reason: error.to_string(),
        })?;
    if bytes.len() > MAX_READY_RECEIPT_BYTES {
        return Err(ReadyReceiptRefusal::TooLarge);
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(ReadyReceiptRefusal::Encode)
}

/// Only restart admission can supply the observed-dead predecessor. Its exact
/// tuple is checked read-only for launcher advice; this does not retire custody.
pub(crate) fn check_dead_predecessor(
    service_root: &Path,
    predecessor: &crate::startup::DeadPredecessor,
) -> Result<(), ReadyReceiptRefusal> {
    let path = service_root.join(READY_RECEIPT_FILE);
    if let Some(existing) = read_receipt(&path)? {
        let expected = ReadyReceipt::from_owner(
            predecessor.owner(),
            existing.serve_window_ms,
            &existing.build,
        );
        if existing != expected {
            return Err(ReadyReceiptRefusal::ForeignCustody { path });
        }
    }
    Ok(())
}

/// Only restart admission can supply the observed-dead predecessor. Its exact
/// owner tuple must match, including generation, PID and endpoint. Build/window
/// are taken from the receipt read while this same custody lock is held.
pub(crate) fn retire_dead_predecessor(
    service_root: &Path,
    predecessor: &crate::startup::DeadPredecessor,
) -> Result<bool, ReadyReceiptRefusal> {
    let _lock = CustodyLock::acquire(service_root)?;
    let path = service_root.join(READY_RECEIPT_FILE);
    let Some(existing) = read_receipt(&path)? else {
        return Ok(false);
    };
    let expected = ReadyReceipt::from_owner(
        predecessor.owner(),
        existing.serve_window_ms,
        &existing.build,
    );
    if existing != expected {
        return Err(ReadyReceiptRefusal::ForeignCustody { path });
    }
    std::fs::remove_file(&path).map_err(|error| ReadyReceiptRefusal::Write {
        path,
        reason: error.to_string(),
    })?;
    Ok(true)
}

/// Publish only into empty custody. An exact replay is idempotent; any other
/// tuple, including the same PID under another generation, is refused.
pub fn publish(
    service_root: &Path,
    owner: &OwnerReceipt,
    serve_window_ms: u64,
    build: &str,
) -> Result<PathBuf, ReadyReceiptRefusal> {
    let _lock = CustodyLock::acquire(service_root)?;
    let receipt = ReadyReceipt::from_owner(owner, serve_window_ms, build);
    let path = service_root.join(READY_RECEIPT_FILE);
    if let Some(existing) = read_receipt(&path)? {
        return if existing == receipt {
            Ok(path)
        } else {
            Err(ReadyReceiptRefusal::ForeignCustody { path })
        };
    }
    let bytes = serde_json::to_vec(&receipt).map_err(ReadyReceiptRefusal::Encode)?;
    if bytes.len() > MAX_READY_RECEIPT_BYTES {
        return Err(ReadyReceiptRefusal::TooLarge);
    }
    let pending = service_root.join(format!(
        ".serve-ready.{}.{}.pending",
        owner.generation().value(),
        owner.pid().value()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending)
        .map_err(|error| ReadyReceiptRefusal::Write {
            path: pending.clone(),
            reason: error.to_string(),
        })?;
    let write_result = file.write_all(&bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&pending);
        return Err(ReadyReceiptRefusal::Write {
            path: pending,
            reason: error.to_string(),
        });
    }
    let linked = std::fs::hard_link(&pending, &path);
    let _ = std::fs::remove_file(&pending);
    match linked {
        Ok(()) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if read_receipt(&path)?.as_ref() == Some(&receipt) {
                Ok(path)
            } else {
                Err(ReadyReceiptRefusal::ForeignCustody { path })
            }
        }
        Err(error) => Err(ReadyReceiptRefusal::Write {
            path,
            reason: error.to_string(),
        }),
    }
}

/// Remove only the exact generation/PID/endpoint/build tuple this process
/// published. A foreign successor is never deleted by predecessor cleanup.
pub fn retire(
    service_root: &Path,
    owner: &OwnerReceipt,
    serve_window_ms: u64,
    build: &str,
) -> Result<bool, ReadyReceiptRefusal> {
    let _lock = CustodyLock::acquire(service_root)?;
    let expected = ReadyReceipt::from_owner(owner, serve_window_ms, build);
    let path = service_root.join(READY_RECEIPT_FILE);
    match read_receipt(&path)? {
        None => Ok(false),
        Some(existing) if existing == expected => {
            std::fs::remove_file(&path).map_err(|error| ReadyReceiptRefusal::Write {
                path: path.clone(),
                reason: error.to_string(),
            })?;
            Ok(true)
        }
        Some(_) => Err(ReadyReceiptRefusal::ForeignCustody { path }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_types::service::{
        Endpoint, EnvironmentReceipt, ProcessId, ServiceGeneration, ServiceInstanceId,
        ServiceSlotId,
    };

    fn owner(generation: u64, pid: u32) -> OwnerReceipt {
        let Some(slot) = ServiceSlotId::new("product") else {
            panic!("slot refused")
        };
        let Some(instance) = ServiceInstanceId::new("instance-a") else {
            panic!("instance refused")
        };
        let Some(environment) = EnvironmentReceipt::new("internal-alpha") else {
            panic!("environment refused")
        };
        let Some(pid) = ProcessId::new(pid) else {
            panic!("pid refused")
        };
        let Some(endpoint) = Endpoint::new("http://127.0.0.1:41111") else {
            panic!("endpoint refused")
        };
        OwnerReceipt::new(
            slot,
            instance,
            environment,
            ServiceGeneration::new(generation),
            pid,
            endpoint,
        )
    }

    #[test]
    fn pid_reuse_cannot_overwrite_or_retire_another_generation() {
        let root =
            std::env::temp_dir().join(format!("grapt-ready-generation-{}", std::process::id()));
        if root.exists() {
            std::fs::remove_dir_all(&root)
                .unwrap_or_else(|error| panic!("ready test reset failed: {error}"));
        }
        std::fs::create_dir_all(&root)
            .unwrap_or_else(|error| panic!("ready test root failed: {error}"));
        let first = owner(7, 41);
        publish(&root, &first, 100, "build-a")
            .unwrap_or_else(|error| panic!("first publication failed: {error}"));
        let reused_pid = owner(8, 41);
        let publication = publish(&root, &reused_pid, 100, "build-a");
        assert!(
            matches!(publication, Err(ReadyReceiptRefusal::ForeignCustody { .. })),
            "a reused PID did not receive the required foreign-custody refusal: {publication:?}"
        );
        assert!(
            matches!(
                retire(&root, &reused_pid, 100, "build-a"),
                Err(ReadyReceiptRefusal::ForeignCustody { .. })
            ),
            "a reused PID retired another service generation"
        );
        assert!(root.join(READY_RECEIPT_FILE).exists());
        assert!(
            matches!(retire(&root, &first, 100, "build-a"), Ok(true)),
            "the matching generation did not retire its receipt"
        );
        std::fs::remove_dir_all(root)
            .unwrap_or_else(|error| panic!("ready test cleanup failed: {error}"));
    }

    #[test]
    fn custody_uses_a_process_lock_not_a_crash_persistent_sentinel() {
        let root = std::env::temp_dir().join(format!("grapt-ready-lock-{}", std::process::id()));
        std::fs::create_dir_all(&root)
            .unwrap_or_else(|error| panic!("lock test root failed: {error}"));
        // An old sentinel, or a file left by a terminated lock owner, is not
        // evidence that an OS process still owns the custody lock.
        std::fs::write(root.join(READY_LOCK_FILE), b"abandoned")
            .unwrap_or_else(|error| panic!("old lock fixture failed: {error}"));
        let guard = CustodyLock::acquire(&root)
            .unwrap_or_else(|error| panic!("abandoned lock was not recoverable: {error}"));
        assert!(matches!(
            CustodyLock::acquire(&root),
            Err(ReadyReceiptRefusal::CustodyBusy { .. })
        ));
        drop(guard);
        assert!(root.join(READY_LOCK_FILE).is_file());
        let next = CustodyLock::acquire(&root)
            .unwrap_or_else(|error| panic!("released custody could not be acquired: {error}"));
        drop(next);
        std::fs::remove_dir_all(root)
            .unwrap_or_else(|error| panic!("lock test cleanup failed: {error}"));
    }
}
