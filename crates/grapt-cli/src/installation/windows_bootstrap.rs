//! The fixed Windows launcher selects immutable versions without moving them.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use grapt_service::installation::PrefixGuard;

use super::{windows_io as io, windows_pointer};

#[path = "fixed_launcher.rs"]
mod install;
#[cfg(test)]
pub(super) use install::prepare;
pub(super) use install::{prepare_update, prepare_update_named};

fn shim_prefix(binary: &Path) -> Option<&Path> {
    if !binary
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("grapt.exe"))
    {
        return None;
    }
    let bin = binary
        .parent()
        .filter(|bin| bin.file_name().is_some_and(|name| name == "bin"))?;
    let prefix = bin.parent()?;
    // Immutable version binaries remain owned by normal active-version
    // selection. The fixed shim is the prefix's bin, never a version's bin.
    if prefix
        .parent()
        .is_some_and(|parent| parent.file_name().is_some_and(|name| name == "versions"))
    {
        return None;
    }
    Some(prefix)
}

pub(super) fn installed_prefix(binary: &Path) -> Result<Option<&Path>, String> {
    let Some(prefix) = shim_prefix(binary) else {
        return Ok(None);
    };
    // An extracted package can also live in a directory named bin. Only an
    // installation marker makes that layout our fixed launcher.
    for name in ["bootstrap.json", "current", "state.json"] {
        match std::fs::symlink_metadata(prefix.join(name)) {
            Ok(_) => return Ok(Some(prefix)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "installation marker is unreadable: {:?}",
                    error.kind()
                ))
            }
        }
    }
    Ok(None)
}

// Resolve through only real existing ancestors before creating anything. The
// remaining suffix is bounded and contains only normal direct-disk components.
pub(super) fn creation_prefix(prefix: &Path, local: &Path) -> Result<PathBuf, String> {
    io::direct_path(prefix)?;
    io::directory(local)?;
    let local = std::fs::canonicalize(local)
        .map_err(|error| format!("user installation route is unreadable: {:?}", error.kind()))?;
    let mut ancestor = prefix.to_path_buf();
    let mut suffix = Vec::new();
    loop {
        match std::fs::symlink_metadata(&ancestor) {
            Ok(_) => {
                io::directory(&ancestor)?;
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                suffix.push(
                    ancestor
                        .file_name()
                        .ok_or("installation prefix has no direct disk ancestor")?
                        .to_os_string(),
                );
                if !ancestor.pop() {
                    return Err("installation prefix has no direct disk ancestor".into());
                }
            }
            Err(error) => {
                return Err(format!(
                    "installation prefix is unreadable: {:?}",
                    error.kind()
                ))
            }
        }
    }
    let mut resolved = std::fs::canonicalize(&ancestor)
        .map_err(|error| format!("installation prefix cannot be resolved: {:?}", error.kind()))?;
    for part in suffix.into_iter().rev() {
        resolved.push(part);
    }
    if resolved == local || !resolved.starts_with(&local) {
        return Err("installation prefix must be inside LOCALAPPDATA".into());
    }
    Ok(resolved)
}

fn contained(prefix: &Path, local: &Path) -> Result<(), String> {
    io::directory(local)?;
    io::directory(prefix)?;
    let local = std::fs::canonicalize(local)
        .map_err(|error| format!("user installation route is unreadable: {:?}", error.kind()))?;
    let prefix = std::fs::canonicalize(prefix)
        .map_err(|error| format!("installation prefix is unreadable: {:?}", error.kind()))?;
    if prefix == local || !prefix.starts_with(local) {
        return Err("installation prefix must be inside LOCALAPPDATA".into());
    }
    Ok(())
}

fn mutation(arguments: &[OsString]) -> bool {
    arguments
        .first()
        .is_some_and(|value| value == "update" || value == "revert" || value == "uninstall")
}

