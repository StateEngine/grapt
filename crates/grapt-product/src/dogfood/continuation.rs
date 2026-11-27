//! Durable reconstruction of one minimum-tool continuation.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use grapt_artifact::{ArtifactStore, FilesystemArtifactStore, ReadBudget};
use grapt_event::session_tool_terminal::{self, StoredTerminal};
use grapt_inbox::Inbox;
use grapt_permission::PermissionEvent;
use grapt_provider::{
    CodexInputItem, CodexToolText, InputMessage, MessageRole, ToolName, MAX_CODEX_TOOL_TEXT_BYTES,
};
use grapt_session::SessionHistoryStore;
use grapt_store::session_event_queries::{scoped_rows, scoped_terminals, Scope, ScopedTerminal};
use grapt_store::write::EventStore;
use grapt_types::{CaptureDisposition, MessageId, SessionId, ToolCallId, ToolTerminalOutcome};

use super::tools::{EDIT_TOOL, READ_TOOL, SHELL_TOOL, WRITE_TOOL};

pub(super) mod admission;
mod artifact_reads;
pub(in crate::dogfood) mod background;
mod compaction;
pub(super) mod compaction_recovery;
mod compatible_reasoning;
pub(in crate::dogfood) mod context_error;
use context_error::{Bound, ContextError};
mod effort;
mod history_tool;
mod mail;
mod output_recovery;
mod queries;
use queries::effect_withheld;
pub(super) use queries::{orphaned_nested, require_resolved_effects};
pub(in crate::dogfood) mod files;
pub(in crate::dogfood) mod progress;
pub(in crate::dogfood) mod recent;
mod recovery_reference;
mod repetition;
mod request_context;
mod response_order;
mod retained;
mod retained_facts;
mod retained_users;
mod shell_preview;
mod structural;
mod summarization;
mod summary_owner;
#[cfg(test)]
mod test_support;
mod unknown_effects;
pub(in crate::dogfood) use artifact_reads::read_text;
use artifact_reads::{fetch_bytes, fetch_bytes_bounded, tool_artifact_read_bound, OutputReads};
pub(super) use compaction::{CompactionAuthorities, ProductCompactor, REFUSED_SUMMARY};
#[cfg(test)]
use history_tool::stored_outputs;
pub(super) use history_tool::{history, tool_projection};
use history_tool::{stored_calls, stored_calls_for, stored_outputs_for, stored_outputs_projected};
#[cfg(test)]
use test_support::promote_test_input;
#[cfg(test)]
pub(super) use test_support::TestContinuationScratch;

// Structural bounds are a safety net behind token admission.
const MAX_CONTINUATION_ROWS: usize = grapt_session::MAX_TOOL_CALL_PAGE_ROWS;
/// The most Responses reasoning one request carries back. Items are kept
/// oldest first, so a request's reasoning prefix never changes; past the
/// bound a newer item is not replayed.
const MAX_REASONING_REPLAY_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
struct PermissionCall {
    input: MessageId,
    call: ToolCallId,
    tool: ToolName,
}

#[derive(Debug)]
struct StoredCall {
    seq: u64,
    message: MessageId,
    call: ToolCallId,
    tool: Option<ToolName>,
    permission_tool: Option<ToolName>,
    arguments: CodexToolText,
    execution: grapt_types::ToolExecution,
    image_read: Option<LocalImageRead>,
}

/// Read provenance for public image causes, derived from durable identities.
#[derive(Debug, Clone, Copy)]
pub(super) enum LocalImageRead {
    Permission,
    ExplicitLocalIdentity,
}

#[derive(Debug)]
struct StoredOutput {
    seq: u64,
    call: ToolCallId,
    output: CodexToolText,
    error: Option<String>,
}

pub(super) type ToolProjections = HashMap<ToolCallId, Result<ToolProjection, String>>;

