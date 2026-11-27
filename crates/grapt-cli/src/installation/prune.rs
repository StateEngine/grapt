//! Explicit retirement of bounded inactive packages under installer exclusion.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use grapt_service::installation::PrefixGuard;

use super::{
    io,
    package::{self, Package},
    prune_policy,
    prune_tree::{self, Tree},
    stage, transition,
};

const INVENTORY: usize = 64;
const PACKAGE_BYTES: u64 = 1024 * 1024 * 1024;

struct Version {
    commit: String,
    path: PathBuf,
    quarantine: bool,
    tree: Tree,
}

fn inventory(prefix: &Path) -> Result<Vec<Version>, String> {
    let root = prefix.join("versions");
    io::directory(&root).map_err(|reason| {
        format!(
            "prune cannot inspect {}: {reason}",
            super::display_path(&root)
        )
    })?;
    let children = std::fs::read_dir(&root)
        .map_err(|error| format!("prune version inventory is unreadable: {:?}", error.kind()))?;
    let mut versions = Vec::new();
    let mut total = 0;
    let mut identities = BTreeSet::new();
    for child in children {
        if versions.len() >= INVENTORY {
            return Err("prune version inventory exceeds 64 entries".into());
        }
        let child = child
            .map_err(|error| format!("prune version entry is unreadable: {:?}", error.kind()))?;
        let name = child
            .file_name()
            .into_string()
            .map_err(|_| "prune version name is not UTF-8")?;
        let (commit, quarantine) = name
            .strip_prefix(".prune-")
            .map_or((name.as_str(), false), |commit| (commit, true));
        if !package::commit(commit) || !identities.insert(commit.to_owned()) {
            return Err(format!(
                "prune refuses unknown or colliding version entry {}",
                super::display_path(&child.path())
            ));
        }
        let path = child.path();
        io::directory(&path).map_err(|reason| {
            format!(
                "prune cannot inspect {}: {reason}",
                super::display_path(&path)
            )
        })?;
        let tree = prune_tree::inspect(&path, &mut total).map_err(|reason| {
            format!(
                "prune cannot inspect {}: {reason}",
                super::display_path(&path)
            )
        })?;
        if tree.bytes()? > PACKAGE_BYTES {
            return Err("prune tree exceeds the package byte bound".into());
        }
        versions.push(Version {
            commit: commit.to_owned(),
            path,
            quarantine,
            tree,
        });
    }
    versions.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(versions)
}

fn recheck_inventory(prefix: &Path, versions: &[Version]) -> Result<(), String> {
    let expected: BTreeSet<PathBuf> = versions
        .iter()
        .map(|version| version.path.clone())
        .collect();
    let root = prefix.join("versions");
    io::directory(&root)?;
    let mut actual = BTreeSet::new();
    for entry in std::fs::read_dir(&root)
        .map_err(|error| format!("prune inventory recheck failed: {:?}", error.kind()))?
    {
        if actual.len() >= INVENTORY {
            return Err("prune inventory grew beyond its bound".into());
        }
        let entry =
            entry.map_err(|error| format!("prune inventory recheck failed: {:?}", error.kind()))?;
        actual.insert(entry.path());
    }
    if actual != expected {
        return Err("prune inventory changed during preparation".into());
    }
    Ok(())
}

fn layout(version: &Version, inputs: &BTreeSet<PathBuf>) -> Result<(), String> {
    let bin = version.path.join("bin");
    let files: BTreeSet<PathBuf> = inputs.iter().map(|input| bin.join(input)).collect();
    let mut directories = BTreeSet::from([version.path.clone(), bin.clone()]);
    for file in &files {
        let mut parent = file.parent();
        while let Some(path) = parent {
            if path == version.path {
                break;
            }
            if !path.starts_with(&bin) {
                return Err("prune declared path escapes its package".into());
            }
            directories.insert(path.to_path_buf());
            parent = path.parent();
        }
    }
    if version.tree.files.iter().any(|path| !files.contains(path))
        || version
            .tree
            .directories
            .iter()
            .any(|path| !directories.contains(path))
    {
        return Err(format!(
            "prune refuses an undeclared package entry in {}",
            super::display_path(&version.path)
        ));
    }
    Ok(())
}

