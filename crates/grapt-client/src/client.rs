//! The session controller: commands out, durable truth in.
//!
//! This is the "session controller" half of what the god-file map asks for in
//! place of the 3,480-line session component. It owns no rendering and no
//! terminal; it owns the loop that keeps a projection equal to durable truth.
//!
//! ## The invariant the whole type exists to hold
//!
//! > After any sequence of live frames, gaps, drops and restarts, the rendered
//! > projection equals the projection folded from durable truth alone.
//!
//! [`Client::pump`] applies live frames for latency. [`Client::reconcile`]
//! rebuilds from the durable re-list. The invariant says the first can only
//! ever anticipate the second, never diverge from it, and
//! `reconciling_converges_to_durable_truth` witnesses exactly that by building
//! both and comparing.
//!
//! That is why reconciliation REPLACES the view rather than patching it.
//! Patching would need the client to know which of its rendered content came
//! from frames the re-list also contains — and getting that wrong duplicates
//! the transcript, which is the defect
//! `reconnect-cursor-convergence-replay` names. Rebuilding cannot duplicate
//! anything, because it starts from nothing.

use std::collections::BTreeMap;

use crate::bounds::Bounds;
use crate::contract::{EventEnvelope, Operation};
use crate::cursor::{Admission, DurableCursor, GapReason, StreamState};
use crate::projection::{ProjectionError, SessionView};
#[cfg(test)]
use crate::transport::Transport;
use crate::transport::{FramePoll, TransportError, WireRequest, WireResponse};
mod claude_plugin;
mod grants;
use crate::wire::{PermissionWireId, SessionWireId};

use history::Page;
use thiserror::Error;

/// The connection the client drives; defined with the transport layer.
pub use grapt_client_transport::connection::{Connection, ResumeToken, StreamBinding};

impl Connection for crate::double::ContractDouble {
    fn subscribed(&self) -> bool {
        crate::double::ContractDouble::subscribed(self)
    }
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        crate::double::ContractDouble::poll_frame(self)
    }
    fn resubscribe(&mut self) {
        crate::double::ContractDouble::resubscribe(self)
    }
    fn retire_stream(&mut self) {
        // The subscription dies and its queued frames with it; durable truth stays.
        crate::double::ContractDouble::restart(self)
    }
}

/// One row in the session picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: SessionWireId,
    pub title: String,
    /// The session that spawned this one, when one did.
    ///
    /// Carried so the picker can say a row belongs to a subagent rather than
    /// to the user. ABSENT MEANS ROOT, not unread: it is only ever set from a
    /// decoded row.
    pub parent: Option<SessionWireId>,
    pub detail: crate::wire::SessionDetail,
}

/// One outstanding permission ask, as the client sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionAsk {
    pub session_id: SessionWireId,
    pub request_id: PermissionWireId,
    pub title: String,
    /// "tool {action}" plus what the ask will do; see the wire projection.
    pub subtitle: String,
    pub live: bool,
    pub allow_always: bool,
    /// The tool call waiting on this ask, when a tool asked.
    pub call: Option<crate::wire::ToolCallWireId>,
    /// What the ask will do, for display; the answer still names `request_id`.
    pub preview: Option<grapt_api::u3a::PermissionPreviewDto>,
}

/// Counters the client can render truthfully.
///
/// These are not telemetry. A user looking at a transcript is entitled to know
/// whether it is complete, and these are how the view says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Diagnostics {
    pub reconciliations: usize,
    pub gaps_observed: usize,
    pub stream_closes: usize,
    /// Live data refused by a transport or client bound.
    pub stream_overflows: usize,
    /// Valid envelopes decoded, including events for another session.
    pub frames_decoded: usize,
    pub frames_applied: usize,
    pub frames_deferred: usize,
    pub duplicates_ignored: usize,
    /// History reads whose walk toward older history FAILED.
    ///
    /// The tail stays live either way; non-zero means some earlier history
    /// could not be read, which the transcript also says.
    pub degraded_reconciliations: usize,
    /// Permission rows the client could not read, or could not key.
    ///
    /// A row naming a session identity the domain refuses is one this client
    /// could never answer. Counting it is what stops an ask vanishing silently
    /// between the server and the screen.
    pub permissions_unreadable: usize,
    /// Permission rows refused by the client's retention bound.
    pub permissions_dropped: usize,
    /// Catalog rows — projects, agents or models — refused by the client's
    /// `max_catalog_rows` bound.
    ///
    /// The bounds now MIRROR the contract's own — 256 for projects and agents,
    /// 2048 for models — so a conformant response is never truncated. This
    /// count remains for anything beyond them, because bounding is correct but
    /// bounding SILENTLY is not: uncounted, a picker showing 512 of 2048 models
    /// looked exactly like a product that has 512, and a user hunting for a
    /// model that is not on screen had no way to learn it was dropped rather
    /// than absent. Absent is not truncated.
    pub catalog_rows_dropped: usize,
}

