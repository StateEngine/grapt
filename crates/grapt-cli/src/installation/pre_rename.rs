//! The installation from before the product rename: where it sits, and which
//! HOME-global command aliases it still owns. Upgrades across the rename install
//! a fresh prefix and never touch the old one, so both answers live here once.

/// The installation directory name from before the product rename.
pub(super) const PREFIX_NAME: &str = "secode"; // grapt-rename: keep

/// The pre-rename installation beside a `grapt` prefix, if one sits there. Only
/// a `grapt` prefix has a legacy sibling; that is true on every platform.
pub(super) fn sibling(prefix: &std::path::Path) -> Option<std::path::PathBuf> {
    if prefix.file_name()? != "grapt" {
        return None;
    }
    Some(prefix.parent()?.join(PREFIX_NAME))
}

/// Whether the HOME-global alias at `path` belongs to the pre-rename
/// installation: its record claims `se` and the link still points at its own
/// binary. `~/.local/bin/se` is global, so another prefix cannot resolve the
/// collision, and publishing over it would take over the old command.
#[cfg(target_os = "linux")]
pub(super) fn owns_se(path: &std::path::Path, legacy: &std::path::Path, owned: &[String]) -> bool {
    owned.iter().any(|name| name == "se")
        && std::fs::read_link(path).is_ok_and(|target| target == legacy.join("bin/se"))
}