#[derive(Debug)]
pub(super) struct ToolProjection {
    pub(super) execution: grapt_types::ToolExecution,
    pub(super) name: String,
    pub(super) permission_tool: Option<ToolName>,
    pub(super) image_read: Option<LocalImageRead>,
    pub(super) input: serde_json::Value,
    /// Why the stored arguments could not be read as an object, if they
    /// could not. A diagnostic about the call, never its outcome: `error`
    /// stays the tool's own terminal error.
    pub(super) input_invalid: Option<String>,
    pub(super) output: String,
    pub(super) error: Option<String>,
    pub(super) failure_reason: Option<grapt_types::ToolFailureReason>,
    pub(super) settled: bool,
    /// Unsettled, and provably never ran: its permission was asked and never
    /// granted (see `effect_withheld`).
    pub(super) never_ran: bool,
    pub(super) progress: Option<grapt_api::u3a::ToolProgressMetadata>,
    pub(super) files: Vec<files::FinalFile>,
}

#[derive(Debug)]
pub(super) struct DurableContinuation {
    readers: Result<Option<grapt_store::read_pool::ReadPool>, String>,
    inbox: Arc<Mutex<Inbox>>,
    artifacts: Arc<FilesystemArtifactStore>,
    session: SessionId,
    input: MessageId,
    /// The target this execution plans for, shared with its compactor: a
    /// provider compaction is sent only to the target that produced it.
    redeemer: Arc<Mutex<Option<grapt_provider::CompactionTarget>>>,
    compatible_reasoning: Arc<Mutex<grapt_provider::CompatibleReasoningReplay>>,
    recovery: Option<compaction_recovery::Session>,
    /// The plans' admission snapshot, whose request bounds a summary of it.
    coverage: Option<admission::DiagnosisSlot>,
    background: Option<String>,
    /// Where a summary window starts when the history since the retained cut
    /// is past the read bounds: the newest bounded window, opening on an input.
    summary_floor: Option<std::num::NonZeroU64>,
}

struct RequestContext {
    current_input: u64,
    ceiling: u64,
    identity: Option<grapt_types::RequestContextIdentity>,
    rows: Vec<(u64, CodexInputItem)>,
    response_order: response_order::ResponseOrder,
    efforts: std::collections::BTreeMap<u64, grapt_provider::ReasoningEffort>,
    summary: Option<String>,
    original_users: bool,
    /// A provider compaction's items, standing where `summary` would.
    provider: Vec<CodexInputItem>,
    /// Original user messages and deterministic tool facts before the cut.
    prefix: Vec<CodexInputItem>,
}

pub(super) struct StructuralAdmission {
    pub seq: grapt_types::EventSeq,
    pub observed: u64,
    pub limit: u64,
}

const SUMMARY_LABEL: &str = "[Model-written continuation summary. The current user input and retained exact history are authoritative. Omitted tool output is recoverable with the history tool.]\n";

fn labelled_summary(summary: &str) -> String {
    format!("{SUMMARY_LABEL}{summary}")
}

impl RequestContext {
    #[cfg(test)]
    fn into_input(self) -> Vec<CodexInputItem> {
        self.into_positioned_input()
            .into_iter()
            .map(|(_, item)| item)
            .collect()
    }

    fn into_positioned_input(self) -> Vec<(Option<u64>, CodexInputItem)> {
        let mut input: Vec<_> = self.prefix.into_iter().map(|item| (None, item)).collect();
        if let Some(summary) = self.summary {
            input.push((
                None,
                CodexInputItem::Message(InputMessage {
                    role: MessageRole::Assistant,
                    content: retained_users::labelled_summary(&summary, self.original_users),
                }),
            ));
        }
        input.extend(self.provider.into_iter().map(|item| (None, item)));
        input.extend(
            self.response_order
                .emit_positioned(self.rows, &self.efforts)
                .into_iter()
                .map(|(seq, item)| (Some(seq), item)),
        );
        input
    }
}

