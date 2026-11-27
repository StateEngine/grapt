//! Recovery information for developer commands, never a command environment.
//!
//! Discovery reads executable metadata only. It does not run a probe, read
//! rustup settings or credentials, install tools, or alter the approved argv.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

const PATH_DIRS: usize = 32;
const TOOLCHAINS: usize = 32;
const CANDIDATES: usize = 2;
const PATH_BYTES: usize = 1024;
const ISSUE_COUNT: usize = 4;

#[derive(Default, Debug)]
pub(in crate::dogfood::tools) struct Installed {
    pub(in crate::dogfood::tools) rustc: Vec<PathBuf>,
    pub(in crate::dogfood::tools) cargo: Vec<PathBuf>,
    pub(in crate::dogfood::tools) sccache: Vec<PathBuf>,
    issues: Vec<String>,
}

impl Installed {
    pub(in crate::dogfood::tools) fn host() -> Self {
        Self::discover(|name| std::env::var_os(name))
    }

    pub(in crate::dogfood::tools) fn discover(
        mut variable: impl FnMut(&str) -> Option<OsString>,
    ) -> Self {
        let mut installed = Self::default();
        let mut directories = Vec::new();
        if let Some(path) = variable("PATH") {
            for (index, directory) in std::env::split_paths(&path).enumerate() {
                if index == PATH_DIRS {
                    installed.issue("PATH discovery stopped at 32 directories");
                    break;
                }
                if directory.is_absolute() && !directories.contains(&directory) {
                    directories.push(directory);
                }
            }
        }
        let home = variable(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)
            .filter(|path| path.is_absolute());
        if let Some(home) = &home {
            directories.extend([home.join(".cargo/bin"), home.join(".local/bin")]);
        }
        for directory in directories {
            installed.bin(&directory);
        }
        let mut roots = Vec::new();
        if let Some(root) = variable("RUSTUP_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
        {
            roots.push(root);
        }
        if let Some(home) = home {
            let root = home.join(".rustup");
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        for root in roots {
            installed.rustup(&root.join("toolchains"));
        }
        for paths in [
            &mut installed.rustc,
            &mut installed.cargo,
            &mut installed.sccache,
        ] {
            paths.sort();
            paths.dedup();
            paths.truncate(CANDIDATES);
        }
        installed
    }

    fn issue(&mut self, message: &str) {
        if self.issues.len() < ISSUE_COUNT && !self.issues.iter().any(|old| old == message) {
            self.issues.push(message.to_owned());
        }
    }

    fn executable(&mut self, path: &Path) -> Option<PathBuf> {
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => {
                self.issue(&format!(
                    "tool metadata could not be read ({:?})",
                    error.kind()
                ));
                return None;
            }
        };
        if !metadata.is_file() {
            return None;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                return None;
            }
        }
        match std::fs::canonicalize(path) {
            Ok(path)
                if path.to_str().is_some_and(|text| {
                    text.len() <= PATH_BYTES && !text.chars().any(char::is_control)
                }) =>
            {
                Some(path)
            }
            Ok(_) => {
                self.issue("a tool path is too long or cannot be displayed");
                None
            }
            Err(error) => {
                self.issue(&format!(
                    "a tool path could not be resolved ({:?})",
                    error.kind()
                ));
                None
            }
        }
    }

    fn bin(&mut self, directory: &Path) {
        // rustup uses symlinks on Unix and may use hard links on Windows.
        // Neither kind is a direct compiler that works without rustup's home.
        let shim_directory = match std::fs::metadata(directory.join(binary("rustup"))) {
            Ok(metadata) => metadata.is_file(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                self.issue(&format!(
                    "rustup shim metadata could not be read ({:?})",
                    error.kind()
                ));
                return;
            }
        };
        for name in ["rustc", "cargo", "sccache"] {
            if shim_directory && name != "sccache" {
                continue;
            }
            let Some(path) = self.executable(&directory.join(binary(name))) else {
                continue;
            };
            if path
                .file_name()
                .is_some_and(|file| file == binary("rustup").as_str())
            {
                continue;
            }
            let paths = match name {
                "rustc" => &mut self.rustc,
                "cargo" => &mut self.cargo,
                _ => &mut self.sccache,
            };
            // Keep the bounded lexically first candidates, independently of
            // directory iteration order; no executable is invoked to rank it.
            paths.push(path);
            paths.sort();
            paths.dedup();
            paths.truncate(CANDIDATES);
        }
    }

