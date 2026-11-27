//! One last-error-only private diagnostic, never durable product history.
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub(crate) const LOG: &str = "logs/provider-error.txt";

pub(crate) fn open(root: &Path) -> Result<grapt_provider::ProviderErrorText, String> {
    if !crate::input::read_provider_error_text(&root.join("service.conf"))
        .map_err(|error| error.to_string())?
    {
        return Ok(grapt_provider::ProviderErrorText::disabled());
    }
    let path = root.join(LOG);
    let label = path
        .to_str()
        .ok_or("provider error-text path is not UTF-8")?;
    grapt_provider::ProviderErrorText::at(label).map_err(str::to_owned)
}

/// The reader carries its own volatile detail to this product observer. One
/// blocking task completes file I/O off-lock, then uses the existing notice
/// append under the normal writer. No raw body or masked text enters that row.
pub(crate) struct Observer {
    pub root: PathBuf,
    pub session: grapt_types::SessionId,
    pub execution: grapt_types::ExecutionId,
    pub inbox: std::sync::Arc<std::sync::Mutex<grapt_inbox::Inbox>>,
    pub ids: std::sync::Arc<grapt_store::OsRandomIds>,
    pub clock: std::sync::Arc<grapt_store::SystemClock>,
}
impl grapt_runtime::ProviderErrorObserver for Observer {
    fn observe(
        &self,
        attempt: grapt_types::PhysicalAttemptId,
        detail: grapt_provider::ProviderErrorDetail,
    ) -> grapt_runtime::BoxFuture<'static, ()> {
        let root = self.root.clone();
        let session = self.session.clone();
        let execution = self.execution.clone();
        let inbox = self.inbox.clone();
        let ids = self.ids.clone();
        let clock = self.clock.clone();
        Box::pin(async move {
            let result = tokio::task::spawn_blocking(move || {
                use grapt_session::notice::ProviderErrorOutcome;
                // With the opt-in off there is nothing to record: the failure
                // row already tells the reader the status, the code and the
                // vendor's reason, so a per-attempt hint about a setting is a
                // second, weaker line at the moment they need the cause. The
                // `EnableHint` notice kind stays readable for rows an earlier
                // build wrote; this build simply stops producing it.
                if !detail.enabled() {
                    return Ok::<(), String>(());
                }
                let sink = FileSink {
                    path: root.join(LOG),
                };
                let saved = if let Some(frame) = detail.terminal_frame() {
                    sink.write_bounded(frame.as_str(), grapt_provider::TERMINAL_DETAIL_BYTES)
                } else {
                    detail
                        .text()
                        .ok_or_else(|| io::Error::other("provider error-text body was unavailable"))
                        .and_then(|text| sink.write(text.as_str()))
                };
                let outcome = match saved {
                    Ok(()) => {
                        crate::diagnostics::report(format!(
                            "provider error details saved to {} (last error only)",
                            sink.path.display()
                        ));
                        ProviderErrorOutcome::Saved
                    }
                    Err(error) => {
                        crate::diagnostics::report(format!(
                            "provider error-text save failed at {}: {error}",
                            sink.path.display()
                        ));
                        ProviderErrorOutcome::Unavailable
                    }
                };
                // File I/O is finished, and the masked value is dropped before
                // acquiring the Inbox/store writer for product-authored text.
                drop(detail);
                let _recorded = {
                    let mut held = inbox
                        .lock()
                        .map_err(|_| "error-detail Inbox lock poisoned".to_owned())?;
                    let authorities = grapt_types::DeterministicAuthorities::new(&*clock, &*ids);
                    grapt_session::notice::record_provider_error_detail(
                        held.store_mut(),
                        &authorities,
                        &session,
                        &execution,
                        &attempt,
                        outcome,
                    )
                }?;
                // The execution terminal publishes and reconciles its normal
                // stream. Ending it here would hide the forthcoming failure.
                Ok::<(), String>(())
            })
            .await;
            match result {
                Ok(Ok(())) => (),
                Ok(Err(error)) => {
                    crate::diagnostics::report(format!(
                        "provider error-detail notice failed: {error}"
                    ));
                }
                Err(error) => {
                    crate::diagnostics::report(format!(
                        "provider error-detail observer failed: {error}"
                    ));
                }
            }
        })
    }
}

/// Called only by the bounded diagnostic writer, never by the provider reader.
pub(crate) fn save_usage(root: &Path, text: &str) -> io::Result<()> {
    FileSink {
        path: root.join(LOG),
    }
    .write_bounded(text, grapt_provider::TERMINAL_DETAIL_BYTES)
}

