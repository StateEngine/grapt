//! Bound construction shared by the witness files.
//!
//! One support module serves four witness files and no file needs every
//! helper, so the module carries a single `dead_code` allowance. The
//! alternative is a private copy of these helpers per file, which is exactly
//! the drift the next paragraph says this module exists to prevent.
//!
//! A module rather than helpers inside one witness file, because a bound set
//! assembled ad hoc in each witness drifts: two files would disagree about what
//! "generous" means and a bound witness would start passing for the wrong
//! reason. `unwrap`/`expect` are denied here too, so each helper matches and
//! panics naming the bound that was refused.

#![allow(dead_code)]

use grapt_config::{
    ConfigBounds, MaxAncestorDepth, MaxDocumentBytes, MaxDocumentNodes, MaxDocuments, MaxValueDepth,
};

/// Generous bounds, so a witness that is not about bounds never trips one.
pub fn bounds() -> ConfigBounds {
    custom_bounds(1024 * 1024, 32, 64, 32, 10_000)
}

pub fn custom_bounds(
    document_bytes: usize,
    ancestor_depth: usize,
    documents: usize,
    value_depth: usize,
    nodes: usize,
) -> ConfigBounds {
    let Ok(document_bytes) = MaxDocumentBytes::try_new(document_bytes) else {
        panic!("document byte bound {document_bytes} refused");
    };
    let Ok(ancestor_depth) = MaxAncestorDepth::try_new(ancestor_depth) else {
        panic!("ancestor depth bound {ancestor_depth} refused");
    };
    let Ok(documents) = MaxDocuments::try_new(documents) else {
        panic!("document count bound {documents} refused");
    };
    let Ok(value_depth) = MaxValueDepth::try_new(value_depth) else {
        panic!("value depth bound {value_depth} refused");
    };
    let Ok(nodes) = MaxDocumentNodes::try_new(nodes) else {
        panic!("node bound {nodes} refused");
    };
    ConfigBounds::new(
        document_bytes,
        ancestor_depth,
        documents,
        value_depth,
        nodes,
    )
}

/// Bound sets, paths and readers for the U1b1 source-custody witnesses.
///
/// The paths are built component-wise from a platform root rather than written
/// as literals. `/project/child` is absolute on Linux and NOT absolute on
/// Windows — it has a root and no prefix — so a literal would make every
/// discovery witness pass on the Linux gate and refuse on the focused Windows
/// run, for a reason that has nothing to do with the law under test.
pub mod source_support {
    use std::path::PathBuf;

    use grapt_config::{
        ConfigFileName, ConfigSourcePath, MaxAncestorDepth, MaxDocumentBytes, MaxDocuments,
        MaxPathBytes, SourceBounds,
    };

    /// An absolute path on this platform, from `parts`.
    pub fn abs(parts: &[&str]) -> PathBuf {
        let mut path = PathBuf::new();
        path.push(if cfg!(windows) { "C:\\" } else { "/" });
        for part in parts {
            path.push(part);
        }
        path
    }

    /// The generous path bound the non-bound witnesses run under.
    pub fn path_bound() -> MaxPathBytes {
        match MaxPathBytes::try_new(4096) {
            Ok(bound) => bound,
            Err(refusal) => panic!("path bound refused: {refusal}"),
        }
    }

    /// A canonical identity for an absolute path built from `parts`.
    pub fn identity(parts: &[&str]) -> ConfigSourcePath {
        match ConfigSourcePath::new(&abs(parts), path_bound()) {
            Ok(identity) => identity,
            Err(refusal) => panic!("path {parts:?} refused: {refusal}"),
        }
    }

    pub fn file_name(raw: &str) -> ConfigFileName {
        match ConfigFileName::new(raw) {
            Some(name) => name,
            None => panic!("config file name {raw:?} refused"),
        }
    }

    /// Generous source bounds, so a witness that is not about bounds never
    /// trips one. The rank space is deliberately larger than the walk.
    pub fn source_bounds() -> SourceBounds {
        custom_source_bounds(1024 * 1024, 32, 64, 4096)
    }

    pub fn custom_source_bounds(
        document_bytes: usize,
        ancestor_depth: usize,
        documents: usize,
        path_bytes: usize,
    ) -> SourceBounds {
        match try_source_bounds(document_bytes, ancestor_depth, documents, path_bytes) {
            Ok(bounds) => bounds,
            Err(refusal) => panic!("source bounds refused: {refusal}"),
        }
    }

    /// The fallible form, for the witness that is ABOUT the refusal.
    pub fn try_source_bounds(
        document_bytes: usize,
        ancestor_depth: usize,
        documents: usize,
        path_bytes: usize,
    ) -> Result<SourceBounds, grapt_config::SourceBoundsRefusal> {
        let Ok(document_bytes) = MaxDocumentBytes::try_new(document_bytes) else {
            panic!("document byte bound {document_bytes} refused");
        };
        let Ok(ancestor_depth) = MaxAncestorDepth::try_new(ancestor_depth) else {
            panic!("ancestor depth bound {ancestor_depth} refused");
        };
        let Ok(documents) = MaxDocuments::try_new(documents) else {
            panic!("document count bound {documents} refused");
        };
        let Ok(path_bytes) = MaxPathBytes::try_new(path_bytes) else {
            panic!("path byte bound {path_bytes} refused");
        };
        SourceBounds::try_new(document_bytes, ancestor_depth, documents, path_bytes)
    }
}
