//! A deterministic contract double for the frozen surface.
//!
//! ## What this is for
//!
//! The client must be provable NOW, against the whole frozen contract, and
//! swappable onto the real process seam when DOGFOOD-1 lands. Those two goals
//! pull in opposite directions unless the thing the client is proved against
//! speaks the contract rather than the current implementation.
//!
//! So this double serves the FROZEN fourteen-member vocabulary and the frozen
//! route set, including the three members `grapt-api` has no emitter for yet
//! (`session.created`, `session.execution.started`, `session.idle`) and the
//! permission routes the U3a vertical does not implement. A client proved
//! against today's eleven members would acquire a hole exactly where the
//! contract promises a value.
//!
//! It is emphatically NOT a second engine. It holds a durable log and replays
//! it; it makes no domain decision, owns no policy and mints no authority.
//!
//! ## Why it is deterministic to the byte
//!
//! Every identifier it mints is a pure function of a counter, and it reads no
//! clock — the crate has no clock at all. That is what makes the snapshot
//! witnesses meaningful: a rendered frame that differed between runs would be
//! untestable, and a double that reached for ambient time or randomness would
//! also trip the workspace's `disallowed_methods` gate.
//!
//! ## The failures it can be asked to produce
//!
//! A double that could only behave well would prove only that the client
//! renders a happy path. This one can be told to drop the subscription, to
//! skip durable frames so the client observes a sequence gap, and to restart
//! while retaining durable truth — the three shapes the reconnect and restart
//! laws are about, and none of which can be scheduled against a live server.

use std::collections::BTreeMap;

use crate::contract::{EventMember, Operation};
use crate::transport::{FramePoll, Transport, TransportError, WireRequest, WireResponse};
use crate::wire::SessionWireId;
use crate::wire::Spelled;

/// One durable entry in a session's log.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DurableEntry {
    seq: u64,
    member: EventMember,
    data: serde_json::Value,
}

/// One session the double knows about.
///
/// The durable log is BOUNDED at its ownership point. Review blocker 4 is that
/// limiting a pump iteration bounds the consumer while the producer's storage
/// grows without limit — so a long-lived double would exhaust memory while
/// every client-side counter read as healthy.
#[derive(Debug, Clone, Default)]
struct DoubleSession {
    project: Option<crate::wire::ProjectRow>,
    rewind_receipts: Vec<(String, u64, serde_json::Value)>,
    preset: Option<crate::preset::Preset>,
    messages: Vec<DoubleMessage>,
    log: Vec<DurableEntry>,
    next_seq: u64,
    title: String,
    /// Messages evicted by the retention bound. Visible, never silent.
    dropped_messages: usize,
    /// Durable log entries evicted by the log bound. Visible, never silent.
    ///
    /// This counter is the observable half of the bound review named: the log
    /// previously grew through an unconditional push, so there was nothing to
    /// count and nothing to see.
    dropped_log_entries: usize,
    /// Permission asks refused by the per-session ask bound.
    dropped_permissions: usize,
    /// The agent this session was switched to, if any.
    ///
    /// Recorded rather than acknowledged. A double that answered 204 and kept
    /// nothing would let a client that never sent the selection pass the same
    /// witness as one that did.
    agent: Option<crate::wire::AgentWireId>,
    /// The model reference this session was switched to, if any. The PAIR is
    /// kept because that is what the frozen `Model.Ref` carries.
    model: Option<(crate::wire::ModelWireId, crate::wire::ProviderWireId)>,
    variant: Option<grapt_api::u3a::WireId<grapt_types::ModelVariantId>>,
    /// The session that spawned this one. A subagent's session has one.
    parent: Option<crate::wire::SessionWireId>,
}

/// One durable message the double will serve from `v2.message.list`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DoubleMessage {
    /// A user row, under the input id its prompt carried, if it carried one.
    User(String, Option<String>),
    Interrupted,
    Assistant(
        Vec<crate::wire::MessageItem>,
        Option<crate::wire::DurableFinish>,
    ),
}

