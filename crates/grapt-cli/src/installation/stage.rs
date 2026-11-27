//! Bounded copy into a private partial and verification before activation.

use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

#[cfg(unix)]
use grapt_service::installation::PrefixError;
use grapt_service::installation::PrefixGuard;
use sha2::{Digest, Sha256};

use super::{
    io,
    package::{Input, Package},
    progress,
};

pub(super) fn directory(path: &Path) -> Result<(), String> {
    io::directory(path.parent().ok_or("staging directory has no parent")?)?;
    #[cfg(unix)]
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(not(unix))]
    let builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    builder.mode(0o700);
    match builder.create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => io::directory(path)?,
        Err(error) => {
            return Err(format!(
                "staging directory cannot be created: {:?}",
                error.kind()
            ))
        }
    }
    Ok(())
}

// Only this fixed, inactive partial may be removed. No symlink is followed;
// abnormal or excessive leftovers refuse instead of widening deletion scope.
fn remove_partial(path: &Path, depth: usize, remaining: &mut usize) -> Result<(), String> {
    if depth > super::bounds::TREE_DEPTH {
        return Err("installation partial exceeds its directory bound".into());
    }
    io::directory(path)?;
    for entry in std::fs::read_dir(path)
        .map_err(|error| format!("partial cannot be read: {:?}", error.kind()))?
    {
        let entry =
            entry.map_err(|error| format!("partial entry cannot be read: {:?}", error.kind()))?;
        *remaining = remaining
            .checked_sub(1)
            .ok_or("partial exceeds its entry bound")?;
        let kind = entry
            .file_type()
            .map_err(|error| format!("partial type cannot be read: {:?}", error.kind()))?;
        if kind.is_dir() {
            remove_partial(&entry.path(), depth + 1, remaining)?;
        } else if kind.is_file() || kind.is_symlink() {
            std::fs::remove_file(entry.path())
                .map_err(|error| format!("partial cannot be removed: {:?}", error.kind()))?;
        } else {
            return Err("installation partial contains a nonregular input".into());
        }
    }
    std::fs::remove_dir(path)
        .map_err(|error| format!("partial directory cannot be removed: {:?}", error.kind()))
}

pub(super) fn copy(source: &Path, destination: &Path, input: &Input) -> Result<(), String> {
    copy_reporting(source, destination, input, None)
}

