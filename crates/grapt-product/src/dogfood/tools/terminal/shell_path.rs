//! Prepared-PATH discovery.
//! Copyright 2025 OpenAI.
//! Apache-2.0; modified by Grapt: prepared environment only, no fallback
//! executable substitution, and narrow Windows App Execution Alias handling.
use super::super::*;

const NAMES: &[&str] = &["pwsh", "powershell", "cmd", "bash", "sh", "zsh", "dash"];

pub(super) fn available(environment: &shell_environment::Environment) -> String {
    let names = NAMES
        .iter()
        .filter(|name| find(name, environment).is_ok())
        .copied()
        .collect::<Vec<_>>();
    if names.is_empty() {
        "none on prepared PATH; use an absolute shell path".into()
    } else {
        names.join(", ")
    }
}

pub(super) fn resolve(
    requested: &str,
    environment: &shell_environment::Environment,
) -> Result<PathBuf, String> {
    let result = if requested.len() > 8192 || requested.contains('\0') {
        Err("shell must contain at most 8192 bytes and no NUL".into())
    } else if NAMES.contains(&requested) {
        find(requested, environment)
    } else if Path::new(requested).is_absolute() {
        executable(Path::new(requested))
    } else {
        Err("use a supported shell name or an absolute path".into())
    };
    result.map_err(|detail| {
        // Debug escaping keeps model-supplied control characters out of diagnostics.
        let path = requested.chars().take(512).collect::<String>();
        format!(
            "shell {path:?} could not be resolved: {detail}; available shells: {}",
            available(environment)
        )
    })
}

fn find(name: &str, environment: &shell_environment::Environment) -> Result<PathBuf, String> {
    let path = environment.iter().find_map(|(key, value)| {
        let matches = if cfg!(windows) {
            key.eq_ignore_ascii_case(std::ffi::OsStr::new("PATH"))
        } else {
            key == "PATH"
        };
        matches.then_some(value)
    });
    let path = path.ok_or("prepared PATH is unset")?;
    let filename = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.into()
    };
    for directory in std::env::split_paths(path).filter(|path| path.is_absolute()) {
        if let Ok(path) = executable(&directory.join(&filename)) {
            return Ok(path);
        }
    }
    Err("no executable found on prepared PATH (empty and relative entries are not searched)".into())
}

fn executable(path: &Path) -> Result<PathBuf, String> {
    let canonical = match std::fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) => {
            #[cfg(windows)]
            if execution_alias(path, &error) {
                return Ok(path.to_owned());
            }
            return Err(error.to_string());
        }
    };
    let metadata = match std::fs::metadata(&canonical) {
        Ok(metadata) => metadata,
        Err(error) => {
            #[cfg(windows)]
            if execution_alias(path, &error) {
                return Ok(path.to_owned());
            }
            return Err(error.to_string());
        }
    };
    if !metadata.is_file() {
        return Err("shell must be a regular executable file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err("shell is not executable".into());
        }
    }
    Ok(canonical)
}

#[cfg(windows)]
pub(super) fn execution_alias(path: &Path, error: &std::io::Error) -> bool {
    use std::os::windows::fs::MetadataExt;
    std::fs::symlink_metadata(path).is_ok_and(|metadata| {
        alias_failure(
            path,
            error.raw_os_error(),
            metadata.file_attributes(),
            metadata.file_type().is_symlink(),
        )
    })
}

#[cfg(any(windows, test))]
fn alias_failure(path: &Path, error: Option<i32>, attributes: u32, symlink: bool) -> bool {
    // Store aliases fail traversal with ERROR_CANT_ACCESS_FILE. Missing paths,
    // access denial and cyclic symlinks (1921) are not alias exceptions.
    // Require a non-directory, non-symlink reparse file beneath WindowsApps.
    error == Some(1920)
        && !symlink
        && attributes & 0x400 != 0
        && attributes & 0x10 == 0
        && path.components().any(|part| {
            part.as_os_str()
                .eq_ignore_ascii_case(std::ffi::OsStr::new("WindowsApps"))
        })
        && path.file_name().is_some_and(|name| {
            NAMES.iter().any(|shell| {
                name.eq_ignore_ascii_case(std::ffi::OsStr::new(&format!("{shell}.exe")))
            })
        })
}

#[cfg(test)]
mod tests;
