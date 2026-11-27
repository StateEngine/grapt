use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use grapt_artifact::{ArtifactKind, CaptureBound, Sensitivity};
use grapt_permission::{
    PermissionCreateEffect, PermissionHttpSource, PermissionHttpState, PermissionOwner,
};
use grapt_provider::{AdapterState, AttemptFragment, FinishReason, ToolExecution};
use grapt_runtime::{
    AttemptOwner, DurableAttemptOwner, DurableFact, NoLiveSubscribers, MAX_BLOCK_CAPTURE_BYTES,
};
use grapt_store::AggregateId;
use grapt_types::{
    AgentId, AuthenticatedClientId, ClockAuthority, EventId, IdAuthority, IdToken, PermissionId,
    PhysicalAttemptId, ProjectId, StepNumber, Timestamp, ToolTerminalArtifact,
};

use super::*;
mod accumulation;
mod background;
mod codemode_failure;
mod compatible_replay;
mod corrupt_outputs;
mod exchange_fixture;
mod facts_bound;
mod history_batches;
mod history_fetch;
mod history_page;
mod history_replay_omissions;
mod image_failure;
mod instruction_epochs;
mod local_capacity;
mod lost_call;
mod mail;
mod msys_history;
mod msys_rejection;
mod nested;
mod original_users;
mod partial_plan_capacity;
mod read_capacity;
mod recent;
mod recent_bounds;
mod recent_overflow;
mod recent_window;
mod requested_compaction;
pub(in crate::dogfood) mod response_groups;
mod retried_response_order;
mod split_exchange;
mod structural_bounds;
mod structural_request;
mod user_images;

thread_local! {
    /// Artifact bytes this thread's continuation reads fetched: the witness
    /// that a refused request never read past its declared bound.
    pub(super) static FETCHED_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
mod shell_preview;
mod summary_guard;
mod sustained_defects;
mod working_set;

static FIXTURE_ORDINAL: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
struct FixedClock;

impl ClockAuthority for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1_788_220_800_000)
    }
}

#[derive(Debug, Default)]
struct CountingIds(AtomicU64);

impl IdAuthority for CountingIds {
    fn mint(&self) -> IdToken {
        let value = self.0.fetch_add(1, Ordering::SeqCst);
        IdToken::new(format!("d2a{value:012x}"))
            .unwrap_or_else(|| panic!("the deterministic identity is non-empty"))
    }
}

#[derive(Debug)]
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Result<Self, String> {
        let ordinal = FIXTURE_ORDINAL.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "grapt-d2a-durable-continuation-{}-{label}-{ordinal}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        Ok(Self(root))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Fixture {
    _scratch: Scratch,
    database: PathBuf,
    artifact_root: PathBuf,
    inbox: Option<Arc<Mutex<Inbox>>>,
    artifacts: Option<Arc<FilesystemArtifactStore>>,
    session: SessionId,
    input: MessageId,
}

impl Fixture {
    fn new(label: &str) -> Result<Self, String> {
        let scratch = Scratch::new(label)?;
        let database = scratch.0.join("events.sqlite3");
        let artifact_root = scratch.0.join("artifacts");
        let session = "ses_d2a_continuation"
            .parse::<SessionId>()
            .map_err(|error| error.to_string())?;
        let input = "msg_d2a_continuation"
            .parse::<MessageId>()
            .map_err(|error| error.to_string())?;
        let mut store =
            EventStore::open(&database, "d2a-continuation").map_err(|error| error.to_string())?;
        PermissionHttpState::initialize(&mut store).map_err(|error| error.to_string())?;
        store
            .elect_writer(&AggregateId::Session(session.clone()))
            .map_err(|error| error.to_string())?;
        let inbox = Arc::new(Mutex::new(
            Inbox::new(store).map_err(|error| error.to_string())?,
        ));
        let artifacts = Arc::new(open_artifacts(&artifact_root)?);
        Ok(Self {
            _scratch: scratch,
            database,
            artifact_root,
            inbox: Some(inbox),
            artifacts: Some(artifacts),
            session,
            input,
        })
    }