pub(super) async fn redirect(
    binary: &Path,
    local: &Path,
    arguments: &[OsString],
) -> Result<Option<i32>, String> {
    let Some(prefix) = shim_prefix(binary) else {
        return Ok(None);
    };
    contained(prefix, local)?;
    io::directory(binary.parent().ok_or("fixed shim has no directory")?)?;
    let guard = PrefixGuard::shared(prefix).map_err(|error| error.to_string())?;
    let identity = match windows_pointer::read(prefix)? {
        Some(identity) => identity,
        None if mutation(arguments) => return Ok(None),
        None => {
            return Err("installation has no active build; install a local package first".into())
        }
    };
    // A build from before the product rename ships its CLI under the legacy
    // name; use it only when the current one is genuinely absent.
    let version = prefix.join("versions").join(identity).join("bin");
    let selected = match std::fs::symlink_metadata(version.join("grapt.exe")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            version.join("secode.exe") // grapt-rename: keep
        }
        _ => version.join("grapt.exe"),
    };
    // Keep this physical file leased through spawn. The child will take its
    // own prefix guard; handles are not used as inherited lifetime authority.
    let binary_lease = io::regular(&selected)?;
    let mut command =
        tokio::process::Command::from(grapt_service::launcher_environment::command(&selected));
    let mut interrupt = tokio::signal::windows::ctrl_c()
        .map_err(|error| format!("interrupt listener cannot start: {:?}", error.kind()))?;
    let mut child = command
        .args(arguments)
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("active build cannot start: {:?}", error.kind()))?;
    drop(binary_lease);
    let retained = if mutation(arguments) {
        // The child must acquire prefix EX itself. Retaining the shim's SH
        // while waiting would refuse every update against its own parent.
        drop(guard);
        None
    } else {
        Some(guard)
    };
    let status = loop {
        tokio::select! {
            result = child.wait() => break result.map_err(|error| format!("active build wait failed: {:?}", error.kind()))?,
            _ = interrupt.recv() => {}
        }
    };
    drop(retained);
    Ok(Some(status.code().unwrap_or(2)))
}

// Command paths retain verbatim spelling when Windows needs it for I/O.
pub(super) fn display_path(path: &Path) -> String {
    crate::operator_text::windows_command_path(path)
}

pub(super) fn refresh_notice(
    prefix: &Path,
    source: &Path,
    age: crate::operator_text::LauncherAge,
    no_path: bool,
) -> Option<String> {
    crate::operator_text::launcher_refresh_notice(prefix, source, age, no_path)
}

#[cfg(test)]
mod tests {
    use super::{display_path, mutation, shim_prefix};
    use std::ffi::OsString;
    use std::path::Path;

    #[test]
    fn operator_paths_keep_verbatim_spelling_only_when_needed() {
        assert_eq!(
            display_path(Path::new(r"\\?\C:\Users\example\Grapt")),
            r"C:\Users\example\Grapt"
        );
        let long = format!(r"\\?\C:\{}", "x".repeat(257));
        for value in [
            r"\\?\UNC\server\share",
            r"\\?\C:\name.",
            r"\\?\C:\name ",
            r"\\?\C:\CON",
            long.as_str(),
        ] {
            assert_eq!(display_path(Path::new(value)), value);
        }
    }

    #[test]
    fn only_the_fixed_cli_layout_is_a_shim() {
        assert_eq!(
            shim_prefix(Path::new(
                r"C:\Users\example\AppData\Local\grapt\bin\grapt.exe"
            )),
            Some(Path::new(r"C:\Users\example\AppData\Local\grapt"))
        );
        for value in [
            r"C:\package\grapt.exe",
            r"C:\prefix\bin\other.exe",
            r"C:\prefix\versions\1111111111111111111111111111111111111111\bin\grapt.exe",
        ] {
            assert!(shim_prefix(Path::new(value)).is_none());
        }
    }

    #[test]
    fn only_installer_commands_release_the_parent_guard_after_spawn() {
        for arguments in [
            vec![OsString::from("update")],
            vec![OsString::from("update"), OsString::from("--prune")],
            vec![OsString::from("revert")],
        ] {
            assert!(mutation(&arguments));
        }
        for arguments in [
            vec![],
            vec![OsString::from("--version")],
            vec![OsString::from("code")],
            vec![OsString::from("run"), OsString::from("update")],
        ] {
            assert!(!mutation(&arguments));
        }
    }

    #[tokio::test]
    async fn refused_spawn_releases_the_shim_guard() -> Result<(), Box<dyn std::error::Error>> {
        let root = crate::testfs::ScratchDir::new("shim-spawn")?;
        let prefix = root.join("installation");
        let identity = "1111111111111111111111111111111111111111";
        let selected = prefix.join("versions").join(identity).join("bin");
        std::fs::create_dir_all(&selected)?;
        std::fs::create_dir(prefix.join("bin"))?;
        super::io::write_new(&selected.join("grapt.exe"), b"not an executable")?;
        super::windows_pointer::publish(&prefix, identity)?;
        let refused = super::redirect(
            &prefix.join("bin/grapt.exe"),
            &root,
            &[OsString::from("--version")],
        )
        .await;
        assert!(refused.is_err_and(|message| message.contains("active build cannot start")));
        let _guard = grapt_service::installation::PrefixGuard::exclusive(&prefix)?;
        Ok(())
    }
}
