//! The refusal taxonomy: every refusal the boundary owes, typed.
//!
//! The contract matrix carries twenty-one reasons, `R01`..`R22`;
//! `R12` (`format-unsupported`) was unreachable and is removed in
//! contract revision 2, and `R22` (`archive-malformed`) is the
//! structurally malformed archive that revision 3 separates from the
//! structurally truncated one. The format vocabulary is a closed typed
//! value, so a foreign format string is a malformed pin refused `R06` at
//! pin decode. No two refusals collapse into one error, and no diagnostic
//! carries an operator filesystem path or a secret: the layout is fixed by
//! [`crate::LAYOUT`], so an operator path in a diagnostic adds no
//! information and leaks the operator's tree.

use core::fmt;

use serde::Serialize;

/// The twenty-one typed refusal reasons (contract matrix R01..R22 minus R12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefusalReason {
    /// R01 — the activation root does not exist or is not a directory.
    RootMissing,
    /// R02 — the tool directory tree cannot be read or created.
    RootUnreadable,
    /// R03 — the pin file does not exist.
    PinMissing,
    /// R04 — the pin file exists but cannot be read or is not a regular
    /// file.
    PinUnreadable,
    /// R05 — the pin file exceeds the 64 KiB bound.
    PinOversized,
    /// R06 — the pin file is not a JSON object, carries a foreign schema,
    /// or a typed field is absent, duplicated or out of bound.
    PinMalformed,
    /// R07 — the pin's directory is not a Git worktree, or the git
    /// attribution plumbing cannot be believed.
    PinSourceNotGit,
    /// R08 — the pin sits in a worktree but is not tracked at HEAD.
    PinSourceUntracked,
    /// R09 — the pin is tracked but its on-disk bytes differ from the HEAD
    /// blob: an uncommitted re-cut is not an admissible pin.
    PinSourceDirty,
    /// R10 — the well-formed pin table has no entry for the requested
    /// target; there is no default target.
    TargetUnsupported,
    /// R11 — the pin names a source scheme the shipped deterministic
    /// adapter does not admit: a network or URI source is an explicit
    /// release-input boundary, not a core capability.
    SourceUnsupported,
    /// R13 — the deterministic adapter could not deliver the pinned
    /// archive bytes.
    SourceFetchFailed,
    /// R14 — the delivered archive bytes do not hash to the pinned archive
    /// sha256; nothing is staged or activated.
    ArchiveDigestMismatch,
    /// R15 — the verified archive does not contain the pinned entry.
    ArchiveMissingBinary,
    /// R16 — a symlink, hardlink, absolute path, dot segment, non-regular
    /// entry, or a bound violation inside the archive.
    ArchiveUnsafeEntry,
    /// R17 — the tar stream is structurally incomplete: it is not a whole
    /// number of blocks, it has no single zero block, or it carries data
    /// after the terminator. A structurally malformed header is `R22`.
    ArchiveTruncated,
    /// R18 — another activation holds the exclusive lock; concurrent
    /// callers serialize rather than race.
    ActivationInFlight,
    /// R19 — the staged executable re-reads to a digest different from the
    /// verified binary digest; the staged file is removed and the previous
    /// activation is untouched.
    StagedCopyIncomplete,
    /// R20 — the publish rename failed; the canonical path is untouched
    /// (the previous executable remains when there was one) and the staged
    /// file is removed.
    ActivationFailed,
    /// R21 — the identity record cannot be written after the binary is in
    /// place; activation rolls back so an unattributable executable is
    /// never left at the canonical path.
    IdentityWriteFailed,
    /// R22 — the tar stream is structurally malformed: over the byte
    /// bound, a name that is not NUL-terminated, a non-octal size or
    /// checksum field, an unverified header checksum, a non-ustar magic,
    /// a foreign ustar version, a prefix without a name, or a directory
    /// entry with a non-zero size.
    ArchiveMalformed,
}