    fn inbox(&self) -> Result<&Arc<Mutex<Inbox>>, String> {
        self.inbox
            .as_ref()
            .ok_or_else(|| "fixture inbox is closed".to_owned())
    }

    fn artifacts(&self) -> Result<&Arc<FilesystemArtifactStore>, String> {
        self.artifacts
            .as_ref()
            .ok_or_else(|| "fixture artifact store is closed".to_owned())
    }

    fn continuation(&self, input: MessageId) -> Result<DurableContinuation, String> {
        Ok(DurableContinuation::new(
            Arc::clone(self.inbox()?),
            Arc::clone(self.artifacts()?),
            self.session.clone(),
            input,
        ))
    }

    fn ask_permission(
        &self,
        event_raw: &str,
        permission_raw: &str,
        call: &ToolCallId,
    ) -> Result<(), String> {
        self.ask_permission_for(event_raw, permission_raw, call, "read")
    }

    /// The same, with the recorded ACTION chosen. An MCP permission records
    /// its published qualified name here, not a built-in verb.
    fn ask_permission_for(
        &self,
        event_raw: &str,
        permission_raw: &str,
        call: &ToolCallId,
        action: &str,
    ) -> Result<(), String> {
        let event = event_raw
            .parse::<EventId>()
            .map_err(|error| error.to_string())?;
        let permission = permission_raw
            .parse::<PermissionId>()
            .map_err(|error| error.to_string())?;
        let client = AuthenticatedClientId::new("client_d2a_continuation")
            .ok_or_else(|| "client identity was refused".to_owned())?;
        let owner = PermissionOwner {
            subject: "human-d2a-continuation",
            client: &client,
        };
        let agent = AgentId::new("agt_d2a_continuation")
            .ok_or_else(|| "agent identity was refused".to_owned())?;
        let project = ProjectId::new("prj_d2a_continuation")
            .ok_or_else(|| "project identity was refused".to_owned())?;
        let source = PermissionHttpSource::Tool {
            message_id: self.input.as_str().to_owned(),
            call_id: call.as_str().to_owned(),
        };
        let mut state = PermissionHttpState::default();
        let mut inbox = lock(self.inbox()?);
        let effect = state
            .create(
                inbox.store_mut(),
                &owner,
                &event,
                &permission,
                &self.session,
                &agent,
                &[],
                &project,
                action,
                &["fixture.txt".to_owned()],
                None,
                None,
                Some(&source),
                1_788_220_800_002,
            )
            .map_err(|error| error.to_string())?;
        if effect != PermissionCreateEffect::Ask {
            return Err("permission fixture unexpectedly bypassed its ask".to_owned());
        }
        Ok(())
    }

    fn record_exchange(&self) -> Result<grapt_types::ArtifactRef, String> {
        self.record_exchange_state(true, false)?
            .ok_or_else(|| "complete exchange omitted its output artifact".to_owned())
    }

    fn record_incomplete_exchange(&self) -> Result<(), String> {
        if self.record_exchange_state(false, false)?.is_some() {
            return Err("incomplete exchange unexpectedly captured output".to_owned());
        }
        Ok(())
    }

    fn record_exchange_state(
        &self,
        include_terminal: bool,
        failed_before_permission: bool,
    ) -> Result<Option<grapt_types::ArtifactRef>, String> {
        self.record_exchange_failure(include_terminal, failed_before_permission, false)
    }

    fn record_exchange_failure(
        &self,
        include_terminal: bool,
        failed_before_permission: bool,
        interrupted: bool,
    ) -> Result<Option<grapt_types::ArtifactRef>, String> {
        self.record_exchange_named(
            include_terminal,
            failed_before_permission,
            interrupted,
            READ_TOOL,
            "read",
        )
    }

