//! The projects a user's Sessions live in, for PROJECTS and `run --project`.
//!
//! Each project once, newest first, at its project directory: the nearest
//! ancestor of its newest Session's directory that holds `.git`, or the
//! directory itself when Git is absent, as location discovery places it.
//! The profile's own service root is never a project, and a Session placed
//! in or under it never makes one: a Session there would put the agent's
//! tools beside the database and the credential files.

use std::path::{Path, PathBuf};

use grapt_api::u3a::ProjectListResponseDto;
use grapt_session::SessionListStore;
use serde_json::{json, Value};

use super::support::decode;
use super::DogfoodApplication;

/// The most projects listed; the TUI keeps the same number of catalog rows.
pub(super) const MAX_PROJECTS: usize = 128;

/// (project, directory) pairs read to find them: four directories a project.
const MAX_LOCATIONS: usize = 4 * MAX_PROJECTS;

pub(super) fn list(application: &DogfoodApplication) -> Result<ProjectListResponseDto, String> {
    let rows = {
        let mut inbox = application.lock_inbox();
        SessionListStore::over(inbox.store_mut())
            .recent_projects(MAX_LOCATIONS)
            .map_err(|error| error.to_string())?
    };
    let provider_catalog = application.catalog_snapshot();
    let service_root = Path::new(&provider_catalog.directory);
    let mut seen = std::collections::HashSet::<String>::new();
    let mut projects = Vec::new();
    for (project, directory, updated) in rows {
        if projects.len() >= MAX_PROJECTS {
            break;
        }
        // Newest first, so the first usable directory is the project's newest.
        if seen.contains(project.as_str()) || within(Path::new(&directory), service_root) {
            continue;
        }
        let worktree = project_directory(Path::new(&directory)).unwrap_or_else(|| directory.into());
        if within(&worktree, service_root) {
            continue;
        }
        seen.insert(project.as_str().to_owned());
        let Some(worktree) = worktree.to_str() else {
            continue;
        };
        projects.push(json!({
            "id": project.as_str(),
            "worktree": worktree,
            "time": {"created": updated, "updated": updated},
            "sandboxes": [],
        }));
    }
    decode(Value::Array(projects))
}

/// The directory location discovery places `directory`'s project at: its
/// repository root, or its own canonical path outside Git; `None`
/// when the directory is gone or unreadable.
pub(super) fn project_directory(directory: &Path) -> Option<PathBuf> {
    if !std::fs::metadata(directory).ok()?.is_dir() {
        return None;
    }
    match super::locations::repository_root(directory).ok()? {
        Some(root) => Some(root.to_path_buf()),
        None => std::fs::canonicalize(directory).ok(),
    }
}

/// Whether `path` is `root` or inside it, as written or once resolved.
fn within(path: &Path, root: &Path) -> bool {
    if path.starts_with(root) {
        return true;
    }
    match (std::fs::canonicalize(path), std::fs::canonicalize(root)) {
        (Ok(path), Ok(root)) => path.starts_with(root),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_service_root_and_everything_inside_it_are_never_a_project() {
        let root = Path::new("/srv/profile");
        assert!(within(Path::new("/srv/profile"), root));
        assert!(within(Path::new("/srv/profile/repo"), root));
        assert!(!within(Path::new("/srv/profile-other"), root));
        assert!(!within(Path::new("/srv"), root));
        assert!(!within(Path::new("/"), root));
    }
}
