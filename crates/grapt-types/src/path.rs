//! Path identity. Law (foundational report row `path`/`hook`/`credential`):
//! "one `PathIdentity` outcome; protected control metadata is removed from
//! writable authority and enforced consistently." There is exactly one
//! normalization outcome type, so different call sites cannot disagree on what a
//! path resolves to, and a path that cannot be resolved is a typed fact rather
//! than a silent fallback.

// Copyright 2025 OpenAI
// SPDX-License-Identifier: Apache-2.0
// Modified by Grapt: comparison-only ASCII folding; preserve other namespaces.
/// A comparison spelling for an already resolved path or a path pattern.
/// This does not resolve links, short names, or traversal and grants no authority.
/// Callers comparing filesystem objects must resolve them first.
pub fn comparison_key(path: &str) -> String {
    if cfg!(windows) {
        windows_comparison_key(path)
    } else {
        path.to_owned()
    }
}

/// Compare explicit Windows path spellings, including in cross-platform command parsing.
pub fn windows_comparison_key(path: &str) -> String {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    if let Some(unc) = path.strip_prefix("//?/unc/") {
        return format!("//{unc}");
    }
    if let Some(drive) = path.strip_prefix("//?/") {
        let bytes = drive.as_bytes();
        if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && &bytes[1..3] == b":/" {
            return drive.to_owned();
        }
    }
    path
}

/// The single outcome of normalizing a path. One type across every call site so
/// two normalizers cannot disagree; an unresolvable path is an explicit variant,
/// never an empty-string or "as given" fallback.
// `Ord` is derived so authority root lists have one deterministic order:
// capability identity must not depend on the order roots were configured in.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PathIdentity {
    /// The path canonicalized to this exact normalized form.
    Canonical(String),
    /// The path could not be canonicalized (e.g. it escapes an allowed root or
    /// its real form cannot be resolved). Never treated as a usable path.
    Unresolvable,
}

impl PathIdentity {
    /// Construct a canonical outcome. An empty normalized form is not a path, so
    /// it resolves to [`PathIdentity::Unresolvable`] rather than a blank path.
    pub fn canonical(normalized: impl Into<String>) -> Self {
        let normalized = normalized.into();
        if normalized.is_empty() {
            PathIdentity::Unresolvable
        } else {
            PathIdentity::Canonical(normalized)
        }
    }

    pub fn as_canonical(&self) -> Option<&str> {
        match self {
            PathIdentity::Canonical(path) => Some(path),
            PathIdentity::Unresolvable => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PathIdentity;

    #[test]
    fn a_path_has_one_outcome_and_empty_is_unresolvable() {
        assert_eq!(
            PathIdentity::canonical("/repo/src/main.rs").as_canonical(),
            Some("/repo/src/main.rs")
        );
        assert_eq!(PathIdentity::canonical(""), PathIdentity::Unresolvable);
        assert_eq!(PathIdentity::Unresolvable.as_canonical(), None);
        for (ordinary, verbatim) in [
            (r"C:\Repo\File", r"\\?\c:\repo\file"),
            (r"\\Host\Share\File", r"\\?\UNC\host\share\file"),
        ] {
            assert_eq!(
                super::windows_comparison_key(ordinary),
                super::windows_comparison_key(verbatim)
            );
        }
        #[cfg(not(windows))]
        for path in ["/Repo/Case", r"C:\Repo\Case", r"\\?\UNC\Host\Share"] {
            assert_eq!(super::comparison_key(path), path);
        }
    }
}
