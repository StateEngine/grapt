//! Bounded user-configuration edits, serialized across service processes.
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::Edit;

use crate::dogfood::catalog_config::DOCUMENT_BYTES;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

fn io(error: std::io::Error) -> String {
    format!("MCP configuration I/O refused: {:?}", error.kind())
}

fn linked(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn regular(path: &Path) -> Result<Option<fs::Metadata>, String> {
    match fs::symlink_metadata(path) {
        Ok(value) if value.is_file() && !linked(&value) => Ok(Some(value)),
        Ok(_) => Err("MCP configuration requires regular, unlinked files".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io(error)),
    }
}

fn options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
            .mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000).share_mode(3);
    }
    options
}

fn opened(path: &Path, options: &OpenOptions) -> Result<File, String> {
    regular(path)?;
    let file = options.open(path).map_err(io)?;
    let metadata = file.metadata().map_err(io)?;
    let named = regular(path)?.ok_or("MCP configuration disappeared while opening")?;
    if !metadata.is_file() || linked(&metadata) {
        return Err("MCP configuration changed type while opening".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.dev() != named.dev() || metadata.ino() != named.ino() || metadata.nlink() != 1 {
            return Err("MCP configuration changed identity or has multiple links".into());
        }
    }
    #[cfg(windows)]
    {
        let _ = named;
        if same_file::Handle::from_file(file.try_clone().map_err(io)?).map_err(io)?
            != same_file::Handle::from_path(path).map_err(io)?
        {
            return Err("MCP configuration changed identity while opening".into());
        }
    }
    Ok(file)
}

fn read(path: &Path) -> Result<Option<Vec<u8>>, String> {
    if regular(path)?.is_none() {
        return Ok(None);
    }
    let file = opened(path, &options())?;
    let mut bytes = Vec::new();
    file.take(DOCUMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() > DOCUMENT_BYTES {
        return Err("MCP configuration exceeds its document bound".into());
    }
    Ok(Some(bytes))
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _removed = fs::remove_file(&self.0);
    }
}

pub(in crate::dogfood) fn update(
    environment: crate::dogfood::catalog_config::CatalogEnvironment,
    location: &str,
    name: &str,
    request: &Edit,
) -> Result<(), String> {
    publish(environment, move |bytes, path, environment| {
        let output = match request {
            Edit::Add(value) => super::edit::server(bytes, name, Some(value))?,
            Edit::Remove => super::edit::server(bytes, name, None)?,
            Edit::SetEnabled(enabled) => super::edit::enabled(bytes, name, *enabled)?,
            Edit::ProjectApproval {
                fingerprint,
                enabled,
            } => super::edit::approval(
                bytes,
                location,
                name,
                Some(&serde_json::json!({"fingerprint":fingerprint,"enabled":enabled})),
            )?,
            Edit::RevokeProjectApproval => super::edit::approval(
                bytes,
                location,
                name,
                Some(&serde_json::json!({"revoked":true})),
            )?,
        };
        super::validate(&output, path, environment, location)?;
        Ok((output, ()))
    })
}

/// The existing user-config lock, origin fence and atomic publisher also own
/// explicit hook imports. Domain validation happens before publication.
pub(in crate::dogfood) fn publish<T>(
    environment: crate::dogfood::catalog_config::CatalogEnvironment,
    transform: impl FnOnce(
        &[u8],
        &Path,
        crate::dogfood::catalog_config::CatalogEnvironment,
    ) -> Result<(Vec<u8>, T), String>,
) -> Result<T, String> {
    let root = &environment.config;
    let mut directory = fs::DirBuilder::new();
    directory.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    directory.create(root).map_err(io)?;
    let metadata = fs::symlink_metadata(root).map_err(io)?;
    if !metadata.is_dir() || linked(&metadata) {
        return Err("MCP configuration requires a real user-config directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o022 != 0 {
            return Err("MCP user-config directory is writable by other users".into());
        }
    }
    let root = fs::canonicalize(root).map_err(io)?;
    let lock_path = root.join(".mcp-config.lock");
    let lock = opened(
        &lock_path,
        options().write(true).create(true).truncate(false),
    )?;
    lock.try_lock()
        .map_err(|error| format!("MCP configuration lock refused: {error}"))?;
    // Write where the catalog reads: a legacy user config stays legacy, and
    // a new one is created under the current name.
    let [json_path, jsonc_path] = crate::dogfood::legacy_names::config_files(&root);
    let json = read(&json_path)?;
    let jsonc = read(&jsonc_path)?;
    // Refuse ambiguity rather than remove a declaration only to reveal an older
    // copy in the other file. The user can consolidate deliberately.
    if json.is_some() && jsonc.is_some() {
        return Err(format!(
            "MCP edits require one user config: consolidate {} and {}",
            file_name(&json_path),
            file_name(&jsonc_path)
        ));
    }
    let (path, before) = if let Some(bytes) = json {
        (json_path, Some(bytes))
    } else {
        (jsonc_path, jsonc)
    };
    use grapt_platform::profile_configuration::{frozen, FrozenRead};
    if !matches!(
        frozen(&path, DOCUMENT_BYTES).map_err(io)?,
        FrozenRead::Unbound
    ) {
        return Err(
            "MCP edits require user configuration outside an approved repository profile".into(),
        );
    }
    let bytes = before.as_deref().unwrap_or(b"{}\n");
    let (output, result) = transform(bytes, &path, environment)?;
    if output == bytes {
        return Ok(result);
    }
    let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let temporary_path = root.join(format!(".mcp-{}-{serial}.tmp", std::process::id()));
    let mut temp_options = options();
    temp_options.write(true).create_new(true);
    let mut file = temp_options.open(&temporary_path).map_err(io)?;
    let temporary = Temporary(temporary_path);
    file.write_all(&output)
        .and_then(|()| file.sync_all())
        .map_err(io)?;
    drop(file);
    if read(&path)? != before {
        return Err("MCP configuration changed while editing; retry".into());
    }
    fs::rename(&temporary.0, &path).map_err(io)?;
    #[cfg(unix)]
    File::open(&root)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| "MCP configuration was published, but syncing its directory failed")?;
    Ok(result)
}

fn file_name(path: &Path) -> std::borrow::Cow<'_, str> {
    path.file_name()
        .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy())
}
