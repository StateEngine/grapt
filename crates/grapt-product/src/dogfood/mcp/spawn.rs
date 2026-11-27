//! Starting a declared local MCP server as a child process.
//!
//! Split out of `mcp.rs` under the workspace god-file ceiling. This is the
//! whole of "how a configured command becomes a running child": program
//! resolution against the parent's PATH, the isolated environment the child
//! actually receives, and the configured entries layered over it.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;

/// A local server's process and the OS container that owns its whole tree.
///
/// A server is often a launcher (`npx`, `uvx`) whose real server is a
/// grandchild, so killing the direct child alone would leave it running. The
/// group is a Job Object on Windows and a cgroup or process group on Unix;
/// dropping this ends every descendant with the server.
pub(in crate::dogfood) struct LocalProcess {
    pub(in crate::dogfood) child: tokio::process::Child,
    group: processkit::ProcessGroup,
}

impl LocalProcess {
    /// Stop the whole tree now, ahead of the drop.
    pub(in crate::dogfood) fn kill_tree(&self) -> Result<(), String> {
        self.group
            .kill_all()
            .map_err(|error| format!("the local MCP server tree could not be stopped: {error}"))
    }
}

/// Spawn a declared local server.
///
/// The posture matches this crate's shell adapter deliberately: the
/// environment is CLEARED and repopulated from an explicit allowlist, stdio is
/// piped, and the whole process tree is owned by one OS container, so an
/// abandoned start cannot leave a process behind. An MCP server is configured code with the product's own
/// privileges; inheriting this process's environment would hand it every
/// secret in it.
/// Spawn a declared local server, returning the child AND the program that
/// was actually resolved and run.
///
/// The resolved program is returned rather than recomputed by callers: a bare
/// `npx` is resolved here against the parent's PATH, so the string in
/// configuration is not necessarily the file that ran, and an approval that
/// bound the configured string would be binding something that did not
/// execute.
pub(in crate::dogfood) fn spawn_local(
    command: &[String],
    cwd: &Path,
    environment: &BTreeMap<String, String>,
) -> Result<(LocalProcess, String), String> {
    let (program, arguments) = command
        .split_first()
        .ok_or_else(|| "a local MCP command needs a program".to_owned())?;
    // Resolve the program against the PARENT's PATH before spawning.
    //
    // Two things make a bare `npx` unstartable otherwise, and `npx` is the form
    // every MCP server's documentation uses. Rust's `Command` does not consult
    // PATHEXT on Windows, so `npx` never matches `npx.cmd`; and this adapter
    // deliberately clears the child environment, so there is no PATH inside it
    // to search either. Measured before this: a canonical
    // `["npx","-y","@modelcontextprotocol/server-everything"]` declaration
    // failed with "program not found" while the same server started from an
    // absolute path.
    //
    // Resolution reads the parent's PATH to CHOOSE the executable; it does not
    // hand the child an environment. The isolation below is unchanged.
    let resolved = resolve_program(program);
    let executed = resolved.as_deref().unwrap_or(program.as_ref());
    let resolved_text = executed.to_string_lossy().into_owned();
    let mut child = tokio::process::Command::new(executed);
    child
        .args(arguments)
        .current_dir(cwd)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    allow_server_environment(&mut child)?;
    // CONFIGURED ENTRIES LAYER OVER THE ISOLATED BASE, and the order matters.
    // `env_clear` above plus the single platform name below is this product's
    // isolation; these are applied AFTER it, so a declaration carries exactly
    // what it asked for and nothing ambient.
    //
    // This deliberately does NOT follow the oracle, whose `localEnvironment`
    // spreads the whole parent `process.env` under the configured entries. The
    // frozen behaviour hands every MCP server the product's entire
    // environment, credentials included; reproducing that would undo an
    // isolation this reconstruction already has and turn a configuration
    // feature into a credential leak. The configured overrides are the
    // compatible part and are what is carried.
    for (name, value) in environment {
        child.env(name, value);
    }
    // The PROGRAM is named. "program not found" without saying which program
    // is the difference between an operator fixing a typo in seconds and
    // reading their whole configuration looking for it — and with several
    // servers declared, it does not even say which one failed.
    let group = processkit::ProcessGroup::new().map_err(|error| {
        format!("the local MCP server {program:?} has no process containment: {error}")
    })?;
    let spawned = group.spawn(child).map_err(|error| {
        format!("the local MCP server {program:?} could not be started: {error}")
    })?;
    Ok((
        LocalProcess {
            child: spawned,
            group,
        },
        resolved_text,
    ))
}

/// Find `program` on the parent's PATH, honouring PATHEXT on Windows.
///
/// Returns `None` when the name already looks like a path, or when nothing
/// matches — in which case the spawn proceeds with the name as given so the
/// failure and its message stay exactly what they were.
pub(super) fn resolve_program(program: &str) -> Option<std::ffi::OsString> {
    if program.contains('/') || program.contains('\\') {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    let extensions: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| String::from(".COM;.EXE;.BAT;.CMD"))
            .split(';')
            .filter(|extension| !extension.is_empty())
            .map(str::to_owned)
            .collect()
    } else {
        Vec::new()
    };
    for directory in std::env::split_paths(&path) {
        // EXTENSIONS FIRST on Windows. Node ships an extensionless `npx` shell
        // script beside `npx.cmd`, and picking the bare name finds a file that
        // exists and cannot be executed — "%1 is not a valid Win32
        // application", which is a worse failure than not finding it at all
        // because it looks like a broken server rather than a launcher choice.
        for extension in &extensions {
            let candidate = directory.join(format!("{program}{extension}"));
            if candidate.is_file() {
                return Some(candidate.into_os_string());
            }
        }
        let candidate = directory.join(program);
        if candidate.is_file() {
            return Some(candidate.into_os_string());
        }
    }
    None
}

#[cfg(windows)]
fn allow_server_environment(command: &mut tokio::process::Command) -> Result<(), String> {
    // The same single name the shell adapter admits. Windows cannot start a
    // process without it; nothing else is inherited.
    let root = std::env::var_os("SystemRoot")
        .ok_or_else(|| "SystemRoot is unavailable for the MCP adapter".to_owned())?;
    command.env("SystemRoot", root);
    Ok(())
}

#[cfg(not(windows))]
fn allow_server_environment(_command: &mut tokio::process::Command) -> Result<(), String> {
    Ok(())
}
