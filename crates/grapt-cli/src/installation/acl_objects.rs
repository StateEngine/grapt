//! Which objects the install-permission check judges (review ACL-H1, H3).
//!
//! Parent protection does not establish child protection: a file inside the
//! build being activated can carry its own grant. So the build's whole tree
//! is judged, every directory and file under `versions\<target>`, bounded in
//! count. Only `std::fs` is used, so the enumeration is witnessed on every
//! platform. An object is omitted only when it is optional and absent; a
//! mandatory one that is missing, and any other error, cannot be verified.

use std::path::{Path, PathBuf};

use super::acl::Refusal;

/// The most entries `versions\<target>` may hold; above it the check refuses
/// rather than judging part of the build.
pub(super) const MAX_TREE: usize = super::bounds::TREE_ENTRIES;

fn inspected(object: &Path) -> Result<Option<std::fs::Metadata>, Refusal> {
    match std::fs::symlink_metadata(object) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Refusal::Unverifiable(format!(
            "{} cannot be inspected: {:?}",
            super::display_path(object),
            error.kind()
        ))),
    }
}

fn mandatory(object: &Path) -> Result<std::fs::Metadata, Refusal> {
    inspected(object)?
        .ok_or_else(|| Refusal::Unverifiable(format!("{} is missing", super::display_path(object))))
}

/// Every directory and file under `root`, `root` first, children sorted, at
/// most `bound` entries. A link inside the build refuses: what it points at
/// is not the build's own file.
fn tree(root: &Path, bound: usize) -> Result<Vec<PathBuf>, Refusal> {
    let mut objects = vec![root.to_path_buf()];
    let mut next = 0;
    while next < objects.len() {
        let object = objects[next].clone();
        next += 1;
        let metadata = mandatory(&object)?;
        if metadata.file_type().is_symlink() {
            return Err(Refusal::Unverifiable(format!(
                "{} is a link inside the build",
                super::display_path(&object)
            )));
        }
        if !metadata.is_dir() {
            continue;
        }
        let unreadable = |error: std::io::Error| {
            Refusal::Unverifiable(format!(
                "{} cannot be listed: {:?}",
                super::display_path(&object),
                error.kind()
            ))
        };
        let mut children = Vec::new();
        for entry in std::fs::read_dir(&object).map_err(unreadable)? {
            children.push(entry.map_err(unreadable)?.path());
            if objects.len() + children.len() > bound {
                return Err(Refusal::Unverifiable(format!(
                    "{} holds more than {bound} entries, which this check reads at most",
                    super::display_path(root)
                )));
            }
        }
        children.sort();
        objects.extend(children);
    }
    Ok(objects)
}

