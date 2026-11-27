//! Validate the actual post-import graph, including generated wrappers/headings.
//! Uses the native instruction import grammar and limits, with planned files as
//! a read-only overlay. No destination directories are created by validation.
use super::files;
use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
};

#[derive(Default)]
pub(super) struct Fence(Option<(u8, usize)>);
impl Fence {
    pub(super) fn reference<'a>(&mut self, line: &'a str) -> Result<Option<&'a str>, String> {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let marker = trimmed.trim_start_matches(' ');
        let first = marker.as_bytes().first().copied().unwrap_or(0);
        let run = marker.bytes().take_while(|byte| *byte == first).count();
        if trimmed.len() - marker.len() <= 3 && matches!(first, b'`' | b'~') && run >= 3 {
            match self.0 {
                None => self.0 = Some((first, run)),
                Some((kind, length))
                    if kind == first && run >= length && marker[run..].trim().is_empty() =>
                {
                    self.0 = None
                }
                _ => {}
            }
        }
        if self.0.is_none() && trimmed.starts_with('@') {
            let name = &trimmed[1..];
            if name.is_empty()
                || name.chars().any(char::is_whitespace)
                || name.contains(['\\', ':', '\0'])
                || name.starts_with(['/', '~'])
            {
                return Err(
                    "malformed instruction @import; use @relative/path on its own line".into(),
                );
            }
            Ok(Some(name))
        } else {
            Ok(None)
        }
    }
}

pub(super) fn normalized(path: &Path) -> Result<PathBuf, String> {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if !result.pop() {
                    return Err("instruction import escapes its root".into());
                }
            }
            Component::CurDir => {}
            component => result.push(component),
        }
    }
    if !result.is_absolute() {
        return Err("instruction path must be absolute".into());
    }
    Ok(result)
}

/// Walk existing components through the filesystem and planned components
/// lexically. Parent components therefore follow real symlinks, but can also
/// leave a directory that this import will create. No directories are created.
fn resolved(path: &Path, planned: Option<&BTreeSet<PathBuf>>) -> Result<PathBuf, String> {
    if !path.is_absolute() || path.as_os_str().len() > 4096 {
        return Err("instruction path must be absolute and at most 4096 bytes".into());
    }
    let components: Vec<_> = path.components().collect();
    if components.len() > 256 {
        return Err("instruction path exceeds 256 components".into());
    }
    let mut current = PathBuf::new();
    let mut virtual_depth = 0usize;
    for (index, component) in components.iter().enumerate() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => {
                current.push(component.as_os_str());
                current = std::fs::canonicalize(&current)
                    .map_err(|_| "instruction filesystem root is unresolved")?;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !current.pop() {
                    return Err("instruction import escapes its filesystem root".into());
                }
                virtual_depth = virtual_depth.saturating_sub(1);
            }
            Component::Normal(name) => {
                current.push(name);
                let more = index + 1 < components.len();
                if virtual_depth == 0 {
                    match std::fs::canonicalize(&current) {
                        Ok(canonical) => {
                            if more
                                && !std::fs::metadata(&canonical)
                                    .map_err(|_| "instruction parent is unreadable")?
                                    .is_dir()
                            {
                                return Err("instruction parent is not a directory".into());
                            }
                            current = canonical;
                            continue;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            match std::fs::symlink_metadata(&current) {
                                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                                _ => {
                                    return Err(
                                        "instruction component exists but cannot be resolved"
                                            .into(),
                                    )
                                }
                            }
                        }
                        Err(_) => return Err("instruction path is unreadable or unresolved".into()),
                    }
                }
                if more && planned.is_some_and(|directories| !directories.contains(&current)) {
                    return Err(
                        "instruction reference traverses a missing, unplanned directory".into(),
                    );
                }
                virtual_depth += 1;
            }
        }
    }
    Ok(current)
}