/// How the double should behave on the next subscription poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// Behave.
    None,
    /// End the subscription at the next poll.
    CloseStream,
    /// Silently withhold the next `n` durable frames, so the client observes a
    /// sequence gap exactly as a lossy transport would produce.
    SkipDurable(usize),
    /// Serve one durable content item this client cannot render.
    ///
    /// A TOOL item is real durable truth inside the frozen assistant union that
    /// this terminal does not draw. A producer that never emitted one would
    /// leave the client's "I could not read all of it, so I am not current"
    /// path unwitnessed — and that path is the one thing standing between a
    /// short transcript and a short transcript advertised as complete.
    ///
    /// It is a tool item rather than a `system` MESSAGE because the frozen
    /// message union refuses `system` outright, so a producer emitting one
    /// would be serving a shape the product cannot: the refusal would be the
    /// codec's, not the client's, and this witness would be proving the wrong
    /// thing.
    UnrenderableRow,
    /// Answer every continuation with the cursor it was given, and the same
    /// row again.
    ///
    /// A real product build does this: it accepts the cursor query and pages
    /// nowhere. A client that followed such a cursor duplicated the whole
    /// transcript once per iteration, which is why progress is checked rather
    /// than assumed.
    StuckCursor,
    /// Offer a `next` cursor on EVERY page, including continuations, and never
    /// run out of rows.
    ///
    /// A producer that pages forever is what makes the client's page budget a
    /// bound rather than a comment. Without it, `max_message_pages` would be a
    /// declared-but-unenforced limit — which the law calls a failure — because
    /// no witness would ever reach it.
    EndlessPages,
    /// An execution that stops mid-run to ask for authority.
    ///
    /// The real shape, which no other fault models. Two halves, both required:
    ///
    /// * the turn never reaches its terminal, so the execution stays RUNNING —
    ///   without that, a poll bounded to running executions never engages and a
    ///   witness over it passes while exercising nothing;
    /// * the ask does not exist yet when the client reads the list at startup
    ///   OR immediately after submitting. A tool asks seconds later, so an ask
    ///   that were visible to either of those reads would be found by code that
    ///   already existed, and the mid-run branch would not be load-bearing.
    ///
    /// It is materialised on the SECOND list read after the prompt, which only
    /// a later poll can make.
    BlockedOnPermission,
    /// Refuse to answer which model subagents run as.
    ///
    /// The case that makes clearing the field load-bearing: without a failure
    /// there is always a fresh answer to overwrite the old one, so a client
    /// that never cleared would look correct until the first read that did not
    /// come back.
    SubagentModelUnavailable,
    /// Refuse to list the session's pending questions.
    ///
    /// Makes the up-front clear measurable: while the form read SUCCEEDS it
    /// replaces whatever was held, so a client that never cleared looks correct
    /// — until the read that does not come back, and then the previous
    /// session's question is still on screen.
    FormsUnavailable,
    /// Refuse every permission answer with 409, as the product does when the
    /// ask ended (its run was interrupted or timed out) after it was read.
    PermissionReplyConflict,
    /// Admit the next prompt, then lose its reply: the request was written
    /// and nothing came back (a dry run). One-shot.
    PromptReplyLost,
    /// As [`Self::PromptReplyLost`], and the message read after it fails too,
    /// so the client cannot confirm the admission. One-shot.
    PromptReplyLostUnconfirmed,
    /// Lose the next prompt's reply BEFORE admitting it. One-shot.
    PromptLostBeforeAdmission,
    /// Fail the next message read. One-shot.
    MessageListUnavailable,
    /// Accept an answer, then refuse to list the open asks with 503: the
    /// answer lands but the observation after it fails.
    PermissionListFailsAfterAnswer,
    /// Answer `v2.mcp.list` with a bare 400 and no body.
    ///
    /// The shape that keeps the ingress reading the body conditional: a
    /// 400 the client cannot parse must stay a status, not become a refusal
    /// with an invented message.
    McpUnavailable,
}