impl RefusalReason {
    /// The stable wire name, matching the contract matrix exactly.
    pub fn wire(self) -> &'static str {
        match self {
            Self::RootMissing => "root-missing",
            Self::RootUnreadable => "root-unreadable",
            Self::PinMissing => "pin-missing",
            Self::PinUnreadable => "pin-unreadable",
            Self::PinOversized => "pin-oversized",
            Self::PinMalformed => "pin-malformed",
            Self::PinSourceNotGit => "pin-source-not-git",
            Self::PinSourceUntracked => "pin-source-untracked",
            Self::PinSourceDirty => "pin-source-dirty",
            Self::TargetUnsupported => "target-unsupported",
            Self::SourceUnsupported => "source-unsupported",
            Self::SourceFetchFailed => "source-fetch-failed",
            Self::ArchiveDigestMismatch => "archive-digest-mismatch",
            Self::ArchiveMissingBinary => "archive-missing-binary",
            Self::ArchiveUnsafeEntry => "archive-unsafe-entry",
            Self::ArchiveTruncated => "archive-truncated",
            Self::ActivationInFlight => "activation-in-flight",
            Self::StagedCopyIncomplete => "staged-copy-incomplete",
            Self::ActivationFailed => "activation-failed",
            Self::IdentityWriteFailed => "identity-write-failed",
            Self::ArchiveMalformed => "archive-malformed",
        }
    }
}

/// The context every refusal carries: the failed tool, version, target,
/// source and pin archive digest, plus the digest pair where a digest
/// comparison refused. No field ever carries an operator filesystem path
/// or a secret: a source is a release input, never the operator's tree.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct RefusalContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_sha256: Option<String>,
}

/// One refusal of an activation attempt: a typed reason, a stable detail
/// (never a path), and the context the diagnostic law demands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub reason: RefusalReason,
    pub detail: String,
    /// Boxed: the context is a cold-path diagnostic payload, and the
    /// refusal itself is returned by value on every refused attempt.
    pub context: Box<RefusalContext>,
}

