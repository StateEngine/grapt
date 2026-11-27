// Copyright 2025 OpenAI
// SPDX-License-Identifier: Apache-2.0
// Modified by Grapt: reject names whose ordinary Windows spelling aliases them.
//! Git for Windows requires drive/UNC paths rather than Rust's verbatim prefix.

use std::borrow::Cow;
use std::path::Path;

pub(super) fn git_argument(path: &Path) -> Result<Cow<'_, Path>, String> {
    #[cfg(windows)]
    {
        let raw = path.to_str().ok_or("Git worktree path is not UTF-8")?;
        Ok(Cow::Owned(std::path::PathBuf::from(windows_path(raw)?)))
    }
    #[cfg(not(windows))]
    {
        Ok(Cow::Borrowed(path))
    }
}

#[cfg(any(windows, test))]
fn windows_path(raw: &str) -> Result<String, String> {
    let ordinary = if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else if let Some(drive) = raw.strip_prefix(r"\\?\") {
        let bytes = drive.as_bytes();
        if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || &bytes[1..3] != b":\\" {
            return Err("Git worktree path uses an unsupported Windows namespace".into());
        }
        drive.to_owned()
    } else {
        raw.to_owned()
    };
    let names = if let Some(unc) = ordinary.strip_prefix(r"\\") {
        unc
    } else {
        ordinary.get(3..).unwrap_or_default()
    };
    for name in names.split(['\\', '/']) {
        let stem = name
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || stem
                .strip_prefix("COM")
                .or_else(|| stem.strip_prefix("LPT"))
                .is_some_and(|suffix| {
                    matches!(
                        suffix,
                        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                    )
                });
        if device
            || name.ends_with(['.', ' '])
            || name
                .chars()
                .any(|ch| ch.is_ascii_control() || "<>:\"|?*".contains(ch))
        {
            return Err("Git worktree path requires verbatim Windows name semantics".into());
        }
    }
    Ok(ordinary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_receives_drive_and_unc_paths_without_namespace_aliases() -> Result<(), String> {
        assert_eq!(
            windows_path(r"\\?\E:\temp\isolated checkout")?,
            r"E:\temp\isolated checkout"
        );
        assert_eq!(
            windows_path(r"\\?\UNC\server\share\nested checkout")?,
            r"\\server\share\nested checkout"
        );
        for raw in [
            r"\\?\GLOBALROOT\Device",
            r"\\?\E:\parent.\child",
            r"\\?\E:\parent \child",
            r"\\?\E:\CON\child",
            r"\\?\E:\COM¹\child",
            r"\\?\E:\repo\file:stream",
        ] {
            assert!(windows_path(raw).is_err(), "aliased path accepted: {raw}");
        }
        Ok(())
    }
}