impl DurableContinuation {
    pub(super) fn new(
        inbox: Arc<Mutex<Inbox>>,
        artifacts: Arc<FilesystemArtifactStore>,
        session: SessionId,
        input: MessageId,
    ) -> Self {
        let readers = lock(&inbox)
            .store()
            .read_pool()
            .map_err(|error| error.to_string());
        Self {
            readers,
            inbox,
            artifacts,
            session,
            input,
            redeemer: Arc::default(),
            compatible_reasoning: Arc::new(Mutex::new(Default::default())),
            recovery: None,
            coverage: None,
            background: None,
            summary_floor: None,
        }
    }

    pub(super) fn with_background_recovery(mut self, notice: Option<String>) -> Self {
        self.background = notice;
        self
    }

    pub(super) fn with_compaction_recovery(
        mut self,
        registry: compaction_recovery::Registry,
    ) -> Self {
        self.recovery = Some(compaction_recovery::Session::new(
            registry,
            self.session.clone(),
            self.input.clone(),
        ));
        self
    }

    /// Read the promoted input before replacing this chain's context coordinate.
    pub(super) fn promote_input(&mut self, input: &MessageId) -> Result<String, String> {
        let promoted = self.read_projection(|reader| {
            lock(&reader.inbox)
                .promoted_input(&reader.session, input)
                .map_err(|error| format!("promoted recovery input is unreadable: {error}"))
        })?;
        self.input = input.clone();
        if let Some(recovery) = &mut self.recovery {
            recovery.promote_input(input);
        }
        Ok(promoted.text)
    }

    pub(in crate::dogfood) fn trim_requested(&self) -> bool {
        self.recovery
            .as_ref()
            .is_some_and(|recovery| recovery.trim_requested())
    }

    /// The same continuation, sharing its selected target, for the compactor.
    pub(super) fn share(&self) -> Self {
        Self {
            readers: self.readers.clone(),
            inbox: Arc::clone(&self.inbox),
            artifacts: Arc::clone(&self.artifacts),
            session: self.session.clone(),
            input: self.input.clone(),
            redeemer: Arc::clone(&self.redeemer),
            compatible_reasoning: Arc::clone(&self.compatible_reasoning),
            recovery: self.recovery.clone(),
            coverage: self.coverage.clone(),
            background: self.background.clone(),
            summary_floor: self.summary_floor,
        }
    }

    /// Whether every provider compaction item may go to the selected target.
    /// Without a selection (tests and readers that only need coordinates) the
    /// items stand.
    fn redeems(&self, items: &[grapt_provider::ProviderReplay]) -> bool {
        lock(&self.redeemer)
            .as_ref()
            .is_none_or(|target| items.iter().all(|item| item.redeemable_for(target)))
    }

    #[cfg(test)]
    pub(super) fn request_input(
        &self,
        prompt: &str,
        continuation_required: bool,
    ) -> Result<Vec<CodexInputItem>, String> {
        let input = self
            .request_context(prompt, continuation_required, None, true)?
            .into_input();
        Ok(input)
    }

    pub(in crate::dogfood) fn operation_mail(
        &self,
        step: grapt_types::StepNumber,
    ) -> Result<Option<grapt_inbox::operation_mail::OperationMailBatch>, String> {
        super::agent_mail::prepare(&self.inbox, &self.session, step)
    }

    #[cfg(test)]
    pub(super) fn request_with_identity(
        &self,
        prompt: &str,
        continuation_required: bool,
    ) -> Result<
        (
            Vec<CodexInputItem>,
            Option<grapt_types::RequestContextIdentity>,
        ),
        String,
    > {
        self.request_with_instructions(prompt, continuation_required, &[])
            .map(|(input, identity, _)| (input, identity))
            .map_err(String::from)
    }
}

// A context that exceeds these bounds is refused, never silently shortened.
const MAX_CONTEXT_ITEMS: usize = 4 * MAX_CONTINUATION_ROWS;

