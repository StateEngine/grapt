//! Observational dispatch ledger. Nothing reads this file to authorize an effect.
//!
//! Reuse the existing bounded diagnostic writer: queue overflow, an unavailable
//! sink and the file cap lose observations without changing the tool result.
//! Only host coordinates and closed shape/phase vocabularies reach the writer;
//! tool names, call IDs, arguments, paths, output and command text never do.
//! Entry means the runtime reached the product runner; earlier structural or
//! capacity refusals are not product permission attempts and are not inferred.
use super::*;
use crate::dogfood::stream_diagnostics::{FileSink, Recorder};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub(in crate::dogfood) const LOG: &str = "diagnostics/tool-dispatch-shadow.jsonl";
const SCHEMA: &str = "grapt.tool-dispatch-shadow.v1";
const QUEUE: usize = 256;
const SHAPE_INPUT_BYTES: usize = 8192;
const COORDINATE_BYTES: usize = 256;

#[derive(Debug, Clone, Copy)]
pub(super) enum Origin {
    Direct,
    Nested,
    PluginHost,
}
impl Origin {
    fn label(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Nested => "nested",
            Self::PluginHost => "plugin_host",
        }
    }
}

pub(in crate::dogfood) struct Ledger {
    recorder: Option<Recorder>,
    instance: Option<EventId>,
    next: AtomicU64,
}
impl std::fmt::Debug for Ledger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShadowLedger").finish_non_exhaustive()
    }
}
impl Ledger {
    pub(in crate::dogfood) fn disabled() -> Self {
        Self {
            recorder: None,
            instance: None,
            next: AtomicU64::new(0),
        }
    }

    pub(in crate::dogfood) fn open(service_root: &Path, ids: &dyn IdAuthority) -> Self {
        let minted = ids.mint();
        if minted.as_str().len() > COORDINATE_BYTES - "evt_shadow_".len() {
            return Self::disabled();
        }
        let Ok(instance) = format!("evt_shadow_{}", minted.as_str()).parse() else {
            return Self::disabled();
        };
        let mut sink = FileSink::new(service_root.join(LOG), SCHEMA);
        Self {
            recorder: Recorder::spawn(SCHEMA, QUEUE, move |line| sink.write(line)),
            instance: Some(instance),
            next: AtomicU64::new(0),
        }
    }

    pub(super) fn dispatch(
        self: &Arc<Self>,
        session: &SessionId,
        attempt: Option<&grapt_types::PhysicalAttemptId>,
        origin: Origin,
        tool: &str,
    ) -> Option<Arc<Dispatch>> {
        self.recorder.as_ref()?;
        let ordinal = self
            .next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .ok()?
            .checked_add(1)?;
        let session = (session.as_str().len() <= COORDINATE_BYTES).then(|| session.clone());
        let over_bound_attempt = attempt.is_some_and(|id| id.as_str().len() > COORDINATE_BYTES);
        let dispatch = Arc::new(Dispatch {
            ledger: Arc::clone(self),
            coordinates_over_bound: session.is_none() || over_bound_attempt,
            session,
            attempt: attempt.filter(|_| !over_bound_attempt).cloned(),
            ordinal,
            origin,
            tool: tool_kind(tool),
            finished: AtomicBool::new(false),
        });
        dispatch.record("dispatch", "unmeasured");
        Some(dispatch)
    }
}

#[derive(Debug)]
pub(super) struct Dispatch {
    ledger: Arc<Ledger>,
    session: Option<SessionId>,
    attempt: Option<grapt_types::PhysicalAttemptId>,
    coordinates_over_bound: bool,
    ordinal: u64,
    origin: Origin,
    tool: &'static str,
    finished: AtomicBool,
}
impl Dispatch {
    fn record(&self, phase: &'static str, shape: &'static str) {
        if let Some(recorder) = &self.ledger.recorder {
            recorder.offer(serde_json::json!({
                "session": self.session.as_ref().map(SessionId::as_str),
                "attempt": self.attempt.as_ref().map(grapt_types::PhysicalAttemptId::as_str),
                "coordinates_over_bound": self.coordinates_over_bound,
                "instance": self.ledger.instance.as_ref().map(EventId::as_str),
                "dispatch": self.ordinal,
                "origin": self.origin.label(),
                "tool": self.tool,
                "phase": phase,
                "shape": shape,
                "routing": "unmeasured",
            }));
        }
    }

    pub(super) fn prepared(&self, target: &EffectTarget) {
        self.record("prepared", shape(target));
    }

