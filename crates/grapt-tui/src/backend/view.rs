//! What one view carries to the UI, including every command result settled
//! since the view before it. Results accumulate in order and are never
//! overwritten while a view waits in the hand-off.
use super::{commands, first_prompt, followups, Agent, CommandId, Link, Preview};
use grapt_client::{wire::SessionWireId, PermissionAsk, Slice};

#[derive(Debug)]
#[expect(clippy::struct_excessive_bools, reason = "legacy")]
pub struct Update {
    /// The Select command this view answers, when it answers one.
    pub selection: Option<CommandId>,
    pub previews: Vec<Preview>,
    pub queued: grapt_client::wire::QueuedInputPreview,
    pub slash: crate::slash::Catalog,
    pub forms: Vec<grapt_client::wire::FormRow>,
    pub form_location: Option<grapt_client::wire::FormLocation>,
    pub startup: crate::launch::Readiness,
    pub prompt_ready: bool,
    pub reset: bool,
    pub reconcile: bool,
    pub selected: Option<SessionWireId>,
    pub running: bool,
    pub streamed_tokens: Option<usize>,
    pub connected: bool,
    /// The connection as the footer's heartbeat names it.
    pub link: Link,
    /// Unknown activity must not turn into a completion notification.
    pub observed_running: Option<bool>,
    /// The selected Session's running execution, which a stop key aims at.
    pub execution: Option<grapt_client::wire::ExecutionWireId>,
    pub compacting: bool,
    pub completion: Option<(grapt_client::wire::MessageWireId, grapt_client::Outcome)>,
    pub start: usize,
    pub rows: Vec<Slice>,
    pub agents: Vec<Agent>,
    /// Whether the last session-list read could list anything.
    pub catalog: grapt_client::CatalogReach,
    pub permission: Option<PermissionAsk>,
    pub policy: Option<String>,
    pub bypass_permissions: bool,
    pub mail_attention: Option<grapt_api::u3a::MailAttentionInfo>,
    pub continuity_attention: Option<grapt_api::u3a::ContinuityAttentionInfo>,
    pub status: String,
    /// A live-event gap the client is recovering from; shown as one neutral
    /// line that clears itself rather than folded into `status`.
    pub stream_gap: bool,
    pub repository_status: Option<String>,
    pub feedback: Option<grapt_client::projection::feedback::Feedback>,
    pub failed_execution: Option<grapt_client::wire::ExecutionWireId>,
    pub usage: grapt_client::view::status::Snapshot,
    pub title: String,
    pub mcp_summary: Vec<String>,
    pub mcp_detail: Vec<String>,
    pub patches: Vec<(usize, Slice)>,
    pub history_edges: (bool, bool),
    /// The newest page move that ran, with the command that asked for it.
    pub navigation: Option<(CommandId, grapt_client::client::HistoryMove, bool)>,
    /// Commands that could have moved the page, discarded by a quit: each
    /// settles its request without moving the view.
    pub navigation_dropped: Vec<CommandId>,
    pub browsing: bool,
    pub model: Option<grapt_client::wire::ModelRow>,
    pub models: Option<Vec<grapt_client::wire::ModelRow>>,
    pub roster: Option<Vec<grapt_client::wire::AgentRow>>,
    /// Every command result settled since the last view, in order.
    pub outcomes: Vec<Settled>,
}

#[derive(Debug)]
pub struct Failure {
    pub message: String,
    /// Bridge initialization failed before any Session attachment could run.
    pub unattached: bool,
}

/// One command's result, with the command that started the work it settles.
/// Every result has one: there is no server-initiated outcome in this ledger.
#[derive(Debug)]
pub struct Settled {
    pub id: CommandId,
    pub outcome: Outcome,
}

#[derive(Debug)]
pub enum Outcome {
    Followup(crate::followups::Custody),
    Image(crate::images::Settlement),
    /// The prompt text, whether it was accepted, and whether a human wrote it.
    Submission(String, bool, bool),
    Command(commands::Completion),
    Model(bool),
    Form(grapt_tui_forms::Reply),
    Session(crate::sessions::Message),
}

/// The backend's side: results move here from the holders that produce them
/// as soon as each command is handled, so a later one cannot replace them.
/// Each holder carries the command that started its work, and keeps it
/// through intermediate states until the work settles.
#[derive(Default)]
pub(super) struct Ledger {
    settled: Vec<Settled>,
}

impl Ledger {
    /// Move every settled result out of its holder, with its own origin.
    pub(super) fn collect(
        &mut self,
        submissions: &mut first_prompt::Submissions,
        followups: &mut followups::Submissions,
        model_result: &mut Option<(CommandId, bool)>,
        replies: &mut Vec<(CommandId, grapt_tui_forms::Reply)>,
        messages: &mut Vec<(CommandId, crate::sessions::Message)>,
    ) {
        if let Some((id, custody)) = followups.result.take() {
            self.push(id, Outcome::Followup(custody));
        }
        if let Some((id, settlement)) = submissions.image_result.take() {
            self.push(id, Outcome::Image(settlement));
        }
        if let Some((id, (text, accepted, human))) = submissions.outcome.take() {
            self.push(id, Outcome::Submission(text, accepted, human));
        }
        if let Some((id, done)) = submissions.command.take() {
            self.push(id, Outcome::Command(done));
        }
        if let Some((id, accepted)) = model_result.take() {
            self.push(id, Outcome::Model(accepted));
        }
        for (id, reply) in replies.drain(..) {
            self.push(id, Outcome::Form(reply));
        }
        for (id, message) in messages.drain(..) {
            self.push(id, Outcome::Session(message));
        }
    }

    fn push(&mut self, id: CommandId, outcome: Outcome) {
        self.settled.push(Settled { id, outcome });
    }

    /// Settle a command that never ran.
    pub(super) fn settle(&mut self, id: CommandId, outcome: Outcome) {
        self.push(id, outcome);
    }

    pub(super) fn len(&self) -> usize {
        self.settled.len()
    }

    /// The note of the newest form reply settled after `mark`.
    pub(super) fn form_note_after(&self, mark: usize) -> Option<String> {
        self.settled
            .get(mark..)?
            .iter()
            .rev()
            .find_map(|settled| match &settled.outcome {
                Outcome::Form(reply) => Some(reply.note.clone()),
                _ => None,
            })
    }

    /// Completions waiting for the next view, which a later selection may
    /// still reshape before publication.
    pub(super) fn completions(&mut self) -> impl Iterator<Item = &mut commands::Completion> {
        self.settled
            .iter_mut()
            .filter_map(|settled| match &mut settled.outcome {
                Outcome::Command(done) => Some(done),
                _ => None,
            })
    }

    pub(super) fn take(&mut self) -> Vec<Settled> {
        std::mem::take(&mut self.settled)
    }
}
