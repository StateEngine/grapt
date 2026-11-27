//! Dispatch the separately licensed runtime selected by the installation guard.
use grapt_service::installation::{prefix_of_binary, PrefixGuard};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::{io, package, transition};

pub async fn run(arguments: &[OsString]) -> Result<i32, String> {
    if arguments.len() > 256
        || arguments.iter().map(|value| value.len()).sum::<usize>() > 256 * 1024
    {
        return Err(
            "bundled command exceeds 256 arguments or 256 KiB; use its file or stdin input".into(),
        );
    }
    let binary = std::env::current_exe()
        .map_err(|error| format!("launcher cannot be located: {:?}", error.kind()))?;
    let (root, _guard) = selected(&binary)?;
    let entry = package::bundled_entry(&root)?;
    // Keep the selected physical entry leased until its process exits. The
    // selected guard prevents prune from retiring the runtime while it runs.
    let _entry = io::regular(&entry)?;
    #[cfg(target_os = "linux")]
    let mut interrupt =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
            .map_err(|error| format!("interrupt listener cannot start: {:?}", error.kind()))?;
    #[cfg(windows)]
    let mut interrupt = tokio::signal::windows::ctrl_c()
        .map_err(|error| format!("interrupt listener cannot start: {:?}", error.kind()))?;
    let mut child =
        tokio::process::Command::from(grapt_service::launcher_environment::command(entry))
            .args(arguments)
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("bundled StateEngine cannot start: {:?}", error.kind()))?;
    // Console interrupts reach the child directly in the inherited group.
    // Retain the guard while it handles the interrupt and exits.
    loop {
        tokio::select! {
            status = child.wait() => return status
                .map(|status| status.code().unwrap_or(130))
                .map_err(|error| format!("bundled StateEngine wait failed: {:?}", error.kind())),
            _ = interrupt.recv() => {}
        }
    }
}

fn fixed_prefix(binary: &Path) -> Result<Option<PathBuf>, String> {
    let name = if cfg!(windows) { "se.exe" } else { "se" };
    if binary.file_name().is_none_or(|actual| {
        if cfg!(windows) {
            !actual.to_string_lossy().eq_ignore_ascii_case(name)
        } else {
            actual != name
        }
    }) {
        return Ok(None);
    }
    let Some(bin) = binary
        .parent()
        .filter(|bin| bin.file_name().is_some_and(|name| name == "bin"))
    else {
        return Ok(None);
    };
    let prefix = bin
        .parent()
        .ok_or("bundled launcher has no installation prefix")?;
    match std::fs::symlink_metadata(prefix.join("se-bootstrap.json")) {
        Ok(_) => {
            io::read(&prefix.join("se-bootstrap.json"), 16 * 1024)?;
            Ok(Some(prefix.to_path_buf()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "bundled launcher ownership is unreadable: {:?}",
            error.kind()
        )),
    }
}

fn selected(binary: &Path) -> Result<(PathBuf, Option<PrefixGuard>), String> {
    let prefix = match fixed_prefix(binary)? {
        Some(prefix) => Some(prefix),
        None => prefix_of_binary(binary).map_err(|error| error.to_string())?,
    };
    let Some(prefix) = prefix else {
        return Ok((
            binary
                .parent()
                .ok_or("launcher has no package directory")?
                .to_path_buf(),
            None,
        ));
    };
    #[cfg(windows)]
    let prefix = {
        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or("LOCALAPPDATA is unknown")?;
        super::windows_bootstrap::creation_prefix(&prefix, &local)?
    };
    let guard = PrefixGuard::shared(&prefix).map_err(|error| error.to_string())?;
    let identity = transition::pointer(&prefix)?.ok_or("installation has no active build")?;
    Ok((
        prefix.join("versions").join(identity).join("bin"),
        Some(guard),
    ))
}

#[cfg(all(test, target_os = "linux"))]
pub(super) fn selected_root(binary: &Path) -> Result<PathBuf, String> {
    selected(binary).map(|(root, _guard)| root)
}