    /// The same, with the identity's recorded TOOL NAME and its permission's
    /// recorded ACTION chosen, so a witness can make the two disagree. They
    /// describe one event, so a history where they differ is corrupt.
    fn record_exchange_named(
        &self,
        include_terminal: bool,
        failed_before_permission: bool,
        interrupted: bool,
        tool: &str,
        action: &str,
    ) -> Result<Option<grapt_types::ArtifactRef>, String> {
        self.record_exchange_output(
            include_terminal,
            failed_before_permission,
            interrupted,
            tool,
            action,
            b"fixture output",
        )
    }

    fn record_exchange_output(
        &self,
        include_terminal: bool,
        failed_before_permission: bool,
        interrupted: bool,
        tool: &str,
        action: &str,
        output: &[u8],
    ) -> Result<Option<grapt_types::ArtifactRef>, String> {
        self.record_exchange_call(
            (include_terminal, failed_before_permission, interrupted),
            tool,
            action,
            output,
            r#"{"path":"fixture.txt"}"#,
        )
    }

    /// The same, with the call's stored arguments chosen.
    fn record_exchange_call(
        &self,
        (include_terminal, failed_before_permission, interrupted): (bool, bool, bool),
        tool: &str,
        action: &str,
        output: &[u8],
        arguments: &str,
    ) -> Result<Option<grapt_types::ArtifactRef>, String> {
        self.record_owned_exchange_call(
            (include_terminal, failed_before_permission, interrupted),
            tool,
            action,
            output,
            arguments,
            ToolExecution::Local,
        )
    }

    fn close(&mut self) {
        drop(self.inbox.take());
        drop(self.artifacts.take());
    }

    fn reopen(&self, input: MessageId) -> Result<DurableContinuation, String> {
        let store = EventStore::open(&self.database, "d2a-continuation")
            .map_err(|error| error.to_string())?;
        let inbox = Arc::new(Mutex::new(
            Inbox::new(store).map_err(|error| error.to_string())?,
        ));
        let artifacts = Arc::new(open_artifacts(&self.artifact_root)?);
        Ok(DurableContinuation::new(
            inbox,
            artifacts,
            self.session.clone(),
            input,
        ))
    }
}

fn open_artifacts(root: &Path) -> Result<FilesystemArtifactStore, String> {
    FilesystemArtifactStore::open(
        root,
        b"d2a-continuation-test-key".to_vec(),
        Box::new(FixedClock),
    )
    .map_err(|error| error.to_string())
}