fn prepare(versions: &[Version]) -> Result<(), String> {
    for version in versions {
        if version.quarantine {
            let paths = package::prune_paths(&version.path.join("bin"), &version.commit).map_err(
                |reason| {
                    format!(
                        "prune cannot verify {}: {reason}",
                        super::display_path(&version.path)
                    )
                },
            )?;
            match paths {
                Some(paths) => layout(version, &paths)?,
                None => {
                    if !version.tree.files.is_empty()
                        || version
                            .tree
                            .directories
                            .iter()
                            .any(|path| path != &version.path && path != &version.path.join("bin"))
                    {
                        return Err(format!(
                            "prune quarantine {} has payload without its manifest",
                            super::display_path(&version.path)
                        ));
                    }
                }
            }
        } else {
            let verified = Package::verify(&version.path.join("bin")).map_err(|reason| {
                format!(
                    "prune cannot verify {}: {reason}",
                    super::display_path(&version.path)
                )
            })?;
            if verified.commit != version.commit {
                return Err("prune version name differs from its verified build".into());
            }
            let mut paths: BTreeSet<PathBuf> = verified
                .inputs
                .iter()
                .map(|input| input.relative.clone())
                .collect();
            paths.insert("release-manifest.json".into());
            // Materialized compatibility aliases leave with their version.
            paths.extend(
                verified
                    .compat_aliases()
                    .iter()
                    .map(|(alias, _)| PathBuf::from(alias)),
            );
            layout(version, &paths)?;
            verified.recheck(&verified.root)?;
            // In particular, Windows cannot move/remove an ancestor while our
            // verified identity handles are retained. One package at a time.
            drop(verified);
        }
        version.tree.recheck()?;
    }
    Ok(())
}

fn protected(prefix: &Path, commit: &str) -> Result<(), String> {
    let state = transition::state(prefix)?.ok_or("prune installation has no current build")?;
    if state.current == commit || state.previous.as_deref() == Some(commit) {
        return Err(format!("prune refuses protected build {commit}"));
    }
    Ok(())
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn remove(prefix: &Path, version: &mut Version) -> Result<(), String> {
    protected(prefix, &version.commit)?;
    version.tree.recheck()?;
    if !version.quarantine {
        let quarantine = prefix
            .join("versions")
            .join(format!(".prune-{}", version.commit));
        match std::fs::symlink_metadata(&quarantine) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(format!(
                    "prune quarantine already exists at {}",
                    super::display_path(&quarantine)
                ))
            }
            Err(error) => {
                return Err(format!(
                    "prune quarantine cannot be inspected: {:?}",
                    error.kind()
                ))
            }
        }
        protected(prefix, &version.commit)?;
        std::fs::rename(&version.path, &quarantine)
            .map_err(|error| format!("prune cannot quarantine {}: {:?}; quit processes using that inactive build and retry", super::display_path(&version.path), error.kind()))?;
        version.tree.relocate(&version.path, &quarantine)?;
        version.path = quarantine;
        version.quarantine = true;
        io::sync_directory(&prefix.join("versions"))?;
    }
    let bin = version.path.join("bin");
    let manifest = bin.join("release-manifest.json");
    let mut removed = 0;
    let result = (|| {
        for path in version.tree.files.iter().filter(|path| *path != &manifest) {
            protected(prefix, &version.commit)?;
            version.tree.recheck_entry(path)?;
            std::fs::remove_file(path).map_err(|error| {
                format!(
                    "prune cannot remove {}: {:?}",
                    super::display_path(path),
                    error.kind()
                )
            })?;
            removed += 1;
        }
        // Empty payload directories go before the manifest. After the manifest
        // is removed, recovery accepts only the empty bin/root skeleton.
        for path in version
            .tree
            .directories
            .iter()
            .filter(|path| *path != &bin && *path != &version.path)
        {
            protected(prefix, &version.commit)?;
            version.tree.recheck_entry(path)?;
            io::sync_directory(path)?;
            std::fs::remove_dir(path).map_err(|error| {
                format!(
                    "prune cannot remove directory {}: {:?}",
                    super::display_path(path),
                    error.kind()
                )
            })?;
            io::sync_directory(path.parent().ok_or("prune directory has no parent")?)?;
        }
        if version.tree.files.contains(&manifest) {
            protected(prefix, &version.commit)?;
            version.tree.recheck_entry(&manifest)?;
            io::sync_directory(&bin)?;
            std::fs::remove_file(&manifest).map_err(|error| {
                format!(
                    "prune cannot remove manifest {}: {:?}",
                    super::display_path(&manifest),
                    error.kind()
                )
            })?;
            removed += 1;
            io::sync_directory(&bin)?;
        }
        for path in version
            .tree
            .directories
            .iter()
            .filter(|path| *path == &bin || *path == &version.path)
        {
            protected(prefix, &version.commit)?;
            version.tree.recheck_entry(path)?;
            io::sync_directory(path)?;
            std::fs::remove_dir(path).map_err(|error| {
                format!(
                    "prune cannot remove directory {}: {:?}",
                    super::display_path(path),
                    error.kind()
                )
            })?;
            io::sync_directory(path.parent().ok_or("prune directory has no parent")?)?;
        }
        Ok::<(), String>(())
    })();
    result.map_err(|reason| {
        format!(
            "{reason}; {removed} files removed from {}; rerun `grapt update --prune` to resume",
            super::display_path(&version.path)
        )
    })
}

