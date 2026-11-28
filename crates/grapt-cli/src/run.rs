//! Session selection, cancellation, and retry boundary for noninteractive use.

use std::io::Write;

mod admission;
mod background;
mod exit;
mod gates;
mod jsonl;
mod location;
pub use background::BackgroundReceipt;
pub use exit::ExitClass;
pub use gates::{RetryGate, TerminalSettlement};
mod permission_stall;
mod resident;
mod session_selection;
use session_selection::newest_for_project;

use grapt_types::{MessageId, ProjectId, SessionId};
use serde::Serialize;
use thiserror::Error;

const MAX_TERMINAL_POLLS: u16 = 300;
const TERMINAL_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
/// How long a cancelled run has to hand its output back. A run parked at an
/// await answers at once; only a stdout write that the reader is not draining
/// holds its task, and one terminal-poll interval is the run loop's own cadence.
pub const CANCELLATION_HANDOFF: std::time::Duration = TERMINAL_POLL_INTERVAL;
/// The longest a run takes to exit Remote after losing its server, while its
/// stdout and stderr accept writes: every request, the event stream included,
/// ends within one request allowance, and an idle loop issues a request within
/// one terminal-poll interval. See `docs/cli-server-loss/CONTRACT.md`.
pub const SERVER_LOSS_EXIT_BOUND: std::time::Duration =
    TERMINAL_POLL_INTERVAL.saturating_add(crate::client::REQUEST_ALLOWANCE);
/// The longest a run stays alive once its service stops answering: the
/// first stalled request ends within one request allowance of one idle
/// tick, and each of the reissued durable reads ends within one request
/// allowance — the same allowance and failure count the stall policy
/// bounds itself by, so the run's bound is the policy's total time plus
/// the run loop's own tick. See `docs/cli-server-loss/CONTRACT.md`.
pub const TRANSPORT_STALL_EXIT_BOUND: std::time::Duration =
    TERMINAL_POLL_INTERVAL.saturating_add(crate::client::stall::MAX_TRANSPORT_STALL_TIME);
const MAX_CANCELLATION_POLLS: u8 = 8;
const CANCELLATION_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);

use crate::render::{admission, execution_failure, not_run_yet_for, TextRendering};
use crate::{
    AdvisoryError, ApiClient, ApiClientError, BoundedOutput, ConvergenceError, CursorConvergence,
    CursorPage, OutputError, OutputMode, OutputReceipt, SessionSelector,
};
use grapt_api::u3a::SessionMessageInfoDto;
use grapt_api::SessionMessagesResponseDto;

const MAX_SSE_RECONNECTS: u8 = 8;
const MAX_OBSERVED_SSE_EVENTS: u32 = 65_536;
/// What a history walk found for a run's admitted prompt.
enum RunRows {
    /// The prompt's own rows, newest-first.
    Run(Vec<SessionMessageInfoDto>),
    /// The prompt is queued; its not-run-yet notice's words.
    Queued(String),
}

struct RunRendering {
    text: TextRendering,
    jsonl: jsonl::JsonlMessages,
}

