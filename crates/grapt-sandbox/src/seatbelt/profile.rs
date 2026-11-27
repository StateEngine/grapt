// Copyright 2025 OpenAI. Apache-2.0.
// . Modified: globally intersect
// writable grants with protected paths, bounded arguments and authority anchors.
use crate::{NetworkAccess, Policy};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const MAX_ARGUMENT_BYTES: usize = 128 * 1024;
const MAX_PARAMETERS: usize = 512;
const BASE: &str = include_str!("base.sbpl");
const NETWORK: &str = include_str!("network.sbpl");

pub(super) struct Profile {
    pub text: String,
    pub parameters: Vec<(String, PathBuf)>,
}

impl Profile {
    #[cfg(target_os = "macos")]
    pub(super) fn passthrough_stderr(&mut self, descriptor: i32) -> Result<(), String> {
        let parameter = self.parameter(format!("/dev/fd/{descriptor}").into())?;
        self.text.push_str(&format!(
            "(allow file-read* file-write* (literal {parameter}))\n"
        ));
        self.check_bound()
    }

    fn check_bound(&self) -> Result<(), String> {
        let bytes = self.text.len()
            + self
                .parameters
                .iter()
                .map(|(key, path)| key.len() + path.as_os_str().len() + 4)
                .sum::<usize>();
        if bytes > MAX_ARGUMENT_BYTES {
            return Err("Seatbelt profile exceeds 128 KiB".into());
        }
        Ok(())
    }

    fn parameter(&mut self, path: PathBuf) -> Result<String, String> {
        if path.to_str().is_none_or(|value| value.len() > 32 * 1024) {
            return Err("Seatbelt resolved path is not bounded Unicode".into());
        }
        if self.parameters.len() >= MAX_PARAMETERS {
            return Err("Seatbelt policy exceeds 512 path parameters".into());
        }
        let key = format!("PATH_{}", self.parameters.len());
        self.parameters.push((key.clone(), path));
        Ok(format!("(param \"{key}\")"))
    }
}

/// Resolve missing names from their nearest existing parent without creating
/// anything. Both logical paths and resolved targets are retained as exclusions.
fn resolved(path: &Path) -> Result<PathBuf, String> {
    let mut suffix = Vec::new();
    let mut current = path;
    for _ in 0..64 {
        match std::fs::canonicalize(current) {
            Ok(mut root) => {
                for component in suffix.into_iter().rev() {
                    root.push(component);
                }
                return Ok(root);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if std::fs::symlink_metadata(current).is_ok() {
                    return Err("Seatbelt protected path contains a dangling symlink".into());
                }
                suffix.push(
                    current
                        .file_name()
                        .ok_or("Seatbelt path has no existing parent")?,
                );
                current = current.parent().ok_or("Seatbelt path has no parent")?;
            }
            Err(error) => return Err(format!("Seatbelt path cannot be resolved: {error}")),
        }
    }
    Err("Seatbelt path exceeds 64 ancestors".into())
}

fn ancestors(path: &Path) -> Result<Vec<&Path>, String> {
    let ancestors = path.ancestors().take(65).collect::<Vec<_>>();
    if ancestors.len() > 64 {
        return Err("Seatbelt path exceeds 64 ancestors".into());
    }
    Ok(ancestors)
}

