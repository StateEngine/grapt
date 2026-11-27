//! Missing reads may suggest directory entries only through an authorized list.
use super::*;

fn parent(root: &Path, requested: &str) -> Result<Option<(String, PathBuf)>, String> {
    let relative = relative_path(root, requested)?;
    let requested = relative
        .to_str()
        .ok_or("read path is not UTF-8")?
        .replace('\\', "/");
    let mut current = root.to_path_buf();
    for (depth, part) in relative.components().enumerate() {
        if depth >= 32 {
            return Ok(None);
        }
        let Component::Normal(name) = part else {
            continue;
        };
        if name.to_str().is_some_and(discovery::excluded) {
            return Ok(None);
        }
        let next = current.join(name);
        match std::fs::symlink_metadata(&next) {
            Ok(metadata) if metadata.is_dir() && !is_link(&metadata) => current = next,
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Some((requested, current)));
            }
            Err(_) => return Ok(None),
        }
    }
    Ok(None)
}

pub(super) fn prepare(root: &Path, requested: &str) -> Result<Option<PreparedEffect>, String> {
    let Some((requested, parent)) = parent(root, requested)? else {
        return Ok(None);
    };
    let resource = path_text(&parent);
    Ok(Some(PreparedEffect {
        capability: filesystem_capability(EffectClass::FilesystemRead, &resource, false),
        target: EffectTarget::Coding(CodingTarget::MissingRead { requested, parent }),
        action: LIST_TOOL.to_owned(),
        resource,
    }))
}

pub(super) fn run(root: &Path, requested: &str, expected: &Path) -> Result<Vec<u8>, String> {
    let Some((_, parent)) = parent(root, requested)? else {
        return Err(format!(
            "{requested} changed after preparation; retry the read"
        ));
    };
    if parent != expected {
        return Err(format!(
            "{requested} parent changed after preparation; retry the read"
        ));
    }
    let relative = parent
        .strip_prefix(root)
        .map_err(|_| "read parent escaped workspace")?;
    let label = if relative.as_os_str().is_empty() {
        ".".to_owned()
    } else {
        path_text(relative)
    };
    let listing = discovery::list(root, &parent, 16, false).map_err(|error| {
        format!(
            "{requested} not found; nearest existing parent: {label}; entries unavailable: {error}"
        )
    })?;
    let mut listing: serde_json::Value =
        serde_json::from_slice(&listing).map_err(|error| error.to_string())?;
    loop {
        let text = serde_json::to_string(&listing).map_err(|error| error.to_string())?;
        if text.len() <= 8192 {
            return Err(format!(
                "{requested} not found; nearest existing parent: {label}; entries: {text}"
            ));
        }
        let entries = listing["entries"]
            .as_array_mut()
            .ok_or("parent listing has no entries")?;
        if entries.pop().is_none() {
            return Err(format!("{requested} not found; nearest existing parent: {label}; entries unavailable: diagnostic byte bound"));
        }
        listing["truncated"] = true.into();
        listing["complete"] = false.into();
    }
}