/// History pages read back to a run's admitted prompt: 200 default pages of 50
/// rows. A turn longer than that is refused by name, never shown in part.
const MAX_ADMISSION_PAGES: u16 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationCause {
    Sigint,
    Sigterm,
    /// A hangup: the terminal or console that owned this run went away. It
    /// takes the same explicit-stop path as SIGINT/SIGTERM, because a hangup is
    /// a deliberate close, not the silent loss the close grace covers.
    Sighup,
    Acp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationOutcome {
    AlreadyTerminal,
    InterruptSettled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunReceipt {
    pub schema: &'static str,
    #[serde(serialize_with = "serialize_session")]
    pub session: SessionId,
    pub prompted: bool,
    pub cancelled: Option<CancellationCause>,
    pub cancellation_outcome: Option<CancellationOutcome>,
    pub exit: ExitClass,
    pub pre_visible_attempts: u32,
}

#[derive(Debug, Error)]
pub enum RunError {
    #[error("API client refused the operation: {0}")]
    Api(#[from] ApiClientError),
    #[error("output refused the operation: {0}")]
    Output(#[from] OutputError),
    #[error("cannot start in the working directory: {0}")]
    WorkingDirectory(String),
    #[error("no session matched the requested project")]
    NoSession,
    #[error(
        "matching sessions have unreadable metadata; select another session or start a new one"
    )]
    SessionMetadataUnavailable,
    #[error("session selection reached its page bound or a page made no progress; select a session explicitly")]
    SessionSelectionIncomplete,
    #[error("could not fork session {session}: {source}")]
    Fork {
        session: SessionId,
        source: ApiClientError,
    },
    #[error(
        "session {session} belongs to project {found}, not {requested}; select a session of the requested project or omit --project"
    )]
    WrongProject {
        session: SessionId,
        requested: ProjectId,
        found: ProjectId,
    },
    #[error("cancellation raced with terminal settlement")]
    CancellationRace,
    #[error("retry bound is zero or exceeds the production maximum")]
    RetryBound,
    #[error("signal stream failed: {0:?}")]
    Signal(std::io::ErrorKind),
    #[error("session published no terminal truth and no durable progress within {maximum} consecutive quiet polls{pending}")]
    TerminalTimeout { maximum: u16, pending: String },
    #[error("advisory event was refused: {0}")]
    Advisory(#[from] AdvisoryError),
    #[error("advisory cursor convergence was refused: {0}")]
    Convergence(#[from] ConvergenceError),
    #[error("the Session's execution failed: {0}")]
    Execution(String),
    /// The prompt was admitted and stays queued; its turn was blocked.
    #[error("{0}")]
    NotRunYet(String),
    #[error("session {session} history never showed this run's admitted prompt {message}")]
    AdmissionUnseen { session: SessionId, message: String },
    #[error("this run's rows exceeded {maximum} history pages before its admitted prompt")]
    AdmissionPageBound { maximum: u16 },
    #[error("SSE reconnected more than {maximum} consecutive times without durable progress")]
    SseReconnectBound { maximum: u8 },
    #[error("observed SSE events exceeded their finite {maximum}-event bound")]
    SseEventBound { maximum: u32 },
}

/// Waiting without durable progress, bounded.
///
/// An event stream ends routinely (the request allowance ends a healthy one every
/// 30 s), so neither bound counts wall time or total reconnects. Both count only since
/// the last change in the session's durable messages or permissions, as read from the
/// authoritative relists. A turn that keeps progressing survives any number of routine
/// stream ends and quiet polls; a server that stays up without progressing is refused
/// after `MAX_SSE_RECONNECTS` consecutive reconnects or `MAX_TERMINAL_POLLS` consecutive
/// quiet polls. A lost server never reaches either bound: its relist, activity read or
/// resubscription fails and ends the run at once.
#[derive(Debug, Default)]
struct ProgressBound {
    reconnects: u8,
    quiet_polls: u16,
    progressed_since_reconnect: bool,
}

impl ProgressBound {
    fn observe(&mut self, changed: bool) {
        if changed {
            self.quiet_polls = 0;
            self.progressed_since_reconnect = true;
        }
    }

    fn reconnect(&mut self) -> Result<(), RunError> {
        if std::mem::take(&mut self.progressed_since_reconnect) {
            self.reconnects = 0;
        }
        self.reconnects = self.reconnects.saturating_add(1);
        if self.reconnects > MAX_SSE_RECONNECTS {
            return Err(RunError::SseReconnectBound {
                maximum: MAX_SSE_RECONNECTS,
            });
        }
        Ok(())
    }

    fn quiet_poll_exhausted(&mut self) -> bool {
        self.quiet_polls = self.quiet_polls.saturating_add(1);
        self.quiet_polls >= MAX_TERMINAL_POLLS
    }
}

#[derive(Clone)]
pub struct Runner {
    client: ApiClient,
    input_id: Option<MessageId>,
    reasoning: bool,
    reasoning_effort: Option<String>,
    selection: crate::StartupSelection,
    max_steps: Option<std::num::NonZeroU32>,
    fork: bool,
    worktree: Option<std::path::PathBuf>,
    preset: Option<&'static str>,
    auto: crate::AutoReply,
    resident: bool,
    title: Option<String>,
    images: Vec<grapt_api::u3a::PromptInputFileAttachmentDto>,
}
impl std::fmt::Debug for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runner")
            .field("images", &self.images.len())
            .finish_non_exhaustive()
    }
}