fn roots(paths: &[PathBuf]) -> Result<BTreeMap<PathBuf, bool>, String> {
    paths
        .iter()
        .map(|path| {
            let root = std::fs::canonicalize(path)
                .map_err(|error| format!("Seatbelt grant is unavailable: {error}"))?;
            // Top-level aliases are OS-owned on macOS. Refuse mutable symlinks
            // below them instead of silently granting a different subtree.
            for ancestor in ancestors(path)? {
                let metadata = std::fs::symlink_metadata(ancestor)
                    .map_err(|error| format!("Seatbelt grant cannot be inspected: {error}"))?;
                if metadata.is_symlink() && ancestor.parent().and_then(Path::parent).is_some() {
                    return Err("Seatbelt writable grant contains a mutable symlink".into());
                }
            }
            let directory = std::fs::metadata(&root)
                .map_err(|error| format!("Seatbelt grant cannot be inspected: {error}"))?
                .is_dir();
            Ok((root, directory))
        })
        .collect()
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn prepare(policy: &Policy) -> Result<Profile, String> {
    let writable = roots(&policy.writable_roots)?;
    let carveouts = policy
        .protected_paths
        .iter()
        .chain(&policy.denied_paths)
        .cloned()
        .collect::<Vec<_>>();
    #[cfg(unix)]
    crate::aliases::check(&carveouts, &writable.keys().cloned().collect::<Vec<_>>())?;
    let mut protected = BTreeSet::new();
    for path in &carveouts {
        protected.insert(path.clone());
        protected.insert(resolved(path)?);
        // Missing config parents must remain absent, and existing mutable
        // symlinks must not be retargetable after their target was resolved.
        for ancestor in ancestors(path)?.into_iter().rev() {
            match std::fs::symlink_metadata(ancestor) {
                Ok(metadata) if metadata.is_symlink() => {
                    // Resolve the parent, not the link: `..` and immutable
                    // aliases can hide the writable entry's lexical prefix.
                    let parent = ancestor.parent().ok_or("Seatbelt link has no parent")?;
                    let entry = std::fs::canonicalize(parent)
                        .map_err(|error| format!("Seatbelt link parent is unavailable: {error}"))?
                        .join(ancestor.file_name().ok_or("Seatbelt link has no name")?);
                    if writable
                        .keys()
                        .chain(&policy.writable_roots)
                        .any(|root| ancestor.starts_with(root) || entry.starts_with(root))
                    {
                        return Err("Seatbelt protected path crosses a writable symlink".into());
                    }
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    protected.insert(ancestor.to_path_buf());
                    protected.insert(resolved(ancestor)?);
                    break;
                }
                Err(error) => {
                    return Err(format!("Seatbelt protected path is unavailable: {error}"))
                }
            }
        }
    }
    let mut result = Profile {
        text: BASE.into(),
        parameters: Vec::new(),
    };
    let mut read = String::new();
    for path in &policy.readable_roots {
        let parameter = result.parameter(resolved(path)?)?;
        read.push_str(&format!(" (subpath {parameter})"));
    }
    for path in &policy.writable_roots {
        let parameter = result.parameter(resolved(path)?)?;
        read.push_str(&format!(" (subpath {parameter})"));
    }
    if !read.is_empty() {
        result
            .text
            .push_str(&format!("\n(allow file-read*{read})\n"));
    }
    let mut grant = String::new();
    for (path, directory) in &writable {
        let parameter = result.parameter(path.clone())?;
        let filter = if *directory { "subpath" } else { "literal" };
        grant.push_str(&format!(" ({filter} {parameter})"));
    }
    if !grant.is_empty() {
        result.text.push_str(&format!(
            "(allow file-write* (require-all (require-any{grant})"
        ));
        for path in &protected {
            let parameter = result.parameter(path.clone())?;
            result.text.push_str(&format!(
                " (require-not (literal {parameter})) (require-not (subpath {parameter}))"
            ));
        }
        result.text.push_str("))\n");
    }
    for path in &policy.denied_paths {
        let parameter = result.parameter(resolved(path)?)?;
        result.text.push_str(&format!(
            "(deny file-read* (literal {parameter}) (subpath {parameter}))\n"
        ));
    }
    let mut anchors = writable.keys().cloned().collect::<BTreeSet<_>>();
    for path in &protected {
        for ancestor in ancestors(path)?.into_iter().skip(1) {
            if writable.keys().any(|root| ancestor.starts_with(root)) {
                anchors.insert(ancestor.to_path_buf());
            }
        }
    }
    for anchor in anchors {
        let parameter = result.parameter(anchor)?;
        result.text.push_str(&format!(
            "(deny file-write-unlink (require-all (literal {parameter}) (vnode-type DIRECTORY)))\n"
        ));
    }
    result
        .text
        .push_str("(deny system-fcntl (fcntl-command 80 110))\n");
    if policy.network == NetworkAccess::On {
        result.text.push_str("(allow network*)\n");
        result.text.push_str(NETWORK);
    } else {
        result.text.push_str("(deny network*)\n");
    }
    result.check_bound()?;
    Ok(result)
}
