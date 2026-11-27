use super::*;
use crate::bounds::Bounds;
use crate::projection::SessionView;

fn tool() -> DurableTool {
    DurableTool {
        duration_ms: None,
        id: crate::wire::tool_call_id("call_plan_update_001").unwrap_or_else(|e| panic!("{e}")),
        name: "update_plan".into(),
        state: DurableToolState::Completed {
            output: "Plan updated".into(),
            input: serde_json::json!({"explanation":"The failing case is understood", "plan":[
                {"step":"Reproduce","status":"completed"},
                {"step":"Fix it","status":"in_progress"},
                {"step":"Run tests","status":"pending"}
            ]})
            .to_string(),
        },
        progress: Vec::new(),
    }
}

#[test]
fn completed_plan_shows_explanation_and_each_checkbox() {
    assert_eq!(present(&tool()).as_deref(),Some("Plan updated · 1/3 complete\n  The failing case is understood\n  [x] Reproduce\n  [>] Fix it\n  [ ] Run tests"));
}

#[test]
fn durable_relist_replaces_the_same_tool_and_restart_rebuilds_the_plan() {
    let session = crate::wire::session_id("ses_plan_update_001").unwrap_or_else(|e| panic!("{e}"));
    let mut view = SessionView::new(&session);
    let completed = tool();
    let mut running = completed.clone();
    running.state = DurableToolState::Running {
        input: r#"{"plan":[]}"#.into(),
    };
    view.push_tool(&running, Bounds::PRODUCTION);
    assert!(!view.slices[0].closed);
    assert!(!view.slices[0].text.contains("Plan updated"));
    view.push_tool(&completed, Bounds::PRODUCTION);
    view.push_tool(&completed, Bounds::PRODUCTION);
    view.push_tool(&running, Bounds::PRODUCTION);
    assert_eq!(view.slices.len(), 1);
    assert!(view.slices[0].closed);
    assert!(view.slices[0].text.contains("[>] Fix it"));
    assert!(view.slices[0].read_detail_start.is_none());
    let mut restarted = SessionView::new(&session);
    restarted.push_tool(&completed, Bounds::PRODUCTION);
    assert_eq!(view.slices, restarted.slices);
}

#[test]
fn malformed_running_and_unrelated_tools_keep_the_generic_presentation() {
    let mut tool = tool();
    tool.state = DurableToolState::Completed {
        output: "Plan updated".into(),
        input: r#"{"plan":[{"step":"misleading","status":"done"}]}"#.into(),
    };
    assert!(present(&tool).is_none());
    tool.state = DurableToolState::Running { input: "{}".into() };
    assert!(present(&tool).is_none());
    tool.name = "plugin_update_plan".into();
    assert!(present(&tool).is_none());
}

#[test]
fn a_clear_and_terminal_control_text_are_rendered_honestly() {
    let mut tool = tool();
    tool.state = DurableToolState::Completed {
        output: "Plan updated".into(),
        input: r#"{"plan":[]}"#.into(),
    };
    assert_eq!(present(&tool).as_deref(), Some("Plan cleared"));
    tool.state = DurableToolState::Completed {
        output: "Plan updated".into(),
        input: serde_json::json!({"plan":[{"step":"a\n\u{1b}[2Jb","status":"pending"}]})
            .to_string(),
    };
    let shown = present(&tool).unwrap_or_else(|| panic!("valid plan not rendered"));
    assert!(!shown.contains('\u{1b}'));
    assert_eq!(shown.lines().count(), 2);
}
