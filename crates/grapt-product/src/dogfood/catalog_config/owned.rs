//! The documents a user wrote for themselves: the service profile's and the
//! user configuration directory's. Each is read through the one handle it was
//! opened with, and its origin is decided while its path still names that
//! file, so the authority found is the authority of the bytes read.
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
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(kind(error)),
    };
    let opened = same_file::Handle::from_file(file.try_clone().map_err(kind)?).map_err(kind)?;
    let origin = crate::dogfood::catalog_origin::source_origin_below(
        path,
        PluginOrigin::UserGlobal,
        &environment.service_root,
        &environment.service_root,
        environment.fixture_root(),
    )?;
    if same_file::Handle::from_path(path).map_err(kind)? != opened {
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
        directory: path.parent().unwrap_or(path).to_path_buf(),
        source: path.to_string_lossy().into_owned(),
        origin,
        value: decode(&bytes, super::bounds()?)?,
    }))
}
