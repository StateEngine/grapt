//! Bounded, in-place instruction imports. No new filesystem authority is implied.

use std::path::{Path, PathBuf};

use super::source::InstructionFile;
use super::{read_scoped_relative, MAX_FILES, MAX_FILE_BYTES, MAX_INSTRUCTION_BYTES};

const MAX_DEPTH: usize = 16;

pub(super) fn expand(source: InstructionFile) -> Result<String, String> {
    let mut graph = Graph {
        scope: source.scope,
        active: vec![source.path.clone()],
        files: 1,
        bytes: source.text.len(),
    };
    graph.render(&source.path, &source.text)
}

struct Graph {
    scope: PathBuf,
    active: Vec<PathBuf>,
    files: usize,
    bytes: usize,
}

impl Graph {
    fn render(&mut self, path: &Path, text: &str) -> Result<String, String> {
        let mut output = String::new();
        let mut fence = None;
        for line in text.split_inclusive('\n') {
            let trimmed = line.trim_end_matches(['\r', '\n']);
            let marker = trimmed.trim_start_matches(' ');
            let first = marker.as_bytes().first().copied().unwrap_or(0);
            let run = marker.bytes().take_while(|byte| *byte == first).count();
            if trimmed.len() - marker.len() <= 3 && matches!(first, b'`' | b'~') && run >= 3 {
                match fence {
                    None => fence = Some((first, run)),
                    Some((kind, length))
                        if kind == first && run >= length && marker[run..].trim().is_empty() =>
                    {
                        fence = None
                    }
                    _ => {}
                }
            }
            if fence.is_none() && trimmed.starts_with('@') {
                let name = &trimmed[1..];
                if name.is_empty()
                    || name.chars().any(char::is_whitespace)
                    || name.contains(['\\', ':', '\0'])
                    || name.starts_with(['/', '~'])
                {
                    return Err(format!(
                        "malformed instruction @import in {}: use @relative/path on its own line",
                        path.display()
                    ));
                }
                let parent = path.parent().ok_or("instruction import has no parent")?;
                let import = parent.join(name);
                // Keep the scope-relative path uncollapsed (avoiding Windows
                // verbatim-prefix `Path::join` collapse of `..` components) so
                // every original component is checked by the no-follow walk.
                let scope_relative = parent
                    .strip_prefix(&self.scope)
                    .map_err(|_| {
                        "instruction source resolves outside the directory it governs".to_owned()
                    })?
                    .join(name);
                if self.active.len() >= MAX_DEPTH || self.files >= MAX_FILES {
                    return Err(
                        "instruction import graph exceeds 16 levels or 64 source occurrences"
                            .into(),
                    );
                }
                // Scope, regular-file and byte checks apply to the SAME acquired
                // handle. Never re-resolve the authority anchor between imports.
                let source = read_scoped_relative(&self.scope, &scope_relative)?
                    .ok_or_else(|| format!("instruction import {name} is missing"))?;
                if self.active.contains(&source.path) {
                    return Err(format!("instruction import cycle at {}", import.display()));
                }
                self.files += 1;
                self.bytes = self.bytes.saturating_add(source.text.len());
                if self.bytes > MAX_INSTRUCTION_BYTES {
                    return Err("instruction import input exceeds 1 MiB".into());
                }
                self.active.push(source.path.clone());
                let expanded = self.render(&source.path, &source.text)?;
                self.active.pop();
                let heading = format!("[Imported instructions from: {}]\n", import.display());
                append(&mut output, &heading)?;
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
    if output.len().saturating_add(text.len()) > MAX_FILE_BYTES {
        return Err("expanded instruction source exceeds 64 KiB".into());
    }
    output.push_str(text);
    Ok(())
}