struct FileSink {
    path: PathBuf,
}
impl FileSink {
    fn write(&self, text: &str) -> io::Result<()> {
        self.write_bounded(text, grapt_provider::ERROR_TEXT_BYTES)
    }
    fn write_bounded(&self, text: &str, bound: usize) -> io::Result<()> {
        if text.len() > bound {
            return Err(io::Error::other(
                "provider error text exceeded its byte bound",
            ));
        }
        let directory = self
            .path
            .parent()
            .ok_or_else(|| io::Error::other("diagnostic has no parent"))?;
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.recursive(false).create(directory) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let held = fs::symlink_metadata(directory)?;
                if !held.is_dir() || linked(&held) {
                    return Err(io::Error::other(
                        "diagnostic directory is not a real directory",
                    ));
                }
            }
            Err(error) => return Err(error),
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        }
        match fs::symlink_metadata(&self.path) {
            Ok(held) if !held.is_file() || linked(&held) => {
                return Err(io::Error::other("diagnostic is not a regular file"))
            }
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
        // Exclusive creation cannot follow a preexisting pending link or FIFO.
        // At most two bounded files exist; no per-attempt filenames accumulate.
        let pending = self.path.with_extension("txt.pending");
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&pending)?;
        let saved = (|| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            file.write_all(text.as_bytes())?;
            drop(file);
            // Windows rename cannot replace an existing destination. Only this
            // component-owned last-error file is removed, after the new record
            // is complete. Windows inherits the service profile's ACL policy.
            #[cfg(windows)]
            if self.path.exists() {
                fs::remove_file(&self.path)?;
            }
            fs::rename(&pending, &self.path)
        })();
        if saved.is_err() {
            let _cleanup = fs::remove_file(&pending);
        }
        saved
    }
}

fn linked(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    false
}

/// Fixed advisory copy rendered through the public system-message DTO.
pub(crate) fn notice_row(
    notice: grapt_session::notice::ProviderErrorDetailNotice,
) -> Result<(u64, String, String), String> {
    let suffix = notice
        .position
        .event
        .as_str()
        .strip_prefix("evt_")
        .ok_or("notice event identity has no prefix")?;
    let id = format!("msg_{suffix}");
    let message: grapt_api::u3a::SessionMessageSystemDto = serde_json::from_value(serde_json::json!({
        "type":"system", "id":id,
        "metadata":{"providerErrorDetail":true,"executionID":notice.execution.as_str(),"attemptID":notice.attempt.as_str()},
        "time":{"created":notice.position.created_ms}, "text":notice.outcome.text(),
    })).map_err(|error| error.to_string())?;
    let raw = serde_json::to_string(&message).map_err(|error| error.to_string())?;
    Ok((notice.position.seq, id, raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn last_error_overwrites_with_exact_bound_and_private_permissions(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root =
            std::env::temp_dir().join(format!("grapt-provider-error-file-{}", std::process::id()));
        fs::create_dir(&root)?;
        let sink = FileSink {
            path: root.join(LOG),
        };
        sink.write(&"x".repeat(512))?;
        assert_eq!(fs::metadata(&sink.path)?.len(), 512);
        sink.write("second error")?;
        assert_eq!(fs::read_to_string(&sink.path)?, "second error");
        assert!(!sink.path.with_extension("txt.pending").exists());
        assert!(sink.write(&"x".repeat(513)).is_err());
        assert_eq!(fs::read_to_string(&sink.path)?, "second error");
        sink.write_bounded(&"x".repeat(4096), grapt_provider::TERMINAL_DETAIL_BYTES)?;
        assert_eq!(fs::metadata(&sink.path)?.len(), 4096);
        assert!(sink
            .write_bounded(&"x".repeat(4097), grapt_provider::TERMINAL_DETAIL_BYTES)
            .is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&sink.path)?.permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(sink.path.parent().ok_or("parent")?)?
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn linked_blocked_or_missing_destinations_fail_without_altering_other_files(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "grapt-provider-error-refusal-{}",
            std::process::id()
        ));
        fs::create_dir(&root)?;
        let sink = FileSink {
            path: root.join(LOG),
        };
        fs::create_dir(root.join("logs"))?;
        fs::create_dir(&sink.path)?;
        assert!(sink.write("first").is_err());
        fs::remove_dir(&sink.path)?;
        fs::write(
            sink.path.with_extension("txt.pending"),
            "prior pending record",
        )?;
        assert!(sink.write("second").is_err());
        assert_eq!(
            fs::read_to_string(sink.path.with_extension("txt.pending"))?,
            "prior pending record"
        );
        fs::remove_file(sink.path.with_extension("txt.pending"))?;
        #[cfg(unix)]
        {
            let outside = root.join("unrelated");
            fs::write(&outside, "unchanged")?;
            std::os::unix::fs::symlink(&outside, &sink.path)?;
            assert!(sink.write("third").is_err());
            assert_eq!(fs::read_to_string(outside)?, "unchanged");
        }
        fs::remove_dir_all(&root)?;
        let missing = FileSink {
            path: root.join("missing/logs/provider-error.txt"),
        };
        assert!(missing.write("fourth").is_err());
        Ok(())
    }
}