impl RunError {
    pub const fn exit_class(&self) -> ExitClass {
        match self {
            Self::Output(_) => ExitClass::Output,
            Self::Api(error) | Self::Fork { source: error, .. } => error.exit_class(),
            Self::NoSession => ExitClass::NotFound,
            Self::WrongProject { .. } => ExitClass::Refused,
            Self::WorkingDirectory(_) | Self::RetryBound => ExitClass::Usage,
            _ => ExitClass::Remote,
        }
    }
}

impl Runner {
    pub fn new(client: ApiClient) -> Self {
        Self {
            client,
            input_id: None,
            reasoning: false,
            reasoning_effort: None,
            selection: crate::StartupSelection::default(),
            max_steps: None,
            fork: false,
            worktree: None,
            preset: None,
            auto: crate::AutoReply::Off,
            resident: false,
            title: None,
            images: Vec::new(),
        }
    }

    pub fn with_images(
        mut self,
        images: Vec<grapt_api::u3a::PromptInputFileAttachmentDto>,
    ) -> Self {
        self.images = images;
        self
    }

    /// Choose a model and/or agent for a new Session, without changing defaults.
    pub fn with_selection(mut self, selection: crate::StartupSelection) -> Self {
        self.selection = selection;
        self
    }

    /// Select one offered effort together with the session's model before admission.
    pub fn with_reasoning_effort(mut self, effort: Option<String>) -> Self {
        self.reasoning_effort = effort;
        self
    }

    /// Runs the Session at this permission preset (`--inspect`, `--author`,
    /// `--maintainer`).
    pub fn with_preset(mut self, preset: Option<&'static str>) -> Self {
        self.preset = preset;
        self
    }

    /// Answers the turn's permission requests (`--auto`, `--yolo`,
    /// `--dangerously-skip-permissions`).
    pub fn with_auto(mut self, auto: crate::AutoReply) -> Self {
        self.auto = auto;
        self
    }

    /// Also renders reasoning (`Thinking: ...`) in a text-mode run.
    pub fn with_reasoning(mut self, reasoning: bool) -> Self {
        self.reasoning = reasoning;
        self
    }

    /// Keep the headless client attached after its initial prompt settles.
    pub fn with_resident(mut self, resident: bool) -> Self {
        self.resident = resident;
        self
    }

    /// Names the Session the run targets (`--title`) before the prompt.
    pub fn with_title(mut self, title: Option<String>) -> Self {
        self.title = title;
        self
    }

    /// Forks the selected Session and runs in the child (`--fork`).
    pub fn with_fork(mut self, fork: bool) -> Self {
        self.fork = fork;
        self
    }

    /// Creates a detached checkout for a new local Session.
    pub fn with_worktree(mut self, destination: Option<std::path::PathBuf>) -> Self {
        self.worktree = destination;
        self
    }

    /// A runner for a task of its own; an embedded child stays owned by `self`.
    pub(crate) fn for_task(&self) -> Self {
        Self {
            client: self.client.without_child_ownership(),
            input_id: self.input_id.clone(),
            reasoning: self.reasoning,
            reasoning_effort: self.reasoning_effort.clone(),
            selection: self.selection.clone(),
            max_steps: self.max_steps,
            fork: self.fork,
            worktree: self.worktree.clone(),
            preset: self.preset,
            auto: self.auto,
            resident: self.resident,
            title: self.title.clone(),
            images: self.images.clone(),
        }
    }

    pub async fn attach<O: Write, E: Write>(
        &self,
        selector: SessionSelector,
        output: &mut BoundedOutput<O, E>,
    ) -> Result<RunReceipt, RunError> {
        let (session, attempts) = self.select(selector, output.receipt()).await?;
        let response = self.client.session(&session).await?;
        emit(output, &response, &format!("attached {session}"))?;
        Ok(RunReceipt {
            schema: "grapt.cli.run-receipt.v1",
            session,
            prompted: false,
            cancelled: None,
            cancellation_outcome: None,
            exit: ExitClass::Success,
            pre_visible_attempts: attempts,
        })
    }

