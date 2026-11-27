//! Publish only owned command aliases; shell configuration remains the user's.
use std::path::Path;

#[cfg(target_os = "linux")]
#[path = "fixed_launcher.rs"]
mod fixed;

#[cfg(target_os = "linux")]
use super::inventory::Inventory;
#[cfg(target_os = "linux")]
use super::io;
#[cfg(target_os = "linux")]
use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
use std::path::PathBuf;

#[cfg(target_os = "linux")]
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Aliases {
    schema: String,
    home: PathBuf,
    owned: Vec<String>,
}

#[cfg(target_os = "linux")]
fn home(prefix: &Path) -> Result<PathBuf, String> {
    #[cfg(test)]
    {
        Ok(prefix.join("test-home"))
    }
    #[cfg(not(test))]
    {
        let _ = prefix;
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| {
                "HOME must name an absolute user directory for command installation".into()
            })
    }
}

#[cfg(target_os = "linux")]
fn read_aliases(prefix: &Path, home: &Path, owner: Owner) -> Result<Aliases, String> {
    let path = prefix.join("launchers.json");
    let record = match std::fs::symlink_metadata(&path) {
        Ok(_) => serde_json::from_slice::<Aliases>(&io::read(&path, 16 * 1024)?)
            .map_err(|_| "installed command ownership record is malformed")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Aliases {
            schema: "secode.install.aliases.v1".into(),
            home: home.into(),
            owned: Vec::new(),
        },
        Err(error) => {
            return Err(format!(
                "command ownership is unreadable: {:?}",
                error.kind()
            ))
        }
    };
    if record.schema != "secode.install.aliases.v1"
        || record.home != home
        || record.owned.len() > 2
        || record.owned.iter().any(|name| !license(owner, name))
    {
        return Err("installed command ownership does not match this user".into());
    }
    Ok(record)
}

/// Which command names a record owned by `owner` may claim.
///
/// A current prefix owns only the names this build publishes. The one relaxed
/// vocabulary belongs to the pre-rename installation alone: its own record
/// names `secode`, and refusing to read it would leave this build unable to
/// recognise the HOME-global link it must not take over. A current prefix
/// claiming a legacy name is a corrupt record, not an owner.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner {
    Current,
    PreRename,
}

#[cfg(target_os = "linux")]
fn license(owner: Owner, name: &str) -> bool {
    match owner {
        Owner::Current => name == "grapt" || name == "se",
        Owner::PreRename => {
            name == "grapt" || name == "se" || name == "secode" // grapt-rename: keep
        }
    }
}

/// The current installation's own record. Strict: only this build's names.
#[cfg(target_os = "linux")]
fn aliases(prefix: &Path, home: &Path) -> Result<Aliases, String> {
    read_aliases(prefix, home, Owner::Current)
}

/// The pre-rename installation's record, which may name `secode`.
#[cfg(target_os = "linux")]
fn pre_rename_aliases(prefix: &Path, home: &Path) -> Result<Aliases, String> {
    read_aliases(prefix, home, Owner::PreRename)
}

#[cfg(target_os = "linux")]
fn link_matches(path: &Path, target: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => std::fs::read_link(path)
            .map(|actual| actual == target)
            .map_err(|error| format!("command alias cannot be read: {:?}", error.kind())),
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "command alias cannot be inspected: {:?}",
            error.kind()
        )),
    }
}

