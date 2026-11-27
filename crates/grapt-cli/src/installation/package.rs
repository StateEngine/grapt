//! Verification of the existing release package, before installation effects.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    inventory::{self, Inventory},
    io, progress,
    snapshot::{self, Stamp},
};

#[path = "stateengine_package.rs"]
mod stateengine;

pub(super) const MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
const ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;
const ASSET_BYTES: u64 = 160 * 1024 * 1024;
const GUIDE_BYTES: u64 = 64 * 1024;
const PACKAGE_BYTES: u64 = 1024 * 1024 * 1024;
const ASSETS: usize = 64;
#[cfg(target_os = "linux")]
const TARGET: &str = "x86_64-unknown-linux-gnu";
#[cfg(windows)]
const TARGET: &str = "x86_64-pc-windows-msvc";
#[cfg(target_os = "linux")]
const NODE: &str = "plugin-runtime/node";
#[cfg(windows)]
const NODE: &str = "plugin-runtime/node.exe";

/// The single known inventory this manifest declares in `binaries`,
/// `entry_points` and `signing` alike. Retained packages of either spelling
/// remain verifiable for upgrade, revert and prune; a mix is refused.
fn binary_inventory(manifest: &Manifest) -> Result<Inventory, String> {
    inventory::complete(
        manifest.binaries.keys(),
        &manifest.entry_points,
        manifest.signing.keys(),
    )
    .ok_or_else(|| "package does not declare the complete shipped binary set".into())
}

#[derive(Debug, Deserialize)]
struct Attested {
    target: String,
    profile: String,
    sha256: String,
    bytes: u64,
    source_commit: String,
    build_receipt: String,
    build_receipt_sha256: String,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    schema: String,
    profile: String,
    target: String,
    engine_version: String,
    binaries: BTreeMap<String, Attested>,
    entry_points: Vec<String>,
    signing: BTreeMap<String, Value>,
    publisher: Option<String>,
    plugin_runtime: Value,
    #[serde(default)]
    stateengine: Value,
    #[serde(default)]
    compat_aliases: Value,
    #[serde(default)]
    compat_aliases_removal: Value,
}

#[derive(Debug)]
enum InputIdentity {
    Retained(Stamp),
    Content,
}

#[derive(Debug)]
pub(super) struct Input {
    pub(super) relative: PathBuf,
    pub(super) bytes: u64,
    pub(super) sha256: String,
    #[cfg(unix)]
    pub(super) executable: bool,
    identity: InputIdentity,
}

#[derive(Debug)]
pub(super) struct Package {
    pub(super) root: PathBuf,
    pub(super) commit: String,
    pub(super) inputs: Vec<Input>,
    pub(super) manifest: Vec<u8>,
    pub(super) stateengine_entry: Option<PathBuf>,
    /// The inventory this version declares; callers take names from it.
    pub(super) inventory: Inventory,
    aliases: &'static [(&'static str, &'static str)],
    root_stamp: Stamp,
    manifest_stamp: Stamp,
}

pub(super) fn commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn relative(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if value.is_empty()
        || value.len() > 1024
        || value.chars().any(char::is_control)
        || value.contains('\\')
        || value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || !path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err("package input has an unsafe relative path".into());
    }
    let depth = if path.starts_with("stateengine") {
        18
    } else {
        8
    };
    if path.components().count() > depth {
        return Err("package input path exceeds its nesting bound".into());
    }
    Ok(path.to_path_buf())
}

fn hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(super) fn fingerprint(path: &Path, cap: u64) -> Result<(String, u64, bool, Stamp), String> {
    fingerprint_reporting(path, cap, None)
}

