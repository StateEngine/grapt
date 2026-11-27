//! Compact tool metadata; retained detail and canonical wire output stay unchanged.
//! Disclosure for retained tool detail.
//! Copyright 2025 OpenAI. Apache-2.0; see licenses/openai-codex/
use crate::wire::{DurableTool, DurableToolState};
use serde_json::Value;
mod preview;
mod terminal;
mod titles;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdinAction {
    Wait,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSummary {
    pub terminal_session: Option<grapt_types::OperationId>,
    pub stdin: Option<StdinAction>,
    pub title: String,
    pub duration_ms: Option<u64>,
    pub preview: Vec<String>,
    pub omitted_lines: usize,
    pub error: Option<String>,
    pub name: String,
    pub argument: String,
    pub result: String,
}

pub(super) fn summarize(tool: &DurableTool) -> ToolSummary {
    let (input, output, error) = match &tool.state {
        DurableToolState::Streaming { input } | DurableToolState::Running { input } => {
            (input, None, None)
        }
        DurableToolState::Completed { input, output } => (input, Some(output.as_str()), None),
        DurableToolState::Error {
            input,
            output,
            error,
        } => (input, output.as_deref(), Some(error)),
    };
    let input: Option<Value> = serde_json::from_str(input).ok();
    let argument = input
        .as_ref()
        .and_then(|input| {
            [
                "command",
                "cmd",
                "path",
                "file_path",
                "filePath",
                "pattern",
                "query",
                "url",
                "title",
                "code",
            ]
            .iter()
            .find_map(|key| input.get(key).and_then(Value::as_str))
        })
        .unwrap_or_default();
    let mut result = if let Some(error) = error {
        compact(&error.kind, 48)
    } else {
        output.map(result_summary).unwrap_or_default()
    };
    if matches!(
        tool.name.as_str(),
        "shell" | "exec_command" | "write_stdin" | "request_sandbox_access"
    ) {
        if let Some(label) =
            enforcement(output).or_else(|| enforcement(error.map(|error| error.message.as_str())))
        {
            if !result.is_empty() {
                result.push_str(" · ");
            }
            result.push_str(&label);
        }
    }
    let title = titles::title(tool, input.as_ref(), argument);
    let (preview, omitted_lines) = output
        .map(|output| preview::build(tool, output))
        .unwrap_or_default();
    ToolSummary {
        terminal_session: terminal::session(tool, input.as_ref(), output),
        stdin: terminal::action(tool, input.as_ref()),
        title,
        duration_ms: tool.duration_ms,
        preview,
        omitted_lines,
        error: error.map(|error| compact(error.message.lines().next().unwrap_or_default(), 512)),
        name: compact(&tool.name, 80),
        argument: compact(argument, 160),
        result,
    }
}

pub(super) fn for_view(tool: &DurableTool, slices: &[super::Slice]) -> ToolSummary {
    let mut summary = summarize(tool);
    terminal::resolve_title(&mut summary, slices);
    summary
}

pub(super) fn output_text(value: &Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value.get("output").and_then(Value::as_str))
}

fn result_summary(output: &str) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(output) {
        if let Some(code) = value
            .get("exit_code")
            .or_else(|| value.get("exitCode"))
            .and_then(Value::as_i64)
        {
            return format!("exit {code}");
        }
        if let Some(lines) = value.get("lines").and_then(Value::as_array) {
            return counted(lines.len(), "line", "lines");
        }
        if let Some(matches) = value.get("matches") {
            if let Some(count) = matches
                .as_u64()
                .or_else(|| matches.as_array().map(|items| items.len() as u64))
            {
                return if count == 1 {
                    "1 match".into()
                } else {
                    format!("{count} matches")
                };
            }
        }
        if let Some(output) = output_text(&value) {
            return counted(output.lines().count(), "output line", "output lines");
        }
    }
    counted(output.lines().count(), "output line", "output lines")
}

fn counted(count: usize, singular: &str, plural: &str) -> String {
    format!("{count} {}", if count == 1 { singular } else { plural })
}

