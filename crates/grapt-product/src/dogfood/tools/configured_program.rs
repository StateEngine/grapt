//! Resolve installed command images without granting workspace PATH authority.
use super::shell_environment::Environment;
use std::path::{Path, PathBuf};

pub(super) fn resolve(
    program: &str,
    root: &Path,
    environment: &Environment,
    label: &str,
) -> Result<PathBuf, String> {
    let program = Path::new(program);
    if program.is_absolute() {
        return Ok(program.to_path_buf());
    }
    if program.components().count() != 1 {
        return Err(format!(
            "{label} executable must be absolute or a PATH name"
        ));
    }
    let path = environment
        .iter()
        .find(|(name, _)| {
            name.to_str()
                .is_some_and(|s| s == "PATH" || cfg!(windows) && s.eq_ignore_ascii_case("PATH"))
        })
        .map(|(_, value)| value)
        .ok_or_else(|| format!("{label} PATH is unavailable; configure an absolute executable"))?;
    for (index, directory) in std::env::split_paths(path).enumerate() {
        if index >= 1024 {
            return Err(format!("{label} PATH exceeds 1024 directories"));
        }
        if !directory.is_absolute() || workspace_owned(root, &directory)? {
            continue;
        }
        // Resolve the directory, not the invoked basename: external shims may
        // dispatch on argv[0], but a workspace PATH alias is not launch authority.
        let directory = match std::fs::canonicalize(directory) {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                return Err(format!(
                    "{label} PATH directory is unreadable; configure an absolute executable"
                ))
            }
        };
        if workspace_owned(root, &directory)? {
            continue;
        }
        let candidate = directory.join(program);
        #[cfg(windows)]
        let candidates = if candidate.extension().is_none() {
            vec![
                candidate.with_extension("exe"),
                candidate.with_extension("cmd"),
                candidate.with_extension("bat"),
            ]
        } else {
            vec![candidate]
        };
        #[cfg(not(windows))]
        let candidates = [candidate];
        for candidate in candidates {
            let resolved = match std::fs::canonicalize(&candidate) {
                Ok(candidate) => candidate,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    return Err(format!(
                        "{label} PATH candidate is unreadable; configure an absolute executable"
                    ))
                }
            };
            // A repository can supply a PATH entry but cannot become executable authority.
            if workspace_owned(root, &resolved)? || !resolved.is_file() {
                continue;
            }
            #[cfg(not(unix))]
            if !resolved
                .file_stem()
                .and_then(|name| name.to_str())
                .zip(program.file_stem().and_then(|name| name.to_str()))
                .is_some_and(|(image, invoked)| image.eq_ignore_ascii_case(invoked))
            {
                return Err(format!("{label} PATH alias changes its dispatch name; configure an absolute user command for this alias"));
            }
            // Spawn the validated image; Unix dispatch names are carried separately.
            return Ok(resolved);
        }
    }
    Err(format!("{label} executable not found outside the workspace; install it or configure an absolute user-owned command"))
}

/// The validated entry image may itself use /usr/bin/env or launch a helper.
/// Give those lookups the same external-only, canonical-directory PATH.
pub(super) fn search_path(
    root: &Path,
    path: &std::ffi::OsStr,
) -> Result<std::ffi::OsString, String> {
    let mut directories = Vec::new();
    for (index, directory) in std::env::split_paths(path).enumerate() {
        if index >= 1024 {
            return Err("configured command PATH exceeds 1024 directories".into());
        }
        if !directory.is_absolute() || workspace_owned(root, &directory)? {
            continue;
        }
        let directory = match std::fs::canonicalize(directory) {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("configured command PATH directory is unreadable; fix the user PATH before retrying".into()),
        };
        if !workspace_owned(root, &directory)? {
            directories.push(directory);
        }
    }
    if directories.is_empty() {
        return Err("configured command PATH has no external directories; add installed tools to the user PATH".into());
    }
    std::env::join_paths(directories).map_err(|_| {
        "configured command PATH cannot be encoded; fix the user PATH before retrying".into()
    })
}

// Reuse native directory-identity checks so Windows aliases cannot change authority.
fn workspace_owned(root: &Path, path: &Path) -> Result<bool, String> {
    const OUTSIDE: &str = "configured command path is outside the workspace";
    match crate::dogfood::authority_path::relative(root, path, || OUTSIDE.into()) {
        Ok(_) => Ok(true),
        Err(reason) if reason == OUTSIDE => Ok(false),
        Err(reason) => Err(reason),
    }
}
