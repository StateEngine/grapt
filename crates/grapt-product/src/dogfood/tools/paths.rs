use std::path::{Component, Path, PathBuf};

use grapt_types::{
    CapabilityId, EffectClass, EgressAuthority, FilesystemAuthority, PathIdentity,
    RealizedEnforcement, ResolvedCapability,
};

use super::path_text;
use crate::dogfood::legacy_names::{is_project_directory, PROJECT_DIRECTORIES};

pub(super) fn filesystem_capability(
    class: EffectClass,
    resource: &str,
    write: bool,
) -> CapabilityId {
    let path = PathIdentity::canonical(resource);
    let filesystem = if write {
        FilesystemAuthority::new(Vec::new(), vec![path], Vec::new())
    } else {
        FilesystemAuthority::new(vec![path], Vec::new(), Vec::new())
    };
    CapabilityId::derive(&ResolvedCapability::new(
        class,
        None,
        filesystem,
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    ))
}

/// Workspace entries an agent may not write. Both project directory names
/// are listed whichever one the repository uses.
const PROTECTED: [&str; 8] = [
    ".git",
    PROJECT_DIRECTORIES[0],
    PROJECT_DIRECTORIES[1],
    ".stateengine",
    ".codex",
    "AGENTS.md",
    "AGENTS.override.md",
    "CLAUDE.md",
];

pub(super) fn protected_paths(root: &Path) -> Vec<PathIdentity> {
    PROTECTED
        .into_iter()
        .map(|path| PathIdentity::canonical(path_text(&root.join(path))))
        .collect()
}

pub(super) fn profile_path(relative: &Path) -> bool {
    relative.components().any(|component| {
        matches!(component, Component::Normal(name) if name.to_str().is_some_and(is_project_directory))
    })
}

/// Instruction files are loaded from nested subdirectories as well as the
/// workspace root, so native write and patch tools must protect them at any
/// depth and across case-insensitive filesystems (APFS/NTFS).
pub(super) fn instruction_path(relative: &Path) -> bool {
    relative
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            ["AGENTS.md", "AGENTS.override.md"]
                .iter()
                .any(|protected| name.eq_ignore_ascii_case(protected))
        })
}

pub(super) fn resolve_existing(
    root: &Path,
    requested: &str,
    writable: bool,
) -> Result<PathBuf, String> {
    let requested = Path::new(requested);
    validate_spelling(requested)?;
    if requested
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("tool path escapes the project root".to_owned());
    }
    let target = std::fs::canonicalize(root.join(requested)).map_err(|error| {
        format!(
            "tool path '{}' could not be resolved: {error}",
            requested.display()
        )
    })?;
    let relative = crate::dogfood::authority_path::relative(root, &target, || {
        "tool path resolves outside the project root".into()
    })?;
    // A profile contains credentials as well as executable configuration.
    // Unlike instruction files, it is not a model-readable surface. Check
    // every component so nested profiles and canonical aliases agree.
    if profile_path(relative) {
        return Err("tool path names protected control metadata".to_owned());
    }
    if writable {
        let protected = relative.components().next().is_some_and(|component| {
            matches!(component, Component::Normal(name) if name.to_str().is_some_and(|name|
                PROTECTED.iter().any(|protected|
                    if cfg!(windows) { name.eq_ignore_ascii_case(protected) } else { name == *protected }
                )
            ))
        });
        if protected || instruction_path(relative) {
            return Err("tool path names protected control metadata".to_owned());
        }
    }
    Ok(root.join(relative))
}

/// Keep file effects out of alternate streams and ambiguous Win32 namespaces.
pub(super) fn validate_spelling(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    for component in path.components() {
        use std::path::Prefix;
        let invalid = match component {
            Component::Normal(name) => {
                let name = name.to_str().ok_or("tool path is not UTF-8")?;
                name.contains(':') || name.ends_with(['.', ' '])
            }
            Component::Prefix(prefix) => !matches!(
                prefix.kind(),
                Prefix::Disk(_)
                    | Prefix::VerbatimDisk(_)
                    | Prefix::UNC(_, _)
                    | Prefix::VerbatimUNC(_, _)
            ),
            _ => false,
        };
        if invalid {
            return Err("tool path contains an ambiguous Windows filename or namespace".into());
        }
    }
    #[cfg(not(windows))]
    let _ = path;
    Ok(())
}