    pub(super) fn started(&self) {
        self.record("effect_started", "unmeasured");
    }

    pub(super) fn authorized(&self) {
        self.record("permission_granted", "unmeasured");
    }

    pub(super) fn finish(&self, outcome: &ToolTerminalOutcome) {
        if self.finished.swap(true, Ordering::Relaxed) {
            return;
        }
        let phase = match outcome {
            ToolTerminalOutcome::Completed { .. } => "completed",
            ToolTerminalOutcome::Failed { reason, .. } => match reason {
                ToolFailureReason::AuthorityUnavailable => "authority_unavailable",
                ToolFailureReason::ApprovalUnavailable => "approval_unavailable",
                ToolFailureReason::EffectFailed => "effect_failed",
                _ => "other_failure",
            },
            ToolTerminalOutcome::Cancelled { .. } => "cancelled",
            ToolTerminalOutcome::ResultMissing => "result_missing",
        };
        self.record(phase, "unmeasured");
    }
}
impl Drop for Dispatch {
    fn drop(&mut self) {
        if !self.finished.load(Ordering::Relaxed) {
            self.record("abandoned", "unmeasured");
        }
    }
}

fn tool_kind(tool: &str) -> &'static str {
    match tool {
        SHELL_TOOL | terminal::EXEC_COMMAND => "shell",
        terminal::WRITE_STDIN => "terminal_input",
        EXECUTE_TOOL => "codemode_envelope",
        READ_TOOL | READ_LINES_TOOL | GLOB_TOOL | GREP_TOOL | LIST_TOOL => "read",
        EDIT_TOOL | WRITE_TOOL | PATCH_TOOL | APPLY_PATCH_TOOL => "edit",
        PLAN_TOOL => "plan",
        SUBAGENT_TOOL => "subagent",
        _ => "external_or_other",
    }
}

fn shape(target: &EffectTarget) -> &'static str {
    match target {
        EffectTarget::Shell { command, .. } => command_shape(command),
        EffectTarget::TerminalExec(spec) => command_shape(&spec.command),
        EffectTarget::CodeMode { .. } => "codemode_envelope",
        EffectTarget::Mcp { .. } => "mcp",
        EffectTarget::Plugin { .. } => "plugin",
        EffectTarget::IntegrationCommand { .. } => "integration_argv",
        _ => "builtin",
    }
}

fn command_shape(command: &str) -> &'static str {
    if command.len() > SHAPE_INPUT_BYTES {
        return "over_bound";
    }
    // The command reader keeps words from unfinished quotes so permission
    // checks can still inspect them. An observer must not label that input as
    // complete: reuse the terminal's quote/continuation scan before reading it.
    let terminated = format!("{command}\n");
    if !grapt_permission::command_safety::unfinished_line(&terminated).is_empty() {
        return "opaque";
    }
    let reading = grapt_permission::command_safety::simple_commands(command);
    if !reading.is_complete() {
        return "opaque";
    }
    let mut shape = None;
    for argv in reading.commands() {
        let next = argv_shape(argv);
        if shape.is_some_and(|shape| shape != next) {
            return "mixed";
        }
        shape = Some(next);
    }
    shape.unwrap_or("opaque")
}

fn argv_shape(argv: &[String]) -> &'static str {
    let Some(program) = argv.first() else {
        return "opaque";
    };
    let program = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match program.trim_end_matches(".exe") {
        "git" => match argv.get(1).map(String::as_str) {
            Some("status") => "git_status",
            Some("diff") => "git_diff",
            Some("log") => "git_log",
            Some("show") => "git_show",
            Some("reset") => "git_reset",
            Some("rebase") => "git_rebase",
            Some("push") => "git_push",
            _ => "git_other",
        },
        "cargo" => match argv.get(1).map(String::as_str) {
            Some("test") => "cargo_test",
            Some("build") => "cargo_build",
            Some("check") => "cargo_check",
            Some("clippy") => "cargo_clippy",
            Some("fmt") => "cargo_fmt",
            _ => "cargo_other",
        },
        "python" | "python3" | "py" => {
            if argv.iter().any(|arg| arg == "-c") {
                "python_inline"
            } else if argv.iter().any(|arg| arg == "-m") {
                "python_module"
            } else {
                "python_other"
            }
        }
        "ssh" => "ssh",
        "sed" | "head" | "tail" | "cat" | "ls" | "rg" | "pwd" => "read_program",
        _ => "opaque",
    }
}

#[cfg(test)]
#[path = "shadow/tests.rs"]
mod tests;
