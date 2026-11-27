use grapt_provider::ToolDefinition;

/// Names reserved by the built-in catalog against configured tool shadowing.
pub(in super::super) const BUILTIN_TOOL_NAMES: &[&str] = &[
    super::READ_TOOL,
    super::EDIT_TOOL,
    super::SHELL_TOOL,
    super::terminal::EXEC_COMMAND,
    super::sandbox_access::TOOL,
    super::terminal::WRITE_STDIN,
    super::SUBAGENT_TOOL,
    super::agents::LIST_AGENTS_TOOL,
    super::skill::SKILL_TOOL,
    super::HISTORY_TOOL,
    super::read_history::TOOL,
    super::WRITE_TOOL,
    super::GLOB_TOOL,
    super::GREP_TOOL,
    super::READ_LINES_TOOL,
    super::LIST_TOOL,
    super::PATCH_TOOL,
    super::APPLY_PATCH_TOOL,
    super::web::FETCH_TOOL,
    super::image::TOOL,
    super::agent_interrupt::INTERRUPT_AGENT_TOOL,
    super::agent_spawn::SPAWN_AGENT_TOOL,
    super::agent_wait::WAIT_AGENT_TOOL,
    super::PLAN_TOOL,
    super::ask::TOOL,
    super::agent_message::SEND_MESSAGE_TOOL,
    super::agent_followup::FOLLOWUP_TASK_TOOL,
];

use super::{
    EditArguments, HistoryArguments, ReadArguments, ShellArguments, SubagentArguments,
    ToolArguments,
};

pub(super) const READ_DESCRIPTION: &str = "Read numbered UTF-8; prefixes are not content. offset is 1-based; limit=2000 lines. Pages end near 50 KB; long lines truncate. Continue at returned offset. Scans ≤ 16 MiB; files ≤ 1 MiB include patch SHA-256.";
pub(super) const EDIT_DESCRIPTION: &str = "Replace unique old_text with new_text; replace_all=true changes every match. Missing/ambiguous anchors refuse with a count.";
pub(super) const SHELL_DESCRIPTION: &str =
    "Run one workspace command with empty stdin and bounded timeout; returns stdout/stderr even on timeout. Stops child/background processes at return; [shell: ...] reports uncertain cleanup.";
#[cfg(windows)]
pub(super) const SHELL_HOST_DESCRIPTION: &str = "Host: cmd.exe /D /S /C; use double-quoted executable paths and &&. Inherits the parent environment under shell_environment_policy, with credentials removed. Windows sandbox only: msys_unconfined=true requests explicit Once/Session consent for MSYS descendants; it grants nothing by itself. Check test OS support.";
#[cfg(not(windows))]
pub(super) const SHELL_HOST_DESCRIPTION: &str =
    "Host: /bin/sh -c, POSIX shell syntax; uses shell_environment_policy, credentials removed. Check test OS support.";
pub(super) const SUBAGENT_DESCRIPTION: &str =
    "Delegate one bounded child task; return its terminal report.";
pub(super) const HISTORY_DESCRIPTION: &str =
    "{} lists outputs; cursor pages; call selects fetch and ignores cursor.";

pub(super) fn definitions() -> Vec<ToolDefinition> {
    vec![
        tool_definition::<ReadArguments>(READ_DESCRIPTION),
        tool_definition::<EditArguments>(EDIT_DESCRIPTION),
        tool_definition::<ShellArguments>(&format!("{SHELL_DESCRIPTION} {SHELL_HOST_DESCRIPTION}")),
        tool_definition::<SubagentArguments>(SUBAGENT_DESCRIPTION),
        tool_definition::<super::skill::SkillArguments>(super::skill::SKILL_DESCRIPTION),
        tool_definition::<HistoryArguments>(HISTORY_DESCRIPTION),
        tool_definition::<super::coding::WriteArguments>(super::coding::WRITE_DESCRIPTION),
        tool_definition::<super::coding::GlobArguments>(super::coding::GLOB_DESCRIPTION),
        tool_definition::<super::coding::GrepArguments>(super::coding::GREP_DESCRIPTION),
        tool_definition::<super::coding::ReadLinesArguments>(super::coding::READ_LINES_DESCRIPTION),
        tool_definition::<super::coding::ListArguments>(super::coding::LIST_DESCRIPTION),
        tool_definition::<super::coding::patch::PatchArguments>(
            super::coding::patch::PATCH_DESCRIPTION,
        ),
        super::coding::patch::apply_patch::definition(),
        tool_definition::<super::web::FetchArguments>(super::web::FETCH_DESCRIPTION),
        tool_definition::<super::image::Arguments>(super::image::DESCRIPTION),
        tool_definition::<super::agent_interrupt::InterruptAgentArguments>(
            super::agent_interrupt::INTERRUPT_AGENT_DESCRIPTION,
        ),
        tool_definition::<super::agent_spawn::SpawnAgentArguments>(
            super::agent_spawn::SPAWN_AGENT_DESCRIPTION,
        ),
        tool_definition::<super::agents::ListAgentsArguments>(
            super::agents::LIST_AGENTS_DESCRIPTION,
        ),
        tool_definition::<super::agent_wait::WaitAgentArguments>(
            super::agent_wait::WAIT_AGENT_DESCRIPTION,
        ),
        tool_definition::<super::plan::UpdatePlanArgs>(super::plan::DESCRIPTION),
        tool_definition::<super::ask::Arguments>(super::ask::DESCRIPTION),
        tool_definition::<super::agent_message::SendMessageArguments>(
            super::agent_message::SEND_MESSAGE_DESCRIPTION,
        ),
        tool_definition::<super::agent_followup::FollowupTaskArguments>(
            super::agent_followup::FOLLOWUP_TASK_DESCRIPTION,
        ),
        tool_definition::<super::sandbox_access::Arguments>(super::sandbox_access::DESCRIPTION),
        tool_definition::<super::terminal::ExecArguments>(super::terminal::EXEC_DESCRIPTION),
        tool_definition::<super::terminal::StdinArguments>(super::terminal::STDIN_DESCRIPTION),
        tool_definition::<super::read_history::Arguments>(super::read_history::DESCRIPTION),
    ]
}

pub(super) fn tool_definition<T: ToolArguments>(description: &str) -> ToolDefinition {
    ToolDefinition {
        input_format: grapt_provider::ToolInputFormat::Json,
        name: T::TOOL_NAME.to_owned(),
        description: Some(description.to_owned()),
        input_schema: T::provider_schema(),
    }
}
