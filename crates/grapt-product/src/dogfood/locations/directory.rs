//! A directory project keeps its own root when Git metadata is absent.
// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// Modified for Grapt:
// canonical path identity and an explicit directory project, without Git writes.
use super::{location, ResolvedLocation};
use std::path::Path;

pub(super) fn resolve(path: &Path) -> Result<ResolvedLocation, String> {
    let root =
        std::fs::canonicalize(path).map_err(|error| format!("location is unreadable: {error}"))?;
    let directory = root.to_str().ok_or("location is not UTF-8")?;
    let digest = ring::digest::digest(&ring::digest::SHA256, directory.as_bytes());
    let identity = format!(
        "dir_{}",
        digest
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    location(directory, &identity, &root)
}
