//! Filesystem resolution for authority identities, including absent targets.
use std::path::Path;

/// A filesystem alias cannot stand in for a literal authority path.
pub(super) fn linked(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

/// Resolve absent authority files against their nearest readable ancestor as
/// well: creating an absent launch config must not raise trust unseen.
pub(super) fn resolve(path: &Path) -> Result<String, String> {
    let mut current = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("authority path base is unreadable: {error}"))?
            .join(path)
    };
    let mut tail = Vec::new();
    for _ in 0..64 {
        match std::fs::canonicalize(&current) {
            Ok(mut resolved) => {
                for name in tail.into_iter().rev() {
                    resolved.push(name);
                }
                return resolved
                    .to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "authority path is not UTF-8".to_owned());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = current
                    .file_name()
                    .ok_or_else(|| "authority path has no filename".to_owned())?
                    .to_os_string();
                let parent = current
                    .parent()
                    .ok_or_else(|| "authority path has no parent".to_owned())?
                    .to_path_buf();
                tail.push(name);
                current = parent;
            }
            Err(error) => return Err(format!("authority path is unreadable: {error}")),
        }
    }
    Err("authority path exceeds 64 ancestors".to_owned())
}

/// Name a target below the workspace in the workspace's spelling. Windows aliases
/// may have different drive/share prefixes; compare existing directory identities.
/// The caller supplies outside-root guidance; unreadable identities retain their cause.
pub(super) fn relative<'a>(
    root: &Path,
    target: &'a Path,
    outside: impl FnOnce() -> String,
) -> Result<&'a Path, String> {
    #[cfg(windows)]
    {
        let identity = same_file::Handle::from_path(root)
            .map_err(|error| format!("workspace identity is unreadable: {error}"))?;
        // Extended Windows paths can contain far more than 64 components.
        // Bound identity probing by the platform path-length ceiling instead
        // of the separate scoped-instruction discovery limit.
        for ancestor in target.ancestors().take(32_768) {
            match same_file::Handle::from_path(ancestor) {
                Ok(candidate) if candidate == identity => {
                    return target
                        .strip_prefix(ancestor)
                        .map_err(|_| "workspace alias has no relative suffix".into());
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("target identity is unreadable: {error}")),
            }
        }
        Err(outside())
    }
    #[cfg(not(windows))]
    target.strip_prefix(root).map_err(|_| outside())
}

/// The native regression measures a real short basename, without changing volume policy.
#[cfg(all(windows, test))]
pub(super) fn short_alias(target: &Path) -> Result<Option<std::path::PathBuf>, String> {
    let text = target.to_str().ok_or("short-name target is not UTF-8")?;
    let ordinary = text.strip_prefix(r"\\?\").unwrap_or(text);
    let command = format!("for %I in (\"{ordinary}\") do @echo %~sI");
    use std::os::windows::process::CommandExt;
    let output = std::process::Command::new(
        std::env::var_os("ComSpec").ok_or("native cmd location unavailable")?,
    )
    .args(["/D", "/C"])
    .raw_arg(&command)
    .output()
    .map_err(|e| e.to_string())?;
    if !output.status.success() || output.stdout.len() > 8192 {
        return Err("native full-file short-name lookup did not complete".into());
    }
    let short = String::from_utf8(output.stdout).map_err(|e| e.to_string())?;
    let short = std::path::PathBuf::from(short.trim());
    let name = |path: &Path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .ok_or("native short name has no filename")
    };
    if name(&short)?.eq_ignore_ascii_case(&name(target)?) {
        eprintln!("SEC-WIN-PREFIX short-file: not measured; file has no alternate short basename");
        return Ok(None);
    }
    assert_eq!(
        same_file::Handle::from_path(&short).map_err(|e| e.to_string())?,
        same_file::Handle::from_path(target).map_err(|e| e.to_string())?,
        "short alias changed object"
    );
    Ok(Some(short))
}
