//! Instruction bytes and their immutable acquisition provenance.
//! Never reopen a checked pathname through the shared source cache.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use grapt_config::{ConfigSourcePath, MaxPathBytes};

use super::MAX_FILE_BYTES;
use crate::dogfood::source_file::{is_symlink_refusal, open_contained, open_contained_nofollow};

pub(super) struct InstructionFile {
    pub(super) path: PathBuf,
    pub(super) scope: PathBuf,
    pub(super) text: String,
}

fn normalize_within_scope(scope: &Path, relative: &Path) -> Option<PathBuf> {
    let mut normalized = scope.to_path_buf();
    let mut depth = 0usize;
    for component in relative.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(segment) => {
                normalized.push(segment);
                depth += 1;
            }
            std::path::Component::ParentDir => {
                if depth == 0 {
                    return None;
                }
                normalized.pop();
                depth -= 1;
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => return None,
        }
    }
    Some(normalized)
}

/// Read a scoped instruction file from its uncollapsed `unfollowed` path
/// relative to `supplied_scope`, without joining onto a verbatim prefix or
/// pre-canonicalizing intermediate segments before the link-refusing walk.
pub(super) fn read_scoped_relative(
    supplied_scope: &Path,
    unfollowed: &Path,
) -> Result<Option<InstructionFile>, String> {
    let outside = || "instruction source resolves outside the directory it governs".to_owned();
    let unreadable = |error| format!("instruction source is unreadable: {error}");
    let joined = supplied_scope.join(unfollowed);
    ConfigSourcePath::new(
        &joined,
        MaxPathBytes::try_new(4096).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let target = normalize_within_scope(supplied_scope, unfollowed).ok_or_else(outside)?;
    ConfigSourcePath::new(
        &target,
        MaxPathBytes::try_new(4096).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    #[cfg(test)]
    testing::observe(testing::Phase::Opening, &target);
    let opened = match open_contained_nofollow(supplied_scope, unfollowed) {
        Ok(opened) => opened,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) if is_symlink_refusal(&error) => {
            if std::fs::canonicalize(&joined)
                .is_ok_and(|resolved| resolved.strip_prefix(supplied_scope).is_err())
            {
                return Err(outside());
            }
            return Err("instruction source must not be a symlink".to_owned());
        }
        Err(error) => return Err(unreadable(error)),
    };
    finish_read(target, supplied_scope.to_path_buf(), opened)
}

pub(super) fn read_file(
    raw_path: &Path,
    scope: Option<&Path>,
) -> Result<Option<InstructionFile>, String> {
    let outside = || "instruction source resolves outside the directory it governs".to_owned();
    if let Some(supplied_scope) = scope {
        let unfollowed = raw_path
            .strip_prefix(supplied_scope)
            .map_err(|_| outside())?;
        return read_scoped_relative(supplied_scope, unfollowed);
    }
    let path = ConfigSourcePath::new(
        raw_path,
        MaxPathBytes::try_new(4096).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let unreadable = |error| format!("instruction source is unreadable: {error}");
    // Global instruction links retain their existing resolved-parent exception.
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
    let scope = target.parent().unwrap_or(&target).to_owned();
    let relative = target.strip_prefix(&scope).map_err(|_| outside())?;
    #[cfg(test)]
    testing::observe(testing::Phase::Opening, &target);
    let opened = open_contained(&scope, relative).map_err(unreadable)?;
    finish_read(target, scope, opened)
}

fn finish_read(
    target: PathBuf,
    scope: PathBuf,
    opened: crate::dogfood::source_file::OpenedResource,
) -> Result<Option<InstructionFile>, String> {
    let outside = || "instruction source resolves outside the directory it governs".to_owned();
    let unreadable = |error| format!("instruction source is unreadable: {error}");
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