    fn rustup(&mut self, root: &Path) {
        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                self.issue(&format!(
                    "rustup toolchains could not be listed ({:?})",
                    error.kind()
                ));
                return;
            }
        };
        for (index, entry) in entries.enumerate() {
            if index == TOOLCHAINS {
                self.issue("rustup discovery stopped at 32 directory entries");
                break;
            }
            match entry {
                Ok(entry) => match entry.file_type() {
                    Ok(kind) if kind.is_dir() || kind.is_symlink() => {
                        self.bin(&entry.path().join("bin"));
                    }
                    Ok(_) => {}
                    Err(error) => self.issue(&format!(
                        "a rustup directory entry could not be inspected ({:?})",
                        error.kind()
                    )),
                },
                Err(error) => self.issue(&format!(
                    "a rustup directory entry could not be read ({:?})",
                    error.kind()
                )),
            }
        }
    }

    pub(in crate::dogfood::tools) fn guidance(&self) -> String {
        let mut text = String::from(
            "The command could not find a developer tool on PATH. Use its absolute path, \
             or set PATH explicitly for this command. Match the project's Rust version. \
             Discovered executable candidates (metadata only; quote paths for your shell):",
        );
        for (name, paths) in [
            ("rustc", &self.rustc),
            ("cargo", &self.cargo),
            ("sccache", &self.sccache),
        ] {
            if paths.is_empty() {
                text.push_str(&format!("\n{name}: not found in the checked locations"));
            } else {
                for path in paths {
                    let text_path = path.to_string_lossy();
                    // Canonical Windows paths carry a filesystem-only prefix;
                    // cmd executable syntax uses the ordinary DOS/UNC spelling.
                    let display = if cfg!(windows) {
                        text_path
                            .strip_prefix(r"\\?\UNC\")
                            .map(|tail| format!(r"\\{tail}"))
                            .or_else(|| text_path.strip_prefix(r"\\?\").map(str::to_owned))
                            .unwrap_or_else(|| text_path.into_owned())
                    } else {
                        text_path.into_owned()
                    };
                    text.push_str(&format!("\n{name}: {display}"));
                }
            }
        }
        for issue in &self.issues {
            text.push_str(&format!("\nDiscovery incomplete: {issue}."));
        }
        text.push_str(
            "\nNo tools were installed or executed by discovery. If needed, install the \
             required toolchain outside the agent command, or supply its explicit path.",
        );
        text
    }
}

fn binary(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

pub(super) fn failures(stdout: &[u8], stderr: &[u8]) -> Vec<String> {
    failures_with(stdout, stderr, Installed::host)
}

pub(in crate::dogfood::tools) fn failures_with(
    stdout: &[u8],
    stderr: &[u8],
    discover: impl FnOnce() -> Installed,
) -> Vec<String> {
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(stderr)
    );
    let missing_rust = ["rustc", "cargo", "sccache"].iter().any(|tool| {
        text.contains(&format!("{tool}: command not found"))
            || text.contains(&format!("{tool}: not found"))
            || text.contains(&format!("'{tool}' is not recognized"))
    });
    let missing_home = [
        "HOME: unbound variable",
        "HOME: parameter not set",
        "HOME not set",
        "KeyError: 'HOME'",
    ]
    .iter()
    .any(|message| text.contains(message));
    let mut notices = Vec::new();
    if missing_rust {
        notices.push(discover().guidance());
    }
    if missing_home {
        notices.push(
            "HOME is intentionally unset in the tool shell. For checks that require a home, \
             explicitly choose a dedicated workspace-local directory for this command; \
             the host's home and credentials are not supplied."
                .to_owned(),
        );
    }
    notices
}