/// The double.
#[derive(Debug)]
pub struct ContractDouble {
    /// How many times the outstanding-ask list has been read.
    ///
    /// COUNTED so a rate bound can be witnessed: a client that fetched on every
    /// keystroke and one that fetched once a second are indistinguishable from
    /// the outside without it.
    permission_list_calls: usize,
    /// How many times a message page has been read, COUNTED so a witness can
    /// say how many reads a session took to go live.
    message_list_calls: usize,
    /// Synthetic long histories served in place of a session's recorded
    /// messages; see `history_pages.rs`.
    long_histories: BTreeMap<SessionWireId, Vec<history_pages::Synthetic>>,
    /// Cut every page after this many rows, as the product's byte bound cuts
    /// a page short while its cursor continues.
    short_pages: Option<usize>,
    /// Whether synthetic tool calls have completed.
    tools_finished: bool,
    /// Every session a compaction was admitted for, in order. Producer state
    /// no client page ever shows, so it takes the producer's own durable-log
    /// bound (`max_retained_log_entries`): a request past it is refused as the
    /// frozen route's 409, never recorded and never dropped silently.
    compactions: Vec<SessionWireId>,
    /// The session whose turn is blocked awaiting authority, if any.
    blocked_session: Option<SessionWireId>,
    /// Per-session subagent model. An entry of `None` is an explicit
    /// inheritance, which is not the same as having no entry at all.
    subagent_models: BTreeMap<SessionWireId, Option<(String, String)>>,
    /// List reads since that turn blocked.
    reads_since_blocked: usize,
    /// How many reads pass before the blocked turn actually asks.
    ///
    /// Two by default, so neither the entry read nor the post-submit one can
    /// find it. A witness about the POLL RATE sets it beyond reach instead: it
    /// needs the list to stay empty, because an ask on screen suppresses the
    /// poll by design and would hide an unbounded fetch behind that.
    reveal_after_reads: usize,
    /// Sessions, keyed by the TYPED identity and BOUNDED in count.
    ///
    /// Both properties were missing. The key was a `String`, so the producer
    /// could hold two spellings of one session; and the map was unbounded, so a
    /// long-lived double grew without limit at an ownership point no
    /// client-side bound can reach.
    sessions: BTreeMap<SessionWireId, DoubleSession>,
    pending: Vec<String>,
    /// Outstanding asks, keyed by the typed identity and bounded per session.
    permissions: BTreeMap<SessionWireId, Vec<PendingPermission>>,
    next_id: u64,
    fault: Fault,
    closed: bool,
    connected_sent: bool,
    /// The SAME bound set the client uses, applied at the producer's own
    /// ownership points.
    bounds: crate::bounds::Bounds,
    /// Frames refused by the pending-queue bound. Visible, never silent.
    dropped_frames: usize,
    /// Sessions refused by the session-retention bound. Visible, never silent.
    dropped_sessions: usize,
    /// Pending forms, raised by a witness rather than fixed.
    ///
    /// Empty by default, because an unprompted session has no pending
    /// question, and a double that always had one would make the empty case --
    /// the ordinary one -- the case nothing exercised.
    forms: Vec<DoubleForm>,
    /// A form to raise after a declared number of form-list reads, with its
    /// session and title. See the `SessionFormList` arm.
    scheduled_form: Option<(SessionWireId, String, usize)>,
    /// Form-list reads seen since a form was scheduled.
    form_list_reads: usize,
    /// The fixed catalog this double serves; see [`ContractDouble::catalog`].
    projects: Vec<crate::wire::ProjectRow>,
    agents: Vec<crate::wire::AgentRow>,
    models: Vec<crate::wire::ModelRow>,
    /// The MCP server list this double serves. The default is one row per
    /// member of the frozen `Mcp.Status` union, so a witness about any one
    /// status need not construct the other five: a client proved against a
    /// list that held only `connected` would acquire a hole at exactly the
    /// status the contract also promises.
    mcp_servers: Vec<crate::wire::McpServerRow>,
    /// When set, `v2.mcp.list` answers the frozen 400 with this message
    /// instead of the list — the refused-config shape the pane must render
    /// in place of its rows.
    mcp_refusal: Option<String>,
    /// Worktrees whose `v2.mcp.list` answers the bare 400 — the unreadable
    /// config shape — while every other project keeps serving its list.
    mcp_unavailable: std::collections::BTreeSet<String>,
    /// When nonzero, `v2.mcp.list` answers the bare 400 from its
    /// `(n + 1)`-th read on: the witness that needs one green read and the
    /// next red on the SAME project.
    mcp_fail_after_reads: usize,
    /// `v2.mcp.list` reads seen.
    mcp_list_reads: usize,
}