/// What a refusal MEANS for the person reading it, and whether to try again.
///
/// A bare number is not actionable. `401` and `503` rendered identically as
/// "answered <n>", so a session that needed the user to authenticate looked
/// exactly like one that needed them to wait, and the screen offered no way to
/// tell which. The distinction that matters is whether retrying the same thing
/// can possibly work.
///
/// THE SET IS THE CONTRACT'S OWN, not a general HTTP table. These are the
/// statuses the frozen `openapi.json` documents for these routes, named by the
/// error types it declares for them. An undocumented status gets NO gloss: the
/// number is shown alone rather than dressed in a meaning nothing promised.
fn status_advice(status: u16) -> &'static str {
    match status {
        400 => {
            ": this client sent something the service refuses; retrying it unchanged will not help"
        }
        401 => ": not authenticated; sign in before retrying",
        403 => ": authenticated but not permitted to do this",
        404 => ": it no longer exists here; reopen or pick another",
        409 => ": busy or in conflict with another change; retrying shortly can work",
        413 => ": too large to accept; send less",
        500 => ": the service failed; retrying can work",
        503 => ": the service is unavailable; retrying can work",
        _ => "",
    }
}

/// Why a client operation failed.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ClientError {
    #[error("Rewind outcome unknown. Reopen the affected session before sending or rewinding.")]
    RewindUnconfirmed,
    #[error("File undo has an unconfirmed outcome. Use /undo apply to reconcile before sending another prompt.")]
    UndoUnconfirmed,
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    Projection(#[from] ProjectionError),
    #[error("{operation} answered {status}{}", status_advice(*status))]
    Status {
        operation: &'static str,
        status: u16,
    },
    #[error("Answer refused: {message}")]
    FormInvalidAnswer {
        operation: &'static str,
        form: crate::wire::FormWireId,
        message: String,
    },
    /// A 400 whose body this client CAN read: the frozen error name and
    /// message, carried so the pane can show WHAT was refused rather than a
    /// status number. The refused route's message is part of the answer, not
    /// a detail of the failure.
    #[error("{operation} answered {status}: {message}")]
    Refused {
        operation: &'static str,
        status: u16,
        message: String,
    },
    #[error("{operation} returned a body this client cannot read: {reason}")]
    Body {
        operation: &'static str,
        reason: String,
    },
    #[error("no session is selected")]
    NoSelection,
    #[error("{0}")]
    Preset(String),
    #[error("no project is selected")]
    NoProject,
    #[error("{0}")]
    Mcp(String),
    #[error("{0}")]
    InvalidInput(&'static str),
    #[error("{0}")]
    Worktree(String),
    #[error("that {what} is not one this location offers")]
    NotOffered { what: &'static str },
    #[error("unsupported --reasoning-effort {0:?} for this model")]
    ReasoningEffort(String),
    #[error("that model is disabled at this location and cannot be selected")]
    NotEnabled,
    #[error("select a project, an agent and a model before composing a prompt")]
    SelectionIncomplete,
    #[error("Unconfirmed. Alt+R retries the original.")]
    Unconfirmed,
    #[error("Edit kept. Alt+R retries the original.")]
    UnconfirmedEdit,
    #[error("Submitted; edit kept. Enter sends the edit.")]
    EarlierSubmitted,
    #[error("Nothing to retry; your draft is kept.")]
    NoUnconfirmedPrompt,
    #[error("Could not prepare your prompt; restart the client.")]
    InputIdentityUnavailable,
    #[error("{held} sessions hold a prompt the server never confirmed; send one of those again before starting another")]
    UnconfirmedElsewhere { held: usize },
    #[error(transparent)]
    Wire(#[from] crate::wire::WireError),
}

/// The controller.
#[derive(Debug)]
#[expect(clippy::struct_excessive_bools, reason = "legacy")]
pub struct Client<C: Connection> {
    connection: C,
    bounds: Bounds,
    cursor: DurableCursor,
    recovery: recovery::Recovery,
    tool_refresh: Option<tool_refresh::Walk>,
    activity: BTreeMap<SessionWireId, activity::Observation>,
    observations: BTreeMap<SessionWireId, observation::Presented>,
    history_window: Option<paged_history::Window>,
    /// Projections, keyed by the TYPED session identity.
    ///
    /// A `String` key here was one of the four ownership points review named. It
    /// is the most consequential of them: the map is what makes two sessions
    /// two, so a key that admitted two spellings of one identity would render
    /// one session twice and reconcile neither.
    views: BTreeMap<SessionWireId, SessionView>,
    /// The client's observation epoch: each pump and each direct history
    /// read consumes one, so a live quiescence observation can be ordered
    /// against the install that first carried the terminal it must survive
    ///. Wire stamps cannot
    /// order this: a producer clock can run behind the one that stamped
    /// the durable terminal it is newer than, so ordering is by observation
    /// order, not by stamp.
    quiescence_gen: u64,
    /// The sessions whose newest quiescence fact this client observed on
    /// the LIVE stream, and the epoch of that observation. A busy fact
    /// survives a relist of an already-held terminal only when it was
    /// observed after that terminal entered the installed history: the
    /// next preparation's busy survives the relist of the preceding
    /// execution's terminal, and a turn's own busy yields to its terminal.
    /// A stream cut clears the map with the facts. Bounded by the sessions
    /// this client holds views for.
    quiescence_fact_epoch: std::collections::HashMap<SessionWireId, u64>,
    /// The sessions whose newest terminal is known to this client, and the
    /// epoch that terminal was first known: a live terminal frame records
    /// its own observation, an install the epoch at which a terminal it
    /// newly carries first enters the history. A newer terminal replaces
    /// the record; repeated relists of one terminal advance nothing, so
    /// they never age out a fact genuinely newer than it. Bounded the same
    /// way.
    quiescence_terminal_epoch: std::collections::HashMap<SessionWireId, u64>,
    sessions: Vec<SessionSummary>,
    permissions: Vec<PermissionAsk>,
    permission_revision: u64,
    /// Bumped by every acknowledged close, rename, create, reply or cancel, so
    /// an off-loop snapshot issued before it is never applied over it.
    acknowledged: u64,
    grants: grants::State,
    selected: Option<SessionWireId>,
    diagnostics: Diagnostics,
    /// The project gateway's rows, and the location every catalog read and
    /// every new session is scoped by.
    projects: Vec<crate::wire::ProjectRow>,
    project: Option<crate::wire::ProjectRow>,
    launch_project: Option<crate::wire::ProjectRow>,
    launch_directory: Option<String>,
    /// The roster and picker for the selected project, and what was chosen
    /// from each. The chosen agent is an IDENTITY; the chosen model is the
    /// whole row, because the composer footer names it and the switch route
    /// needs its provider.
    agents: Vec<crate::wire::AgentRow>,
    agent: Option<crate::wire::AgentWireId>,
    models: Vec<crate::wire::ModelRow>,
    model: Option<crate::wire::ModelRow>,
    default_model: Option<crate::wire::ModelRow>,
    /// The subagent model for the selected session, once read. The outer
    /// `Option` is "read at all"; the inner one is the contract's NULL.
    subagent_model: Option<Option<crate::wire::ModelRow>>,
    /// Pending forms for the SELECTED session.
    ///
    /// Read from `v2.session.form.list` rather than from the floor-wide
    /// `v2.form.request.list`, because a form belongs to the run that raised
    /// it: rendering every pending form under whichever session happens to be
    /// open would attribute another session's question to this transcript.
    forms: Vec<crate::wire::FormRow>,
    location_forms: location_forms::Retained,
    location_forms_dirty: bool,
    session_forms_dirty: bool,
    /// Counts every raise of `session_forms_dirty`, so an off-loop answer
    /// clears only the hints no newer than its own request.
    session_form_hints: u64,
    /// The configured MCP servers for the selected project, once read.
    mcp_servers: Vec<crate::wire::McpServerRow>,
    mcp_watches: Vec<grapt_api::u3a::McpResourceWatchDto>,
    mcp_watch_owner: Option<(Option<SessionWireId>, crate::wire::FormLocation)>,
    mcp_refresh_needed: bool,
    /// Config refusal or native inspection read failure, shown instead of rows.
    mcp_refusal: Option<String>,
    mcp_login: Option<mcp_auth::PendingLogin>,
    mcp_approval: Option<mcp_auth::PendingApproval>,
    mcp_auth: Option<mcp_auth::InFlight>,
    mcp_content: Option<mcp_content::InFlight>,
    side_session: Option<SessionWireId>,
    service_stop_notice: Option<String>,
    /// A stream overflow was observed and its durable baseline has not been
    /// re-established yet.
    ///
    /// Unlike the `stream_overflows` counter, which only ever grows, this is
    /// true for exactly the recovery it describes: set when live data is
    /// refused, cleared when a re-read baseline installs. Every later
    /// reconnect would otherwise re-announce an overflow that has long since
    /// healed.
    gap_recovery_pending: bool,
    /// The resume frontier, the retained suffix after it, and what a cut
    /// holds aside until the next stream's first frame answers.
    resume: resume::Resume,
    /// A gateway directory offer's read is in flight on the read lane.
    directory_offer: bool,
    positions: snapshot::Positions,
    worktree: worktree::State,
    claude_plugin: claude_plugin::State,
    mcp_read: Option<catalogs::McpRead>,
    status_account: Result<Option<String>, String>,
    workspace_diff: Result<Option<crate::wire::WorkspaceDiff>, String>,
    /// Where a prompt's input id comes from ([`crate::input_ids`]).
    input_ids: Option<std::sync::Arc<dyn grapt_types::IdAuthority + Send + Sync>>,
    /// Unresolved submissions retain their original text and identity per session.
    /// Definitely unsent drafts are not retained; switching sessions keeps uncertainty.
    pending: BTreeMap<SessionWireId, submission::PendingPrompt>,
    /// User rows appended by live promotion since selecting this session.
    promoted_inputs: std::collections::VecDeque<crate::wire::MessageWireId>,
    rewind_uncertain: Option<SessionWireId>,
    undo_uncertain: bool,
    undo_review_open: bool,
    undo_preview: Option<crate::wire::UndoSelection>,
    sandbox_worktree_config_preview: Option<sandbox_worktree_config::Preview>,
    /// Set when a submission's reply was lost and the session showed it.
    recovered: bool,
    preset: crate::preset::State,
    launch_effort: Option<effort::LaunchEffort>,
    catalog_refresh: catalog_refresh::State,
    command_catalog: command_catalog::State,
    draft_creation: creation::Draft,
}

impl<C: Connection> Client<C> {
    pub fn new(connection: C, bounds: Bounds) -> Self {
        Self {
            connection,
            bounds,
            subagent_model: None,
            cursor: DurableCursor::new(),
            recovery: recovery::Recovery::default(),
            tool_refresh: None,
            activity: BTreeMap::new(),
            observations: BTreeMap::new(),
            history_window: None,
            views: BTreeMap::new(),
            quiescence_gen: 0,
            quiescence_fact_epoch: std::collections::HashMap::new(),
            quiescence_terminal_epoch: std::collections::HashMap::new(),
            sessions: Vec::new(),
            permissions: Vec::new(),
            permission_revision: 0,
            acknowledged: 0,
            grants: grants::State::default(),
            selected: None,
            diagnostics: Diagnostics::default(),
            projects: Vec::new(),
            project: None,
            launch_project: None,
            launch_directory: None,
            agents: Vec::new(),
            agent: None,
            models: Vec::new(),
            model: None,
            default_model: None,
            forms: Vec::new(),
            location_forms: location_forms::Retained::default(),
            location_forms_dirty: true,
            session_forms_dirty: true,
            session_form_hints: 0,
            mcp_servers: Vec::new(),
            mcp_watches: Vec::new(),
            mcp_watch_owner: None,
            mcp_refresh_needed: false,
            mcp_refusal: None,
            mcp_login: None,
            mcp_approval: None,
            mcp_auth: None,
            mcp_content: None,
            side_session: None,
            service_stop_notice: None,
            gap_recovery_pending: false,
            resume: resume::Resume::default(),
            directory_offer: false,
            positions: snapshot::Positions::default(),
            worktree: worktree::State::default(),
            claude_plugin: claude_plugin::State::default(),
            mcp_read: None,
            status_account: Ok(None),
            workspace_diff: Ok(None),
            input_ids: crate::input_ids::ClientInputIds::seed().map(|ids| {
                std::sync::Arc::new(ids)
                    as std::sync::Arc<dyn grapt_types::IdAuthority + Send + Sync>
            }),
            pending: BTreeMap::new(),
            promoted_inputs: std::collections::VecDeque::new(),
            rewind_uncertain: None,
            undo_uncertain: false,
            undo_review_open: false,
            undo_preview: None,
            sandbox_worktree_config_preview: None,
            recovered: false,
            preset: crate::preset::State::default(),
            launch_effort: None,
            catalog_refresh: catalog_refresh::State::default(),
            command_catalog: command_catalog::State::default(),
            draft_creation: creation::Draft::default(),
        }
    }

    /// Mint prompt input ids from `ids` rather than the client's own source:
    /// a deterministic one in a witness.
    pub fn with_input_ids(
        mut self,
        ids: std::sync::Arc<dyn grapt_types::IdAuthority + Send + Sync>,
    ) -> Self {
        self.input_ids = Some(ids);
        self
    }

    /// The bound set this client was built with.
    ///
    /// The app reads it so the composer can refuse a paste the wire would
    /// refuse, rather than letting the user finish a prompt that cannot be
    /// sent. One bound set, read where it is needed, not copied.
    pub fn bounds(&self) -> Bounds {
        self.bounds
    }

    pub fn sessions(&self) -> &[SessionSummary] {
        &self.sessions
    }

    pub fn permissions(&self) -> &[PermissionAsk] {
        &self.permissions
    }

    pub fn selected(&self) -> Option<&SessionWireId> {
        self.selected.as_ref()
    }

    pub fn diagnostics(&self) -> Diagnostics {
        self.diagnostics
    }

    /// How many rows of the current baseline this client read but cannot draw.
    ///
    /// Exposed so the screen can say so. It is NOT a gap: see
    /// `DurableCursor::presentation_unsupported`.
    pub fn unsupported_rows(&self) -> usize {
        self.cursor.unsupported_rows()
    }

    pub fn stream_state(&self) -> &StreamState {
        self.cursor.state()
    }

    /// The projection for the selected session, if one is selected.
    pub fn view(&self) -> Option<&SessionView> {
        self.selected.as_ref().and_then(|id| self.views.get(id))
    }

    pub fn view_of(&self, session: &SessionWireId) -> Option<&SessionView> {
        self.views.get(session)
    }

    /// The ONE place a response enters this client.
    ///
    /// ## Why this function exists at all
    ///
    /// Review blocker 4 reopened at exactly this seam. The byte bound lived in
    /// `raw`, and `json` was a SECOND, separate path that called
    /// `connection.call` itself and handed the body straight to
    /// `serde_json::from_slice`. Permission list and permission reply took that
    /// path. So the bound was real, tested, and simply not on the route half
    /// the operations used — and no test noticed, because every test that
    /// exercised the bound went through `raw`.
    ///
    /// A bound with two ingress paths is a bound on one of them. The cure is
    /// not another check in `json`; it is that there is no longer a second
    /// path. This is the only function in the crate that calls
    /// `Connection::call`, and `tests/single_ingress.rs` asserts that
    /// mechanically over the source, because "we will remember to route
    /// through it" is precisely the assurance that failed here once already.
    ///
    /// The status check comes BEFORE the byte bound deliberately: a 500 with a
    /// huge body should be reported as the 500 it is, not as an oversized
    /// response, or the operator chases the wrong fault.
    fn ingest(
        &mut self,
        operation: Operation,
        session: Option<&SessionWireId>,
        request: Option<&dyn crate::wire::Spelled>,
        body: Option<Vec<u8>>,
        location: Option<&str>,
        page: Option<Page<'_>>,
    ) -> Result<Vec<u8>, ClientError> {
        let wire = self.wire(operation, session, request, body, location, page)?;
        // A synchronous call is host-started I/O: it never coexists with a
        // pending resume.
        self.abandon_resume();
        let response = self.connection.call(&wire)?;
        let bytes = self.accept_response(operation, response)?;
        if offloop::acknowledges(operation) {
            self.acknowledged = self.acknowledged.wrapping_add(1);
        }
        Ok(bytes)
    }

    /// The request `ingest` sends, built from the client's state.
    fn wire(
        &self,
        operation: Operation,
        session: Option<&SessionWireId>,
        request: Option<&dyn crate::wire::Spelled>,
        body: Option<Vec<u8>>,
        location: Option<&str>,
        page: Option<Page<'_>>,
    ) -> Result<WireRequest, ClientError> {
        let mut wire = WireRequest::new_typed(operation, session, request)?;
        if let Some(body) = body {
            wire = wire.with_body(body);
        }
        match page {
            Some(Page::Sessions { search, limit }) => {
                wire = wire
                    .with_query("search", search)
                    .with_query("limit", &limit.to_string())
                    .with_query("order", "desc");
            }
            Some(Page::First { order }) => wire = wire.with_query("order", order),
            Some(Page::Window { cursor, descending }) => {
                wire = paged_history::window_request(
                    session.ok_or(ClientError::NoSelection)?,
                    cursor,
                    descending,
                )?;
            }
            None => {}
        }
        // Location and paging are named parameters rather than an open query
        // builder, keeping route addressing and response validation here.
        let mcp_catalog = matches!(operation, Operation::McpList | Operation::McpInspect);
        // A saved Session may have a nested cwd. Its identity is the complete
        // MCP coordinate; the checkout root would conflict with that placement.
        let location = location.filter(|_| !mcp_catalog || session.is_none());
        if let Some(directory) = location {
            wire = wire.with_query("location[directory]", directory);
        }
        if matches!(
            operation,
            Operation::ProjectCurrent
                | Operation::AgentList
                | Operation::ModelList
                | Operation::ModelDefault
                | Operation::FormRequestList
        ) {
            let query_session = session.or_else(|| {
                self.catalog_refresh
                    .binding
                    .as_ref()
                    .filter(|bound| {
                        location.is_some_and(|dir| {
                            dir == bound.project.worktree || dir == bound.directory
                        })
                    })
                    .map(|bound| &bound.session)
            });
            if let Some(session) = query_session {
                wire = wire.with_query("session", crate::wire::Spelled::spelled(session));
            }
        }
        Ok(wire)
    }

    // -----------------------------------------------------------------------
    // Selection: project, agent, model
    // -----------------------------------------------------------------------
    //
    // The essential journey starts before a session exists. The frozen frontend
    // opens on a project gateway, then an agent roster, and keeps a model picker
    // reachable from the composer; R5 owes that flow rather than a reduced one,
    // and the client is where "before chatting" is enforced rather than merely
    // documented — see [`Client::create_session`], which REFUSES without a
    // complete selection.

    /// Refresh the session picker, decoding the REAL `SessionsResponseDto`.
    pub fn refresh_sessions(&mut self) -> Result<(), ClientError> {
        let bytes = self.ingest(Operation::SessionList, None, None, None, None, None)?;
        self.accept_sessions(&bytes)
    }

    fn accept_sessions(&mut self, bytes: &[u8]) -> Result<(), ClientError> {
        let (rows, more) = crate::wire::decode_sessions_page(bytes)?;
        let incomplete = more || rows.len() > self.bounds.max_retained_sessions;
        let current = self
            .sessions
            .iter()
            .find(|row| Some(&row.id) == self.selected.as_ref())
            .cloned();
        self.sessions = rows
            .into_iter()
            .take(self.bounds.max_retained_sessions)
            .map(|row| SessionSummary {
                // The identity is CARRIED, not re-stringified. `decode_sessions`
                // already parsed it through the real DTO codec; the previous
                // revision took `.as_inner().to_string()` here and threw that
                // parse away one line after making it.
                id: row.id,
                title: row.title,
                parent: row.parent,
                detail: row.detail,
            })
            .collect();
        if let Some(current) =
            current.filter(|row| incomplete && !self.sessions.iter().any(|new| new.id == row.id))
        {
            self.remember_session(current);
        }
        self.activity
            .retain(|id, _| self.sessions.iter().any(|row| &row.id == id));
        let selected = self.selected.clone();
        self.observations.retain(|id, _| {
            selected.as_ref() == Some(id) || self.sessions.iter().any(|row| &row.id == id)
        });
        Ok(())
    }

    /// Create a session and select it, through the REAL request/response DTOs.
    ///
    /// Takes no title, because the contract's `SessionCreateRequestDto` has no
    /// title field — it carries id, agent, model and location only. The server
    /// names the session. The previous revision invented a `{"title": ...}`
    /// request key and the double dutifully honoured it, which is the same
    /// class of defect as the invented response shapes.
    ///
    /// It carries the SELECTION, and refuses without a complete one. A session
    /// created from an empty request is created against the server's defaults,
    /// which is how a client ends up showing "agent: build, model: Fast" over a
    /// session that is neither.
    pub fn create_session(&mut self) -> Result<SessionWireId, ClientError> {
        if !self.ready_to_chat() {
            return Err(ClientError::SelectionIncomplete);
        }
        let project = self.project.as_ref().ok_or(ClientError::NoProject)?;
        let directory = self
            .launch_directory
            .as_ref()
            .filter(|_| self.launch_project.as_ref() == Some(project))
            .unwrap_or(&project.worktree)
            .clone();
        let body = self.session_create_body_in(&directory)?;
        let bytes = self.create_request(body)?;
        self.select_created_session(&bytes)
    }

    /// Select a session, establishing its durable baseline.
    /// Always reconcile: a previous partial history is not a durable baseline.
    pub fn select(&mut self, session: &SessionWireId) -> Result<(), ClientError> {
        if self.selected.as_ref() != Some(session) {
            self.cancel_mcp_catalog();
            self.cancel_operator_request();
            self.mcp_login = None;
            self.mcp_approval = None;
        }
        self.prepare_session_selection(session)?;
        // A view is a prerequisite for the rest, so this one stops the open.
        self.reconcile(session)?;
        if self.rewind_uncertain.as_ref() == Some(session) {
            self.rewind_uncertain = None;
        }

        // BOTH READS ARE ATTEMPTED, and the first error is reported only after
        // both have run.
        //
        // Sequencing them with `?` made the earlier of the two a gate on the
        // later: a refused subagent read skipped the form refresh entirely, so
        // the previous session's pending interaction stayed on screen and the
        // new session could not recover it without another open. One optional
        // field failing must not decide whether a waiting question is visible.
        let subagent = self.refresh_subagent_model(session);
        let forms = self.refresh_forms();
        let effort = self
            .refresh_selected_model(session)
            .and_then(|()| self.apply_launch_effort());
        subagent.and(forms).and(effort)
    }

    /// Rebuild one session's projection from durable truth, newest first.
    ///
    /// ONE page — the newest — is read, folded and installed, and the client is
    /// live on it. Opening a long session therefore costs one page however
    /// long the session is. Older pages follow in the background
    /// ([`history`]); what they have not yet covered is reported as
    /// [`history::OlderHistory`], never as a gap, because a gap means the tail
    /// may be wrong and the tail is exactly what this page established.
    ///
    /// The previous revision walked every page oldest-first before installing,
    /// and stayed reconciling unless the walk finished: past 64 pages a session
    /// never went live, and recovery re-walked all 64 about once a second.
    pub fn reconcile(&mut self, session: &SessionWireId) -> Result<(), ClientError> {
        // A direct history read is its own epoch: a live observation made
        // between two such reads is newer than everything the later read
        // carries.
        self.quiescence_gen = self.quiescence_gen.saturating_add(1);
        self.begin_tail();
        // One decision with the queued read. A wait is queued, and the
        // queued read applies its bound (this caller has no clock).
        match self.messages_read(None) {
            None => {
                self.request_history();
                return Ok(());
            }
            Some(recovery::MessagesRead::Snapshot) if self.read_snapshot(session)? => {
                self.request_queued_inputs(session);
                return Ok(());
            }
            Some(_) => {}
        }
        let unproven = self.positions.mode.is_none();
        let bytes = self.ingest(
            Operation::MessageList,
            Some(session),
            None,
            None,
            None,
            Some(if self.history_window.is_some() {
                Page::Window {
                    cursor: None,
                    descending: true,
                }
            } else {
                Page::First { order: "desc" }
            }),
        )?;
        let page = crate::wire::decode_messages(&bytes)?;
        self.refresh_permissions()?;
        self.adopt_tail(session, &page, bytes.len())?;
        self.positions.legacy_unproven |= unproven;
        self.request_queued_inputs(session);
        Ok(())
    }

    /// Interrupt the selected session.
    pub fn interrupt(&mut self) -> Result<(), ClientError> {
        let Some(session) = self.selected.clone() else {
            return Err(ClientError::NoSelection);
        };
        self.interrupt_session(&session)
    }

    /// Interrupt one named family member without changing the viewed session.
    pub fn interrupt_session(&mut self, session: &SessionWireId) -> Result<(), ClientError> {
        self.interrupt_session_with_pending(session, false)
    }

    fn interrupt_session_with_pending(
        &mut self,
        session: &SessionWireId,
        resume_pending: bool,
    ) -> Result<(), ClientError> {
        if resume_pending {
            let observed = self.view().and_then(|view| match &view.execution {
                crate::projection::ExecutionState::Running { execution_id } => {
                    Some(execution_id.clone())
                }
                _ => None,
            });
            self.interrupt_session_and_resume(session, observed.as_ref())
        } else {
            self.send_interrupt(session, crate::wire::encode_interrupt()?)
        }
    }

    /// Stop only `execution`. When it has already ended the service refuses,
    /// rather than stopping whatever started after it.
    pub fn interrupt_execution(
        &mut self,
        session: &SessionWireId,
        execution: &crate::wire::ExecutionWireId,
    ) -> Result<(), ClientError> {
        self.send_interrupt(session, crate::wire::encode_interrupt_execution(execution)?)
    }

    /// Stop the execution observed at dispatch, then resume admitted input.
    /// An independent command worker need not attach or re-read the Session.
    pub fn interrupt_session_and_resume(
        &mut self,
        session: &SessionWireId,
        observed: Option<&crate::wire::ExecutionWireId>,
    ) -> Result<(), ClientError> {
        self.send_interrupt(session, crate::wire::encode_interrupt_and_resume(observed)?)
    }

    fn send_interrupt(
        &mut self,
        session: &SessionWireId,
        body: Vec<u8>,
    ) -> Result<(), ClientError> {
        self.ingest(
            Operation::SessionInterrupt,
            Some(session),
            None,
            Some(body),
            None,
            None,
        )?;
        Ok(())
    }

    /// Refresh outstanding permission asks across sessions.
    /// Which model this session's SUBAGENTS run as, when it has been read.
    ///
    /// THREE STATES, not two. `None` is "not read"; `Some(None)` is the
    /// contract's NULL, meaning subagents inherit the agent's model; and
    /// `Some(Some(row))` is an explicit selection. Collapsing the first two
    /// would report "nobody said" where the product said "the default".
    pub fn subagent_model(&self) -> Option<Option<&crate::wire::ModelRow>> {
        self.subagent_model.as_ref().map(Option::as_ref)
    }

    /// Read the subagent model for one session.
    pub fn refresh_subagent_model(
        &mut self,
        session: &crate::wire::SessionWireId,
    ) -> Result<(), ClientError> {
        let bytes = self.ingest(
            Operation::SessionSubagentModelGet,
            Some(session),
            None,
            None,
            None,
            None,
        )?;
        self.subagent_model = Some(crate::wire::decode_subagent_model(&bytes)?);
        Ok(())
    }

    /// The reason the client is reconciling, if it is.
    pub fn gap_reason(&self) -> Option<&GapReason> {
        match self.cursor.state() {
            StreamState::Reconciling { reason } => Some(reason),
            StreamState::Live => None,
        }
    }

    /// Borrow the peer, for witnesses that drive the far side.
    ///
    /// Named `peer_mut` rather than the obvious spelling because
    /// `grapt-store`'s F3B1b resurrection guard bans that identifier
    /// tree-wide by substring, and a guard protecting a landed invariant
    /// outranks one accessor's preferred name.
    pub fn peer_mut(&mut self) -> &mut C {
        &mut self.connection
    }
}

#[cfg(test)]
mod tests;

mod activity;
mod export;
pub use export::SessionExport;
mod history_install;
mod observation;
mod paged_history;
pub use paged_history::HistoryMove;
mod authority;
mod catalog_refresh;
mod catalogs;
mod command_catalog;
mod creation;
mod fork;
mod side;
mod worktree;
mod worktree_attachment;
pub use fork::ForkedSession;
mod gateway;
mod location_forms;
mod mcp_auth;
mod mcp_content;
pub use mcp_content::McpContent;
mod history;
pub use history::OlderHistory;
mod launch;
mod recovery;
mod resume;
pub use activity::SessionActivity;
mod streaming;
mod submission;
mod tool_refresh;
pub use recovery::{ConnectionStatus, RECONNECT_RETRY_MAX};

mod effort;
mod presets;
mod status;

mod rewind;
mod sandbox_worktree_config;
mod undo;
pub use rewind::{RewindEdit, RewindHistory};

mod commands;
mod sessions;
mod workspace_diff;

mod offloop;
pub use offloop::{PeriodicRead, PreparedRead};
mod response;
mod snapshot;