/// The installation objects: the prefix, its launcher, `versions`, the whole
/// tree of the build being activated, and the selection records.
pub(super) fn installation(
    prefix: &Path,
    target: &str,
    bound: usize,
) -> Result<Vec<PathBuf>, Refusal> {
    mandatory(prefix)?;
    let versions = prefix.join("versions");
    mandatory(&versions)?;
    let mut objects = vec![prefix.to_path_buf()];
    let bin = prefix.join("bin");
    for optional in [bin.clone(), bin.join("grapt.exe"), bin.join("se.exe")] {
        if inspected(&optional)?.is_some() {
            objects.push(optional);
        }
    }
    objects.push(versions.clone());
    objects.extend(tree(&versions.join(target), bound)?);
    for record in [
        "current",
        "state.json",
        "bootstrap.json",
        "se-bootstrap.json",
        "user-path.json",
        "transition.json",
    ] {
        let record = prefix.join(record);
        if inspected(&record)?.is_some() {
            objects.push(record);
        }
    }
    Ok(objects)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            // Restore listing rights a fault row removed, then clean up.
            for directory in [
                self.0.clone(),
                self.0.join("versions").join("A").join("bin"),
            ] {
                let _restored =
                    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700));
            }
            let _cleanup = std::fs::remove_dir_all(&self.0);
        }
    }

    /// An installation with build A: `versions/A/bin/{grapt.exe,grapt-server.exe}`
    /// and a nested runtime asset.
    fn installation_with_a(name: &str) -> std::io::Result<Scratch> {
        let root =
            std::env::temp_dir().join(format!("grapt-acl-objects-{}-{name}", std::process::id()));
        let scratch = Scratch(root.clone());
        let bin = root.join("versions").join("A").join("bin");
        std::fs::create_dir_all(bin.join("plugin-runtime"))?;
        for file in ["grapt.exe", "grapt-server.exe"] {
            std::fs::write(bin.join(file), b"fixture")?;
        }
        std::fs::write(bin.join("plugin-runtime").join("node.exe"), b"fixture")?;
        Ok(scratch)
    }

    fn listed(prefix: &Path, bound: usize) -> Vec<String> {
        match installation(prefix, "A", bound) {
            Ok(objects) => objects
                .iter()
                .map(|object| {
                    object
                        .strip_prefix(prefix)
                        .map_or_else(|_| String::new(), |relative| relative.display().to_string())
                })
                .collect(),
            Err(refusal) => panic!("the installation must enumerate: {refusal:?}"),
        }
    }

    #[test]
    fn every_file_inside_the_target_build_is_judged() -> std::io::Result<()> {
        let scratch = installation_with_a("tree")?;
        assert_eq!(
            listed(&scratch.0, MAX_TREE),
            [
                "",
                "versions",
                "versions/A",
                "versions/A/bin",
                "versions/A/bin/grapt-server.exe",
                "versions/A/bin/grapt.exe",
                "versions/A/bin/plugin-runtime",
                "versions/A/bin/plugin-runtime/node.exe",
            ]
        );
        Ok(())
    }

    #[test]
    fn optional_objects_appear_when_present_and_absent_ones_are_omitted() -> std::io::Result<()> {
        let scratch = installation_with_a("optional")?;
        std::fs::create_dir(scratch.0.join("bin"))?;
        std::fs::write(scratch.0.join("bin").join("grapt.exe"), b"launcher")?;
        std::fs::write(scratch.0.join("state.json"), b"{}")?;
        let objects = listed(&scratch.0, MAX_TREE);
        assert_eq!(objects[1..3], ["bin", "bin/grapt.exe"]);
        assert_eq!(objects.last().map(String::as_str), Some("state.json"));
        assert!(!objects.iter().any(|object| object == "current"));
        Ok(())
    }

    #[test]
    fn a_missing_target_or_versions_cannot_be_verified() -> std::io::Result<()> {
        let scratch = installation_with_a("missing")?;
        assert!(matches!(
            installation(&scratch.0, "B", MAX_TREE),
            Err(Refusal::Unverifiable(reason)) if reason.contains("is missing")
        ));
        std::fs::remove_dir_all(scratch.0.join("versions"))?;
        assert!(matches!(
            installation(&scratch.0, "A", MAX_TREE),
            Err(Refusal::Unverifiable(_))
        ));
        Ok(())
    }

    #[test]
    fn a_tree_above_the_bound_refuses_and_names_it() -> std::io::Result<()> {
        let scratch = installation_with_a("bound")?;
        match installation(&scratch.0, "A", 4) {
            Err(Refusal::Unverifiable(reason)) => {
                assert!(reason.contains("more than 4 entries"), "{reason}")
            }
            other => panic!("expected the bound to refuse, got {other:?}"),
        }
        assert_eq!(
            listed(&scratch.0, 6).len(),
            8,
            "the bound counts the build's entries only"
        );
        Ok(())
    }

    #[test]
    fn an_unlistable_directory_or_unreadable_record_is_not_absent() -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        // Root lists and traverses regardless of mode bits, so this fault cannot
        // be staged there: fail loudly rather than pass without testing (the
        // honest-failure law). The name-length row below runs everywhere.
        let scratch = installation_with_a("faults")?;
        assert!(
            !running_as_root(&scratch.0),
            "this witness stages a permission fault, which root bypasses; run it unprivileged"
        );
        let bin = scratch.0.join("versions").join("A").join("bin");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o000))?;
        let listing = installation(&scratch.0, "A", MAX_TREE);
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700))?;
        assert!(
            matches!(listing, Err(Refusal::Unverifiable(reason)) if reason.contains("cannot be listed"))
        );
        // A record whose metadata cannot be read (its directory is untraversable).
        std::fs::write(scratch.0.join("state.json"), b"{}")?;
        std::fs::set_permissions(&scratch.0, std::fs::Permissions::from_mode(0o600))?;
        let record = inspected(&scratch.0.join("state.json"));
        std::fs::set_permissions(&scratch.0, std::fs::Permissions::from_mode(0o700))?;
        assert!(
            matches!(record, Err(Refusal::Unverifiable(reason)) if reason.contains("cannot be inspected"))
        );
        Ok(())
    }

    /// A file just created by this process carries its effective uid.
    /// Failed creation or metadata reads refuse to supply root evidence.
    fn running_as_root(scratch: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(scratch.join("effective-uid"))
            .and_then(|file| file.metadata())
            .unwrap_or_else(|error| panic!("root detection evidence is unreadable: {error}"));
        metadata.uid() == 0
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn effective_uid_probe_agrees_with_proc_status() -> std::io::Result<()> {
        let scratch = installation_with_a("effective-uid")?;
        let status = std::fs::read_to_string("/proc/self/status")?;
        let effective_uid = status
            .lines()
            .find(|line| line.starts_with("Uid:"))
            .and_then(|line| line.split_whitespace().nth(2))
            .ok_or_else(|| std::io::Error::other("root detection evidence has no effective Uid"))?
            .parse::<u32>()
            .map_err(std::io::Error::other)?;
        assert_eq!(running_as_root(&scratch.0), effective_uid == 0);
        Ok(())
    }

    /// A real, deterministic inspection fault that holds for every user,
    /// root included: a path component longer than any filesystem allows
    /// fails metadata with a name-length error, never NotFound. So a mandatory
    /// object behind it cannot be verified, and is not read as absent.
    #[test]
    fn an_uninspectable_path_is_not_absent_for_any_user() -> std::io::Result<()> {
        let scratch = installation_with_a("name-length")?;
        let unnamable = scratch.0.join("x".repeat(300));
        assert!(matches!(
            inspected(&unnamable.join("state.json")),
            Err(Refusal::Unverifiable(reason)) if reason.contains("cannot be inspected")
        ));
        assert!(matches!(
            installation(&unnamable, "A", MAX_TREE),
            Err(Refusal::Unverifiable(reason)) if reason.contains("cannot be inspected")
        ));
        Ok(())
    }

    #[test]
    fn a_link_inside_the_build_refuses() -> std::io::Result<()> {
        let scratch = installation_with_a("link")?;
        std::os::unix::fs::symlink(
            "/etc/hostname",
            scratch
                .0
                .join("versions")
                .join("A")
                .join("bin")
                .join("linked.exe"),
        )?;
        assert!(matches!(
            installation(&scratch.0, "A", MAX_TREE),
            Err(Refusal::Unverifiable(reason)) if reason.contains("is a link inside the build")
        ));
        Ok(())
    }
}