pub(super) fn validate(
    path: &Path,
    scope: &Path,
    body_only: bool,
    writes: &[(PathBuf, Vec<u8>)],
) -> Result<usize, String> {
    let writes = writes
        .iter()
        .map(|(path, bytes)| Ok((resolved(path, None)?, bytes.as_slice())))
        .collect::<Result<Vec<_>, String>>()?;
    let mut directories = BTreeSet::new();
    for (path, _) in &writes {
        for parent in path.ancestors().skip(1) {
            directories.insert(parent.to_path_buf());
            if directories.len() > 4096 {
                return Err("instruction plan exceeds 4096 parent directories".into());
            }
        }
    }
    let mut graph = Graph {
        scope: resolved(scope, None)?,
        writes,
        directories,
        active: Vec::new(),
        files: 0,
        bytes: 0,
    };
    let (path, mut text) = graph.read(path)?;
    if body_only {
        text = rule_body(&text)?.to_owned();
    }
    graph.files = 1;
    graph.bytes = text.len();
    graph.active.push(path.clone());
    graph.render(&path, &text).map(|text| text.len())
}

struct Graph<'a> {
    scope: PathBuf,
    writes: Vec<(PathBuf, &'a [u8])>,
    directories: BTreeSet<PathBuf>,
    active: Vec<PathBuf>,
    files: usize,
    bytes: usize,
}
impl Graph<'_> {
    fn read(&self, path: &Path) -> Result<(PathBuf, String), String> {
        let lookup = resolved(path, Some(&self.directories))?;
        let (resolved, bytes) =
            if let Some((_, bytes)) = self.writes.iter().find(|(path, _)| *path == lookup) {
                (lookup, bytes.to_vec())
            } else {
                let resolved = std::fs::canonicalize(&lookup).map_err(|_| {
                    format!(
                        "instruction import missing or unresolved: {}",
                        super::display(path)
                    )
                })?;
                if !resolved.starts_with(&self.scope) {
                    return Err("instruction import escapes its native scope".into());
                }
                let bytes = files::read(&resolved)?.ok_or("instruction import disappeared")?;
                (resolved, bytes)
            };
        if !resolved.starts_with(&self.scope) {
            return Err("instruction import escapes its native scope".into());
        }
        if bytes.len() > 65_536 {
            return Err("instruction source exceeds 64 KiB".into());
        }
        let text = String::from_utf8(bytes).map_err(|_| "instruction source is not UTF-8")?;
        Ok((resolved, text))
    }
    fn render(&mut self, path: &Path, text: &str) -> Result<String, String> {
        let mut output = String::new();
        let mut fence = Fence::default();
        for line in text.split_inclusive('\n') {
            if let Some(name) = fence.reference(line)? {
                if self.active.len() >= 16 || self.files >= 64 {
                    return Err(
                        "instruction graph exceeds 16 active files or 64 source occurrences".into(),
                    );
                }
                let import = path
                    .parent()
                    .ok_or("instruction source has no parent")?
                    .join(name);
                let (resolved, text) = self.read(&import)?;
                if self.active.contains(&resolved) {
                    return Err("instruction imports contain a cycle".into());
                }
                self.files += 1;
                self.bytes = self.bytes.saturating_add(text.len());
                if self.bytes > 1024 * 1024 {
                    return Err("instruction graph input exceeds 1 MiB".into());
                }
                self.active.push(resolved.clone());
                let expanded = self.render(&resolved, &text)?;
                self.active.pop();
                append(
                    &mut output,
                    &format!("[Imported instructions from: {}]\n", import.display()),
                )?;
                append(&mut output, &expanded)?;
                if line.ends_with('\n') && !expanded.ends_with('\n') {
                    append(&mut output, "\n")?;
                }
            } else {
                append(&mut output, line)?;
            }
        }
        Ok(output)
    }
}
fn append(output: &mut String, text: &str) -> Result<(), String> {
    if output.len().saturating_add(text.len()) > 65_536 {
        return Err("expanded instruction source including headings exceeds 64 KiB".into());
    }
    output.push_str(text);
    Ok(())
}
fn rule_body(text: &str) -> Result<&str, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.split_inclusive('\n');
    let first = lines.next().ok_or("rule is empty")?;
    if first.trim_end_matches(['\r', '\n']) != "---" {
        return Err("scoped rule has no frontmatter".into());
    }
    let mut offset = first.len();
    for line in lines {
        offset += line.len();
        if line.trim_end_matches(['\r', '\n']) == "---" {
            return Ok(&text[offset..]);
        }
    }
    Err("scoped rule frontmatter has no closing delimiter".into())
}
