//! Source provenance stays separate from catalog selection and precedence.
use std::path::{Path, PathBuf};

use super::catalog_config::{DOCUMENT_BYTES, MAX_ANCESTORS};

/// Selection and ordering do not promote repository bytes to user authority.
/// In particular a service root or explicitly selected file under `.grapt`
/// retains Project origin before the plugin/MCP and permission folds see it.
pub(super) fn source_origin(
    path: &Path,
    origin: grapt_plugin::PluginOrigin,
    directory: &Path,
    service_root: &Path,
) -> Result<grapt_plugin::PluginOrigin, String> {
    let ceiling = fixture_ceiling(service_root)?;
    source_origin_below(path, origin, directory, service_root, ceiling.as_deref())
}

/// Only fixture builds read this switch. A normal server always walks through
/// the filesystem root, regardless of Git's ambient discovery ceilings.
pub(super) fn fixture_ceiling(service_root: &Path) -> Result<Option<PathBuf>, String> {
    #[cfg(feature = "loopback-provider-testkit")]
    {
        let Some(raw) = std::env::var_os("GRAPT_TEST_CATALOG_ROOT") else {
            return Ok(None);
        };
        let path = PathBuf::from(raw);
        if !path.is_absolute() || path.as_os_str().len() > 4096 {
            return Err("catalog fixture root must be a bounded absolute path".into());
        }
        let root = std::fs::canonicalize(path).map_err(|error| {
            format!(
                "catalog fixture root cannot be resolved: {:?}",
                error.kind()
            )
        })?;
        let service = std::fs::canonicalize(service_root).map_err(|error| {
            format!(
                "catalog fixture service cannot be resolved: {:?}",
                error.kind()
            )
        })?;
        if !service.starts_with(&root) {
            return Err("catalog fixture root does not contain the service".into());
        }
        Ok(Some(root))
    }
    #[cfg(not(feature = "loopback-provider-testkit"))]
    {
        let _ = service_root;
        Ok(None)
    }
}

pub(super) fn source_origin_below(
    path: &Path,
    origin: grapt_plugin::PluginOrigin,
    directory: &Path,
    service_root: &Path,
    ceiling: Option<&Path>,
) -> Result<grapt_plugin::PluginOrigin, String> {
    use grapt_plugin::PluginOrigin;
    if origin == PluginOrigin::Project {
        return Ok(origin);
    }
    // A repository approval never promotes its captured document to global
    // authority, even if the mutable pathname is removed or redirected later.
    if !matches!(
        grapt_platform::profile_configuration::frozen(path, DOCUMENT_BYTES).map_err(
            |error| format!("approved configuration origin refused: {:?}", error.kind())
        )?,
        grapt_platform::profile_configuration::FrozenRead::Unbound
    ) {
        return Ok(PluginOrigin::Project);
    }
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        format!(
            "configuration origin cannot be resolved: {:?}",
            error.kind()
        )
    })?;
    let boundary = ceiling
        .map(std::fs::canonicalize)
        .transpose()
        .map_err(|error| {
            format!(
                "catalog fixture boundary cannot be resolved: {:?}",
                error.kind()
            )
        })?
        .filter(|root| canonical.starts_with(root));
    // Either project directory name marks a repository profile.
    let profile_component = |path: &Path| {
        path.components().any(|part| {
            part.as_os_str()
                .to_str()
                .is_some_and(super::legacy_names::is_project_directory)
        })
    };
    let physical = boundary
        .as_deref()
        .and_then(|root| canonical.strip_prefix(root).ok())
        .unwrap_or(&canonical);
    let spelling = if let Some(root) = &boundary {
        fixture_spelling(path, root)?
    } else {
        path
    };
    if profile_component(spelling) || profile_component(physical) {
        return Ok(PluginOrigin::Project);
    }
    let project = std::fs::canonicalize(directory)
        .map_err(|error| format!("project origin cannot be resolved: {:?}", error.kind()))?;
    let service = super::authority_path::resolve(service_root)?;
    let project_key =
        grapt_types::path::comparison_key(project.to_str().ok_or("project origin is not UTF-8")?);
    if project_key != grapt_types::path::comparison_key(&service) && canonical.starts_with(project)
    {
        return Ok(PluginOrigin::Project);
    }
    // Startup catalog refreshes use the service root as their directory,
    // before a project is opened. A profile inside a checkout is still not
    // global merely because no project-location argument has arrived yet.
    for ancestor in canonical.ancestors().skip(1).take(MAX_ANCESTORS) {
        match std::fs::symlink_metadata(ancestor.join(".git")) {
            Ok(_) => return Ok(PluginOrigin::Project),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "repository origin is unreadable: {:?}",
                    error.kind()
                ))
            }
        }
        // Markers inside the fixture, including one at its root, still count.
        if boundary.as_deref() == Some(ancestor) {
            break;
        }
    }
    Ok(origin)
}

/// Resolve the fixture boundary in the source's own spelling. An aliased
/// root must not erase a `.grapt` component that itself aliases a plain dir.
fn fixture_spelling<'a>(path: &'a Path, root: &Path) -> Result<&'a Path, String> {
    let mut relative = path;
    for ancestor in path.ancestors().skip(1).take(MAX_ANCESTORS) {
        let canonical = std::fs::canonicalize(ancestor)
            .map_err(|error| format!("fixture spelling cannot be resolved: {:?}", error.kind()))?;
        if canonical == root {
            // Keep the outermost matching spelling, including `.grapt/..`
            // components between equivalent spellings of the same boundary.
            relative = path
                .strip_prefix(ancestor)
                .map_err(|_| "fixture spelling has no source suffix")?;
        }
    }
    Ok(relative)
}

#[cfg(test)]
#[path = "catalog_origin_tests.rs"]
mod tests;