/// One outstanding permission ask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPermission {
    pub request_id: String,
    pub title: String,
    pub settled: Option<crate::wire::PermissionReplyChoice>,
    /// The display preview the engine attaches to the ask, if any.
    pub preview: Option<serde_json::Value>,
}

impl PendingPermission {
    /// The ask as a permission list carries it.
    fn row(&self, session: &SessionWireId) -> serde_json::Value {
        let mut row = serde_json::json!({
            "id": self.request_id,
            "sessionID": session.spelled(),
            "action": self.title,
            "resources": [],
            "status": "pending"
        });
        if let Some(preview) = &self.preview {
            row["preview"] = preview.clone();
        }
        row
    }
}

/// One form the producer holds, with its settlement state.
///
/// The state is the frozen `Form.State` union itself rather than a `settled`
/// flag beside the row: a bool cannot express SUSPENDED, and a producer that
/// could not express it would make the client's suspended handling untestable
/// against anything but a hand-built response.
#[derive(Debug, Clone)]
pub struct DoubleForm {
    pub row: crate::wire::FormRow,
    pub state: crate::wire::FormState,
}

impl Default for ContractDouble {
    fn default() -> Self {
        Self::new()
    }
}

impl ContractDouble {
    pub fn new() -> Self {
        let (projects, agents, models) = Self::catalog();
        Self {
            permission_list_calls: 0,
            message_list_calls: 0,
            long_histories: BTreeMap::new(),
            short_pages: None,
            tools_finished: false,
            compactions: Vec::new(),
            blocked_session: None,
            subagent_models: BTreeMap::new(),
            reads_since_blocked: 0,
            scheduled_form: None,
            form_list_reads: 0,
            reveal_after_reads: 2,
            sessions: BTreeMap::new(),
            pending: Vec::new(),
            permissions: BTreeMap::new(),
            next_id: 0,
            fault: Fault::None,
            closed: false,
            connected_sent: false,
            bounds: crate::bounds::Bounds::PRODUCTION,
            dropped_frames: 0,
            dropped_sessions: 0,
            forms: Vec::new(),
            projects,
            agents,
            models,
            mcp_servers: Self::mcp_catalog(),
            mcp_refusal: None,
            mcp_unavailable: std::collections::BTreeSet::new(),
            mcp_fail_after_reads: 0,
            mcp_list_reads: 0,
        }
    }