impl Refusal {
    pub fn new(reason: RefusalReason, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
            context: Box::new(RefusalContext::default()),
        }
    }

    pub fn with_tool(mut self, tool: &str) -> Self {
        self.context.tool = Some(tool.to_owned());
        self
    }

    pub fn with_version(mut self, version: &str) -> Self {
        self.context.version = Some(version.to_owned());
        self
    }

    pub fn with_target(mut self, target: &str) -> Self {
        self.context.target = Some(target.to_owned());
        self
    }

    pub fn with_digests(mut self, expected: &str, actual: &str) -> Self {
        self.context.expected_sha256 = Some(expected.to_owned());
        self.context.actual_sha256 = Some(actual.to_owned());
        self
    }

    pub fn with_source(mut self, source: &str) -> Self {
        self.context.source = Some(source.to_owned());
        self
    }

    pub fn with_archive_sha256(mut self, digest: &str) -> Self {
        self.context.archive_sha256 = Some(digest.to_owned());
        self
    }

    /// The refusal receipt: the receipt schema a success carries, with
    /// `outcome: "refused"`. Bounded by the receipt bound.
    pub fn receipt_json(&self) -> String {
        let value = serde_json::json!({
            "schema": "grapt.toolchain.receipt.v1",
            "outcome": "refused",
            "reason": self.reason.wire(),
            "detail": self.detail,
            "context": &self.context,
        });
        let text = value.to_string();
        if text.len() <= crate::activate::ActivationReceipt::MAX_BYTES {
            text
        } else {
            // The bound is declared in the contract; a context this large
            // is a defect, and the receipt must still refuse honestly.
            serde_json::json!({
                "schema": "grapt.toolchain.receipt.v1",
                "outcome": "refused",
                "reason": self.reason.wire(),
                "detail": self.detail,
            })
            .to_string()
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "refusal {}: {}", self.reason.wire(), self.detail)?;
        if let Some(tool) = &self.context.tool {
            write!(f, " tool={tool}")?;
        }
        if let Some(version) = &self.context.version {
            write!(f, " version={version}")?;
        }
        if let Some(target) = &self.context.target {
            write!(f, " target={target}")?;
        }
        if let Some(source) = &self.context.source {
            write!(f, " source={source}")?;
        }
        if let Some(archive) = &self.context.archive_sha256 {
            write!(f, " archive_sha256={archive}")?;
        }
        if let Some(expected) = &self.context.expected_sha256 {
            write!(f, " expected={expected}")?;
        }
        if let Some(actual) = &self.context.actual_sha256 {
            write!(f, " actual={actual}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Refusal {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reason_has_a_stable_wire_name() {
        // Exhaustive by construction: adding a reason without a wire name
        // fails to compile, not to review.
        let wire = |reason: RefusalReason| match reason {
            RefusalReason::RootMissing => "root-missing",
            RefusalReason::RootUnreadable => "root-unreadable",
            RefusalReason::PinMissing => "pin-missing",
            RefusalReason::PinUnreadable => "pin-unreadable",
            RefusalReason::PinOversized => "pin-oversized",
            RefusalReason::PinMalformed => "pin-malformed",
            RefusalReason::PinSourceNotGit => "pin-source-not-git",
            RefusalReason::PinSourceUntracked => "pin-source-untracked",
            RefusalReason::PinSourceDirty => "pin-source-dirty",
            RefusalReason::TargetUnsupported => "target-unsupported",
            RefusalReason::SourceUnsupported => "source-unsupported",
            RefusalReason::SourceFetchFailed => "source-fetch-failed",
            RefusalReason::ArchiveDigestMismatch => "archive-digest-mismatch",
            RefusalReason::ArchiveMissingBinary => "archive-missing-binary",
            RefusalReason::ArchiveUnsafeEntry => "archive-unsafe-entry",
            RefusalReason::ArchiveTruncated => "archive-truncated",
            RefusalReason::ActivationInFlight => "activation-in-flight",
            RefusalReason::StagedCopyIncomplete => "staged-copy-incomplete",
            RefusalReason::ActivationFailed => "activation-failed",
            RefusalReason::IdentityWriteFailed => "identity-write-failed",
            RefusalReason::ArchiveMalformed => "archive-malformed",
        };
        let all = [
            RefusalReason::RootMissing,
            RefusalReason::RootUnreadable,
            RefusalReason::PinMissing,
            RefusalReason::PinUnreadable,
            RefusalReason::PinOversized,
            RefusalReason::PinMalformed,
            RefusalReason::PinSourceNotGit,
            RefusalReason::PinSourceUntracked,
            RefusalReason::PinSourceDirty,
            RefusalReason::TargetUnsupported,
            RefusalReason::SourceUnsupported,
            RefusalReason::SourceFetchFailed,
            RefusalReason::ArchiveDigestMismatch,
            RefusalReason::ArchiveMissingBinary,
            RefusalReason::ArchiveUnsafeEntry,
            RefusalReason::ArchiveTruncated,
            RefusalReason::ActivationInFlight,
            RefusalReason::StagedCopyIncomplete,
            RefusalReason::ActivationFailed,
            RefusalReason::IdentityWriteFailed,
            RefusalReason::ArchiveMalformed,
        ];
        assert_eq!(all.len(), 21);
        for reason in all {
            assert_eq!(reason.wire(), wire(reason));
        }
    }

    #[test]
    fn the_display_names_the_failed_facts_and_no_paths() {
        let refusal = Refusal::new(
            RefusalReason::ArchiveDigestMismatch,
            "the delivered archive does not hash to the pinned archive digest",
        )
        .with_tool("ripgrep")
        .with_version("15.1.0")
        .with_target("x86_64-pc-windows-msvc")
        .with_source("archives/rg.tar")
        .with_digests(
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "0000000000000000000000000000000000000000000000000000000000000000",
        );
        let text = refusal.to_string();
        for fact in [
            "archive-digest-mismatch",
            "tool=ripgrep",
            "version=15.1.0",
            "target=x86_64-pc-windows-msvc",
            "source=archives/rg.tar",
            "expected=ba7816bf",
            "actual=0000000000000000000000000000000000000000000000000000000000000000",
        ] {
            assert!(text.contains(fact), "display misses {fact}: {text}");
        }
        // The prose detail never carries a path; a source value is a
        // release input and may carry separators.
        assert!(!refusal.detail.contains('/'), "detail leaks a path: {text}");
    }

    #[test]
    fn the_receipt_carries_the_reason_and_context() {
        let refusal = Refusal::new(RefusalReason::PinMalformed, "pin carries a foreign schema")
            .with_tool("ripgrep")
            .with_version("15.1.0");
        let receipt: serde_json::Value =
            serde_json::from_str(&refusal.receipt_json()).unwrap_or_default();
        assert_eq!(
            receipt["schema"].as_str(),
            Some("grapt.toolchain.receipt.v1")
        );
        assert_eq!(receipt["outcome"].as_str(), Some("refused"));
        assert_eq!(receipt["reason"].as_str(), Some("pin-malformed"));
        assert_eq!(receipt["context"]["tool"].as_str(), Some("ripgrep"));
        assert_eq!(receipt["context"]["version"].as_str(), Some("15.1.0"));
        assert_eq!(receipt["context"].get("target"), None);
    }
}