/// Whether retained rows, the facts item and the summary's own items (one
/// for a text summary, each committed item for a provider compaction) exceed
/// the request's item bound.
fn exceeds_item_bound(
    rows: usize,
    prefix_items: usize,
    summary: Option<&str>,
    provider: &[CodexInputItem],
) -> bool {
    let summary_items = if summary.is_some() { 1 } else { provider.len() };
    rows.saturating_add(prefix_items)
        .saturating_add(summary_items)
        > MAX_CONTEXT_ITEMS
}
/// Public path label; the stored summary stays unchanged for model replay.
pub(in crate::dogfood) fn se_summary_notice(text: &str) -> String {
    format!("[Compacted with SE summary.]\n{text}")
}

/// The public fact, item count and usage of a committed provider compaction.
pub(in crate::dogfood) fn provider_compaction_notice(
    artifacts: &FilesystemArtifactStore,
    descriptor: &grapt_types::CompactionDescriptor,
) -> Result<String, String> {
    let grapt_types::RetainedHistory::DeterministicSelection {
        descriptor: selection,
    } = descriptor.retained()
    else {
        return Err("provider compaction has no deterministic selection".to_owned());
    };
    if selection.length() > retained::MAX_SELECTION_BYTES {
        return Err("provider compaction selection exceeds its bound".to_owned());
    }
    let text = read_text(
        artifacts,
        selection,
        CaptureDisposition::Complete,
        "selection",
    )?;
    match retained::Selection::decode(text.as_str())?.server {
        Some(retained::ServerOutcome::Committed {
            items,
            input_tokens,
            output_tokens,
            ..
        }) => Ok(compaction::provider_notice(
            items,
            (input_tokens, output_tokens),
        )),
        _ => Err("provider compaction has no committed record".to_owned()),
    }
}

pub(super) const MAX_CONTEXT_BYTES: usize = 4 * 1024 * 1024;
fn context_item_bytes(item: &CodexInputItem) -> usize {
    match item {
        CodexInputItem::EffortUpdate(value) => value.as_str().len(),
        CodexInputItem::Message(message) => message.content.len(),
        CodexInputItem::ImageUser {
            text: output,
            images,
        }
        | CodexInputItem::ImageToolOutput { output, images, .. } => {
            output.as_str().len().saturating_add(images.byte_len())
        }
        CodexInputItem::CompatibleReasoning(text) => text.as_str().len(),
        CodexInputItem::ProviderReplay(replay) => replay.byte_len(),
        CodexInputItem::ToolCall { arguments, .. }
        | CodexInputItem::TextToolCall {
            input: arguments, ..
        } => arguments.as_str().len(),
        CodexInputItem::ToolOutput { output, .. } => output.as_str().len(),
        CodexInputItem::AssistantResponseStart | CodexInputItem::CompactionTrigger => 0,
    }
}

fn push_context(
    rows: &mut Vec<(u64, CodexInputItem)>,
    bytes: &mut usize,
    seq: u64,
    item: CodexInputItem,
) -> Result<(), ContextError> {
    let length = context_item_bytes(&item);
    let next = bytes
        .checked_add(length)
        .ok_or("conversation byte count overflowed")?;
    if next > MAX_CONTEXT_BYTES {
        return Err(ContextError::over_bytes(Bound::Conversation, next));
    }
    if rows.len() >= MAX_CONTEXT_ITEMS {
        return Err(ContextError::over_items(
            Bound::Conversation,
            rows.len().saturating_add(1),
        ));
    }
    *bytes = next;
    rows.push((seq, item));
    Ok(())
}

fn permission_calls(
    store: &EventStore,
    session: &SessionId,
    from: Option<grapt_types::EventSeq>,
    until: Option<grapt_types::EventSeq>,
) -> Result<Vec<PermissionCall>, String> {
    permission_calls_for(store, session, from, until, None, MAX_CONTINUATION_ROWS)
}

