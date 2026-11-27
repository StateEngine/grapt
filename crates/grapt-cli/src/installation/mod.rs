//! Local package installation; service election and store writes stay elsewhere.

#[cfg(any(target_os = "linux", windows, test))]
mod bounds;

#[cfg(any(target_os = "linux", windows))]
mod bundled;
#[cfg(any(target_os = "linux", windows))]
mod commands;
#[cfg(any(target_os = "linux", windows))]
mod inventory;
#[cfg(target_os = "linux")]
mod io;
#[cfg(any(target_os = "linux", windows))]
pub(crate) mod launchers;
#[cfg(any(target_os = "linux", windows))]
mod package;
#[cfg(any(target_os = "linux", windows))]
mod pre_rename;
#[cfg(any(target_os = "linux", windows))]
mod progress;
#[cfg(target_os = "linux")]
mod snapshot;
#[cfg(any(target_os = "linux", windows))]
mod stage;
#[cfg(any(target_os = "linux", windows))]
mod start;
#[cfg(any(target_os = "linux", windows))]
mod transition;
#[cfg(any(windows, all(test, target_os = "linux")))]
mod user_path;

#[cfg(not(windows))]
pub const HELP: &str = "usage: grapt update --from <package-directory> [--prefix <installation>]\n       grapt update --prune [--prefix <installation>]\n       grapt revert [--prefix <installation>] [--profile <profile>]\n       grapt uninstall [--prefix <installation>]\n\nInstall a verified local Linux package or return to the previous build.\nQuit grapt code and stop services from this installation first.\nPrune explicitly retires old inactive packages, keeping current and three others including the revert target.\nUninstall removes owned command exposure, retaining installed versions and profiles.\nNo download, signing service or profile migration occurs.\nThe default installation is $XDG_DATA_HOME/grapt or ~/.local/share/grapt.";

#[cfg(windows)]
pub const HELP: &str = "usage: grapt update --from <package-directory> [--prefix <installation>] [--refresh-launcher] [--no-path]\n       grapt update --prune [--prefix <installation>]\n       grapt revert [--prefix <installation>] [--profile <profile>] [--no-path]\n       grapt uninstall [--prefix <installation>] [--no-path]\n\nInstall a verified local Windows package or return to the previous build.\nQuit grapt code and stop services from this installation first.\nPrune explicitly retires old inactive packages, keeping current and three others including the revert target.\nUninstall removes owned command exposure, retaining installed versions and profiles.\nNo download, signing service or profile migration occurs.\nUse --refresh-launcher from an extracted package after quitting installed apps to update the fixed launcher.\nUse --no-path for a portable install that leaves USER Path unchanged; pass it again on update --from, revert and uninstall (not update --prune).\nThe default installation is %LOCALAPPDATA%\\grapt; custom prefixes must stay inside LOCALAPPDATA.";

/// A path as an installer message shows it. On Windows the resolved prefix
/// is verbatim (`\\?\C:\...`); the user sees the ordinary spelling unless
/// Windows needs the verbatim one to reach that path.
#[cfg(any(target_os = "linux", windows, test))]
fn display_path(path: &std::path::Path) -> String {
    #[cfg(windows)]
    {
        windows_bootstrap::display_path(path)
    }
    #[cfg(not(windows))]
    {
        path.display().to_string()
    }
}

#[cfg(windows)]
pub async fn windows_redirect(arguments: &[std::ffi::OsString]) -> Result<Option<i32>, String> {
    let binary = std::env::current_exe()
        .map_err(|error| format!("installation binary cannot be located: {:?}", error.kind()))?;
    if windows_bootstrap::installed_prefix(&binary)?.is_none() {
        return Ok(None);
    }
    let local = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .ok_or("LOCALAPPDATA is unknown; the installed launcher cannot select a build")?;
    windows_bootstrap::redirect(&binary, &local, arguments).await
}

pub async fn run(arguments: &[String]) -> Result<String, String> {
    if arguments
        .first()
        .is_some_and(|value| value == "recover-sign-in")
    {
        return recovery::run(&arguments[1..]).await;
    }
    if arguments
        .get(1)
        .is_some_and(|argument| argument == "--help" || argument == "-h")
    {
        return Ok(HELP.into());
    }
    #[cfg(any(target_os = "linux", windows))]
    {
        commands::run(arguments).await
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        Err("local update and revert are available on Linux and Windows; this platform's launcher is not implemented yet".into())
    }
}

mod recovery;

/// Called before dispatch, covering active-version selection through child startup.
/// The product also takes its own guard, independent of its CLI parent's life.
#[cfg(any(target_os = "linux", windows))]
pub use start::prepare;

/// Shared durable namespace publication for per-user records.
pub(crate) fn sync_directory(path: &std::path::Path) -> Result<(), String> {
    #[cfg(any(target_os = "linux", windows))]
    {
        io::sync_directory(path)
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("directory publication failed: {:?}", error.kind()))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err("durable directory publication is not supported on this platform".into())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests;

#[cfg(any(target_os = "linux", windows))]
mod prune;
#[cfg(any(target_os = "linux", windows))]
mod prune_policy;
#[cfg(any(target_os = "linux", windows))]
mod prune_tree;
#[cfg(any(windows, all(test, target_os = "linux")))]
mod windows_pointer;

#[cfg(any(windows, test))]
mod acl;
#[cfg(any(windows, test))]
mod acl_batch;
#[cfg(any(windows, test))]
mod acl_objects;
#[cfg(windows)]
mod windows_acl;
#[cfg(windows)]
mod windows_io;

#[cfg(windows)]
mod windows_bootstrap;

#[cfg(windows)]
mod windows_reader;
#[cfg(windows)]
mod windows_signing;
#[cfg(windows)]
mod windows_snapshot;

#[cfg(windows)]
use windows_io as io;
#[cfg(windows)]
use windows_snapshot as snapshot;
#[cfg(all(test, windows))]
mod windows_stage_tests;

#[cfg(any(target_os = "linux", windows))]
pub use bundled::run as bundled_stateengine;
#[cfg(windows)]
pub use user_path::worker as user_path_worker;