fn assert_exchange(input: &[CodexInputItem]) -> Result<(), String> {
    assert_eq!(input.len(), 4);
    assert_eq!(input[1], CodexInputItem::AssistantResponseStart);
    assert_eq!(
        input.first(),
        Some(&CodexInputItem::Message(InputMessage {
            role: MessageRole::User,
            content: "use the tool".to_owned(),
        }))
    );
    let Some(CodexInputItem::ToolCall {
        call,
        tool,
        arguments,
    }) = input.get(2)
    else {
        return Err("reloaded input omitted its function_call".to_owned());
    };
    assert_eq!(call.as_str(), "call_d2a_read");
    assert_eq!(tool.as_str(), READ_TOOL);
    assert_eq!(arguments.as_str(), r#"{"path":"fixture.txt"}"#);
    let Some(CodexInputItem::ToolOutput { call, output, .. }) = input.get(3) else {
        return Err("reloaded input omitted its function_call_output".to_owned());
    };
    assert_eq!(call.as_str(), "call_d2a_read");
    assert_eq!(output.as_str(), "fixture output");
    Ok(())
}

#[test]
fn next_prompt_and_reopen_preserve_prior_user_and_assistant_context() -> Result<(), String> {
    let mut fixture = Fixture::new("conversation-memory")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &fixture.input,
        "Remember cobalt orchard.",
    )?;
    let mut owner = DurableAttemptOwner::elect_inbox_owned(
        Arc::clone(fixture.inbox()?),
        open_artifacts(&fixture.artifact_root)?,
        Arc::new(FixedClock),
        Arc::new(CountingIds::default()),
        NoLiveSubscribers,
        fixture.session.clone(),
        StepNumber::new(1).ok_or("fixture Step invalid")?,
        None,
    )
    .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::AttemptStarted {
            attempt: PhysicalAttemptId::new("pat_context_memory")
                .ok_or("fixture attempt invalid")?,
        })
        .map_err(|error| error.reason)?;
    for fragment in [
        AttemptFragment::TextOpened {
            block: grapt_provider::BlockOrdinal::new(0),
            state: AdapterState::none(),
        },
        AttemptFragment::TextDelta {
            block: grapt_provider::BlockOrdinal::new(0),
            text: "I will remember cobalt orchard.".to_owned(),
            state: AdapterState::none(),
        },
    ] {
        owner
            .commit(DurableFact::Fragment {
                fragment: Box::new(fragment),
            })
            .map_err(|error| error.reason)?;
    }
    // Interruption still preserves observed assistant bytes for future input.
    owner
        .commit(DurableFact::StepFailed {
            failure: grapt_runtime::StepFailure::new(
                grapt_types::StepTerminalReason::Interrupted {
                    by: grapt_types::InterruptionSource::User,
                },
                "fixture interrupt",
            ),
        })
        .map_err(|error| error.reason)?;
    drop(owner);
    let next: MessageId = "msg_context_recall"
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &next,
        "What phrase did I give you?",
    )?;
    let expected = vec![
        CodexInputItem::Message(InputMessage {
            role: MessageRole::User,
            content: "Remember cobalt orchard.".to_owned(),
        }),
        CodexInputItem::AssistantResponseStart,
        CodexInputItem::Message(InputMessage {
            role: MessageRole::Assistant,
            content: "I will remember cobalt orchard.".to_owned(),
        }),
        CodexInputItem::Message(InputMessage {
            role: MessageRole::User,
            content: "What phrase did I give you?".to_owned(),
        }),
    ];
    assert_eq!(
        fixture
            .continuation(next.clone())?
            .request_input("What phrase did I give you?", false)?,
        expected
    );
    fixture.close();
    assert_eq!(
        fixture
            .reopen(next)?
            .request_input("What phrase did I give you?", false)?,
        expected
    );
    Ok(())
}

#[test]
fn reopened_history_retains_prior_exchange_as_context_for_new_input() -> Result<(), String> {
    let mut fixture = Fixture::new("reopen")?;
    fixture.record_exchange()?;
    let new_input = "msg_d2a_new_input"
        .parse::<MessageId>()
        .map_err(|error| error.to_string())?;
    promote_test_input(fixture.inbox()?, &fixture.session, &new_input, "new work")?;
    fixture.close();

    let continuation = fixture.reopen(fixture.input.clone())?;
    let new_continuation = fixture.reopen(new_input)?;
    let old_context = continuation.request_input("use the tool", true)?;
    assert_exchange(&old_context)?;
    let mut expected = old_context;
    expected.push(CodexInputItem::Message(InputMessage {
        role: MessageRole::User,
        content: "new work".to_owned(),
    }));
    assert_eq!(
        new_continuation.request_input("new work", false)?,
        expected,
        "a new prompt lost its prior tool exchange from context"
    );
    let refusal = new_continuation
        .request_input("new work", true)
        .err()
        .ok_or_else(|| "continued Step accepted missing tool evidence".to_owned())?;
    assert_eq!(
        refusal,
        "continued Step has no complete durable tool exchange"
    );
    Ok(())
}