fn fingerprint_reporting(
    path: &Path,
    cap: u64,
    reporter: Option<&progress::Reporter>,
) -> Result<(String, u64, bool, Stamp), String> {
    #[cfg(target_os = "linux")]
    let stamp = Stamp::capture(io::regular(path)?)?;
    #[cfg(windows)]
    let file = io::snapshot_regular(path)?;
    #[cfg(windows)]
    let stamp = Stamp::capture(path, &file)?;
    #[cfg(target_os = "linux")]
    let executable = stamp
        .file
        .metadata()
        .map_err(|error| format!("package mode cannot be read: {:?}", error.kind()))?
        .permissions()
        .mode()
        & 0o100
        != 0;
    #[cfg(windows)]
    let executable = path.extension().is_some_and(|extension| extension == "exe");
    #[cfg(target_os = "linux")]
    let mut reader = (&stamp.file).take(cap + 1);
    #[cfg(windows)]
    let mut reader = (&file).take(cap + 1);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut counted = 0u64;
    loop {
        let bytes = reader
            .read(&mut buffer)
            .map_err(|error| format!("package input cannot be read: {:?}", error.kind()))?;
        if bytes == 0 {
            break;
        }
        counted += bytes as u64;
        if counted > cap {
            return Err("package input exceeds its byte bound".into());
        }
        hasher.update(&buffer[..bytes]);
        if let Some(reporter) = reporter {
            reporter.bytes(counted);
        }
    }
    stamp.recheck(path)?;
    Ok((
        format!("{:x}", hasher.finalize()),
        counted,
        executable,
        stamp,
    ))
}

impl Package {
    pub(super) fn verify(root: &Path) -> Result<Self, String> {
        Self::verify_with(root, None)
    }

