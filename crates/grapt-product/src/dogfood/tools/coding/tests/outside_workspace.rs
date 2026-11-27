//! Where the coding tools root their workspace: an outside path refuses with
//! its root and the routes that would make the path inside it.

use super::*;

/// J03B: `run --new` places the session in the caller's directory, and the
/// TUI sessions root at the selected project's worktree (no directory
/// chooser), so a path outside the workspace is a start-here-or-root-here
/// gap, and the refusal must name the root and both routes instead of a bare fact.
#[test]
fn an_outside_path_refusal_names_the_root_and_its_routes() -> Result<(), String> {
    let scratch = Scratch::new("outside-workspace")?;
    let root = &scratch.0;
    let outside = Scratch::new("outside-workspace-target")?;
    let target = outside.0.join("note.txt");
    let refused = call(
        root,
        WRITE_TOOL,
        serde_json::json!({"path": target.to_string_lossy(), "content": "bad"}),
    )
    .err()
    .ok_or("a write reached outside the workspace")?;
    assert!(
        refused.contains(&format!(
            "coding path is outside the workspace rooted at {}",
            crate::dogfood::path_display::failure_text(&root.to_string_lossy())
        )),
        "{refused}"
    );
    assert!(
        refused.contains("run `grapt run --new` from a directory that contains the path"),
        "{refused}"
    );
    assert!(
        refused.contains("open the project containing it in the TUI and start a new session there"),
        "{refused}"
    );
    assert!(refused.contains("shell/exec_command with ssh for remote worktrees"));
    assert!(!refused.contains(r"\\?\"));
    assert!(!target.exists(), "write leaked outside the workspace");
    Ok(())
}