    /// One row per member of the frozen status union, named like a real
    /// configuration's servers. Deterministic by the same law as the rest of
    /// the double: no clock, no counter, no environment.
    fn mcp_catalog() -> Vec<crate::wire::McpServerRow> {
        use crate::wire::McpServerRow;
        use crate::wire::McpServerStatus;
        vec![
            McpServerRow {
                tools: None,
                name: "fs".to_owned(),
                status: McpServerStatus::Connected,
                integration_id: None,
            },
            McpServerRow {
                tools: None,
                name: "search".to_owned(),
                status: McpServerStatus::Pending,
                integration_id: None,
            },
            McpServerRow {
                tools: None,
                name: "legacy".to_owned(),
                status: McpServerStatus::Disabled,
                integration_id: None,
            },
            McpServerRow {
                tools: None,
                name: "broken".to_owned(),
                status: McpServerStatus::Failed {
                    error: "The configured MCP server is failed: spawn refused".to_owned(),
                },
                integration_id: None,
            },
            McpServerRow {
                tools: None,
                name: "github".to_owned(),
                status: McpServerStatus::NeedsAuth,
                integration_id: None,
            },
            McpServerRow {
                tools: None,
                name: "registry".to_owned(),
                status: McpServerStatus::NeedsRegistration {
                    error: "no client id configured".to_owned(),
                },
                integration_id: Some("integ_0000000001".to_owned()),
            },
        ]
    }

    /// Construct a double under an explicit bound set.
    /// Serve a catalog the size a REAL configuration has.
    ///
    /// The fixture serves nine models; the product's own configured catalog is
    /// 366 rows across two accounts. A picker witnessed only against nine is
    /// witnessed against a size no user has, and the difference is not
    /// cosmetic — it decides whether the list can be navigated at all.
    pub fn with_model_catalog_of(mut self, rows: usize) -> Self {
        let existing = self.models.len();
        for index in existing..rows {
            let Ok(id) = crate::wire::model_id(&format!("model-{index:04}")) else {
                continue;
            };
            let Ok(provider_id) = crate::wire::provider_id("double") else {
                continue;
            };
            self.models.push(crate::wire::ModelRow {
                id,
                provider_id,
                name: format!("Synthetic Model {index:04}"),
                enabled: true,
                variant: None,
                default_effort: None,
                context_window: Some(128_000),
                capabilities: reported_capabilities(),
            });
        }
        self
    }

    pub fn with_bounds(bounds: crate::bounds::Bounds) -> Self {
        Self {
            bounds,
            ..Self::new()
        }
    }

    /// Frames currently queued for delivery.
    ///
    /// Lets a witness prove that a pump at its budget left the boundary frame
    /// QUEUED rather than consuming and dropping it.
    pub fn queued_frames(&self) -> usize {
        self.pending.len()
    }

    /// Frames the pending-queue bound refused.
    pub fn dropped_frames(&self) -> usize {
        self.dropped_frames
    }

    /// Messages the per-session retention bound evicted.
    pub fn dropped_messages(&self, session: &SessionWireId) -> usize {
        self.sessions
            .get(session)
            .map(|entry| entry.dropped_messages)
            .unwrap_or_default()
    }

    /// Durable log entries the per-session log bound evicted.
    pub fn dropped_log_entries(&self, session: &SessionWireId) -> usize {
        self.sessions
            .get(session)
            .map(|entry| entry.dropped_log_entries)
            .unwrap_or_default()
    }

    /// Durable log entries currently retained for one session.
    pub fn log_len(&self, session: &SessionWireId) -> usize {
        self.sessions
            .get(session)
            .map(|entry| entry.log.len())
            .unwrap_or_default()
    }

    /// Permission asks the per-session ask bound refused.
    pub fn dropped_permissions(&self, session: &SessionWireId) -> usize {
        self.sessions
            .get(session)
            .map(|entry| entry.dropped_permissions)
            .unwrap_or_default()
    }

    /// Sessions currently retained.
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Sessions the session-retention bound refused.
    pub fn dropped_sessions(&self) -> usize {
        self.dropped_sessions
    }

    /// Queue one frame, refusing past the pending bound.
    fn queue(&mut self, frame: String) {
        if self.pending.len() >= self.bounds.max_pending_events {
            // Refuse VISIBLY. A producer that silently dropped would leave the
            // client short with nothing anywhere saying so.
            self.dropped_frames = self.dropped_frames.saturating_add(1);
            return;
        }
        self.pending.push(frame);
    }