#[cfg(target_os = "linux")]
fn safe_bin(home: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    for directory in [
        home.to_path_buf(),
        home.join(".local"),
        home.join(".local/bin"),
    ] {
        io::directory(&directory)?;
        let metadata = std::fs::symlink_metadata(&directory).map_err(|error| error.to_string())?;
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err("command directory or its user ancestors are writable by other users; remove group/other write access before installing commands".into());
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn target(prefix: &Path, name: &str) -> PathBuf {
    target_for(prefix, name, Inventory::Current.cli())
}

/// The link target for `name` when the active version's CLI is `cli`.
#[cfg(target_os = "linux")]
fn target_for(prefix: &Path, name: &str, cli: &str) -> PathBuf {
    if name == "se" {
        prefix.join("bin/se")
    } else {
        prefix.join("current/bin").join(cli)
    }
}

/// The pre-rename installation beside a `grapt` prefix, if one sits there.
pub(super) fn legacy_prefix(prefix: &Path) -> Option<std::path::PathBuf> {
    let sibling = super::pre_rename::sibling(prefix)?;
    std::fs::symlink_metadata(&sibling).ok()?;
    Some(sibling)
}

/// Whether `path` is this installation's link for `name` under either
/// inventory's CLI name.
#[cfg(target_os = "linux")]
fn installation_link(prefix: &Path, path: &Path, name: &str) -> Result<bool, String> {
    for inventory in [Inventory::Current, Inventory::Legacy] {
        if link_matches(path, &target_for(prefix, name, inventory.cli()))? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(target_os = "linux")]
pub(super) fn prepare_bundled(
    prefix: &Path,
    package: &super::package::Package,
) -> Result<(), String> {
    // Verify existing ownership before any automatic refresh. Linux can replace
    // an inactive executable atomically; the prefix guard excludes live users.
    let prepared = fixed::prepare_update_named(prefix, package, false, "se", "se-bootstrap.json")?;
    let refresh = prepared.finish()?.is_some();
    drop(prepared);
    if refresh {
        fixed::prepare_update_named(prefix, package, true, "se", "se-bootstrap.json")?.finish()?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn install(prefix: &Path, bundled: bool) -> Result<String, String> {
    let home = home(prefix)?;
    let bin = home.join(".local/bin");
    super::commands::prefix_directory(&bin)?;
    safe_bin(&home)?;
    let mut record = aliases(prefix, &home)?;

    let names: &[&str] = if bundled {
        &["grapt", "se"]
    } else {
        &["grapt"]
    };
    // Preflight all collisions before publishing either alias.
    let legacy = legacy_prefix(prefix);
    let mut missing = Vec::new();
    for name in names {
        let path = bin.join(name);
        let old = match &legacy {
            Some(legacy) if *name == "se" => {
                let owned = pre_rename_aliases(legacy, &home).map(|record| record.owned);
                super::pre_rename::owns_se(&path, legacy, &owned.unwrap_or_default())
            }
            _ => false,
        };
        match std::fs::symlink_metadata(&path) {
            Ok(_) if installation_link(prefix, &path, name)? => {},
            // The old installation owns its link; publishing over it would take
            // over its command, and another prefix cannot fix a global name.
            Ok(_) if old => {},
            Ok(_) => return Err(format!("{} already exists and is not this installation's command; choose another prefix or resolve the conflict", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => missing.push(*name),
            Err(error) => return Err(format!("command alias is unreadable: {:?}", error.kind())),
        }
    }
    for name in &missing {
        if !record.owned.iter().any(|owned| owned == name) {
            record.owned.push((*name).into());
        }
    }
    // Journal intent before publication; interrupted creation can be retried.
    io::replace(
        &prefix.join("launchers.json"),
        &serde_json::to_vec(&record).map_err(|_| "command ownership cannot be encoded")?,
    )?;
    for name in missing {
        std::os::unix::fs::symlink(target(prefix, name), bin.join(name))
            .map_err(|error| format!("command alias cannot be published: {:?}", error.kind()))?;
    }
    io::sync_directory(&bin)?;
    let on_path = std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|entry| entry == bin));
    if on_path {
        Ok("Commands are available from ~/.local/bin.".into())
    } else {
        let quoted = bin.to_string_lossy().replace('\'', "'\\''");
        Ok(format!("For this shell and future login shells, add this line to your shell configuration:\nexport PATH='{quoted}':\"$PATH\""))
    }
}

/// Point each owned command link at the active version's own CLI, after
/// activation. An owned link this installation no longer recognises (a
/// user replacement) is left alone.
#[cfg(target_os = "linux")]
pub(super) fn refresh(prefix: &Path, cli: &str) -> Result<(), String> {
    let home = home(prefix)?;
    let record = aliases(prefix, &home)?;
    let bin = home.join(".local/bin");
    safe_bin(&home)?;
    let mut changed = false;
    for name in record.owned.iter().filter(|name| *name != "se") {
        let path = bin.join(name);
        let wanted = target_for(prefix, name, cli);
        if link_matches(&path, &wanted)? || !installation_link(prefix, &path, name)? {
            continue;
        }
        let temporary = bin.join(format!(".{name}.grapt-link.new"));
        match std::fs::symlink_metadata(&temporary) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                std::fs::remove_file(&temporary).map_err(|error| {
                    format!("command link partial cannot be removed: {:?}", error.kind())
                })?;
            }
            Ok(_) => {
                return Err(format!(
                    "{} is not a command link partial; remove it and retry",
                    temporary.display()
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "command link partial is unreadable: {:?}",
                    error.kind()
                ))
            }
        }
        std::os::unix::fs::symlink(&wanted, &temporary)
            .map_err(|error| format!("command link cannot be prepared: {:?}", error.kind()))?;
        std::fs::rename(&temporary, &path)
            .map_err(|error| format!("command link cannot be replaced: {:?}", error.kind()))?;
        changed = true;
    }
    if changed {
        io::sync_directory(&bin)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn uninstall(prefix: &Path) -> Result<String, String> {
    let home = home(prefix)?;
    let record = aliases(prefix, &home)?;
    let bin = home.join(".local/bin");

    safe_bin(&home)?;
    for name in &record.owned {
        let path = bin.join(name);
        if installation_link(prefix, &path, name)? {
            std::fs::remove_file(&path)
                .map_err(|error| format!("owned command cannot be removed: {:?}", error.kind()))?;
        }
    }
    io::sync_directory(&bin)?;
    io::replace(
        &prefix.join("launchers.json"),
        &serde_json::to_vec(&Aliases {
            owned: Vec::new(),
            ..record
        })
        .map_err(|_| "command ownership cannot be encoded")?,
    )?;
    Ok("Removed owned command launchers. Versions and profiles were retained; ~/.local/bin remains on PATH.".into())
}

#[cfg(windows)]
pub(super) async fn install(prefix: &Path, _bundled: bool) -> Result<String, String> {
    super::user_path::install(prefix).await
}
#[cfg(windows)]
pub(super) async fn uninstall(prefix: &Path) -> Result<String, String> {
    super::user_path::uninstall(prefix).await
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Result<Self, String> {
            let root = std::env::temp_dir().join(format!(
                "grapt-aliases-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).map_err(|error| error.to_string())?;
            Ok(Self(root))
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _cleanup = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn foreign_alias_collision_prevents_both_publications() -> Result<(), String> {
        let root = Scratch::new()?;
        let bin = home(&root.0)?.join(".local/bin");
        super::super::commands::prefix_directory(&bin)?;
        std::fs::write(bin.join("se"), b"foreign command").map_err(|error| error.to_string())?;
        assert!(install(&root.0, true).is_err());
        assert!(std::fs::symlink_metadata(bin.join("grapt")).is_err());
        assert!(!root.0.join("launchers.json").exists());
        assert_eq!(
            std::fs::read(bin.join("se")).map_err(|error| error.to_string())?,
            b"foreign command"
        );
        Ok(())
    }
    /// The relaxed vocabulary belongs to the legacy read only: a CURRENT
    /// prefix whose own record claims `secode` is a corrupt record, not an
    /// owner, and must still refuse rather than publish commands.
    #[test]
    fn a_current_prefix_record_claiming_a_legacy_name_still_refuses() -> Result<(), String> {
        let root = Scratch::new()?;
        let home = home(&root.0)?;
        let bin = home.join(".local/bin");
        super::super::commands::prefix_directory(&bin)?;
        let record = serde_json::to_vec(&Aliases {
            schema: "secode.install.aliases.v1".into(),
            home: home.clone(),
            owned: vec!["secode".into()], // grapt-rename: keep
        })
        .map_err(|error| error.to_string())?;
        io::replace(&root.0.join("launchers.json"), &record)?;
        assert!(
            install(&root.0, true).is_err(),
            "a current prefix accepted a legacy ownership claim"
        );
        // The reported regression shape: it must not publish either name, must
        // not widen the record, and the record must still be the bytes it wrote,
        // so the next update is not refused by this one's own output.
        assert!(std::fs::symlink_metadata(bin.join("grapt")).is_err());
        assert!(std::fs::symlink_metadata(bin.join("se")).is_err());
        assert_eq!(
            std::fs::read(root.0.join("launchers.json")).map_err(|error| error.to_string())?,
            record
        );
        Ok(())
    }

    /// The negative controls, one per way a global `se` link can fail
    /// to be the old installation's: no record, a record not claiming `se`, a
    /// foreign home, an unreadable record, a foreign link, and a regular file.
    /// Each must refuse and leave the old record and link untouched.
    #[test]
    fn only_a_proven_legacy_record_and_link_is_recognised() -> Result<(), String> {
        let fail = |error: std::io::Error| error.to_string();
        for case in [
            "missing",
            "unclaimed",
            "foreign-home",
            "malformed",
            "foreign-link",
            "regular-file",
        ] {
            let root = Scratch::new()?;
            let (legacy, fresh) = (root.0.join("secode"), root.0.join("grapt")); // grapt-rename: keep
            std::fs::create_dir(&legacy).map_err(fail)?;
            std::fs::create_dir(&fresh).map_err(fail)?;
            let home = home(&fresh)?;
            let bin = home.join(".local/bin");
            super::super::commands::prefix_directory(&bin)?;
            std::fs::create_dir(legacy.join("bin")).map_err(fail)?;
            let owned = legacy.join("bin/se");
            let record_path = legacy.join("launchers.json");
            let bytes = serde_json::to_vec(&Aliases {
                schema: "secode.install.aliases.v1".into(),
                home: match case {
                    "foreign-home" => root.0.join("someone-else"),
                    _ => home.clone(),
                },
                owned: match case {
                    "unclaimed" => vec!["grapt".into()],
                    _ => vec!["se".into()],
                },
            })
            .map_err(|error| error.to_string())?;
            match case {
                "missing" => {}
                "malformed" => std::fs::write(&record_path, b"{malformed").map_err(fail)?,
                _ => io::replace(&record_path, &bytes)?,
            }
            match case {
                "foreign-link" => {
                    std::os::unix::fs::symlink(root.0.join("foreign"), bin.join("se"))
                        .map_err(fail)?
                }
                "regular-file" => std::fs::write(bin.join("se"), b"foreign\n").map_err(fail)?,
                _ => std::os::unix::fs::symlink(&owned, bin.join("se")).map_err(fail)?,
            }
            let before = std::fs::read_link(bin.join("se")).ok();
            let record_before = std::fs::read(&record_path).ok();
            let refused = install(&fresh, true).is_err();
            assert!(refused, "case `{case}` was accepted");
            assert_eq!(
                std::fs::read_link(bin.join("se")).ok(),
                before,
                "case `{case}` moved the link"
            );
            assert_eq!(
                std::fs::read(&record_path).ok(),
                record_before,
                "case `{case}` rewrote the record"
            );
            assert!(
                std::fs::symlink_metadata(bin.join("grapt")).is_err(),
                "case `{case}` published"
            );
        }
        Ok(())
    }

    /// A fresh `grapt` prefix beside a pre-rename one must not refuse on the
    /// HOME-global `se` link, which only the old installation owns.
    #[test]
    fn a_legacy_installations_global_se_link_does_not_block_a_sibling_prefix() -> Result<(), String>
    {
        let fail = |error: std::io::Error| error.to_string();
        let root = Scratch::new()?;
        let (legacy, fresh) = (root.0.join("secode"), root.0.join("grapt")); // grapt-rename: keep
        std::fs::create_dir(&legacy).map_err(fail)?;
        std::fs::create_dir(&fresh).map_err(fail)?;
        let home = home(&fresh)?;
        let bin = home.join(".local/bin");
        super::super::commands::prefix_directory(&bin)?;
        std::fs::create_dir(legacy.join("bin")).map_err(fail)?;
        let owned = legacy.join("bin/se");
        std::os::unix::fs::symlink(&owned, bin.join("se")).map_err(fail)?;
        let record = serde_json::to_vec(&Aliases {
            schema: "secode.install.aliases.v1".into(),
            home: home.clone(),
            owned: vec!["se".into()],
        })
        .map_err(|error| error.to_string())?;
        io::replace(&legacy.join("launchers.json"), &record)?;
        install(&fresh, true)?;
        assert_eq!(std::fs::read_link(bin.join("se")).map_err(fail)?, owned);
        assert_eq!(aliases(&fresh, &home)?.owned, ["grapt"]);
        assert_eq!(legacy_prefix(&fresh), Some(legacy));
        Ok(())
    }

    #[test]
    fn reinstall_is_idempotent_and_uninstall_retains_borrowed_and_replaced_aliases(
    ) -> Result<(), String> {
        let root = Scratch::new()?;
        let home = home(&root.0)?;
        let bin = home.join(".local/bin");
        super::super::commands::prefix_directory(&bin)?;
        let target = root.0.join("current/bin/grapt");
        std::os::unix::fs::symlink(&target, bin.join("grapt"))
            .map_err(|error| error.to_string())?;
        install(&root.0, true)?;
        install(&root.0, true)?;
        assert_eq!(aliases(&root.0, &home)?.owned, ["se"]);
        std::fs::remove_file(bin.join("se")).map_err(|error| error.to_string())?;
        std::fs::write(bin.join("se"), b"user replacement").map_err(|error| error.to_string())?;
        uninstall(&root.0)?;
        assert!(link_matches(&bin.join("grapt"), &target)?);
        assert!(bin.join("se").is_file(), "user replacement was removed");
        assert_eq!(
            std::fs::read(bin.join("se")).map_err(|error| error.to_string())?,
            b"user replacement"
        );
        Ok(())
    }
    #[test]
    fn an_interrupted_alias_publication_recovers_and_only_owned_links_are_removed(
    ) -> Result<(), String> {
        let root = Scratch::new()?;
        install(&root.0, true)?;
        let bin = home(&root.0)?.join(".local/bin");
        std::fs::remove_file(bin.join("se")).map_err(|error| error.to_string())?;
        install(&root.0, true)?;
        uninstall(&root.0)?;
        for name in ["grapt", "se"] {
            assert!(std::fs::symlink_metadata(bin.join(name)).is_err());
        }
        uninstall(&root.0)?;
        Ok(())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod permission_tests {
    use super::*;
    #[test]
    fn writable_command_directory_refuses_before_alias_publication() -> Result<(), String> {
        use std::os::unix::fs::PermissionsExt;
        let root =
            std::env::temp_dir().join(format!("grapt-alias-permissions-{}", std::process::id()));
        std::fs::create_dir(&root).map_err(|error| error.to_string())?;
        let bin = home(&root)?.join(".local/bin");
        super::super::commands::prefix_directory(&bin)?;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o777))
            .map_err(|error| error.to_string())?;
        assert!(install(&root, true).is_err());
        assert!(std::fs::symlink_metadata(bin.join("grapt")).is_err());
        assert!(!root.join("launchers.json").exists());
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
        std::fs::remove_dir_all(root).map_err(|error| error.to_string())?;
        Ok(())
    }
}

#[cfg(all(test, target_os = "linux"))]
pub(super) fn interrupt_bundled_refresh(
    prefix: &Path,
    package: &super::package::Package,
) -> Result<(), String> {
    fixed::prepare_update_named(prefix, package, true, "se", "se-bootstrap.json")?
        .interrupt_after_launcher()
}