    pub async fn run_prompt<O: Write, E: Write>(
        &self,
        selector: SessionSelector,
        prompt: String,
        output: &mut BoundedOutput<O, E>,
    ) -> Result<RunReceipt, RunError> {
        let (session, attempts) = self.select_for_run(selector, output.receipt()).await?;
        self.run_selected(session, attempts, prompt, output).await
    }

    #[expect(clippy::cognitive_complexity, reason = "legacy")]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub async fn run_selected<O: Write, E: Write>(
        &self,
        session: SessionId,
        attempts: u32,
        prompt: String,
        output: &mut BoundedOutput<O, E>,
    ) -> Result<RunReceipt, RunError> {
        output.retain_on_stdout_disconnect(self.resident);
        // Subscribe before the non-idempotent admission so the advisory stream
        // cannot create a prompt-to-subscribe miss window.
        let mut events = self
            .client
            .events_with_approver((self.auto != crate::AutoReply::Off).then_some(&session))
            .await?;
        let accepted = self.admit_prompt(&session, prompt).await?;
        progress(output, &accepted, &format!("prompt accepted for {session}"))?;
        // Admitted but blocked before its turn began: the acceptance says
        // why, whether or not the durable notice could be written.
        if let Some(queued) = accepted.queued().as_present() {
            return Err(RunError::NotRunYet(format!(
                "Not run yet: {}. Your message is queued and runs on the next turn once this is fixed.",
                queued.reason().get().as_str()
            )));
        }
        let admitted = accepted.data().get().id().get().as_inner().to_string();
        let mut rendering = RunRendering {
            text: TextRendering::new(self.reasoning),
            jsonl: Default::default(),
        };
        let limits = self.client.limits();
        let mut convergence = CursorConvergence::new(limits);
        let mut waiting = ProgressBound::default();
        let mut observed_events = 0_u32;
        let mut last_messages = None;
        let mut last_permissions = None;
        let mut terminal = false;
        // One pinned sleep for the whole loop: a per-iteration sleep would
        // let a steady event stream defer the activity poll without bound.
        let poll = tokio::time::sleep(TERMINAL_POLL_INTERVAL);
        tokio::pin!(poll);
        loop {
            tokio::select! {
                event = events.next() => match event {
                    Ok(event) => {
                        observed_events = observed_events.saturating_add(1);
                        if observed_events > MAX_OBSERVED_SSE_EVENTS {
                            return Err(RunError::SseEventBound {
                                maximum: MAX_OBSERVED_SSE_EVENTS,
                            });
                        }
                        let belongs = event
                            .session_id()
                            .is_none_or(|candidate| candidate == &session);
                        if belongs {
                            if let Some(record) = event.cursor_record()? {
                                let page = CursorPage::try_new(vec![record], None, limits)?;
                                convergence.admit_advisory(page)?;
                            }
                            if event.requires_durable_relist() {
                                let (changed, _) = self
                                    .emit_changed_messages(
                                        &session,
                                        &admitted,
                                        &mut last_messages,
                                        &mut rendering,
                                        output,
                                        false,
                                    )
                                    .await?;
                                waiting.observe(changed);
                                waiting.observe(
                                    self.emit_changed_permissions(
                                        &session,
                                        &mut last_permissions,
                                        output,
                                    )
                                    .await?,
                                );
                            }
                            // Advisory bytes never become terminal or user-visible
                            // truth. They only trigger authoritative durable reads.
                            if event.is_terminal() && !self.client.session_active(&session).await? {
                                terminal = true;
                                break;
                            }
                        }
                    }
                    Err(_advisory_error) => {
                        // SSE is advisory. Re-list durable truth before a bounded,
                        // idempotent transport rebind; no provider/tool effect is retried.
                        // A stream also ends routinely (server reconciliation, the
                        // idle allowance), so only reconnects without durable
                        // progress between them count.
                        let (changed, _) = self
                            .emit_changed_messages(
                                &session,
                                &admitted,
                                &mut last_messages,
                                &mut rendering,
                                output,
                                false,
                            )
                            .await?;
                        waiting.observe(changed);
                        waiting.observe(
                            self.emit_changed_permissions(&session, &mut last_permissions, output)
                                .await?,
                        );
                        if !self.client.session_active(&session).await? {
                            if self.resident {
                                events = self.client.events_with_approver(
                                    (self.auto != crate::AutoReply::Off).then_some(&session),
                                ).await?;
                            }
                            terminal = true;
                            break;
                        }
                        waiting.reconnect()?;
                        events = self.client.events_with_approver((self.auto != crate::AutoReply::Off).then_some(&session)).await?;
                    }
                },
                () = &mut poll => {
                    waiting.observe(
                        self.emit_changed_permissions(&session, &mut last_permissions, output)
                            .await?,
                    );
                    if !self.client.session_active(&session).await? {
                        terminal = true;
                        break;
                    }
                    if waiting.quiet_poll_exhausted() {
                        break;
                    }
                    poll.as_mut().reset(tokio::time::Instant::now() + TERMINAL_POLL_INTERVAL);
                }
            }
        }
        if !terminal {
            return Err(RunError::TerminalTimeout {
                maximum: MAX_TERMINAL_POLLS,
                pending: permission_stall::pending_permission_stall(&last_permissions, &session),
            });
        }
        let (_, run) = self
            .emit_changed_messages(
                &session,
                &admitted,
                &mut last_messages,
                &mut rendering,
                output,
                true,
            )
            .await?;
        if let Some(failure) = execution_failure(&run) {
            return Err(RunError::Execution(failure));
        }
        let receipt = RunReceipt {
            schema: "grapt.cli.run-receipt.v1",
            session,
            prompted: true,
            cancelled: None,
            cancellation_outcome: None,
            exit: ExitClass::Success,
            pre_visible_attempts: attempts,
        };
        if self.resident {
            // The Session's approver attachment must outlive this prompt.
            output.machine(&receipt)?;
            resident::follow(
                self,
                &receipt.session,
                events,
                output,
                rendering.jsonl,
                last_permissions,
            )
            .await?;
        }
        Ok(receipt)
    }

    /// Machine modes emit each changed page; text mode renders the answer.
    /// Returns whether the newest page changed (durable progress) and, in text
    /// mode or at terminal, this run's rows newest-first. At terminal a run
    /// whose admitted prompt its history never shows is refused, not a success.
    async fn emit_changed_messages<O: Write, E: Write>(
        &self,
        session: &SessionId,
        admitted: &str,
        last: &mut Option<Vec<u8>>,
        rendering: &mut RunRendering,
        output: &mut BoundedOutput<O, E>,
        terminal: bool,
    ) -> Result<(bool, Vec<SessionMessageInfoDto>), RunError> {
        let messages = self.client.messages(session).await?;
        let canonical = serde_json::to_vec(&messages).map_err(|_| ApiClientError::Encode)?;
        let changed = last.as_ref() != Some(&canonical);
        let text = matches!(output.mode(), OutputMode::Text);
        if changed {
            if matches!(output.mode(), OutputMode::Json) {
                output.machine(&messages)?;
            }
            *last = Some(canonical);
        }
        if matches!(output.mode(), OutputMode::Jsonl) {
            rendering.jsonl.emit(session, &messages, terminal, output)?;
        }
        if !text && !terminal {
            return Ok((changed, Vec::new()));
        }
        let run = match self.since_admission(session, admitted, messages).await? {
            Some(RunRows::Run(run)) => run,
            // A queued prompt is not history yet; its not-run-yet notice is.
            Some(RunRows::Queued(notice)) if terminal => return Err(RunError::NotRunYet(notice)),
            Some(RunRows::Queued(_)) => Vec::new(),
            None if terminal => {
                return Err(RunError::AdmissionUnseen {
                    session: session.clone(),
                    message: admitted.to_owned(),
                });
            }
            // The prompt is not durable yet: nothing of this run is either.
            None => Vec::new(),
        };
        if text {
            rendering.text.render(&run, output, terminal)?;
        }
        Ok((changed, run))
    }

    /// This run's rows, newest-first: every row newer than its admitted prompt.
    /// History is paged newest-first from `newest`, so a turn longer than one
    /// page is read back through the page cursors, bounded. A queued prompt is
    /// not history until it runs, so the walk that does not meet the prompt
    /// first meets that prompt's own not-run-yet notice, at any depth. `None`
    /// when the history ends with neither.
    async fn since_admission(
        &self,
        session: &SessionId,
        admitted: &str,
        newest: SessionMessagesResponseDto,
    ) -> Result<Option<RunRows>, RunError> {
        let mut run = Vec::new();
        let mut page = newest;
        for _page in 0..MAX_ADMISSION_PAGES {
            let rows = page.data().get().as_slice();
            if let Some(position) = admission(rows, admitted) {
                run.extend_from_slice(&rows[..position]);
                return Ok(Some(RunRows::Run(run)));
            }
            // A prompt that ran later is newer than its notice, so meeting
            // the notice first means the prompt is still queued.
            if let Some(notice) = not_run_yet_for(rows, admitted) {
                return Ok(Some(RunRows::Queued(notice)));
            }
            run.extend_from_slice(rows);
            let next = page
                .cursor()
                .get()
                .next()
                .as_value()
                .map(|cursor| cursor.as_str().to_owned());
            let Some(cursor) = next.filter(|_| !rows.is_empty()) else {
                return Ok(None);
            };
            page = self.client.messages_at(session, &cursor).await?;
        }
        Err(RunError::AdmissionPageBound {
            maximum: MAX_ADMISSION_PAGES,
        })
    }

    async fn emit_changed_permissions<O: Write, E: Write>(
        &self,
        session: &SessionId,
        last: &mut Option<Vec<u8>>,
        output: &mut BoundedOutput<O, E>,
    ) -> Result<bool, RunError> {
        let permissions = self.client.session_permissions(session).await?;
        let canonical = serde_json::to_vec(&permissions).map_err(|_| ApiClientError::Encode)?;
        if last.as_ref() == Some(&canonical) {
            return Ok(false);
        }
        if !permissions.data().get().as_slice().is_empty() {
            progress(
                output,
                &permissions,
                &format!("pending permissions for {session}"),
            )?;
            if self.auto != crate::AutoReply::Off {
                self.auto_reply(session, &permissions, output).await?;
            }
        }
        *last = Some(canonical);
        Ok(true)
    }

    /// Automatic replies use the server's semantic classification. Authority
    /// changes and unknown consequences always require an explicit decision;
    /// ordinary removal also requires one under `--auto`.
    async fn auto_reply<O: Write, E: Write>(
        &self,
        session: &SessionId,
        permissions: &grapt_api::PermissionListResponseDto,
        output: &mut BoundedOutput<O, E>,
    ) -> Result<(), RunError> {
        let rows = serde_json::to_value(permissions).map_err(|_| ApiClientError::Encode)?;
        for row in rows["data"].as_array().into_iter().flatten() {
            let (Some(id), Some(action)) = (row["id"].as_str(), row["action"].as_str()) else {
                continue;
            };
            if row["status"] != "pending" {
                continue;
            }
            if auto_reply_for(action, self.auto, row.get("metadata")) == "reject" {
                let resources: Vec<&str> = row["resources"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .collect();
                let reason = auto_reject_reason(action, row.get("metadata"));
                let refusal = format!(
                    "refused {action} ({}): {reason}. Approve it interactively.",
                    resources.join(", ")
                );
                output.diagnostic(&refusal)?;
                self.client
                    .reply_permission(session, id, "reject", Some(&refusal))
                    .await?;
            } else {
                self.client
                    .reply_permission(session, id, "once", None)
                    .await?;
            }
        }
        Ok(())
    }

    pub async fn select_session(
        &self,
        selector: SessionSelector,
        output: OutputReceipt,
    ) -> Result<(SessionId, u32), RunError> {
        self.select_for_run(selector, output).await
    }

    pub async fn cancel(
        &self,
        session: SessionId,
        cause: CancellationCause,
    ) -> Result<RunReceipt, RunError> {
        if !self.client.session_active(&session).await? {
            return Ok(cancellation_receipt(
                session,
                None,
                CancellationOutcome::AlreadyTerminal,
                ExitClass::Success,
            ));
        }
        if let Err(interrupt_error) = self.client.interrupt(&session).await {
            if !self.client.session_active(&session).await? {
                return Ok(cancellation_receipt(
                    session,
                    None,
                    CancellationOutcome::AlreadyTerminal,
                    ExitClass::Success,
                ));
            }
            return Err(RunError::Api(interrupt_error));
        }
        for _poll in 0..MAX_CANCELLATION_POLLS {
            if !self.client.session_active(&session).await? {
                return Ok(cancellation_receipt(
                    session,
                    Some(cause),
                    CancellationOutcome::InterruptSettled,
                    ExitClass::Cancelled,
                ));
            }
            tokio::time::sleep(CANCELLATION_POLL_INTERVAL).await;
        }
        Err(RunError::CancellationRace)
    }

    /// Selects a run's Session as the TypeScript `run` did (session-target.ts):
    /// a resume with nothing to resume starts a new Session, and `--fork` forks
    /// the selected Session and runs in the child. A new Session is never forked.
    async fn select_for_run(
        &self,
        selector: SessionSelector,
        output: OutputReceipt,
    ) -> Result<(SessionId, u32), RunError> {
        if self.input_id.is_some() && (!matches!(selector, SessionSelector::Id { .. }) || self.fork)
        {
            return Err(RunError::Execution(
                crate::CommandError::InputIdTarget.to_string(),
            ));
        }
        if !self.selection.is_empty() && (!matches!(selector, SessionSelector::New) || self.fork) {
            return Err(RunError::WorkingDirectory(
                crate::CommandError::SelectionTarget.to_string(),
            ));
        }
        if self.worktree.is_some()
            && (!matches!(selector, SessionSelector::New)
                || self.fork
                || self.client.endpoint().mode() == crate::client::ClientMode::RemoteTls)
        {
            return Err(RunError::WorkingDirectory(
                "a worktree requires a new session on a local service".into(),
            ));
        }
        if matches!(selector, SessionSelector::New) {
            return location::create(self).await;
        }
        let scope = match &selector {
            SessionSelector::ResumeLast { project } => project.clone(),
            _ => None,
        };
        let (session, attempts) = match self.select(selector, output).await {
            Err(RunError::NoSession) => {
                return match scope {
                    None => location::create(self).await,
                    Some(project) => self
                        .create_in_project(&project)
                        .await
                        .map(|session| (session, 1)),
                };
            }
            selected => selected?,
        };
        if !self.fork {
            return Ok((session, attempts));
        }
        let forked = self
            .client
            .fork_session(&session)
            .await
            .map_err(|source| RunError::Fork {
                session: session.clone(),
                source,
            })?;
        Ok((forked.data().get().id().get().as_inner().clone(), attempts))
    }

    /// A new Session placed in `project`'s worktree: `--project` scopes an
    /// empty resume as it scopes `-s`. An unknown project starts nothing.
    async fn create_in_project(&self, project: &ProjectId) -> Result<SessionId, RunError> {
        let projects = self.client.projects().await?;
        let worktree = projects
            .as_inner()
            .as_slice()
            .iter()
            .find(|known| known.id().get().as_inner() == project)
            .map(|known| known.worktree().get().clone())
            .ok_or(RunError::NoSession)?;
        let created = self.client.create_session_at(worktree).await?;
        let data = created.data().get();
        let session = data.id().get().as_inner().clone();
        let found = data.project_id().get().as_inner().clone();
        if &found != project {
            return Err(RunError::WrongProject {
                session,
                requested: project.clone(),
                found,
            });
        }
        Ok(session)
    }

    async fn select(
        &self,
        selector: SessionSelector,
        output: OutputReceipt,
    ) -> Result<(SessionId, u32), RunError> {
        match selector {
            SessionSelector::Id { session, project } => {
                if let Some(requested) = project {
                    let response = self.client.session(&session).await?;
                    let found = response.data().get().project_id().get().as_inner().clone();
                    if found != requested {
                        return Err(RunError::WrongProject {
                            session,
                            requested,
                            found,
                        });
                    }
                }
                Ok((session, 1))
            }
            SessionSelector::New => {
                let response = self.client.create_session().await?;
                Ok((response.data().get().id().get().as_inner().clone(), 1))
            }
            SessionSelector::ResumeLast { project } => {
                let mut attempts = 0_u32;
                loop {
                    attempts = attempts.saturating_add(1);
                    match newest_for_project(&self.client, project.as_ref()).await {
                        Ok(session) => return Ok((session, attempts)),
                        Err(RunError::Api(ApiClientError::Transport(_)))
                            if self.retry_gate().allows(output.visible, attempts, true) => {}
                        Err(error) => return Err(error),
                    }
                }
            }
        }
    }

    const fn retry_gate(&self) -> RetryGate {
        RetryGate {
            maximum_attempts: crate::bounds::MAX_PRE_VISIBLE_RETRIES,
        }
    }
}

