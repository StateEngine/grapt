//! The separately licensed resource inventory joins the existing package copy.
use super::*;

const FILES: usize = 8193; // Includes the payload manifest itself.
const PAYLOAD_BYTES: u64 = 512 * 1024 * 1024;
const PAYLOAD_MANIFEST_BYTES: u64 = 2 * 1024 * 1024;

fn assets(value: &Value) -> Result<Option<&serde_json::Map<String, Value>>, String> {
    if value.is_null() || value.get("status").and_then(Value::as_str) == Some("not-included") {
        return Ok(None);
    }
    if value.get("status").and_then(Value::as_str) != Some("included") {
        return Err("package StateEngine status is unsupported".into());
    }
    let assets = value
        .get("assets")
        .and_then(Value::as_object)
        .filter(|assets| !assets.is_empty() && assets.len() <= FILES)
        .ok_or("StateEngine asset count is outside its bound")?;
    Ok(Some(assets))
}

pub(super) fn declared(value: &Value) -> Result<u64, String> {
    let Some(assets) = assets(value)? else {
        return Ok(0);
    };
    assets.values().try_fold(0_u64, |sum, asset| {
        let bytes = asset
            .get("bytes")
            .and_then(Value::as_u64)
            .filter(|bytes| *bytes <= ASSET_BYTES)
            .ok_or("StateEngine asset size exceeds its bound")?;
        sum.checked_add(bytes)
            .filter(|sum| *sum <= PAYLOAD_BYTES)
            .ok_or_else(|| "StateEngine payload exceeds its total byte bound".into())
    })
}

fn resource(value: &str) -> Result<PathBuf, String> {
    if value.contains(['\\', ':']) {
        return Err("StateEngine asset has an unsafe path".into());
    }
    let path = relative(value)?;
    if !path.starts_with("stateengine") || path.components().count() < 2 {
        return Err("StateEngine asset is outside its package directory".into());
    }
    Ok(path)
}

fn payload_resource(value: &Value) -> Result<PathBuf, String> {
    resource(&format!(
        "stateengine/{}",
        value.as_str().ok_or("StateEngine payload path is absent")?
    ))
}