fn permission_calls_for(
    store: &EventStore,
    session: &SessionId,
    from: Option<grapt_types::EventSeq>,
    until: Option<grapt_types::EventSeq>,
    target: Option<&ToolCallId>,
    max_rows: usize,
) -> Result<Vec<PermissionCall>, String> {
    let rows = scoped_rows(
        store,
        session,
        "permission.asked.2",
        from,
        until,
        Scope::Call(target, "$.source.callID"),
        max_rows,
    )?;
    history_tool::decode_permission_calls(rows)
}

/// The tool a recorded permission was asked for.
///
/// The built-in actions are their own vocabulary and keep their mappings. An
/// MCP tool's durable action IS its published qualified name, so it is carried
/// through and VALIDATED rather than matched against a list.
///
/// Refusing every non-built-in meant a Session could not be replanned after an
/// approved MCP call: the tool ran, returned its result, and the next Step's
/// plan was refused because this reconstruction did not recognise the name.
///
/// THE RECORDED NAME IS THE AUTHORITY, not today's MCP catalog. Replaying a
/// completed exchange is reading history, not authorising a call, so a server
/// that has since disappeared or been reconfigured must not change what this
/// history says happened. Nothing here consults the registry.
fn tool_name(action: &str, call: &ToolCallId) -> Result<ToolName, String> {
    let name = match action {
        "read" => READ_TOOL,
        "write" | "fs.modify" => EDIT_TOOL,
        "fs.create" | "fs.overwrite" => WRITE_TOOL,
        "shell" => SHELL_TOOL,
        recorded => recorded,
    };
    ToolName::new(name).ok_or_else(|| format!("tool {call} has unusable durable action {action:?}"))
}

fn tool_source(value: &serde_json::Value) -> Result<Option<(MessageId, ToolCallId)>, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "permission source is not an object".to_owned())?;
    let kind = object
        .get("type")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "permission source type is absent".to_owned())?;
    if kind != "tool" {
        return Ok(None);
    }
    if object.len() != 3 {
        return Err("tool permission source has unknown or missing fields".to_owned());
    }
    let message = object
        .get("messageID")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "permission source messageID is absent".to_owned())?
        .parse::<MessageId>()
        .map_err(|_| "permission source messageID is invalid".to_owned())?;
    let call = ToolCallId::new(
        object
            .get("callID")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "permission source callID is absent".to_owned())?,
    )
    .ok_or_else(|| "permission source callID is invalid".to_owned())?;
    Ok(Some((message, call)))
}

#[derive(Debug)]
struct ToolIdentity {
    seq: u64,
    message: MessageId,
    call: ToolCallId,
    tool: ToolName,
    execution: grapt_types::ToolExecution,
    explicit_execution: bool,
}

fn tool_identities(
    store: &EventStore,
    session: &SessionId,
    from: Option<grapt_types::EventSeq>,
    until: Option<grapt_types::EventSeq>,
    target: Option<&ToolCallId>,
    max_rows: usize,
) -> Result<Vec<ToolIdentity>, String> {
    let mut identities = Vec::new();
    for (version, kind) in [
        (1, "session.tool.input.started.1"),
        (2, "session.tool.input.started.2"),
    ] {
        let rows = scoped_rows(
            store,
            session,
            kind,
            from,
            until,
            Scope::Call(target, "$.callID"),
            max_rows,
        )?;
        let decoded = history_tool::decode_tool_identities(session, rows, version)?;
        identities = history_tool::merge_identities(identities, decoded, max_rows)?;
    }
    Ok(identities)
}

