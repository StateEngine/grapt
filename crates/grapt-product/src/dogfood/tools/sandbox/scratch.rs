//! Per-command private scratch temp directory for Linux and Windows sandboxes
//! when the shared host temp directory is an ancestor of the workspace.
use super::Plan;
use std::path::{Path, PathBuf};

/// Owned private scratch temp directory attached to one sandboxed child.
/// Call [`PrivateScratch::cleanup`] after the child exits so permission-locked
/// subdirectories are relaxed and any removal failure is surfaced explicitly.
pub(super) struct PrivateScratch {
    dir: Option<tempfile::TempDir>,
}

impl PrivateScratch {
    pub(super) fn attach(
        plan: &Plan,
        policy: &mut grapt_sandbox::Policy,
        environment: &mut Vec<String>,
    ) -> Result<Option<Self>, String> {
        let Some(parent) = plan.private_temp_parent() else {
            return Ok(None);
        };
        let dir = tempfile::Builder::new()
            .prefix("grapt-sandbox-tmp-")
            .tempdir_in(parent)
            .map_err(|error| format!("sandbox private temp directory unavailable: {error}"))?;
        let canonical_private = std::fs::canonicalize(dir.path())
            .map_err(|error| format!("sandbox private temp directory unavailable: {error}"))?;
        let text = canonical_private
            .to_str()
            .ok_or("sandbox private temp directory is not Unicode")?;
        environment.retain(|entry| {
            !entry.split_once('=').is_some_and(|(name, _)| {
                name.eq_ignore_ascii_case("TMPDIR")
                    || name.eq_ignore_ascii_case("TEMP")
                    || name.eq_ignore_ascii_case("TMP")
            })
        });
        environment.push(format!("TMPDIR={text}"));
        environment.push(format!("TEMP={text}"));
        environment.push(format!("TMP={text}"));
        policy.writable_roots.push(canonical_private);
        Ok(Some(Self { dir: Some(dir) }))
    }

    #[cfg(test)]
    pub(super) fn path(&self) -> Option<&Path> {
        self.dir.as_ref().map(tempfile::TempDir::path)
    }

    pub(super) fn cleanup(mut self) -> Result<(), String> {
        let Some(dir) = self.dir.take() else {
            return Ok(());
        };
        relax_tree_permissions(dir.path())?;
        dir.close()
            .map_err(|error| format!("sandbox private temp cleanup failed: {error}"))
    }
}

fn relax_tree_permissions(root: &Path) -> Result<(), String> {
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(current) = stack.pop() {
        let meta = match std::fs::symlink_metadata(&current) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "sandbox private temp metadata failed at {}: {error}",
                    current.display()
                ));
            }
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        relax_entry_permissions(&current, &meta)?;
        if meta.is_dir() {
            let entries = std::fs::read_dir(&current).map_err(|error| {
                format!(
                    "sandbox private temp directory scan failed at {}: {error}",
                    current.display()
                )
            })?;
            for entry in entries {
                let entry = entry.map_err(|error| {
                    format!(
                        "sandbox private temp entry scan failed at {}: {error}",
                        current.display()
                    )
                })?;
                stack.push(entry.path());
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn relax_entry_permissions(path: &Path, meta: &std::fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = meta.permissions().mode();
    let required = if meta.is_dir() { 0o700 } else { 0o600 };
    if mode & required != required {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | required)).map_err(
            |error| {
                format!(
                    "sandbox private temp permission relax failed at {}: {error}",
                    path.display()
                )
            },
        )?;
    }
    Ok(())
}

#[cfg(windows)]
#[allow(clippy::permissions_set_readonly_false)]
fn relax_entry_permissions(path: &Path, meta: &std::fs::Metadata) -> Result<(), String> {
    let mut perms = meta.permissions();
    if perms.readonly() {
        perms.set_readonly(false);
        std::fs::set_permissions(path, perms).map_err(|error| {
            format!(
                "sandbox private temp permission relax failed at {}: {error}",
                path.display()
            )
        })?;
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn relax_entry_permissions(_path: &Path, _meta: &std::fs::Metadata) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_scratch_honors_planner_parent_and_cleans_readonly_subdir() -> Result<(), String> {
        let configured_temp = tempfile::Builder::new()
            .prefix("grapt-configured-tmpdir-")
            .tempdir()
            .map_err(|e| e.to_string())?;
        let canonical_parent =
            std::fs::canonicalize(configured_temp.path()).map_err(|e| e.to_string())?;
        let workspace = canonical_parent.join("workspace");
        std::fs::create_dir(&workspace).map_err(|e| e.to_string())?;
        let plan = Plan {
            git_metadata_read_only: false,
            readable: vec![PathBuf::from("/")],
            writable: vec![workspace],
            protected: Vec::new(),
            denied: Vec::new(),
            network: false,
            private_temp_parent: Some(canonical_parent.clone()),
        };
        let mut policy = plan.policy();
        let mut env = vec![format!("TMPDIR={}", canonical_parent.display())];
        let scratch = PrivateScratch::attach(&plan, &mut policy, &mut env)?
            .ok_or("expected private scratch from planner decision")?;
        let scratch_path = std::fs::canonicalize(scratch.path().ok_or("missing scratch path")?)
            .map_err(|e| e.to_string())?;
        assert!(scratch_path.starts_with(&canonical_parent));
        assert!(policy.writable_roots.contains(&scratch_path));
        assert!(!policy.writable_roots.contains(&canonical_parent));
        assert!(env.contains(&format!("TMPDIR={}", scratch_path.display())));
        assert!(env.contains(&format!("TEMP={}", scratch_path.display())));
        assert!(env.contains(&format!("TMP={}", scratch_path.display())));

        // Create a read-only subdirectory + file inside scratch (e.g. chmod 0500)
        // and verify explicit cleanup relaxes permissions and removes it cleanly.
        let locked_dir = scratch_path.join("locked");
        std::fs::create_dir(&locked_dir).map_err(|e| e.to_string())?;
        std::fs::write(locked_dir.join("artifact.txt"), "data").map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&locked_dir, std::fs::Permissions::from_mode(0o500))
                .map_err(|e| e.to_string())?;
        }
        scratch.cleanup()?;
        assert!(
            !scratch_path.exists(),
            "private scratch directory survived cleanup: {}",
            scratch_path.display()
        );
        Ok(())
    }
}
