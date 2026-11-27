//! Witnesses for local program resolution.

use super::spawn::resolve_program;

/// A name carrying a path separator is the caller's own choice and is left
/// exactly as given.
#[test]
fn an_explicit_path_is_not_searched() {
    assert!(resolve_program("C:/Windows/System32/cmd.exe").is_none());
    assert!(resolve_program("./local-server").is_none());
}

/// A name nothing matches resolves to nothing, so the spawn proceeds with
/// the name as given and the "program not found" message is unchanged.
#[test]
fn an_absent_program_resolves_to_nothing() {
    assert!(resolve_program("grapt-no-such-mcp-server").is_none());
}

/// On Windows the EXTENSION candidates must win over the bare name.
///
/// Node ships an extensionless `npx` shell script beside `npx.cmd`. Picking
/// the bare name finds a file that exists and cannot be executed, which
/// fails as "%1 is not a valid Win32 application" — a worse outcome than not
/// finding it, because it reads as a broken server rather than a launcher
/// choice. Measured: with the bare name first, a canonical
/// `["npx","-y","@modelcontextprotocol/server-everything"]` declaration
/// produced exactly that error.
#[cfg(windows)]
#[test]
fn a_windows_launcher_resolves_to_an_executable_extension() {
    let Some(resolved) = resolve_program("npx") else {
        // npx is not installed on this machine; nothing to assert.
        return;
    };
    let resolved = resolved.to_string_lossy().to_ascii_lowercase();
    assert!(
        resolved.ends_with(".cmd") || resolved.ends_with(".exe") || resolved.ends_with(".bat"),
        "npx must resolve to an executable extension, got {resolved}"
    );
}

/// Resolution finds a program that IS on PATH, which is the whole point.
#[test]
fn a_program_on_path_resolves() {
    let name = if cfg!(windows) { "cmd" } else { "sh" };
    assert!(
        resolve_program(name).is_some(),
        "{name} is expected on PATH"
    );
}