#[test]
fn failure_before_permission_keeps_identity_output_and_reopen_truth() -> Result<(), String> {
    let mut fixture = Fixture::new("failure-before-permission")?;
    fixture.record_exchange_state(true, true)?;
    let call = ToolCallId::new("call_d2a_read").ok_or("invalid fixture call")?;
    let projection = tool_projection(
        fixture.inbox()?,
        fixture.artifacts()?,
        &fixture.session,
        &call,
    )?;
    assert_eq!(projection.name, READ_TOOL);
    assert_eq!(
        projection.error.as_deref(),
        Some("Tool failed: EffectFailed")
    );
    assert!(projection.settled);
    let expected = fixture
        .continuation(fixture.input.clone())?
        .request_input("use the tool", true)?;
    assert!(
        matches!(expected.get(2), Some(CodexInputItem::ToolCall { tool, .. }) if tool.as_str() == READ_TOOL)
    );
    assert!(
        matches!(expected.get(3), Some(CodexInputItem::ToolOutput { output, .. }) if output.as_str() == "Tool failed: EffectFailed")
    );
    fixture.close();
    assert_eq!(
        fixture
            .reopen(fixture.input.clone())?
            .request_input("use the tool", true)?,
        expected
    );
    Ok(())
}

/// Benchmark B01 (2026-10-07): one stored call whose arguments were "" failed
/// every history read, and with it the turn. Malformed arguments are now a
/// diagnostic beside the call; the durable outcome still decides its state
/// (review HOLD on b0d40602).
#[test]
fn a_stored_call_with_unparseable_arguments_keeps_its_durable_outcome() -> Result<(), String> {
    // (arguments, include_terminal, failed_before_permission, step_terminal)
    // -> public status and whether the diagnostic is reported.
    let cases = [
        (r#"[1]"#, true, false, true, "completed", true),
        ("{not json", false, false, false, "running", true),
        ("{not json", true, true, true, "error", true),
        ("", true, false, true, "completed", false),
        ("  ", true, false, true, "completed", false),
    ];
    for (index, (arguments, terminal, failed, step_terminal, status, reported)) in
        cases.into_iter().enumerate()
    {
        let fixture = Fixture::new(&format!("invalid-arguments-{index}"))?;
        fixture.record_exchange_call(
            (terminal, failed, false),
            READ_TOOL,
            "read",
            b"fixture output",
            arguments,
        )?;
        let call = ToolCallId::new("call_d2a_read").ok_or("invalid fixture call")?;
        let projection = tool_projection(
            fixture.inbox()?,
            fixture.artifacts()?,
            &fixture.session,
            &call,
        )?;
        assert_eq!(projection.input, serde_json::json!({}), "{arguments:?}");
        let state: serde_json::Value =
            serde_json::from_str(&crate::dogfood::history_json::tool_state_json(
                &projection,
                fixture.artifacts()?,
                step_terminal,
            )?)
            .map_err(|error| error.to_string())?;
        assert_eq!(state["status"], status, "{arguments:?}: {state}");
        assert_eq!(
            state["metadata"]["inputError"]
                .as_str()
                .is_some_and(|error| error.starts_with("Invalid tool arguments: ")),
            reported,
            "{arguments:?}: {state}"
        );
        match status {
            "completed" => assert_eq!(
                state["content"][0]["text"], "fixture output",
                "{arguments:?}: a recorded output was lost: {state}"
            ),
            "error" => assert_eq!(
                state["error"]["message"], "Tool failed: EffectFailed",
                "{arguments:?}: the recorded failure lost precedence: {state}"
            ),
            _ => {}
        }
        // A settled exchange still rebuilds the provider history; an
        // unsettled one is refused for its missing terminal, as before.
        if terminal {
            fixture
                .continuation(fixture.input.clone())?
                .request_input("use the tool", true)?;
        }
    }
    Ok(())
}

#[test]
fn missing_terminal_refuses_continuation() -> Result<(), String> {
    let fixture = Fixture::new("missing")?;
    fixture.record_incomplete_exchange()?;
    let refusal = fixture
        .continuation(fixture.input.clone())?
        .request_input("use the tool", true)
        .err()
        .ok_or_else(|| "missing terminal evidence was accepted".to_owned())?;
    assert!(refusal.contains("no durable terminal output"));
    Ok(())
}

#[test]
fn new_input_after_failed_tool_preserves_uncertainty_without_replay() -> Result<(), String> {
    let mut fixture = Fixture::new("failed-tool-new-input")?;
    fixture.record_exchange_failure(false, false, true)?;
    let next: MessageId = "msg_after_failed_tool"
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &next,
        "inspect current state",
    )?;
    let expected = fixture
        .continuation(next.clone())?
        .request_input("inspect current state", false)?;
    assert_eq!(expected.len(), 4);
    assert!(
        matches!(expected.get(2), Some(CodexInputItem::Message(message))
        if message.role == MessageRole::Assistant
        && message.content.contains("call_d2a_read")
        && message.content.contains("effects are unknown"))
    );
    assert!(expected.iter().all(|item| matches!(
        item,
        CodexInputItem::Message(_) | CodexInputItem::AssistantResponseStart
    )));
    assert!(
        fixture
            .continuation(fixture.input.clone())?
            .request_input("use the tool", true)
            .is_err(),
        "old tool continuation resurrected"
    );
    assert!(
        fixture
            .continuation(fixture.input.clone())?
            .request_input("use the tool", false)
            .is_err(),
        "old input acquired uncertain context"
    );
    fixture.close();
    assert_eq!(
        fixture
            .reopen(next.clone())?
            .request_input("inspect current state", false)?,
        expected
    );
    assert!(
        fixture
            .reopen(next)?
            .request_input("inspect current state", true)
            .is_err(),
        "historical uncertainty authorized tool continuation"
    );
    Ok(())
}