pub(super) fn inventory(root: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let mut pending = vec![(root.join("stateengine"), 0)];
    let mut files = BTreeSet::new();
    let mut entries = 0;
    while let Some((directory, depth)) = pending.pop() {
        if depth > 17 {
            return Err("StateEngine resource tree exceeds its nesting bound".into());
        }
        io::directory(&directory)?;
        for entry in std::fs::read_dir(directory)
            .map_err(|error| format!("StateEngine directory cannot be read: {:?}", error.kind()))?
        {
            entries += 1;
            if entries > FILES * 2 {
                return Err("StateEngine resource tree exceeds its entry bound".into());
            }
            let entry = entry
                .map_err(|error| format!("StateEngine entry cannot be read: {:?}", error.kind()))?;
            let path = entry.path();
            let kind = entry
                .file_type()
                .map_err(|error| format!("StateEngine type cannot be read: {:?}", error.kind()))?;
            if kind.is_dir() {
                pending.push((path, depth + 1));
            } else if kind.is_file() {
                files.insert(
                    path.strip_prefix(root)
                        .map_err(|_| "StateEngine inventory escaped its package")?
                        .to_path_buf(),
                );
            } else {
                return Err("StateEngine resource tree contains a nonregular entry".into());
            }
        }
    }
    Ok(files)
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn verify(
    root: &Path,
    value: &Value,
    inputs: &mut Vec<Input>,
    reporter: Option<&progress::Reporter>,
) -> Result<Option<PathBuf>, String> {
    let Some(assets) = assets(value)? else {
        return Ok(None);
    };
    declared(value)?;
    let entry = resource(
        value
            .get("entry")
            .and_then(Value::as_str)
            .ok_or("StateEngine entry is absent")?,
    )?;
    let (manifest_name, schema, native) = match (
        assets.contains_key("stateengine/native-payload.json"),
        assets.contains_key("stateengine/stateengine-payload.json"),
    ) {
        (true, false) => (
            "stateengine/native-payload.json",
            "stateengine.native-payload/v1",
            true,
        ),
        (false, true) => (
            "stateengine/stateengine-payload.json",
            "stateengine.se-payload.v1",
            false,
        ),
        _ => return Err("StateEngine payload manifest is missing or ambiguous".into()),
    };
    let manifest_path = Path::new(manifest_name);
    let (raw, _stamp) = snapshot::read(&root.join(manifest_path), PAYLOAD_MANIFEST_BYTES)?;
    let digest = format!("{:x}", Sha256::digest(&raw));
    if value.get("manifest_sha256").and_then(Value::as_str) != Some(digest.as_str()) {
        return Err("StateEngine manifest differs from its attestation".into());
    }
    let payload: Value =
        serde_json::from_slice(&raw).map_err(|_| "StateEngine payload manifest is malformed")?;
    if payload.get("schema").and_then(Value::as_str) != Some(schema)
        || (native
            && (payload.get("runtime").and_then(Value::as_str) != Some("rust-embedded")
                || payload.get("node").is_some()))
        || payload.get("name").and_then(Value::as_str) != Some("stateengine")
        || payload.get("target").and_then(Value::as_str) != Some(TARGET)
        || value.get("target").and_then(Value::as_str) != Some(TARGET)
        || payload.get("version") != value.get("version")
        || payload.get("source_commit") != value.get("source_commit")
        || !value
            .get("source_commit")
            .and_then(Value::as_str)
            .is_some_and(commit)
        || payload_resource(payload.get("entry").ok_or("StateEngine entry is absent")?)? != entry
    {
        return Err("StateEngine payload and package identities disagree".into());
    }
    let files = payload
        .get("files")
        .and_then(Value::as_object)
        .ok_or("StateEngine payload has no file inventory")?;
    if files.len() + 1 != assets.len() || !assets.contains_key(&manifest_path.to_string_lossy()[..])
    {
        return Err("StateEngine payload and package inventories disagree".into());
    }
    let mut required = vec![entry.clone(), manifest_path.to_path_buf()];
    for field in ["license_path", "notices_path"] {
        required.push(payload_resource(
            payload
                .get(field)
                .ok_or("StateEngine legal notice is absent")?,
        )?);
    }
    let node = if cfg!(windows) {
        Path::new("stateengine/node.exe")
    } else {
        Path::new("stateengine/node")
    };
    if !native {
        required.push(payload_resource(
            payload
                .get("node")
                .and_then(|node| node.get("license_path"))
                .ok_or("StateEngine Node license is absent")?,
        )?);
        required.push(node.to_path_buf());
    }
    if required
        .iter()
        .any(|path| !assets.contains_key(&path.to_string_lossy()[..]))
    {
        return Err("StateEngine runtime or legal notice is missing from its inventory".into());
    }
    let expected = assets
        .keys()
        .map(|name| resource(name))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if inventory(root)? != expected {
        return Err("StateEngine resource tree contains unlisted or missing files".into());
    }
    for (name, asset) in assets {
        let path = resource(name)?;
        if path != manifest_path {
            let inner = name
                .strip_prefix("stateengine/")
                .ok_or("StateEngine resource prefix is malformed")?;
            if files.get(inner) != Some(asset) {
                return Err("StateEngine payload and package file attestations disagree".into());
            }
        }
        let sha = asset
            .get("sha256")
            .and_then(Value::as_str)
            .filter(|sha| hex(sha))
            .ok_or("StateEngine resource digest is malformed")?;
        let bytes = asset
            .get("bytes")
            .and_then(Value::as_u64)
            .ok_or("StateEngine resource size is malformed")?;
        if let Some(reporter) = reporter {
            reporter.file(inputs.len() + 2, 0, &path, bytes);
        }
        let (actual, counted, executable, _stamp) =
            fingerprint_reporting(&root.join(&path), ASSET_BYTES, reporter)?;
        if actual != sha
            || counted != bytes
            || (!cfg!(windows) && (path == entry || (!native && path == node)) && !executable)
        {
            return Err(
                "StateEngine resource differs from its digest, size or executable mode".into(),
            );
        }
        inputs.push(Input {
            relative: path,
            bytes,
            sha256: actual,
            #[cfg(unix)]
            executable,
            identity: InputIdentity::Content,
        });
    }
    Ok(Some(entry))
}

pub(super) fn prune(
    root: &Path,
    value: &Value,
    paths: &mut BTreeSet<PathBuf>,
) -> Result<(), String> {
    declared(value)?;
    let Some(assets) = assets(value)? else {
        return Ok(());
    };
    for (name, asset) in assets {
        let path = resource(name)?;
        let sha = asset
            .get("sha256")
            .and_then(Value::as_str)
            .filter(|sha| hex(sha))
            .ok_or("prune StateEngine digest is malformed")?;
        let bytes = asset
            .get("bytes")
            .and_then(Value::as_u64)
            .ok_or("prune StateEngine size is malformed")?;
        remaining_input(root, &path, ASSET_BYTES, Some(sha), Some(bytes))?;
        paths.insert(path);
    }
    Ok(())
}

pub(super) fn entry(root: &Path, value: &Value) -> Result<PathBuf, String> {
    let assets = assets(value)?.ok_or("this build does not include StateEngine")?;
    if value.get("target").and_then(Value::as_str) != Some(TARGET) {
        return Err("bundled StateEngine target is unsupported".into());
    }
    let path = resource(
        value
            .get("entry")
            .and_then(Value::as_str)
            .ok_or("bundled StateEngine entry is absent")?,
    )?;
    if !assets.contains_key(&path.to_string_lossy()[..]) {
        return Err("bundled StateEngine entry is not an attested resource".into());
    }
    #[cfg(windows)]
    if path
        .extension()
        .is_none_or(|extension| !extension.to_string_lossy().eq_ignore_ascii_case("exe"))
    {
        return Err("bundled StateEngine requires a native Windows entry".into());
    }
    Ok(root.join(path))
}
