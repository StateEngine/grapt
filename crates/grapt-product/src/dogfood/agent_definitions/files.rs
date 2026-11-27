//! Bounded, deterministic capture. Missing roots are normal; unreadable inputs fail.
use crate::dogfood::authority_path::linked;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Default)]
pub(super) struct Capture {
    pub entries: usize,
    pub bytes: usize,
    pub aliases: Vec<PathBuf>,
}

/// Imported sources select one flat extension; native sources recurse through Markdown.
pub(super) fn collect(
    root: &Path,
    entries: &mut usize,
    extension: Option<&str>,
    user_owned: bool,
    aliases: &mut Vec<PathBuf>,
) -> Result<Vec<PathBuf>, String> {
    let mut pending = vec![(root.to_path_buf(), 0)];
    let mut paths = Vec::new();
    while let Some((directory, depth)) = pending.pop() {
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if linked(&metadata) && user_owned => {
                aliases.push(fs::canonicalize(&directory).map_err(|error| {
                    format!("agent directory {}: {error}", directory.display())
                })?);
            }
            Ok(metadata) if linked(&metadata) => {
                return Err(format!(
                    "{}: linked agent directory is not loaded; place definitions in a regular directory",
                    directory.display()
                ));
            }
            Ok(_) => {}
            Err(error) if depth == 0 && error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("agent directory {}: {error}", directory.display())),
        }
        let read = fs::read_dir(&directory)
            .map_err(|error| format!("agent directory {}: {error}", directory.display()))?;
        for entry in read {
            *entries += 1;
            if *entries > 2048 {
                return Err(format!(
                    "{}: agent discovery exceeds 2048 entries",
                    directory.display()
                ));
            }
            let entry = entry
                .map_err(|error| format!("agent directory {}: {error}", directory.display()))?;
            let path = entry.path();
            if path.as_os_str().len() > 4096 {
                return Err(format!(
                    "{}: agent path exceeds 4096 bytes",
                    directory.display()
                ));
            }
            // Documentation and unrelated links are not agent declarations.
            let documentation = extension.is_none()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.eq_ignore_ascii_case("README.md"));
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| format!("agent source {}: {error}", path.display()))?;
            let candidate = !documentation
                && path.extension().is_some_and(|ext| match extension {
                    Some(selected) => ext == selected,
                    None => ext == "md" || (depth == 0 && ext == "toml"),
                });
            if linked(&metadata) {
                let directory_link = metadata.is_dir()
                    || match fs::metadata(&path) {
                        Ok(target) => target.is_dir(),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                        Err(error) => {
                            return Err(format!("agent source {}: {error}", path.display()))
                        }
                    };
                if user_owned && directory_link {
                    if extension.is_none() {
                        if depth == 16 {
                            return Err(format!(
                                "{}: agent discovery exceeds 16 directory levels",
                                path.display()
                            ));
                        }
                        pending.push((path, depth + 1));
                    }
                    continue;
                }
                if candidate || directory_link {
                    return Err(format!(
                        "{}: linked agent source is not loaded; place definitions in regular files and directories",
                        path.display()
                    ));
                }
                continue;
            }
            if metadata.is_dir() && extension.is_none() {
                if depth == 16 {
                    return Err(format!(
                        "{}: agent discovery exceeds 16 directory levels",
                        path.display()
                    ));
                }
                pending.push((path, depth + 1));
            } else if candidate {
                if !metadata.is_file() {
                    return Err(format!(
                        "{}: agent definition must be a regular file",
                        path.display()
                    ));
                }
                paths.push(path);
            }
        }
    }
    paths.sort();
    Ok(paths)
}

fn read_text(path: &Path, total: &mut usize) -> Result<String, String> {
    let mut capture = || {
        let file = open_regular(path).map_err(|error| format!("cannot open: {error}"))?;
        let identity = same_file::Handle::from_file(file).map_err(|error| error.to_string())?;
        let mut text = String::new();
        identity
            .as_file()
            .take(64 * 1024 + 1)
            .read_to_string(&mut text)
            .map_err(|error| format!("cannot read UTF-8: {error}"))?;
        *total += text.len();
        if text.len() > 64 * 1024 || *total > 1024 * 1024 {
            return Err("agent definitions exceed the 64 KiB file or 1 MiB aggregate bound".into());
        }
        let current =
            same_file::Handle::from_file(open_regular(path).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        if identity != current {
            return Err("agent definition changed during capture".into());
        }
        Ok(text)
    };
    capture().map_err(|reason: String| format!("{}: {reason}", path.display()))
}

pub(super) fn read_toml(path: &Path, total: &mut usize) -> Result<serde_json::Value, String> {
    let text = read_text(path, total)?;
    let value: toml::Value = toml::from_str(&text)
        .map_err(|error| format!("{}: invalid agent TOML: {error}", path.display()))?;
    serde_json::to_value(value).map_err(|error| format!("{}: {error}", path.display()))
}

pub(super) fn read_markdown(
    path: &Path,
    total: &mut usize,
) -> Result<(yaml_rust2::Yaml, String), String> {
    let text = read_text(path, total)?;
    let markdown = super::super::catalog_markdown::parse(&text)
        .map_err(|error| format!("{}: invalid agent Markdown: {error}", path.display()))?;
    Ok((markdown.metadata, markdown.content))
}

// The same no-follow, nonblocking capture used by coding/patch/staging.
// Check the opened object: discovery metadata cannot constrain a later open.
fn open_regular(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        // Retain the directory entry while capturing and checking its identity.
        options
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .share_mode(0x1 | 0x2);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || linked(&metadata) {
        return Err(std::io::Error::other(
            "agent definition must be a regular file",
        ));
    }
    Ok(file)
}