/// The explicit bounds one bounded history read admits: the sequence range
/// and the row ceiling of that read.
#[derive(Clone, Copy)]
struct BoundedWindow {
    from: Option<grapt_types::EventSeq>,
    until: Option<grapt_types::EventSeq>,
    max_rows: usize,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// How one projection carrier records a reader-pool failure. Every carrier
/// states it: exhausted reader capacity is a wait where the carrier has one,
/// never a detail string that later reads as a terminal refusal.
pub(in crate::dogfood) trait ReadFailure: From<String> {
    /// This carrier's typed capacity wait, or `None` when it has no wait form.
    fn capacity_busy() -> Option<Self>;

    fn read_failed(error: grapt_store::read_pool::ReadPoolError) -> Self {
        match (&error, Self::capacity_busy()) {
            (grapt_store::read_pool::ReadPoolError::Busy, Some(busy)) => busy,
            _ => Self::from(error.to_string()),
        }
    }
}

impl ReadFailure for grapt_runtime::PlanRefusal {
    fn capacity_busy() -> Option<Self> {
        Some(Self::capacity_busy())
    }
}

impl ReadFailure for grapt_runtime::provider_source::ContextAdmissionRefusal {
    fn capacity_busy() -> Option<Self> {
        Some(Self::CapacityBusy)
    }
}

// The remaining carriers have no wait form: their callers either run nested
// inside a typed planning or admission read (which already holds the reader,
// so no checkout happens) or report the read failure as their own failure.
impl ReadFailure for String {
    fn capacity_busy() -> Option<Self> {
        None
    }
}

impl ReadFailure for ContextError {
    fn capacity_busy() -> Option<Self> {
        None
    }
}

impl ReadFailure for summarization::PreparationError {
    fn capacity_busy() -> Option<Self> {
        None
    }
}

impl DurableContinuation {
    pub(in crate::dogfood) fn read_projection<T, E: ReadFailure>(
        &self,
        project: impl FnOnce(&Self) -> Result<T, E>,
    ) -> Result<T, E> {
        crate::dogfood::read_projection::blocking(|| self.read_at(project))
    }

    /// Retain typed capacity refusals for one finite planning projection.
    pub(in crate::dogfood) fn read_for_plan<T>(
        &self,
        project: impl FnOnce(&Self) -> Result<T, grapt_runtime::PlanRefusal>,
    ) -> Result<T, grapt_runtime::PlanRefusal> {
        self.read_projection(project)
    }

    /// Context admission is read-only and shares one finite snapshot.
    pub(in crate::dogfood) fn read_for_context_admission<T>(
        &self,
        project: impl FnOnce(&Self) -> Result<T, grapt_types::ContextFailure>,
    ) -> Result<T, grapt_runtime::provider_source::ContextAdmissionRefusal> {
        use grapt_runtime::provider_source::ContextAdmissionRefusal;
        self.read_projection(|reader| project(reader).map_err(ContextAdmissionRefusal::from))
    }

    fn read_at<T, E: ReadFailure>(
        &self,
        project: impl FnOnce(&Self) -> Result<T, E>,
    ) -> Result<T, E> {
        let pool = self
            .readers
            .as_ref()
            .map_err(|error| E::from(error.clone()))?;
        let Some(pool) = pool else {
            return project(self);
        };
        let mut reader = self.share();
        reader.readers = Ok(None);
        reader.inbox =
            crate::dogfood::read_projection::inbox_typed(pool).map_err(E::read_failed)?;
        let result = project(&reader);
        // A projection that produced no value closes its reader instead of
        // idling a connection that may no longer reach the database; only a
        // completed read recycles. Same rule as the async read path.
        if result.is_err() {
            crate::dogfood::read_projection::discard(reader.inbox);
            return result;
        }
        crate::dogfood::read_projection::finish_typed(reader.inbox).map_err(
            |error| match error {
                crate::dogfood::read_projection::Finish::Pool(error) => E::read_failed(error),
                crate::dogfood::read_projection::Finish::Retained(detail) => {
                    E::from(detail.to_owned())
                }
            },
        )?;
        result
    }
}

impl DurableContinuation {
    pub(in crate::dogfood) fn request_with_instructions(
        &self,
        prompt: &str,
        continuation_required: bool,
        updates: &[grapt_instruction::InstructionUpdate],
    ) -> Result<
        (
            Vec<CodexInputItem>,
            Option<grapt_types::RequestContextIdentity>,
            usize,
        ),
        ContextError,
    > {
        self.read_projection(|reader| {
            reader.request_with_instructions_at(prompt, continuation_required, updates)
        })
    }

