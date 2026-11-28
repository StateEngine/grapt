//! The documents a user wrote for themselves: the service profile's and the
//! user configuration directory's. Each is opened as an entry of its resolved
//! directory, never through a link, and its origin is decided for that entry
//! while it is still the file that was opened, so the authority found is the
//! authority of the bytes read. A link, or a file inside a repository, is
//! refused.
use super::{decode, legacy_names, refusal_text, CatalogDocument, CatalogEnvironment};
use grapt_plugin::PluginOrigin;
use std::io::Read;
use std::path::Path;

/// The user's own documents, profile first; `None` without a catalog home.
pub(in crate::dogfood) fn user_documents(
    service_root: &Path,
) -> Result<Option<Vec<CatalogDocument>>, String> {
    CatalogEnvironment::configured(service_root)?
        .map(|environment| documents_in(&environment))
        .transpose()
}

pub(in crate::dogfood) fn documents_in(
    environment: &CatalogEnvironment,
) -> Result<Vec<CatalogDocument>, String> {
    let mut documents = Vec::new();
    for directory in [&environment.service_root, &environment.config] {
        for path in legacy_names::config_files(directory) {
            let named = |reason: String| refusal_text(&reason, &path);
            if let Some(document) = read_bound(&path, environment).map_err(named)? {
                documents.push(document);
            }
        }
    }
    Ok(documents)
}

fn read_bound(
    path: &Path,
    environment: &CatalogEnvironment,
) -> Result<Option<CatalogDocument>, String> {
    let kind = |error: std::io::Error| format!("configuration is unreadable: {:?}", error.kind());
    let (Some(directory), Some(name)) = (path.parent(), path.file_name()) else {
        return Err("configuration has no directory".to_owned());
    };
    let directory = match std::fs::canonicalize(directory) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(kind(error)),
    };
    let path = directory.join(name);
    let Some(file) = open_entry(&path).map_err(|error| error.to_string())? else {
        return Ok(None);
    };
    let opened = same_file::Handle::from_file(file.try_clone().map_err(kind)?).map_err(kind)?;
    let origin = crate::dogfood::catalog_origin::source_origin_below(
        &path,
        PluginOrigin::UserGlobal,
        &environment.service_root,
        &environment.service_root,
        environment.fixture_root(),
    )?;
    if same_file::Handle::from_path(&path).map_err(kind)? != opened {
        return Err("configuration was replaced while it was read".to_owned());
    }
    let mut bytes = Vec::new();
    file.take(super::DOCUMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(kind)?;
    if bytes.len() > super::DOCUMENT_BYTES {
        return Err("configuration exceeds its 64 KiB bound".to_owned());
    }
    Ok(Some(CatalogDocument {
        directory,
        source: path.to_string_lossy().into_owned(),
        origin,
        value: decode(&bytes, super::bounds()?)?,
    }))
}

/// The directory entry at `path` itself, refusing a link to anywhere else.
#[cfg(unix)]
fn open_entry(path: &Path) -> std::io::Result<Option<std::fs::File>> {
    use std::os::unix::fs::MetadataExt;
    let entry = match std::fs::symlink_metadata(path) {
        Ok(entry) => entry,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let file = std::fs::File::open(path)?;
    let opened = file.metadata()?;
    // Same device and inode: the open did not pass through a link.
    if entry.file_type().is_symlink() || (opened.dev(), opened.ino()) != (entry.dev(), entry.ino())
    {
        return Err(linked());
    }
    regular(file, &opened)
}

/// The directory entry at `path` itself, refusing a link to anywhere else.
#[cfg(windows)]
fn open_entry(path: &Path) -> std::io::Result<Option<std::fs::File>> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    // Opens a link itself rather than its target, so a link is seen as one.
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let opened = file.metadata()?;
    if opened.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(linked());
    }
    regular(file, &opened)
}

fn regular(
    file: std::fs::File,
    opened: &std::fs::Metadata,
) -> std::io::Result<Option<std::fs::File>> {
    if opened.is_file() {
        Ok(Some(file))
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "configuration is not a regular file",
        ))
    }
}

fn linked() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "configuration is a link; name the file itself",
    )
}