/// The USER Path route must not pass through a replaceable user ancestor.
/// Prefix itself is already in installation(); return its parents up to local.
pub(super) fn user_ancestors(prefix: &Path, local: &Path) -> Result<Vec<PathBuf>, Refusal> {
    let relative = prefix
        .strip_prefix(local)
        .map_err(|_| Refusal::Unverifiable("command prefix is outside LOCALAPPDATA".into()))?;
    let parts: Vec<_> = relative.components().collect();
    if parts.is_empty()
        || parts.len() > 64
        || parts
            .iter()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(Refusal::Unverifiable(
            "command ancestor path is not bounded and direct".into(),
        ));
    }
    let mut current = local.to_path_buf();
    let mut result = Vec::new();
    for part in parts {
        let metadata = mandatory(&current)?;
        #[cfg(windows)]
        let reparse = {
            use std::os::windows::fs::MetadataExt;
            metadata.file_attributes() & 0x400 != 0
        };
        #[cfg(not(windows))]
        let reparse = metadata.file_type().is_symlink();
        if !metadata.is_dir() || reparse {
            return Err(Refusal::Unverifiable(
                "USER Path route contains a non-directory or linked ancestor".into(),
            ));
        }
        result.push(current.clone());
        current.push(part);
    }
    Ok(result)
}

#[cfg(all(test, unix))]
mod route_tests {
    use super::*;
    #[test]
    fn every_user_parent_is_checked_and_linked_or_outside_routes_refuse(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("grapt-acl-route-{}", std::process::id()));
        std::fs::create_dir(&root)?;
        let prefix = root.join("inside/installation");
        std::fs::create_dir_all(&prefix)?;
        assert_eq!(
            user_ancestors(&prefix, &root).map_err(|error| error.message())?,
            [root.clone(), root.join("inside")]
        );
        assert!(user_ancestors(&root, &root).is_err());
        assert!(user_ancestors(&root.join("other"), &prefix).is_err());
        std::os::unix::fs::symlink(root.join("inside"), root.join("alias"))?;
        assert!(user_ancestors(&root.join("alias/installation"), &root).is_err());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
