//! What the SPAWNED CHILD actually receives: its directory and its environment.
//!
//! Split out of `bounds.rs` under the workspace god-file ceiling, and they
//! belong together for a reason beyond size: every witness here is observed AT
//! THE CHILD, by running a shell in it and reading what it printed. Asserting
//! that `spawn_local` was handed a map proves only that a test can read its own
//! argument.

use std::collections::BTreeMap;
use std::path::Path;
use tokio::io::AsyncReadExt as _;

use super::super::*;

/// Run a shell in the spawned child and return what it printed.
///
/// The child is the WITNESS. Asserting that `spawn_local` was handed a map
/// proves only that this test can read its own argument; asserting what the
/// process actually received proves the plumbing.
async fn child_output(
    command: &[String],
    cwd: &Path,
    environment: &BTreeMap<String, String>,
) -> String {
    let Ok((mut process, _resolved)) = spawn_local(command, cwd, environment) else {
        panic!("the child witness could not start its shell on this machine");
    };
    let child = &mut process.child;
    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut output).await;
    }
    let _ = child.wait().await;
    output
}

fn shell_printing(what: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![
            "cmd.exe".to_owned(),
            "/D".into(),
            "/C".into(),
            what.to_owned(),
        ]
    } else {
        vec!["/bin/sh".to_owned(), "-c".into(), what.to_owned()]
    }
}

/// A CONFIGURED ENVIRONMENT ENTRY REACHES THE CHILD.
///
/// The sentinel is deliberately NOT secret-shaped and its value is a constant
/// in this file: an MCP server's environment is exactly where an API token
/// lives, so a witness that printed a realistic-looking credential would put
/// one in a transcript to prove a point about not leaking them.
#[tokio::test]
async fn a_configured_environment_entry_reaches_the_spawned_child() {
    let mut environment = BTreeMap::new();
    environment.insert(
        "GRAPT_MCP_CONFIGURED_SENTINEL".to_owned(),
        "carried-by-configuration".to_owned(),
    );
    let printed = child_output(
        &shell_printing(if cfg!(windows) {
            "echo %GRAPT_MCP_CONFIGURED_SENTINEL%"
        } else {
            "echo $GRAPT_MCP_CONFIGURED_SENTINEL"
        }),
        Path::new("."),
        &environment,
    )
    .await;
    assert!(
        printed.contains("carried-by-configuration"),
        "the configured entry never reached the child: {printed:?}"
    );
}

/// CONFIGURED ENTRIES DO NOT REOPEN AMBIENT INHERITANCE.
///
/// The oracle's `localEnvironment` spreads the whole parent `process.env`
/// beneath the configured entries. This product does not, and this is the
/// witness that adding the configured layer did not quietly restore it: a
/// marker set in THIS process must still be absent from a child that was given
/// an unrelated configured entry.
#[tokio::test]
async fn a_configured_environment_does_not_restore_ambient_inheritance() {
    std::env::set_var("GRAPT_MCP_AMBIENT_PROBE", "must-not-be-inherited");
    let mut environment = BTreeMap::new();
    environment.insert("GRAPT_MCP_UNRELATED".to_owned(), "present".to_owned());
    let printed = child_output(
        &shell_printing(if cfg!(windows) { "set" } else { "env" }),
        Path::new("."),
        &environment,
    )
    .await;
    assert!(
        printed.contains("GRAPT_MCP_UNRELATED"),
        "the configured entry should be present: {printed:?}"
    );
    assert!(
        !printed.contains("must-not-be-inherited"),
        "the parent's environment leaked into the child: {printed:?}"
    );
}

/// THE DECLARED WORKING DIRECTORY IS WHERE THE CHILD ACTUALLY RUNS.
#[tokio::test]
async fn the_declared_working_directory_is_the_childs_actual_directory() {
    let root = std::env::temp_dir().join(format!("grapt-mcp-cwd-{}", std::process::id()));
    let nested = root.join("declared");
    if std::fs::create_dir_all(&nested).is_err() {
        panic!("the cwd witness could not create its directory");
    }
    let printed = child_output(
        &shell_printing(if cfg!(windows) { "cd" } else { "pwd" }),
        &nested,
        &BTreeMap::new(),
    )
    .await;
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        printed.to_lowercase().contains("declared"),
        "the child did not run in its declared directory: {printed:?}"
    );
}

#[tokio::test]
async fn a_spawned_server_inherits_no_environment_beyond_the_allowlist() {
    // Proven against the real spawn path rather than asserted: the child
    // prints its own environment, and the marker this process holds must
    // not appear in it.
    std::env::set_var("GRAPT_MCP_ENV_LEAK_PROBE", "leaked");
    let program = if cfg!(windows) { "cmd.exe" } else { "/bin/sh" };
    let command: Vec<String> = if cfg!(windows) {
        vec![program.to_owned(), "/D".into(), "/C".into(), "set".into()]
    } else {
        vec![program.to_owned(), "-c".into(), "env".into()]
    };
    let Ok((mut process, _resolved)) = spawn_local(&command, Path::new("."), &BTreeMap::new())
    else {
        // A machine without the shell cannot run this witness, and a
        // silent skip would be a false pass; say so loudly instead.
        panic!("the environment witness needs {program} on this machine");
    };
    let child = &mut process.child;
    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut output).await;
    }
    let _ = child.wait().await;
    std::env::remove_var("GRAPT_MCP_ENV_LEAK_PROBE");
    assert!(
        !output.contains("GRAPT_MCP_ENV_LEAK_PROBE"),
        "the spawned server inherited this process's environment"
    );
}

/// A launcher's grandchild is the real server; it must die with the server.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn dropping_a_local_server_ends_its_grandchild_too() {
    use tokio::io::AsyncBufReadExt as _;
    let command: Vec<String> = vec![
        "/bin/sh".to_owned(),
        "-c".into(),
        "sleep 300 & echo $!; wait".into(),
    ];
    let Ok((mut process, _resolved)) = spawn_local(&command, Path::new("."), &BTreeMap::new())
    else {
        panic!("the grandchild witness needs /bin/sh on this machine");
    };
    let Some(stdout) = process.child.stdout.take() else {
        panic!("the witness exposed no stdout");
    };
    let mut line = String::new();
    let _ = tokio::io::BufReader::new(stdout).read_line(&mut line).await;
    let Ok(pid) = line.trim().parse::<u32>() else {
        panic!("the witness printed no grandchild pid: {line:?}");
    };
    let alive = || {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            !stat
                .rsplit(')')
                .next()
                .is_some_and(|rest| rest.trim_start().starts_with('Z'))
        })
    };
    assert!(alive(), "the grandchild never started");
    drop(process);
    for _ in 0..200 {
        if !alive() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the grandchild outlived its dropped server");
}