pub(super) fn compact(text: &str, limit: usize) -> String {
    let mut chars = text.chars();
    let mut shown: String = chars
        .by_ref()
        .take(limit)
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    if chars.next().is_some() {
        shown.push('…');
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn summaries_keep_the_key_argument_and_result_without_copying_output_or_ids(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut tool = DurableTool {
            duration_ms: None,
            id: crate::wire::tool_call_id("call_hidden_correlation")?,
            name: "shell".into(),
            state: DurableToolState::Completed {
                input: r#"{"command":"se --help","cwd":"/tmp"}"#.into(),
                output: (0..113).map(|i| format!("HELP_BODY_{i}\n")).collect(),
            },
            progress: Vec::new(),
        };
        let summary = summarize(&tool);
        assert_eq!(
            summary,
            ToolSummary {
                terminal_session: None,
                stdin: None,
                title: "Ran se".into(),
                duration_ms: None,
                preview: (0..3).map(|i| format!("HELP_BODY_{i}")).collect(),
                omitted_lines: 110,
                error: None,
                name: "shell".into(),
                argument: "se --help".into(),
                result: "113 output lines".into()
            }
        );
        tool.state = DurableToolState::Completed {
            input: r#"{"path":"AGENTS.md"}"#.into(),
            output: r#"{"lines":[{"line":1,"text":"private body"}]}"#.into(),
        };
        assert_eq!(summarize(&tool).result, "1 line");
        tool.state = DurableToolState::Completed {
            input: "{}".into(),
            output: r#"{"exit_code":7}"#.into(),
        };
        assert_eq!(summarize(&tool).result, "exit 7");
        tool.state = DurableToolState::Completed {
            input: r#"{"pattern":"needle"}"#.into(),
            output: r#"{"matches":[]}"#.into(),
        };
        assert_eq!(summarize(&tool).result, "0 matches");
        Ok(())
    }
    #[test]
    fn display_metadata_is_bounded_and_controls_cannot_insert_transcript_rows() {
        let raw = "中\n\x1b[31m".repeat(1000);
        let shown = compact(&raw, 160);
        assert!(shown.chars().count() <= 161 && !shown.chars().any(char::is_control));
    }
    #[test]
    fn failure_summary_keeps_only_the_bounded_first_error_line(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut tool = DurableTool {
            duration_ms: None,
            id: crate::wire::tool_call_id("call_failed")?,
            name: "shell".into(),
            state: DurableToolState::Error {
                input: "{}".into(),
                output: None,
                error: crate::wire::DurableError {
                    kind: "failed".into(),
                    message: format!("{}\nPRIVATE_STACK", "中".repeat(1000)),
                },
            },
            progress: vec![],
        };
        let summary = summarize(&tool);
        assert!(summary
            .error
            .as_ref()
            .is_some_and(|error| error.chars().count() == 513 && !error.contains("PRIVATE_STACK")));
        if let DurableToolState::Error { error, .. } = &mut tool.state {
            error.message = "Permission denied\nPRIVATE_STACK".into();
        }
        assert_eq!(summarize(&tool).error.as_deref(), Some("Permission denied"));
        Ok(())
    }
    #[test]
    fn human_titles_and_json_output_previews_preserve_the_raw_result(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let output = serde_json::json!({"output":"\u{1b}[31mfirst\u{1b}[0m\nsecond\nthird\nfourth", "exit_code":0}).to_string();
        let mut tool = DurableTool { id: crate::wire::tool_call_id("call_preview")?, name: "exec_command".into(), duration_ms: Some(850), progress: vec![],
            state: DurableToolState::Completed { input: r#"{"description":"Check the current build and review status","cmd":"ssh build-host status"}"#.into(), output: output.clone() } };
        let summary = summarize(&tool);
        assert_eq!(summary.title, "Check the current build and review status");
        assert_eq!(summary.duration_ms, Some(850));
        assert_eq!(summary.preview, ["first", "second", "third"]);
        assert_eq!(summary.omitted_lines, 1);
        assert!(
            matches!(&tool.state, DurableToolState::Completed { output: retained, .. } if retained == &output)
        );
        for (name, input, title) in [
            ("exec_command", r#"{"cmd":"ssh build-host status"}"#, "Ran ssh"),
            ("read_lines", r#"{"path":"src/main.rs"}"#, "Read src/main.rs"),
            ("grep", r#"{"pattern":"footer","path":"src"}"#, "Searched footer in src"),
            ("apply_patch", "{\"patchText\":\"*** Begin Patch\\n*** Update File: src/main.rs\\n*** End Patch\"}", "Patched src/main.rs"),
        ] {
            tool.name = name.into();
            if let DurableToolState::Completed { input: retained, .. } = &mut tool.state { *retained = input.into(); }
            assert_eq!(summarize(&tool).title, title);
        }
        Ok(())
    }

    #[test]
    fn numbered_read_previews_keep_positions_and_count_hidden_lines(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let tool = DurableTool { id: crate::wire::tool_call_id("call_read_preview")?, name: "read_lines".into(), duration_ms: None, progress: vec![],
            state: DurableToolState::Completed { input: r#"{"path":"src/main.rs"}"#.into(), output: serde_json::json!({"totalLines":4,"complete":true,"nextLine":null,"lines":[{"line":1,"text":"one"},{"line":2,"text":"two"},{"line":3,"text":"three"},{"line":4,"text":"four"}]}).to_string() } };
        let summary = summarize(&tool);
        assert_eq!(summary.preview, ["1: one", "2: two", "3: three"]);
        assert_eq!(summary.omitted_lines, 1);
        Ok(())
    }
    #[test]
    fn edit_previews_reuse_the_existing_diff_instead_of_showing_receipt_json(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let tool = DurableTool {
            id: crate::wire::tool_call_id("call_edit_preview")?,
            name: "edit".into(),
            duration_ms: Some(100),
            progress: vec![],
            state: DurableToolState::Completed {
                input:
                    serde_json::json!({"path":"src/main.rs","old_text":"old\n","new_text":"new\n"})
                        .to_string(),
                output: "{}".into(),
            },
        };
        let summary = summarize(&tool);
        assert_eq!(summary.title, "Edited src/main.rs");
        assert!(summary.preview.iter().any(|line| line.contains("-old")));
        assert!(summary.preview.iter().any(|line| line.contains("+new")));
        assert!(summary.preview.len() <= 3);
        Ok(())
    }
}

#[cfg(test)]
mod terminal_tests;

fn enforcement(raw: Option<&str>) -> Option<String> {
    let value: Value = serde_json::from_str(raw?).ok()?;
    let enforcement = value.get("enforcement")?;
    match enforcement.get("kind")?.as_str()? {
        "platform_sandbox" => Some("sandboxed".into()),
        "degraded" => Some(format!(
            "degraded: {}",
            compact(enforcement.get("reason")?.as_str()?, 160)
        )),
        _ => None,
    }
}
