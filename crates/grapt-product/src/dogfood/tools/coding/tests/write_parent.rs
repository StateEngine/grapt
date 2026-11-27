//! Where a write may create its file: a missing parent points to the shell,
//! any other parent failure keeps its real cause.

use super::*;

/// Read failures point to existing entries; write failures still name the missing parent.
#[test]
fn a_missing_read_points_to_its_parent_and_a_missing_write_parent_to_the_shell(
) -> Result<(), String> {
    let scratch = Scratch::new("missing-file")?;
    let root = &scratch.0;
    let read = call(
        root,
        READ_LINES_TOOL,
        serde_json::json!({"path":"DIAGNOSIS.md"}),
    )
    .err()
    .ok_or("a missing file was read")?;
    assert!(
        read.contains("DIAGNOSIS.md not found; nearest existing parent: .")
            && !read.contains("use write"),
        "{read}"
    );
    let write = call(
        root,
        WRITE_TOOL,
        serde_json::json!({"path":"docs/DIAGNOSIS.md","content":"x"}),
    )
    .err()
    .ok_or("a file was written under a missing directory")?;
    assert!(
        write.contains("write parent directory docs does not exist"),
        "{write}"
    );
    Ok(())
}

/// Finish-work review HOLD: an unreadable parent is not a missing one, so it
/// must keep its real cause instead of sending the model to the shell.
#[cfg(unix)]
#[test]
fn an_unreadable_write_parent_keeps_its_cause_unlike_a_missing_one() -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new("unreadable-parent")?;
    let root = &scratch.0;
    let docs = root.join("docs");
    std::fs::create_dir_all(docs.join("hidden")).map_err(|error| error.to_string())?;
    let missing = call(
        root,
        WRITE_TOOL,
        serde_json::json!({"path":"docs/absent/NOTE.md","content":"x"}),
    )
    .err()
    .ok_or("a file was written under a missing directory")?;
    assert!(
        missing.contains("write parent directory docs/absent does not exist; create it first"),
        "{missing}"
    );
    std::fs::set_permissions(&docs, std::fs::Permissions::from_mode(0o000))
        .map_err(|error| error.to_string())?;
    // The counterexample needs a real PermissionDenied (a privileged runner
    // reads through mode 000), so its absence fails rather than passes, and
    // is checked before the write it would otherwise let through.
    let probe = std::fs::symlink_metadata(docs.join("hidden")).map(|_| ());
    let denied = match &probe {
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Some(call(
            root,
            WRITE_TOOL,
            serde_json::json!({"path":"docs/hidden/NOTE.md","content":"x"}),
        )),
        _ => None,
    };
    std::fs::set_permissions(&docs, std::fs::Permissions::from_mode(0o755))
        .map_err(|error| error.to_string())?;
    let denied = denied
        .ok_or_else(|| {
            format!(
                "precondition unavailable: mode 000 parent was not PermissionDenied ({probe:?})"
            )
        })?
        .err()
        .ok_or("a file was written under an unreadable directory")?;
    assert!(
        denied.contains("docs/hidden is unreadable") && !denied.contains("does not exist"),
        "{denied}"
    );
    assert!(!docs.join("hidden/NOTE.md").exists(), "write leaked");
    Ok(())
}