#[test]
fn new_input_cannot_hide_an_unsettled_tool_step() -> Result<(), String> {
    let fixture = Fixture::new("live-tool-new-input")?;
    fixture.record_incomplete_exchange()?;
    let next: MessageId = "msg_after_live_tool"
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    promote_test_input(fixture.inbox()?, &fixture.session, &next, "new work")?;
    let error = fixture
        .continuation(next)?
        .request_input("new work", false)
        .err()
        .ok_or("an unsettled tool Step was hidden")?;
    assert!(error.contains("no durable terminal output"));
    Ok(())
}

/// R5: an unreadable terminal row is no evidence. The next prompt continues,
/// the call reads as having no outcome, and the row gets its own notice.
#[test]
fn unreadable_terminal_row_degrades_with_notice() -> Result<(), String> {
    for (label, corrupt) in [
        ("unreadable-json", "{"),
        ("unreadable-fact", r#"{"call":"call_d2a_read"}"#),
        // A recognized outcome kind missing its required fields.
        (
            "unreadable-kind",
            r#"{"attempt":"x","call":"call_d2a_read","outcome":{"kind":"cancelled"}}"#,
        ),
    ] {
        let mut fixture = Fixture::new(label)?;
        fixture.record_exchange()?;
        let next: MessageId = "msg_after_unreadable"
            .parse()
            .map_err(|error: grapt_types::IdError| error.to_string())?;
        promote_test_input(fixture.inbox()?, &fixture.session, &next, "keep going")?;
        fixture.close();
        recent::raw_conn(&fixture)?
            .execute(
                "UPDATE event SET data = ?1 WHERE type = 'session.tool.terminal.1'",
                [corrupt],
            )
            .map_err(|error| error.to_string())?;
        let (input, _) = fixture
            .reopen(next.clone())?
            .request_with_identity("keep going", false)?;
        let text = |needle: &str| {
            input.iter().any(|item| {
                matches!(item,
            CodexInputItem::Message(message) if message.content.contains(needle))
            })
        };
        assert!(
            text("call_d2a_read has no readable recorded outcome"),
            "{input:?}"
        );
        assert!(text("recorded outcome is unreadable"), "{input:?}");
        // A row that still names its call keeps that call listed, as failed.
        let reopened = fixture.reopen(next)?;
        let listed = history(
            &reopened.inbox,
            &reopened.artifacts,
            &fixture.session,
            None,
            None,
            None,
        )?;
        let listed: serde_json::Value =
            serde_json::from_slice(&listed).map_err(|e| e.to_string())?;
        let named = listed["calls"].as_array().map(Vec::len) == Some(1);
        assert_eq!(named, label != "unreadable-json", "{listed}");
        assert!(
            !named || listed["calls"][0]["outcome"] == "failed",
            "{listed}"
        );
    }
    Ok(())
}

#[test]
fn duplicate_permission_call_refuses_continuation() -> Result<(), String> {
    let fixture = Fixture::new("duplicate")?;
    fixture.record_exchange()?;
    let call =
        ToolCallId::new("call_d2a_read").ok_or_else(|| "call identity was refused".to_owned())?;
    fixture.ask_permission(
        "evt_d2a_permission_duplicate",
        "per_d2a_permission_duplicate",
        &call,
    )?;
    let refusal = fixture
        .continuation(fixture.input.clone())?
        .request_input("use the tool", true)
        .err()
        .ok_or_else(|| "duplicate durable call evidence was accepted".to_owned())?;
    assert!(refusal.contains("duplicate permission asks"));
    Ok(())
}

/// AN MCP TOOL'S DURABLE ACTION IS ITS PUBLISHED QUALIFIED NAME.
///
/// Reconstruction refused every action that was not one of the three built-ins,
/// so a Session could not be replanned after an approved MCP call: the tool
/// ran, returned its result, and the next Step's plan was refused because this
/// function did not recognise the recorded name. Measured live as
/// `unknown durable action "journey_journey_echo"`.
#[test]
fn an_mcp_qualified_action_reconstructs_to_its_recorded_name() {
    let call = match ToolCallId::new("call_reconstructed") {
        Some(call) => call,
        None => panic!("the witness needs a usable call id"),
    };
    match super::tool_name("journey_journey_echo", &call) {
        Ok(name) => assert_eq!(name.as_str(), "journey_journey_echo"),
        Err(refusal) => panic!("a recorded MCP action must reconstruct: {refusal}"),
    }
}

/// The built-in vocabulary is its own and keeps its mappings. Without this the
/// change above is satisfied by returning the action verbatim for everything,
/// which would reinterpret historical edits as the new create-only write tool.
#[test]
fn the_built_in_actions_keep_their_own_mappings() {
    let call = match ToolCallId::new("call_builtin") {
        Some(call) => call,
        None => panic!("the witness needs a usable call id"),
    };
    for (action, expected) in [
        ("read", super::READ_TOOL),
        ("write", super::EDIT_TOOL),
        ("fs.modify", super::EDIT_TOOL),
        ("fs.create", super::WRITE_TOOL),
        ("fs.overwrite", super::WRITE_TOOL),
        ("shell", super::SHELL_TOOL),
    ] {
        match super::tool_name(action, &call) {
            Ok(name) => assert_eq!(name.as_str(), expected, "{action} mapped wrongly"),
            Err(refusal) => panic!("{action} must still map: {refusal}"),
        }
    }
}

/// A corrupt action is still a refusal. Carrying the recorded name through is
/// not the same as accepting anything.
#[test]
fn an_empty_durable_action_is_refused() {
    let call = match ToolCallId::new("call_corrupt") {
        Some(call) => call,
        None => panic!("the witness needs a usable call id"),
    };
    match super::tool_name("", &call) {
        Ok(name) => panic!("an empty action must not reconstruct: {}", name.as_str()),
        Err(refusal) => assert!(refusal.contains("unusable durable action"), "{refusal}"),
    }
}

/// A RECORDED IDENTITY THAT DISAGREES WITH ITS OWN PERMISSION IS REFUSED.
///
/// Carrying the recorded qualified name through `tool_name` made non-built-in
/// actions reconstructable. That must not make them TRUSTED: the identity
/// written when the tool was called and the action recorded on its permission
/// describe the same event, and a history where they disagree is corrupt.
///
/// This is the counterexample for that fix. Without the agreement check a
/// replay could attribute a completed call to a tool the permission never
/// authorised, which is exactly the shape an approval exists to prevent.
#[test]
fn a_recorded_identity_conflicting_with_its_permission_is_refused() -> Result<(), String> {
    let scratch = Fixture::new("conflicting-name")?;
    let call = ToolCallId::new("call_d2a_read").ok_or_else(|| "call refused".to_owned())?;
    // The identity says one qualified name; the permission records another.
    scratch.record_exchange_named(
        true,
        false,
        false,
        "journey_journey_echo",
        "journey_other_tool",
    )?;
    let _ = call;

    // The reconstruction happens when the continuation BUILDS ITS INPUT, not
    // when it is constructed. Asserting on construction alone would have
    // passed for a product that never checked anything.
    let continuation = scratch.continuation(scratch.input.clone())?;
    match continuation.request_input("use the tool", true) {
        Ok(_) => Err("a conflicting identity and permission must be refused".to_owned()),
        Err(refusal) => {
            assert!(
                refusal.contains("conflicts with its permission action"),
                "the refusal must name the conflict: {refusal}"
            );
            Ok(())
        }
    }
}

/// A SESSION WITH AN MCP EXCHANGE REOPENS WITH THAT EXCHANGE INTACT.
///
/// The continuation fix made a recorded qualified name reconstructable while
/// planning the NEXT Step. Reopening is a different entry into the same
/// reconstruction — a user closing the client and coming back — and it reads
/// the same durable rows through the same `stored_calls`. So it needs its own
/// witness: a fix that worked only on the continue path would leave a reopened
/// Session unable to see the tool call it had just made.
///
/// The tool name here is the qualified `journey_journey_echo`, which is what
/// the live journeys record, and it must survive the round trip unchanged
/// rather than being normalised to some built-in.
#[test]
fn a_reopened_session_retains_its_mcp_tool_exchange() -> Result<(), String> {
    let mut fixture = Fixture::new("reopen-mcp")?;
    fixture.record_exchange_named(
        true,
        false,
        false,
        "journey_journey_echo",
        "journey_journey_echo",
    )?;
    fixture.close();

    let reopened = fixture.reopen(fixture.input.clone())?;
    let context = reopened.request_input("use the tool", true)?;

    let Some(CodexInputItem::ToolCall { call, tool, .. }) = context.get(2) else {
        return Err("the reopened history lost its tool call".to_owned());
    };
    assert_eq!(call.as_str(), "call_d2a_read");
    assert_eq!(
        tool.as_str(),
        "journey_journey_echo",
        "the reopened history must carry the recorded qualified name, unaltered"
    );
    Ok(())
}

#[path = "signed_tests.rs"]
mod signed_tests;

#[path = "reasoning_tests.rs"]
mod reasoning_tests;

#[cfg(feature = "loopback-provider-testkit")]
#[path = "provider_compaction_tests.rs"]
mod provider_compaction_tests;

#[test]
fn fu11_new_coding_actions_survive_history_reopen() -> Result<(), String> {
    for (name, action) in [("glob", "glob"), ("grep", "grep"), ("write", "fs.create")] {
        let mut fixture = Fixture::new(&format!("fu-replay-{name}"))?;
        fixture.record_exchange_named(true, false, false, name, action)?;
        fixture.close();
        let reopened = fixture.reopen(fixture.input.clone())?;
        let context = reopened.request_input("use the tool", true)?;
        let Some(CodexInputItem::ToolCall { call, tool, .. }) = context.get(2) else {
            return Err("coding replay lost its tool call".into());
        };
        assert_eq!(tool.as_str(), name, "coding replay changed tool identity");
        assert_eq!(
            super::tool_name("write", call)?.as_str(),
            super::EDIT_TOOL,
            "historical write actions must remain edits"
        );
    }
    Ok(())
}

#[path = "summary_tests.rs"]
mod summary_tests;

mod hosted;