fn cancellation_receipt(
    session: SessionId,
    cancelled: Option<CancellationCause>,
    cancellation_outcome: CancellationOutcome,
    exit: ExitClass,
) -> RunReceipt {
    RunReceipt {
        schema: "grapt.cli.run-receipt.v1",
        session,
        prompted: false,
        cancelled,
        cancellation_outcome: Some(cancellation_outcome),
        exit,
        pre_visible_attempts: 1,
    }
}

fn serialize_session<S>(session: &SessionId, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.collect_str(session)
}

/// A run's progress: machine modes emit the record, text mode keeps stdout for
/// the answer and writes the line to stderr.
fn progress<O: Write, E: Write, T: Serialize>(
    output: &mut BoundedOutput<O, E>,
    machine: &T,
    text: &str,
) -> Result<(), OutputError> {
    match output.mode() {
        OutputMode::Text => output.diagnostic(text),
        OutputMode::Json | OutputMode::Jsonl => output.machine(machine),
    }
}

fn emit<O: Write, E: Write, T: Serialize>(
    output: &mut BoundedOutput<O, E>,
    machine: &T,
    text: &str,
) -> Result<(), OutputError> {
    match output.mode() {
        OutputMode::Text => output.text(text),
        OutputMode::Json | OutputMode::Jsonl => output.machine(machine),
    }
}