pub(super) fn run(prefix: &Path) -> Result<String, String> {
    io::directory(prefix)?;
    let _writer = stage::writer(prefix)?;
    let verified_current = transition::verified_current(prefix)?;
    let mut versions = inventory(prefix)?;
    prepare(&versions)?; // Full content hashing never holds prefix EX.
    let _guard = PrefixGuard::exclusive(prefix).map_err(|error| error.to_string())?;
    recheck_inventory(prefix, &versions)?;
    for version in &versions {
        version.tree.recheck()?;
    }
    transition::recover_verified(prefix, verified_current.as_ref())?;
    // Drop the final active-package snapshot as well. Only the kernel guards
    // and non-handle metadata descriptions survive into deletion.
    drop(verified_current);
    let state = transition::state(prefix)?.ok_or("prune installation has no current build")?;
    let complete: Vec<String> = versions
        .iter()
        .filter(|version| !version.quarantine)
        .map(|version| version.commit.clone())
        .collect();
    let activations = transition::activations(prefix)?;
    let obsolete = prune_policy::obsolete(
        &state.current,
        state.previous.as_deref(),
        &complete,
        &activations,
    )?;
    for version in versions.iter().filter(|version| version.quarantine) {
        protected(prefix, &version.commit)?;
    }
    let fallback = complete
        .iter()
        .filter(|commit| !activations.contains(commit))
        .count();
    let mut removed = Vec::new();
    // Existing interrupted work comes first, without consuming retained slots.
    for quarantine in [true, false] {
        for version in versions
            .iter_mut()
            .filter(|version| version.quarantine == quarantine)
        {
            if quarantine || obsolete.contains(&version.commit) {
                remove(prefix, version)?;
                removed.push(version.commit.clone());
            }
        }
    }
    let kept: Vec<&str> = complete
        .iter()
        .filter(|commit| !obsolete.contains(commit))
        .map(String::as_str)
        .collect();
    let mut message = format!(
        "kept builds: {}\nremoved builds: {}",
        kept.join(", "),
        if removed.is_empty() {
            "none".into()
        } else {
            removed.join(", ")
        }
    );
    if fallback != 0 {
        message.push_str(&format!("\nActivation history is missing for {fallback} installed builds; unrecorded builds use commit identity as the fallback."));
    }
    Ok(message)
}

pub(super) fn hint_inventory(prefix: &Path) -> Result<Vec<(String, u64)>, String> {
    inventory(prefix)?
        .into_iter()
        .filter(|version| !version.quarantine)
        .map(|version| version.tree.bytes().map(|bytes| (version.commit, bytes)))
        .collect()
}

pub(super) fn hint(prefix: &Path, versions: &[(String, u64)]) -> Result<Option<String>, String> {
    if versions.len() <= 4 {
        return Ok(None);
    }
    let state = transition::state(prefix)?.ok_or("installation has no current build")?;
    let complete = versions
        .iter()
        .map(|(commit, _)| commit.clone())
        .collect::<Vec<_>>();
    let activations = transition::activations(prefix)?;
    let obsolete = prune_policy::obsolete(
        &state.current,
        state.previous.as_deref(),
        &complete,
        &activations,
    )?;
    let bytes = versions
        .iter()
        .filter(|(commit, _)| obsolete.contains(commit))
        .try_fold(0_u64, |total, (_, bytes)| {
            total
                .checked_add(*bytes)
                .ok_or("old-version byte count overflows")
        })?;
    let megabytes = bytes.div_ceil(1024 * 1024);
    // The active version's own CLI, which a revert may have made pre-rename.
    #[cfg(target_os = "linux")]
    let cli =
        super::inventory::installed(&prefix.join("versions").join(&state.current).join("bin"))?
            .cli();
    #[cfg(target_os = "linux")]
    let command = format!(
        "'{}' update --prune",
        prefix
            .join("current/bin")
            .join(cli)
            .display()
            .to_string()
            .replace('\'', "'\\''")
    );
    #[cfg(windows)]
    let command = format!(
        "& '{}' update --prune",
        super::windows_bootstrap::display_path(&prefix.join("bin/grapt.exe")).replace('\'', "''")
    );
    Ok(Some(format!(
        "{} old versions use {megabytes} MB; run `{command}`",
        obsolete.len()
    )))
}