fn copy_reporting(
    source: &Path,
    destination: &Path,
    input: &Input,
    reporter: Option<&progress::Reporter>,
) -> Result<(), String> {
    let mut source = io::regular(source)?.take(input.bytes + 1);
    io::directory(destination.parent().ok_or("staging file has no parent")?)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut destination = options
        .open(destination)
        .map_err(|error| format!("staging file cannot be created: {:?}", error.kind()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut length = 0;
    loop {
        let count = source
            .read(&mut buffer)
            .map_err(|error| format!("package copy failed: {:?}", error.kind()))?;
        if count == 0 {
            break;
        }
        length += count as u64;
        if length > input.bytes {
            return Err("package input grew during copying".into());
        }
        hasher.update(&buffer[..count]);
        destination
            .write_all(&buffer[..count])
            .map_err(|error| format!("staging write failed: {:?}", error.kind()))?;
        if let Some(reporter) = reporter {
            reporter.bytes(length);
        }
    }
    if length != input.bytes || format!("{:x}", hasher.finalize()) != input.sha256 {
        return Err("verified package input changed during copying".into());
    }
    #[cfg(unix)]
    destination
        .set_permissions(std::fs::Permissions::from_mode(if input.executable {
            0o700
        } else {
            0o600
        }))
        .map_err(|error| format!("staging mode cannot be set: {:?}", error.kind()))?;
    destination
        .sync_all()
        .map_err(|error| format!("staging file cannot be synced: {:?}", error.kind()))
}

// Mutation commands hold this before preparation and keep it through activation.
// Starts/products never wait on it; activation uses the separate prefix lock.
pub(super) fn writer(prefix: &Path) -> Result<PrefixGuard, String> {
    io::directory(prefix)?;
    #[cfg(unix)]
    if std::fs::symlink_metadata(prefix)
        .map_err(|error| format!("installation prefix is unreadable: {:?}", error.kind()))?
        .permissions()
        .mode()
        & 0o022
        != 0
    {
        return Err(PrefixError::Writable.to_string());
    }
    let staging = prefix.join(".staging");
    directory(&staging)?;
    PrefixGuard::exclusive(&staging).map_err(|error| error.to_string())
}

#[cfg(test)]
pub(super) fn install(prefix: &Path, package: &Package) -> Result<Package, String> {
    install_reporting(prefix, package, false)
}

pub(super) fn install_reporting(
    prefix: &Path,
    package: &Package,
    report: bool,
) -> Result<Package, String> {
    let versions = prefix.join("versions");
    directory(&versions)?;
    let destination = versions.join(&package.commit);
    match std::fs::symlink_metadata(&destination) {
        Ok(_) => {
            io::directory(&destination)?;
            let existing = Package::verify_reporting(
                &destination.join("bin"),
                report,
                "Verifying installed files",
            )?;
            if existing.manifest != package.manifest {
                return Err("installed identity has a different manifest".into());
            }
            return Ok(existing);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "installed version is unreadable: {:?}",
                error.kind()
            ))
        }
    }
    package.recheck(&package.root)?;
    let partial = versions.join(".partial");
    match std::fs::symlink_metadata(&partial) {
        Ok(_) => {
            let mut remaining = super::bounds::TREE_ENTRIES;
            remove_partial(&partial, 0, &mut remaining)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "installation partial is unreadable: {:?}",
                error.kind()
            ))
        }
    }
    directory(&partial)?;
    let bin = partial.join("bin");
    directory(&bin)?;
    progress::during(report, "Copying package files", |reporter| {
        for (index, input) in package.inputs.iter().enumerate() {
            reporter.file(
                index + 1,
                package.inputs.len() + 1,
                &input.relative,
                input.bytes,
            );
            if let Some(parent) = input.relative.parent() {
                let mut path = bin.clone();
                for component in parent.components() {
                    path.push(component);
                    directory(&path)?;
                }
            }
            copy_reporting(
                &package.root.join(&input.relative),
                &bin.join(&input.relative),
                input,
                Some(reporter),
            )?;
        }
        reporter.file(
            package.inputs.len() + 1,
            package.inputs.len() + 1,
            Path::new("release-manifest.json"),
            package.manifest.len() as u64,
        );
        io::write_new(&bin.join("release-manifest.json"), &package.manifest)?;
        reporter.bytes(package.manifest.len() as u64);
        Ok(())
    })?;
    #[cfg(target_os = "linux")]
    let mut installed = Package::verify_reporting(&bin, report, "Verifying installed files")?;
    // Sync every created directory before publishing its version name.
    progress::during(report, "Syncing installed files", |_| {
        let mut remaining = super::bounds::TREE_ENTRIES;
        sync_tree(&partial, 0, &mut remaining)
    })?;
    io::directory(&versions)?;
    io::directory(&partial)?;
    std::fs::rename(&partial, &destination)
        .map_err(|error| format!("verified version cannot be published: {:?}", error.kind()))?;
    io::sync_directory(&versions)?;
    // Windows refuses ancestor moves with any retained descendant handle,
    // including metadata-only handles. Keep the package inactive and verify
    // at its final name before activation ever acquires prefix EX.
    #[cfg(windows)]
    let mut installed = Package::verify_reporting(
        &destination.join("bin"),
        report,
        "Verifying installed files",
    )?;
    installed.root = destination.join("bin");
    installed.recheck(&installed.root)?;
    Ok(installed)
}

fn sync_tree(path: &Path, depth: usize, remaining: &mut usize) -> Result<(), String> {
    if depth > super::bounds::TREE_DEPTH {
        return Err("staging directory nesting exceeds its bound".into());
    }
    io::directory(path)?;
    for entry in std::fs::read_dir(path)
        .map_err(|error| format!("staging cannot be read: {:?}", error.kind()))?
    {
        let entry =
            entry.map_err(|error| format!("staging entry cannot be read: {:?}", error.kind()))?;
        *remaining = remaining
            .checked_sub(1)
            .ok_or("staging exceeds its entry bound")?;
        if entry
            .file_type()
            .map_err(|error| format!("staging type unreadable: {:?}", error.kind()))?
            .is_dir()
        {
            sync_tree(&entry.path(), depth + 1, remaining)?;
        }
    }
    io::sync_directory(path)
}
