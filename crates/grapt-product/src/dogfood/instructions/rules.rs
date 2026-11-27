//! Scoped rule discovery over agent instruction files.
// Copyright 2023-2026 SpaceXAI. Apache-2.0.
// Modified by Grapt: bounded discovery, strict paths/globs and session containment.

use std::path::{Path, PathBuf};

use globset::GlobBuilder;
use yaml_rust2::Yaml;

use super::{bounded_reason, contained, imports, read_file, ScopedInstruction, ScopedRead};
use super::{MAX_FILES, MAX_INSTRUCTION_BYTES, MAX_SCOPED_BYTES};

pub(super) struct Discovery {
    scope: PathBuf,
    entries: usize,
    files: usize,
    bytes: usize,
    stopped: bool,
}

impl Discovery {
    pub(super) fn new(scope: PathBuf) -> Self {
        Self {
            scope,
            entries: 0,
            files: 0,
            bytes: 0,
            stopped: false,
        }
    }

    pub(super) fn append(
        &mut self,
        root: &Path,
        directory: &Path,
        file: &Path,
        found: &mut ScopedRead,
        bytes: &mut usize,
    ) {
        if self.stopped {
            return;
        }
        // The repository's project directory: `.grapt`, else a legacy one.
        let rules = crate::dogfood::legacy_names::project_directory(directory).join("rules");
        let paths = match self.paths(&rules) {
            Ok(paths) => paths,
            Err(error) => {
                self.stopped = true;
                found.instructions.push(ScopedInstruction {
                    path: relative(root, &rules),
                    content: Err(bounded_reason(&format!("rule discovery incomplete; remaining rule directories not checked: {error}"))),
                });
                return;
            }
        };
        for path in paths {
            let content = self.rule(directory, file, &path).and_then(|content| {
                let Some(content) = content else {
                    return Ok(None);
                };
                *bytes = bytes.saturating_add(content.len());
                if *bytes > MAX_SCOPED_BYTES {
                    Err("scoped instructions for this read exceed 64 KiB".into())
                } else {
                    Ok(Some(content))
                }
            });
            let content = match content {
                Ok(None) => continue,
                Ok(Some(content)) => Ok(content),
                Err(error) => Err(bounded_reason(&error)),
            };
            found.instructions.push(ScopedInstruction {
                path: relative(root, &path),
                content,
            });
        }
    }

    /// Direct children only, sorted independently of filesystem enumeration.
    fn paths(&mut self, rules: &Path) -> Result<Vec<PathBuf>, String> {
        match std::fs::symlink_metadata(rules) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("rule directory metadata: {error}")),
            Ok(_) => {}
        }
        contained(rules, &self.scope)?;
        let directory =
            std::fs::read_dir(rules).map_err(|error| format!("rule directory scan: {error}"))?;
        let mut paths = Vec::new();
        for entry in directory {
            self.entries += 1;
            if self.entries > 256 {
                return Err("rule discovery exceeds 256 directory entries".into());
            }
            let path = entry.map_err(|error| error.to_string())?.path();
            if path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
            {
                self.files += 1;
                if self.files > MAX_FILES {
                    return Err("rule discovery exceeds 64 Markdown files".into());
                }
                paths.push(path);
            }
        }
        paths.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        Ok(paths)
    }

    fn rule(
        &mut self,
        directory: &Path,
        file: &Path,
        path: &Path,
    ) -> Result<Option<String>, String> {
        let mut source = read_file(path, Some(&self.scope))?.ok_or("discovered rule is missing")?;
        self.bytes = self.bytes.saturating_add(source.text.len());
        if self.bytes > MAX_INSTRUCTION_BYTES {
            return Err("rule source input exceeds 1 MiB".into());
        }
        let rule = super::super::catalog_markdown::parse(&source.text)?;
        let metadata = match (&rule.metadata["paths"], &rule.metadata["globs"]) {
            (paths, Yaml::BadValue) => paths,
            (Yaml::BadValue, globs) => globs,
            _ => return Err("rule frontmatter cannot contain both paths and globs".into()),
        };
        let patterns =
            match metadata {
                Yaml::String(pattern) => vec![pattern.as_str()],
                Yaml::Array(patterns) => patterns
                    .iter()
                    .map(|pattern| pattern.as_str().ok_or("rule patterns must contain strings"))
                    .collect::<Result<Vec<_>, _>>()?,
                _ => return Err(
                    "rule frontmatter requires paths or globs: a string or nonempty string list"
                        .into(),
                ),
            };
        if patterns.is_empty() || patterns.len() > 64 {
            return Err("rule patterns must contain 1 to 64 patterns".into());
        }
        let target = relative(directory, file);
        let mut matches = false;
        for pattern in patterns {
            if pattern.is_empty()
                || pattern.len() > 1024
                || pattern.starts_with(['/', '!', '~'])
                || pattern.contains(['\\', ':', '\0'])
                || pattern.split('/').any(|part| part == "..")
            {
                return Err(
                    "rule glob must be a relative, forward-slash pattern of at most 1024 bytes"
                        .into(),
                );
            }
            let glob = GlobBuilder::new(pattern)
                .literal_separator(true)
                .backslash_escape(false)
                .build()
                .map_err(|_| "rule glob is malformed")?;
            matches |= glob.compile_matcher().is_match(&target);
        }
        if !matches {
            return Ok(None);
        }
        // Unmatched rule imports never become ambient dependencies.
        source.text = rule.content;
        imports::expand(source).map(Some)
    }
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}