    /// The session and text a user row was admitted under `input` with, in
    /// ANY session: the product holds an input id once across all of them.
    fn admitted_input(&self, input: &str) -> Option<(&SessionWireId, &str)> {
        self.sessions.iter().find_map(|(session, entry)| {
            entry.messages.iter().find_map(|message| match message {
                DoubleMessage::User(text, Some(held)) if held == input => {
                    Some((session, text.as_str()))
                }
                _ => None,
            })
        })
    }

    /// The input ids `session`'s user rows were admitted under, in order.
    pub fn user_inputs(&self, session: &SessionWireId) -> Vec<Option<String>> {
        self.sessions
            .get(session)
            .map(|entry| {
                entry
                    .messages
                    .iter()
                    .filter_map(|message| match message {
                        DoubleMessage::User(_, input) => Some(input.clone()),
                        DoubleMessage::Assistant(..) | DoubleMessage::Interrupted => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Record one durable message, evicting past the retention bound.
    fn record(&mut self, session: &SessionWireId, message: DoubleMessage) {
        let max = self.bounds.max_retained_slices;
        let Some(entry) = self.sessions.get_mut(session) else {
            return;
        };
        entry.messages.push(message);
        while entry.messages.len() > max {
            entry.messages.remove(0);
            entry.dropped_messages = entry.dropped_messages.saturating_add(1);
        }
    }

    /// The fixed catalog every double serves.
    ///
    /// It is FIXED rather than mutable state, because the double exists to
    /// prove the client against the frozen contract, and a catalog the test
    /// could rewrite would let a witness prove the client against whatever
    /// shape it happened to need. What varies between witnesses is which rows
    /// the client SELECTS, not what the product offers.
    ///
    /// The rows are deliberately awkward rather than tidy: two projects so a
    /// gateway with one row cannot pass by accident, an unnamed project, a
    /// hidden agent so the roster's filter is exercised, one model id repeated
    /// under two providers, and a disabled model the picker must render and
    /// refuse.
    ///
    /// Every id is PARSED through the same constructor the client uses, and a
    /// row whose id the domain refuses is dropped rather than served. Dropping
    /// is not a silent fallback: `serves_the_whole_catalog` asserts the exact
    /// row counts, so a literal that stopped parsing fails a witness by name
    /// instead of reaching a client that would have to refuse it and look
    /// broken.
    fn catalog() -> (
        Vec<crate::wire::ProjectRow>,
        Vec<crate::wire::AgentRow>,
        Vec<crate::wire::ModelRow>,
    ) {
        let projects = [
            ("prj_double_a", "/work/project-a", Some("Project A")),
            ("prj_double_b", "/work/project-b", None),
        ]
        .into_iter()
        .filter_map(|(id, worktree, name)| {
            Some(crate::wire::ProjectRow {
                id: crate::wire::project_id(id).ok()?,
                worktree: worktree.to_owned(),
                name: name.map(str::to_owned),
            })
        })
        .collect();

        let agents = [
            (
                "agent-build",
                "build",
                Some("the ordinary coding agent"),
                false,
            ),
            ("agent-review", "review", None, false),
            ("agent-internal", "internal", None, true),
        ]
        .into_iter()
        .filter_map(|(id, name, description, hidden)| {
            Some(crate::wire::AgentRow {
                id: crate::wire::agent_id(id).ok()?,
                name: name.to_owned(),
                description: description.map(str::to_owned),
                mode: grapt_api::u3a::AgentInfoDtoMode::Primary,
                hidden,
                source: None,
            })
        })
        .collect();

        let models = [
            ("model-fast", "double", "Fast", true),
            ("model-deep", "double", "Deep", true),
            ("model-fast", "other", "Fast (other)", true),
            ("model-retired", "double", "Retired", false),
        ]
        .into_iter()
        .filter_map(|(id, provider, name, enabled)| {
            Some(crate::wire::ModelRow {
                id: crate::wire::model_id(id).ok()?,
                provider_id: crate::wire::provider_id(provider).ok()?,
                name: name.to_owned(),
                enabled,
                variant: None,
                default_effort: None,
                context_window: Some(128_000),
                capabilities: reported_capabilities(),
            })
        })
        .collect();

        (projects, agents, models)
    }

    /// Which project a catalog request is scoped to.
    ///
    /// The frozen catalog routes take `location[directory]`. A request that
    /// names one gets THAT project and nothing else; a request that names none
    /// gets the first, which is what an unscoped gateway read means. A
    /// directory naming no project resolves to `None` and the caller answers
    /// 400 — a double that guessed a project here would let a client show one
    /// location's agents under another location's name.
    fn located(&self, request: &WireRequest) -> Option<&crate::wire::ProjectRow> {
        let directory = request
            .query()
            .iter()
            .find(|(key, _)| key == "location[directory]")
            .map(|(_, value)| value.as_str());
        match directory {
            Some(directory) => self
                .projects
                .iter()
                .find(|project| project.worktree == directory),
            None => self.projects.first(),
        }
    }

    /// Mint the next deterministic identifier.
    fn mint(&mut self, prefix: &str) -> String {
        self.next_id = self.next_id.saturating_add(1);
        format!("{prefix}{}", self.next_id)
    }

    /// Append exactly `count` durable facts, for bound witnesses.
    ///
    /// `run_turn` appends a whole scripted turn, which is the wrong instrument
    /// for an EXACT-AT-the-bound control: it moves the log by more than one, so
    /// a witness using it can only ever show "eventually evicted", never
    /// "evicted at exactly one over". This appends one fact at a time so the
    /// boundary itself can be asserted.
    #[cfg(test)]
    pub(crate) fn append_witness_entries(&mut self, session: &SessionWireId, count: usize) {
        let spelled = session.spelled().to_owned();
        for _ in 0..count {
            self.append(
                session,
                EventMember::TextStarted,
                serde_json::json!({"sessionID": spelled}),
            );
        }
    }

    /// Inject a fault for the next poll.
    pub fn set_fault(&mut self, fault: Fault) {
        self.fault = fault;
    }

    /// Restart the server: the subscription dies, live state is lost, durable
    /// truth survives. This is the restart law's whole setup.
    pub fn restart(&mut self) {
        self.pending.clear();
        self.closed = true;
        self.connected_sent = false;
    }

    /// Re-open a subscription after a restart or close.
    pub fn resubscribe(&mut self) {
        self.closed = false;
        self.connected_sent = false;
        self.fault = Fault::None;
    }

    pub fn subscribed(&self) -> bool {
        !self.closed
    }

    /// The session page, as the REAL `SessionsResponseDto`.
    fn session_page(&self) -> Result<Vec<u8>, crate::wire::WireError> {
        let mut rows = Vec::new();
        for (id, session) in &self.sessions {
            // No re-parse: the key IS the parsed identity now.
            rows.push(crate::wire::session_info(
                id,
                &session.title,
                "/work",
                session.parent.as_ref(),
            )?);
        }
        crate::wire::encode_sessions(rows)
    }

    /// Append one durable row outside this client's vocabulary.
    ///
    /// Built as JSON and then RE-VALIDATED through the real decoder, because a
    /// `system` message has no constructor reachable from here. A row the
    /// frozen union would refuse never leaves: the validation fails first, so
    /// this fault cannot serve a shape the product could not.
    fn with_unrenderable_row(
        mut rows: Vec<grapt_api::u3a::SessionMessageInfoDto>,
    ) -> Result<Vec<u8>, crate::wire::WireError> {
        rows.push(crate::wire::model_switched_message("msg_000000000099")?);
        crate::wire::encode_messages(rows, Some("more"))
    }

    fn ok(body: serde_json::Value) -> Result<WireResponse, TransportError> {
        Ok(WireResponse {
            status: 200,
            body: body.to_string().into_bytes(),
        })
    }

    /// The session id embedded in a resolved path, if any.
    ///
    /// PARSED, not merely extracted. A path naming a session identity the domain
    /// refuses answers 404 here rather than reaching a map lookup, which is the
    /// same refusal the client applies at its own edge — so the two sides of the
    /// seam cannot disagree about what a session id is.
    fn session_of(path: &str) -> Option<SessionWireId> {
        let raw = path
            .strip_prefix("/api/session/")
            .and_then(|rest| rest.split('/').next())
            .filter(|id| !id.is_empty())?;
        crate::wire::session_id(raw).ok()
    }

    /// The request id embedded in a permission path, if any.
    fn request_of(path: &str) -> Option<&str> {
        let mut parts = path.split('/').skip(1);
        let mut found = None;
        while let Some(part) = parts.next() {
            if part == "permission" {
                found = parts.next();
            }
        }
        found.filter(|id| !id.is_empty())
    }

    /// The form id embedded in a form path, if any.
    fn form_of(path: &str) -> Option<&str> {
        let mut parts = path.split('/').skip(1);
        let mut found = None;
        while let Some(part) = parts.next() {
            if part == "form" {
                found = parts.next();
            }
        }
        found.filter(|id| !id.is_empty())
    }

    /// Poll for the next live frame, honouring any injected fault.
    pub fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        if self.closed {
            return Ok(FramePoll::Closed);
        }
        match self.fault {
            Fault::CloseStream => {
                self.closed = true;
                return Ok(FramePoll::Closed);
            }
            Fault::SkipDurable(count) if count > 0 => {
                // Withhold durable frames the way a lossy transport does: the
                // client never sees them and must notice the sequence gap.
                let mut remaining = count;
                while remaining > 0 && !self.pending.is_empty() {
                    let frame = self.pending.remove(0);
                    if frame.contains("\"durable\"") {
                        remaining -= 1;
                    }
                }
                self.fault = Fault::None;
            }
            _ => {}
        }
        if !self.connected_sent {
            self.connected_sent = true;
            let frame = Self::render(
                EventMember::ServerConnected,
                &serde_json::json!({}),
                None,
                self.next_id,
            );
            return Ok(FramePoll::Frame(frame));
        }
        if self.pending.is_empty() {
            return Ok(FramePoll::Idle);
        }
        Ok(FramePoll::Frame(self.pending.remove(0)))
    }
}

impl Transport for ContractDouble {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        self.route(request)
    }
}

#[cfg(test)]
impl ContractDouble {
    /// Make `v2.mcp.list` answer the frozen 400 with this message, clearing
    /// the rows a refusal replaces.
    pub(crate) fn refuse_mcp_list(mut self, message: impl Into<String>) -> Self {
        self.mcp_refusal = Some(message.into());
        self.mcp_servers.clear();
        self
    }

    /// Make `v2.mcp.list` for `worktree` answer the bare 400 — the
    /// unreadable config shape — while other projects keep serving.
    pub(crate) fn mcp_unavailable_for(mut self, worktree: impl Into<String>) -> Self {
        self.mcp_unavailable.insert(worktree.into());
        self
    }

    /// Make `v2.mcp.list` answer the bare 400 from its `(n + 1)`-th read on:
    /// one green read, the next red, on the same project.
    pub(crate) fn mcp_fail_after_reads(mut self, n: usize) -> Self {
        self.mcp_fail_after_reads = n;
        self
    }
}

/// The double's models report an ordinary text-and-tools surface.
fn reported_capabilities() -> crate::wire::ModelCapabilities {
    crate::wire::ModelCapabilities {
        tools: Some(true),
        input_text: Some(true),
        output_text: Some(true),
    }
}

#[cfg(test)]
mod tests;

mod catalogs;
mod routes;

mod producer;
mod session_management;
mod session_snapshot;

mod rewind;

mod history_pages;
#[cfg(test)]
pub(crate) use history_pages::Synthetic;
