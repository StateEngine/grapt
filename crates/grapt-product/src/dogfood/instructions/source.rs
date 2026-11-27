//! Instruction bytes and their immutable acquisition provenance.
//! Never reopen a checked pathname through the shared source cache.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use grapt_config::{ConfigSourcePath, MaxPathBytes};

use super::MAX_FILE_BYTES;
use crate::dogfood::source_file::open_contained;

pub(super) struct InstructionFile {
    pub(super) path: PathBuf,
    pub(super) scope: PathBuf,
    pub(super) text: String,
}

pub(super) fn read_file(
    path: &Path,
    scope: Option<&Path>,
) -> Result<Option<InstructionFile>, String> {
    let path = ConfigSourcePath::new(
        path,
        MaxPathBytes::try_new(4096).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let target = match std::fs::canonicalize(path.as_path()) {
        Ok(target) => target,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("instruction source cannot be resolved: {error}")),
    };
    ConfigSourcePath::new(
        &target,
        MaxPathBytes::try_new(4096).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    // A supplied scope was resolved by the owner ONCE. Re-resolving it here
    // could follow a replacement link and silently widen that read authority.
    // Global instruction links retain their existing resolved-parent exception.
    let scope = scope
        .unwrap_or_else(|| target.parent().unwrap_or(&target))
        .to_owned();
    let outside = || "instruction source resolves outside the directory it governs".to_owned();
    let relative = target.strip_prefix(&scope).map_err(|_| outside())?;
    #[cfg(test)]
    testing::observe(testing::Phase::Opening, &target);
    let unreadable = |error| format!("instruction source is unreadable: {error}");
    let opened = open_contained(&scope, relative).map_err(unreadable)?;
    #[cfg(test)]
    testing::observe(testing::Phase::Opened, &target);
    let metadata = opened.file.metadata().map_err(unreadable)?;
    if !metadata.is_file() {
        return Err("instruction source is not a bounded regular file".into());
    }
    if !opened.inside(&scope).map_err(unreadable)? {
        return Err(outside());
    }
    let oversized =
        || "instruction source is not a bounded regular file: exceeds its file bound".to_owned();
    if metadata.len() > MAX_FILE_BYTES as u64 {
        return Err(oversized());
    }
    #[cfg(test)]
    testing::observe(testing::Phase::Reading, &target);
    let mut bytes = Vec::new();
    // Keep the handle and its Windows directory pins alive through this read.
    (&opened.file)
        .take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(oversized());
    }
    let text =
        String::from_utf8(bytes).map_err(|_| "instruction source is not UTF-8".to_owned())?;
    Ok(Some(InstructionFile {
        path: target,
        scope,
        text,
    }))
}

#[cfg(test)]
pub(in crate::dogfood) mod testing {
    use std::cell::RefCell;
    use std::path::Path;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(in crate::dogfood) enum Phase {
        Opening,
        Opened,
        Reading,
    }
    type Observer = Box<dyn FnMut(Phase, &Path)>;
    thread_local! {
        static OBSERVER: RefCell<Option<Observer>> = const { RefCell::new(None) };
    }
    pub(in crate::dogfood) struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            OBSERVER.with(|slot| *slot.borrow_mut() = None);
        }
    }
    pub(in crate::dogfood) fn install(observer: Observer) -> Reset {
        OBSERVER.with(|slot| *slot.borrow_mut() = Some(observer));
        Reset
    }
    pub(super) fn observe(phase: Phase, path: &Path) {
        OBSERVER.with(|slot| {
            if let Some(observer) = slot.borrow_mut().as_mut() {
                observer(phase, path);
            }
        });
    }
}
