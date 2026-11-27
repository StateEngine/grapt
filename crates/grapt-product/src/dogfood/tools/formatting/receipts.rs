//! Original-to-final display receipts, independent of durable checkpoint ownership.
use super::*;
use std::io::Read;
pub(super) type Originals = BTreeMap<PathBuf, Result<Vec<u8>, String>>;

impl Context<'_> {
    pub(super) fn preimages(&self, planned: &PlannedSteps) -> Originals {
        let mut originals = BTreeMap::new();
        let Ok(settings) = &self.settings else {
            return originals;
        };
        let mut remaining = 4 * 1024 * 1024;
        for (path, content) in planned.postimages(usize::MAX, &[]) {
            if content.is_none()
                || !settings
                    .0
                    .values()
                    .any(|entry| !entry.disabled && matches_extension(path, entry))
            {
                continue;
            }
            let original = (|| {
                let target = self.root.join(path);
                match std::fs::symlink_metadata(&target) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Ok(Vec::new())
                    }
                    Err(_) => return Err("formatter diff preimage metadata unavailable".into()),
                    Ok(_) => {}
                }
                let requested = path.to_str().ok_or("formatter diff path is not UTF-8")?;
                let target = resolve_existing(self.root, requested, true)?;
                let mut bytes = Vec::new();
                coding::patch::open_stage(&target)
                    .map_err(|_| "formatter diff preimage open failed")?
                    .take((remaining as u64).min(FILE_EFFECT_MAX_BYTES) + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_| "formatter diff preimage read failed")?;
                if bytes.len() > remaining || bytes.len() as u64 > FILE_EFFECT_MAX_BYTES {
                    return Err("formatter diff preimages exceed byte bound".into());
                }
                remaining -= bytes.len();
                Ok(bytes)
            })();
            originals.insert(path.to_path_buf(), original);
        }
        originals
    }
}

pub(super) fn metadata(
    path: &Path,
    original: Option<&Result<Vec<u8>, String>>,
    after: &[u8],
) -> Value {
    let before = match original {
        Some(Ok(before)) => before,
        _ => {
            return json!({"relativePath":path,"patchOmitted":true,"countsAvailable":false,"diff":null,
            "omittedReason":original.and_then(|result| result.as_ref().err()).map(String::as_str).unwrap_or("formatter diff preimage absent")})
        }
    };
    let path = path.to_string_lossy();
    coding::patch::diff_metadata(&path, Some(before), Some(after))
}

pub(super) fn annotate(
    output: &[u8],
    feedback: Vec<Value>,
    images: &BTreeMap<PathBuf, Vec<u8>>,
) -> Result<Vec<u8>, String> {
    let mut value: Value = serde_json::from_slice(output).map_err(|e| e.to_string())?;
    if value.get("files").is_none() {
        value["files"] = json!([]);
    }
    let files = value["files"]
        .as_array_mut()
        .ok_or("file result files is not an array")?;
    for file in feedback.iter().filter_map(|item| item.get("file")) {
        if let Some(existing) = files
            .iter_mut()
            .find(|existing| existing["relativePath"] == file["relativePath"])
        {
            *existing = file.clone();
        } else {
            files.push(file.clone());
        }
    }
    if let [(_, bytes)] = images.iter().collect::<Vec<_>>().as_slice() {
        for field in ["bytes", "afterBytes"] {
            if value.get(field).is_some() {
                value[field] = bytes.len().into();
            }
        }
    }
    value["formatting"] = feedback.into();
    serde_json::to_vec(&value).map_err(|e| e.to_string())
}
