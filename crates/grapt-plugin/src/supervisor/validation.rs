use super::*;

pub(super) fn child_path(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    raw.strip_prefix(r"\\?\")
        .map_or_else(|| path.to_path_buf(), PathBuf::from)
}

#[cfg(target_os = "linux")]
pub(super) trait CommandEnvironment {
    fn spawn_with_required_environment(&mut self) -> std::io::Result<Child>;
}

pub(super) fn required_environment() -> Vec<(&'static str, std::ffi::OsString)> {
    [
        "SystemRoot",
        "WINDIR",
        "TEMP",
        "TMP",
        "TMPDIR",
        "LANG",
        "LC_ALL",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
    ]
    .into_iter()
    .filter_map(|name| std::env::var_os(name).map(|value| (name, value)))
    .collect()
}

#[cfg(target_os = "linux")]
impl CommandEnvironment for Command {
    fn spawn_with_required_environment(&mut self) -> std::io::Result<Child> {
        // The bridge never inherits the service's credential-bearing environment.
        // These variables are the minimum platform/runtime plumbing needed by an
        // absolute Node executable and temporary-file APIs; absent values remain
        // absent instead of being synthesized.
        for (name, value) in required_environment() {
            self.env(name, value);
        }
        self.spawn()
    }
}