/// The reply `--auto` gives a pending request for `action`: `once`, or
/// `reject` for one that would remove a file from its path, which no
/// recovery stands behind. Authority-source or unclassified requests require
/// an explicit decision under either automatic mode.
pub(crate) fn auto_reply_for(
    action: &str,
    auto: crate::AutoReply,
    metadata: Option<&serde_json::Value>,
) -> &'static str {
    let policy = metadata.and_then(|value| value.get("policy"));
    let owner_floor = policy
        .and_then(|value| value.get("ownerFloor"))
        .and_then(serde_json::Value::as_bool);
    let removal = policy
        .and_then(|value| value.get("removesFiles"))
        .and_then(serde_json::Value::as_bool);
    let (Some(owner_floor), Some(removal)) = (owner_floor, removal) else {
        return "reject";
    };
    let removes = matches!(action, "fs.delete" | "fs.move") || removal;
    let lowers_containment = action.starts_with("sandbox.");
    if owner_floor || lowers_containment || (removes && auto == crate::AutoReply::Auto) {
        "reject"
    } else {
        "once"
    }
}

pub(crate) fn auto_reject_reason(
    action: &str,
    metadata: Option<&serde_json::Value>,
) -> &'static str {
    if metadata
        .and_then(|value| value.get("policy"))
        .and_then(|value| value.get("ownerFloor"))
        == Some(&serde_json::Value::Bool(true))
    {
        "changes to permission settings require an explicit decision"
    } else if action.starts_with("sandbox.") {
        "lowering or expanding sandbox containment requires explicit interactive consent"
    } else {
        "automatic approval will not remove files or approve an unclassified request"
    }
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