    pub(super) fn verify_reporting(
        root: &Path,
        enabled: bool,
        phase: &str,
    ) -> Result<Self, String> {
        progress::during(enabled, phase, |reporter| {
            Self::verify_with(root, Some(reporter))
        })
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(super) fn verify_with(
        root: &Path,
        reporter: Option<&progress::Reporter>,
    ) -> Result<Self, String> {
        let root_stamp = Stamp::directory(root)?;
        if let Some(reporter) = reporter {
            reporter.file(1, 0, Path::new("release-manifest.json"), 0);
        }
        let (bytes, manifest_stamp) =
            snapshot::read(&root.join("release-manifest.json"), MANIFEST_BYTES)?;
        if let Some(reporter) = reporter {
            reporter.bytes(bytes.len() as u64);
        }
        let manifest: Manifest =
            serde_json::from_slice(&bytes).map_err(|_| "release-manifest.json is malformed")?;
        if !matches!(manifest.profile.as_str(), "debug" | "release")
            || manifest.schema != "secode.release.package.v1"
            || manifest.target != TARGET
            || !commit(&manifest.engine_version)
        {
            return Err("package schema, platform or build identity is unsupported".into());
        }
        let inventory = binary_inventory(&manifest)?;
        let aliases = inventory::compat_aliases(
            inventory,
            &manifest.compat_aliases,
            &manifest.compat_aliases_removal,
        )?;

        // Reject excessive declared work before opening or hashing artifacts.
        let mut declared = MANIFEST_BYTES + GUIDE_BYTES + bytes.len() as u64;
        declared = declared
            .checked_add(stateengine::declared(&manifest.stateengine)?)
            .ok_or("package byte count overflow")?;
        for entry in manifest.binaries.values() {
            if entry.bytes == 0 || entry.bytes > ARTIFACT_BYTES {
                return Err("package binary size is outside its bound".into());
            }
            declared = declared
                .checked_add(entry.bytes)
                .ok_or("package byte count overflow")?;
        }
        if let Some(assets) = manifest
            .plugin_runtime
            .get("assets")
            .and_then(Value::as_object)
        {
            if assets.len() > ASSETS {
                return Err("plugin asset count exceeds its bound".into());
            }
            for asset in assets.values() {
                let length = asset
                    .get("bytes")
                    .and_then(Value::as_u64)
                    .filter(|length| *length <= ASSET_BYTES)
                    .ok_or("plugin asset size exceeds its bound")?;
                declared = declared
                    .checked_add(length)
                    .ok_or("package byte count overflow")?;
            }
        }
        if declared > PACKAGE_BYTES {
            return Err("package exceeds its total byte bound".into());
        }
        let mut inputs = Vec::new();
        let mut receipts = BTreeMap::new();
        for (name, entry) in &manifest.binaries {
            if entry.target != manifest.target
                || entry.profile != manifest.profile
                || entry.source_commit != manifest.engine_version
                || !hex(&entry.sha256)
                || !hex(&entry.build_receipt_sha256)
            {
                return Err("package binaries disagree on build identity or digests".into());
            }
            let signing = &manifest.signing[name];
            if let Some(publisher) = manifest.publisher.as_deref() {
                if publisher.trim().is_empty()
                    || publisher.len() > 2048
                    || signing.get("signed").and_then(Value::as_bool) != Some(true)
                {
                    return Err("package publisher or signing claim is malformed".into());
                }
                #[cfg(windows)]
                super::windows_signing::verify(
                    &root.join(name),
                    publisher,
                    &entry.sha256,
                    entry.bytes,
                )?;
                #[cfg(not(windows))]
                return Err("package publisher verification requires Windows".into());
            } else if signing.get("signed").and_then(Value::as_bool) != Some(false)
                || signing
                    .get("reason")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
            {
                return Err("package signing claim cannot be verified".into());
            }
            if let Some(reporter) = reporter {
                reporter.file(inputs.len() + 2, 0, Path::new(name), entry.bytes);
            }
            let (sha, length, executable, stamp) =
                fingerprint_reporting(&root.join(name), ARTIFACT_BYTES, reporter)?;
            if !sha.eq_ignore_ascii_case(&entry.sha256) || length != entry.bytes || !executable {
                return Err(
                    "package binary differs from its digest, size or executable mode".into(),
                );
            }
            inputs.push(Input {
                relative: relative(name)?,
                bytes: length,
                sha256: sha,
                #[cfg(unix)]
                executable,
                identity: InputIdentity::Retained(stamp),
            });
            let receipt = relative(&entry.build_receipt)?;
            if receipt.components().count() != 1
                || inventory::reserved(&entry.build_receipt)
                || ["release-manifest.json", "QUICKSTART.md"]
                    .contains(&entry.build_receipt.as_str())
            {
                return Err("package build receipt has an unsafe location".into());
            }
            if let Some(previous) = receipts.insert(receipt, entry.build_receipt_sha256.clone()) {
                if previous != entry.build_receipt_sha256 {
                    return Err("package binaries disagree on their build receipt digest".into());
                }
            }
        }
        if receipts.len() != 1 {
            return Err("package must carry one common build receipt".into());
        }
        for (path, attested) in receipts {
            if let Some(reporter) = reporter {
                reporter.file(inputs.len() + 2, 0, &path, 0);
            }
            let (receipt_bytes, stamp) = snapshot::read(&root.join(&path), MANIFEST_BYTES)?;
            if let Some(reporter) = reporter {
                reporter.bytes(receipt_bytes.len() as u64);
            }
            let sha = format!("{:x}", Sha256::digest(&receipt_bytes));
            let receipt: Value = serde_json::from_slice(&receipt_bytes)
                .map_err(|_| "package build receipt is malformed")?;
            let supported_schema = matches!(
                receipt.get("schema").and_then(Value::as_str),
                Some(
                    "secode.release.build.v1"
                        | "secode.release.linux-build.v1"
                        | "secode.daily-use.candidate-build.v1"
                )
            );
            if !supported_schema
                || receipt.get("profile").and_then(Value::as_str) != Some(manifest.profile.as_str())
                || !sha.eq_ignore_ascii_case(&attested)
                || receipt.get("source_commit").and_then(Value::as_str)
                    != Some(manifest.engine_version.as_str())
                || receipt.get("target").and_then(Value::as_str) != Some(manifest.target.as_str())
                || receipt.get("tree_dirty").and_then(Value::as_bool) != Some(false)
            {
                return Err("package build receipt does not attest these inputs".into());
            }
            for (name, binary) in &manifest.binaries {
                let accounted = receipt
                    .get("commands")
                    .and_then(Value::as_array)
                    .is_some_and(|commands| {
                        commands.iter().any(|step| {
                            step.get("produces").is_some_and(|produces| {
                                produces.as_str() == Some(name.as_str())
                                    || produces.as_array().is_some_and(|names| {
                                        names.iter().any(|produced| {
                                            produced.as_str() == Some(name.as_str())
                                        })
                                    })
                            })
                        })
                    });
                if !accounted {
                    return Err("package receipt has no build command for a binary".into());
                }
                let output = receipt.get("outputs").and_then(|outputs| outputs.get(name));
                if output
                    .and_then(|out| out.get("sha256"))
                    .and_then(Value::as_str)
                    != Some(binary.sha256.as_str())
                    || output
                        .and_then(|out| out.get("bytes"))
                        .and_then(Value::as_u64)
                        != Some(binary.bytes)
                {
                    return Err("package build receipt does not attest each binary".into());
                }
            }
            inputs.push(Input {
                relative: path,
                bytes: receipt_bytes.len() as u64,
                sha256: sha,
                #[cfg(unix)]
                executable: false,
                identity: InputIdentity::Retained(stamp),
            });
        }

        match manifest
            .plugin_runtime
            .get("status")
            .and_then(Value::as_str)
        {
            Some("included") => {
                let assets = manifest
                    .plugin_runtime
                    .get("assets")
                    .and_then(Value::as_object)
                    .ok_or("included plugin runtime has no asset inventory")?;
                if assets.len() > ASSETS {
                    return Err("plugin runtime asset count is outside its bound".into());
                }
                for required in [
                    NODE,
                    "plugin-runtime/LICENSE",
                    "plugin-runtime/runtime.json",
                    "plugin-bridge/index.mjs",
                ] {
                    if !assets.contains_key(required) {
                        return Err(
                            "included plugin runtime is missing a required package asset".into(),
                        );
                    }
                }
                for (name, attested) in assets {
                    let path = relative(name)?;
                    if !name.starts_with("plugin-runtime/") && !name.starts_with("plugin-bridge/") {
                        return Err("plugin runtime asset is outside its package directory".into());
                    }
                    let sha = attested
                        .get("sha256")
                        .and_then(Value::as_str)
                        .filter(|sha| hex(sha))
                        .ok_or("plugin asset digest is malformed")?;
                    let length = attested
                        .get("bytes")
                        .and_then(Value::as_u64)
                        .ok_or("plugin asset size is malformed")?;
                    let mut parent = root.to_path_buf();
                    if let Some(parents) = path.parent() {
                        for component in parents.components() {
                            parent.push(component);
                            io::directory(&parent)?;
                        }
                    }
                    if let Some(reporter) = reporter {
                        reporter.file(inputs.len() + 2, 0, &path, length);
                    }
                    let (actual, counted, executable, stamp) =
                        fingerprint_reporting(&root.join(&path), ASSET_BYTES, reporter)?;
                    if !actual.eq_ignore_ascii_case(sha)
                        || counted != length
                        || (name == NODE && !executable)
                    {
                        return Err("plugin runtime asset differs from its attestation".into());
                    }
                    inputs.push(Input {
                        relative: path,
                        bytes: counted,
                        sha256: actual,
                        #[cfg(unix)]
                        executable,
                        identity: InputIdentity::Retained(stamp),
                    });
                }
            }
            Some("not-included") => {}
            _ => return Err("package plugin-runtime status is unsupported".into()),
        }
        let stateengine_entry =
            stateengine::verify(root, &manifest.stateengine, &mut inputs, reporter)?;
        match std::fs::symlink_metadata(root.join("QUICKSTART.md")) {
            Ok(_) => {
                if let Some(reporter) = reporter {
                    reporter.file(inputs.len() + 2, 0, Path::new("QUICKSTART.md"), 0);
                }
                let (sha256, length, _executable, stamp) =
                    fingerprint_reporting(&root.join("QUICKSTART.md"), GUIDE_BYTES, reporter)?;
                inputs.push(Input {
                    relative: "QUICKSTART.md".into(),
                    bytes: length,
                    sha256,
                    #[cfg(unix)]
                    executable: _executable,
                    identity: InputIdentity::Retained(stamp),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("package guide is unreadable: {:?}", error.kind())),
        }
        let total = inputs
            .iter()
            .try_fold(bytes.len() as u64, |sum, input| {
                sum.checked_add(input.bytes)
            })
            .ok_or("package byte count overflow")?;
        if total > PACKAGE_BYTES {
            return Err("package exceeds its total byte bound".into());
        }
        Ok(Self {
            root: root.to_path_buf(),
            commit: manifest.engine_version,
            inputs,
            manifest: bytes,
            stateengine_entry,
            inventory,
            aliases,
            root_stamp,
            manifest_stamp,
        })
    }

    /// The compatibility aliases (alias, target) this package declares.
    pub(super) fn compat_aliases(&self) -> &'static [(&'static str, &'static str)] {
        self.aliases
    }
    pub(super) fn recheck(&self, root: &Path) -> Result<(), String> {
        self.root_stamp.recheck(root)?;
        self.manifest_stamp
            .recheck(&root.join("release-manifest.json"))?;
        if self.stateengine_entry.is_some() {
            let expected = self
                .inputs
                .iter()
                .filter(|input| matches!(&input.identity, InputIdentity::Content))
                .map(|input| input.relative.clone())
                .collect();
            if stateengine::inventory(root)? != expected {
                return Err("verified bundled resource inventory changed before activation".into());
            }
        }
        for input in &self.inputs {
            match &input.identity {
                InputIdentity::Retained(stamp) => stamp.recheck(&root.join(&input.relative))?,
                InputIdentity::Content => {
                    // Large resource trees retain content proofs, rather than
                    // one descriptor per file. Copying checks the same digest.
                    let (sha, bytes, _executable, _) =
                        fingerprint(&root.join(&input.relative), ASSET_BYTES)?;
                    #[cfg(unix)]
                    if _executable != input.executable {
                        return Err(
                            "verified bundled resource mode changed before activation".into()
                        );
                    }
                    if sha != input.sha256 || bytes != input.bytes {
                        return Err("verified bundled resource changed before activation".into());
                    }
                }
            }
        }
        Ok(())
    }
}

fn remaining_input(
    root: &Path,
    path: &Path,
    cap: u64,
    expected_sha: Option<&str>,
    expected_bytes: Option<u64>,
) -> Result<(), String> {
    let input = root.join(path);
    match std::fs::symlink_metadata(&input) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "prune remaining input is unreadable: {:?}",
                error.kind()
            ))
        }
    }
    let (sha, bytes, _executable, stamp) = fingerprint(&input, cap)?;
    if expected_sha.is_some_and(|expected| !sha.eq_ignore_ascii_case(expected))
        || expected_bytes.is_some_and(|expected| bytes != expected)
    {
        return Err(format!(
            "prune remaining input differs from its manifest at {}",
            super::display_path(&input)
        ));
    }
    drop(stamp);
    Ok(())
}

