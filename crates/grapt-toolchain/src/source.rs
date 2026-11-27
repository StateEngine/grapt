//! The deterministic archive source: scheme-less relative paths only.
//!
//! The core logic is network-free by construction. The shipped adapter
//! ([`FileSource`]) delivers archive bytes from a path relative to one base
//! directory; a source naming any scheme prefix — `https://`, `file:`, a
//! drive letter — is an explicit release-input boundary, refused
//! `source-unsupported`, never a silent capability.

use std::path::{Component, Path, PathBuf};

use crate::refusal::{Refusal, RefusalReason};
use crate::value::SourceUrl;

/// The archive byte bound: the single 8 MiB bound the boundary declares,
/// named for the fetch path.
pub const SOURCE_MAX_ARCHIVE_BYTES: usize = crate::archive::ARCHIVE_MAX_BYTES;

/// A deterministic source of pinned archive bytes.
pub trait ArchiveSource: std::fmt::Debug {
    /// Deliver the archive bytes for the exact source named by the pin, or
    /// refuse: an unsupported source shape, or a delivery that failed.
    fn fetch(&self, source: &SourceUrl) -> Result<Vec<u8>, Refusal>;
}

/// The shipped adapter: scheme-less paths, relative to one base directory,
/// never escaping it.
#[derive(Debug)]
pub struct FileSource {
    base: PathBuf,
}

impl FileSource {
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }
}

impl ArchiveSource for FileSource {
    fn fetch(&self, source: &SourceUrl) -> Result<Vec<u8>, Refusal> {
        let raw = source.as_str();
        if has_scheme(raw) {
            return Err(Refusal::new(
                RefusalReason::SourceUnsupported,
                "the pin names a source scheme the shipped deterministic adapter does not admit",
            ));
        }
        let relative = Path::new(raw);
        for component in relative.components() {
            if !matches!(component, Component::Normal(_)) {
                return Err(Refusal::new(
                    RefusalReason::SourceUnsupported,
                    "the pin source is not a relative path under the activation root",
                ));
            }
        }
        let path = self.base.join(relative);
        let metadata = std::fs::metadata(&path).map_err(|_| {
            Refusal::new(
                RefusalReason::SourceFetchFailed,
                "the deterministic adapter could not deliver the pinned archive",
            )
        })?;
        if !metadata.is_file() {
            return Err(Refusal::new(
                RefusalReason::SourceFetchFailed,
                "the pinned archive is not a regular file",
            ));
        }
        let mut file = std::fs::File::open(&path).map_err(|_| {
            Refusal::new(
                RefusalReason::SourceFetchFailed,
                "the deterministic adapter could not open the pinned archive",
            )
        })?;
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 65536];
        loop {
            let read = std::io::Read::read(&mut file, &mut buffer).map_err(|_| {
                Refusal::new(
                    RefusalReason::SourceFetchFailed,
                    "the deterministic adapter could not read the pinned archive",
                )
            })?;
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
            if bytes.len() > SOURCE_MAX_ARCHIVE_BYTES {
                return Err(Refusal::new(
                    RefusalReason::SourceFetchFailed,
                    "the delivered archive exceeds the 8 MiB bound",
                ));
            }
        }
        Ok(bytes)
    }
}

/// A scheme prefix in the RFC 3986 shape:
/// ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ) ":".
fn has_scheme(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    let colon = match bytes.iter().position(|b| *b == b':') {
        Some(position) => position,
        None => return false,
    };
    if colon == 0 || !bytes[0].is_ascii_alphabetic() {
        return false;
    }
    bytes[..colon]
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn source_of(raw: &str) -> SourceUrl {
        SourceUrl::new(raw).unwrap_or_else(|| panic!("fixture {raw} must parse"))
    }

    fn refusal_of(fetch: &dyn ArchiveSource, raw: &str) -> Refusal {
        match fetch.fetch(&source_of(raw)) {
            Ok(_) => panic!("this source must be refused"),
            Err(refusal) => refusal,
        }
    }

    #[test]
    fn a_scheme_less_relative_path_delivers_bytes() {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base =
            std::env::temp_dir().join(format!("grapt-toolchain-{}-src-{n}", std::process::id()));
        std::fs::create_dir_all(base.join("archives"))
            .unwrap_or_else(|error| panic!("test: create base: {error}"));
        std::fs::write(base.join("archives/rg.tar"), b"archive-bytes")
            .unwrap_or_else(|error| panic!("test: write archive: {error}"));
        let source = FileSource::new(base.clone());
        let bytes = source
            .fetch(&source_of("archives/rg.tar"))
            .unwrap_or_else(|refusal| panic!("the archive must deliver: {refusal}"));
        assert_eq!(bytes, b"archive-bytes");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_scheme_prefix_refuses_unsupported() {
        let source = FileSource::new(std::env::temp_dir());
        for raw in [
            "https://example.com/rg.tar",
            "file:archives/rg.tar",
            "C:archives/rg.tar",
            "ftp://example.com/rg.tar",
        ] {
            assert_eq!(
                refusal_of(&source, raw).reason,
                RefusalReason::SourceUnsupported,
                "{raw} must refuse source-unsupported"
            );
        }
    }

    #[test]
    fn an_absolute_or_escaping_path_refuses_unsupported() {
        let source = FileSource::new(std::env::temp_dir());
        for raw in ["../outside.tar", "a/../../outside.tar", "/absolute/rg.tar"] {
            assert_eq!(
                refusal_of(&source, raw).reason,
                RefusalReason::SourceUnsupported,
                "{raw} must refuse source-unsupported"
            );
        }
    }

    #[test]
    fn a_missing_or_unreadable_archive_refuses_fetch_failed() {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!(
            "grapt-toolchain-{}-srcmiss-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&base).unwrap_or_else(|error| panic!("test: create base: {error}"));
        std::fs::create_dir_all(base.join("adir"))
            .unwrap_or_else(|error| panic!("test: create dir: {error}"));
        let source = FileSource::new(base.clone());
        assert_eq!(
            refusal_of(&source, "missing.tar").reason,
            RefusalReason::SourceFetchFailed
        );
        assert_eq!(
            refusal_of(&source, "adir").reason,
            RefusalReason::SourceFetchFailed
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_overbound_archive_refuses_fetch_failed() {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base =
            std::env::temp_dir().join(format!("grapt-toolchain-{}-srcbig-{n}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap_or_else(|error| panic!("test: create base: {error}"));
        let mut big = vec![0u8; SOURCE_MAX_ARCHIVE_BYTES + 1];
        let last = big.len() - 1;
        big[last] = 1;
        std::fs::write(base.join("big.tar"), &big)
            .unwrap_or_else(|error| panic!("test: write big archive: {error}"));
        let source = FileSource::new(base.clone());
        assert_eq!(
            refusal_of(&source, "big.tar").reason,
            RefusalReason::SourceFetchFailed
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