    fn request_with_instructions_at(
        &self,
        prompt: &str,
        continuation_required: bool,
        updates: &[grapt_instruction::InstructionUpdate],
    ) -> Result<
        (
            Vec<CodexInputItem>,
            Option<grapt_types::RequestContextIdentity>,
            usize,
        ),
        ContextError,
    > {
        let mut context = self.request_context(prompt, continuation_required, None, true)?;
        let mut bytes = context
            .rows
            .iter()
            .map(|(_, item)| context_item_bytes(item))
            .sum::<usize>()
            .saturating_add(
                context
                    .efforts
                    .values()
                    .map(|value| value.as_str().len())
                    .sum::<usize>(),
            )
            .saturating_add(context.prefix.iter().map(context_item_bytes).sum::<usize>())
            .saturating_add(context.summary.as_deref().map_or(0, |summary| {
                retained_users::labelled_summary(summary, context.original_users).len()
            }))
            .saturating_add(
                context
                    .provider
                    .iter()
                    .map(context_item_bytes)
                    .sum::<usize>(),
            );
        for update in updates
            .iter()
            .filter(|update| update.seq.value() < context.ceiling)
        {
            push_context(
                &mut context.rows,
                &mut bytes,
                update.seq.value(),
                crate::dogfood::instructions::epoch_update(update),
            )?;
        }
        // Unknown effects no output shows (nested, or before the retained cut)
        // still warn the model; see `unknown_effects`.
        let shown = context.rows.iter().map(|(seq, _)| *seq).collect();
        let notices = unknown_effects::notices(
            lock(&self.inbox).store(),
            &self.session,
            context.ceiling,
            &shown,
        )?;
        for (seq, notice) in notices {
            push_context(&mut context.rows, &mut bytes, seq, notice)?;
        }
        if let Some(notice) = &self.background {
            let item = grapt_provider::CodexInputItem::Message(grapt_provider::InputMessage {
                role: grapt_provider::MessageRole::System,
                content: notice.clone(),
            });
            bytes = bytes.saturating_add(context_item_bytes(&item));
            if bytes > MAX_CONTEXT_BYTES {
                return Err(ContextError::over_bytes(Bound::Conversation, bytes));
            }
            context.prefix.insert(0, item);
        }
        let items =
            context.rows.len() + context.response_order.boundaries() + context.efforts.len();
        if exceeds_item_bound(
            items,
            context.prefix.len(),
            context.summary.as_deref(),
            &context.provider,
        ) {
            return Err(ContextError::over_items(
                Bound::Conversation,
                context_error::retained_items(
                    items,
                    context.prefix.len(),
                    context.summary.as_deref(),
                    &context.provider,
                ),
            ));
        }
        context.rows.sort_by_key(|(seq, _)| *seq);
        if context.rows.windows(2).any(|rows| rows[0].0 == rows[1].0) {
            return Err("instruction update repeats a conversation position".into());
        }
        let identity = context.identity.clone();
        let current = context.current_input;
        let positioned = context.into_positioned_input();
        let protected_from = positioned
            .iter()
            .position(|(seq, _)| *seq == Some(current))
            .ok_or("current input has no projected durable position")?;
        let input = positioned.into_iter().map(|(_, item)| item).collect();
        Ok((input, identity, protected_from))
    }
}

#[cfg(test)]
pub(in crate::dogfood) mod tests;