// A quarantine may have missing payloads after an interrupted removal. Keep its
// manifest until the end, and admit only its bounded declared paths. This is
// deletion recovery, never a package that may be activated.
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn prune_paths(
    root: &Path,
    expected: &str,
) -> Result<Option<BTreeSet<PathBuf>>, String> {
    let path = root.join("release-manifest.json");
    let bytes = match std::fs::symlink_metadata(&path) {
        Ok(_) => io::read(&path, MANIFEST_BYTES)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("prune manifest is unreadable: {:?}", error.kind())),
    };
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|_| "prune manifest is malformed")?;
    if manifest.schema != "secode.release.package.v1"
        || manifest.target != TARGET
        || manifest.engine_version != expected
        || !commit(expected)
        || !matches!(manifest.profile.as_str(), "debug" | "release")
    {
        return Err("prune manifest does not identify this package".into());
    }
    let declared = inventory::declared(&[manifest
        .binaries
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()])
    .ok_or("prune manifest does not identify this package")?;
    let aliases = inventory::compat_aliases(
        declared,
        &manifest.compat_aliases,
        &manifest.compat_aliases_removal,
    )
    .map_err(|_| "prune manifest declares unknown compatibility aliases")?;
    let mut paths = BTreeSet::from([
        PathBuf::from("release-manifest.json"),
        PathBuf::from("QUICKSTART.md"),
    ]);
    let mut receipts = BTreeSet::new();
    for (name, entry) in &manifest.binaries {
        if entry.target != TARGET
            || entry.source_commit != expected
            || entry.profile != manifest.profile
            || !hex(&entry.sha256)
            || !hex(&entry.build_receipt_sha256)
            || entry.bytes == 0
            || entry.bytes > ARTIFACT_BYTES
        {
            return Err("prune manifest has an invalid binary declaration".into());
        }
        let receipt = relative(&entry.build_receipt)?;
        if receipt.components().count() != 1
            || inventory::reserved(&entry.build_receipt)
            || ["release-manifest.json", "QUICKSTART.md"].contains(&entry.build_receipt.as_str())
        {
            return Err("prune manifest has an invalid receipt path".into());
        }
        let binary = relative(name)?;
        remaining_input(
            root,
            &binary,
            ARTIFACT_BYTES,
            Some(&entry.sha256),
            Some(entry.bytes),
        )?;
        remaining_input(
            root,
            &receipt,
            MANIFEST_BYTES,
            Some(&entry.build_receipt_sha256),
            None,
        )?;
        paths.insert(binary);
        paths.insert(receipt.clone());
        receipts.insert(receipt);
    }
    if receipts.len() != 1 {
        return Err("prune manifest must identify one build receipt".into());
    }
    // Materialized aliases go with their version; each may only hold its
    // target's attested bytes.
    for (alias, target) in aliases {
        let entry = manifest
            .binaries
            .get(*target)
            .ok_or("prune manifest has no compatibility alias target")?;
        let alias = relative(alias)?;
        remaining_input(
            root,
            &alias,
            ARTIFACT_BYTES,
            Some(&entry.sha256),
            Some(entry.bytes),
        )?;
        paths.insert(alias);
    }
    match manifest
        .plugin_runtime
        .get("status")
        .and_then(Value::as_str)
    {
        Some("included") => {
            let assets = manifest
                .plugin_runtime
                .get("assets")
                .and_then(Value::as_object)
                .ok_or("prune manifest has no plugin inventory")?;
            if assets.len() > ASSETS {
                return Err("prune manifest asset count exceeds its bound".into());
            }
            for (name, asset) in assets {
                let path = relative(name)?;
                if (!name.starts_with("plugin-runtime/") && !name.starts_with("plugin-bridge/"))
                    || asset
                        .get("bytes")
                        .and_then(Value::as_u64)
                        .is_none_or(|bytes| bytes > ASSET_BYTES)
                    || asset
                        .get("sha256")
                        .and_then(Value::as_str)
                        .is_none_or(|value| !hex(value))
                {
                    return Err("prune manifest has an invalid asset declaration".into());
                }
                remaining_input(
                    root,
                    &path,
                    ASSET_BYTES,
                    asset.get("sha256").and_then(Value::as_str),
                    asset.get("bytes").and_then(Value::as_u64),
                )?;
                paths.insert(path);
            }
        }
        Some("not-included") => {}
        _ => return Err("prune manifest has an unsupported plugin inventory".into()),
    }
    stateengine::prune(root, &manifest.stateengine, &mut paths)?;
    remaining_input(root, Path::new("QUICKSTART.md"), GUIDE_BYTES, None, None)?;
    Ok(Some(paths))
}

pub(super) fn bundled_entry(root: &Path) -> Result<PathBuf, String> {
    let raw = io::read(&root.join("release-manifest.json"), MANIFEST_BYTES)?;
    let manifest: Manifest =
        serde_json::from_slice(&raw).map_err(|_| "installed package manifest is malformed")?;
    stateengine::entry(root, &manifest.stateengine)
}
