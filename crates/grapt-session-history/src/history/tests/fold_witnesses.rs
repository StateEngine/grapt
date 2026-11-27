pub(super) use super::*;
#[test]
fn completion_observation_is_step_bound_and_preserves_legacy_coordinates(
) -> Result<(), Box<dyn std::error::Error>> {
    for failed_terminal in [false, true] {
        let open = opened(1, "msg_completion", "pat_first", 1);
        let mut terminal = if failed_terminal {
            failed(3, 1)
        } else {
            ended(3, 1)
        };
        let mut opening_position = committed(1);
        opening_position.created_ms = 200;
        let mut terminal_position = committed(3);
        terminal_position.created_ms = 100;
        let mut legacy = MessageHistory::default();
        legacy.apply_committed(open.borrow(), Some(opening_position.clone()))?;
        legacy.apply_committed(terminal.borrow(), Some(terminal_position.clone()))?;
        let old = legacy.messages()?;
        assert!(old[0].completed_at.is_none());
        assert!(
            old[0].terminal.is_some(),
            "unknown historic time must not resurrect a Step"
        );
        assert_eq!(
            old[0].terminal_committed.as_ref().map(|at| at.created_ms),
            Some(100)
        );

        terminal.kind_version = 3;
        terminal.data["completedAt"] = serde_json::json!(300);
        let mut current = MessageHistory::default();
        current.apply_committed(open.borrow(), Some(opening_position.clone()))?;
        current.apply_committed(terminal.borrow(), Some(terminal_position.clone()))?;
        let messages = current.messages()?;
        assert_eq!(
            messages[0].completed_at.map(|at| at.as_millis()),
            Some(300),
            "completion projection reused a reservation rather than its settlement observation"
        );
        assert_eq!(
            messages[0].terminal_committed,
            Some(terminal_position.clone())
        );
        assert!(
            current
                .apply_committed(terminal.borrow(), Some(terminal_position.clone()))
                .is_err(),
            "a second observed terminal resurrected the Step"
        );

        terminal.data["completedAt"] = serde_json::json!(150);
        let mut retrograde = MessageHistory::default();
        retrograde.apply_committed(open.borrow(), Some(opening_position))?;
        assert!(
            retrograde
                .apply_committed(terminal.borrow(), Some(terminal_position))
                .is_err(),
            "completion observation before message opening was accepted"
        );
    }
    Ok(())
}

pub(super) fn reported(value: u64) -> UsageField {
    let count = match TokenCount::new(value) {
        Ok(count) => count,
        Err(error) => panic!("fixture token count is bounded: {error}"),
    };
    UsageField::reported(count)
}

pub(super) fn attempt(raw: &str) -> PhysicalAttemptId {
    match PhysicalAttemptId::new(raw) {
        Some(attempt) => attempt,
        None => panic!("{raw} is a non-empty attempt identity"),
    }
}

pub(super) fn step(value: u32) -> StepNumber {
    match StepNumber::new(value) {
        Some(step) => step,
        None => panic!("{value} is a positive step number"),
    }
}

pub(super) fn message(raw: &str) -> MessageId {
    match raw.parse() {
        Ok(message) => message,
        Err(error) => panic!("{raw} is a valid message identity: {error}"),
    }
}

pub(super) fn reference(length: u64) -> ArtifactRef {
    let id = match ArtifactId::new(format!("art_{length}")) {
        Some(id) => id,
        None => panic!("artifact identity is non-empty"),
    };
    let digest = match ArtifactDigest::new("sha256:aa") {
        Some(digest) => digest,
        None => panic!("digest is non-empty"),
    };
    ArtifactRef::new(id, length, digest)
}

/// One durable row, owned so a test can build a list of them.
pub(super) struct Row {
    pub(super) seq: u64,
    pub(super) kind_name: &'static str,
    pub(super) kind_version: u32,
    pub(super) data: Value,
}

impl Row {
    pub(super) fn borrow(&self) -> HistoryRow<'_> {
        HistoryRow {
            seq: self.seq,
            kind_name: self.kind_name,
            kind_version: self.kind_version,
            data: &self.data,
        }
    }
}

pub(super) fn opened(seq: u64, msg: &str, at: &str, number: u32) -> Row {
    Row {
        seq,
        kind_name: "session.assistant-message.opened",
        kind_version: 1,
        data: sm::encode_opened(&AssistantMessageOpenedFact::new(
            attempt(at),
            step(number),
            message(msg),
        )),
    }
}

#[test]
fn opening_selection_survives_picker_changes_and_replay() -> Result<(), Box<dyn std::error::Error>>
{
    let selection = sm::AssistantSelection::new(
        grapt_types::AgentId::new("agt_selected").ok_or("agent")?,
        grapt_types::ProviderId::new("openai").ok_or("provider")?,
        grapt_types::ModelId::new("first-model").ok_or("model")?,
        Some(grapt_types::ModelVariantId::new("high").ok_or("variant")?),
    );
    let mut row = opened(1, "msg_selected", "pat_selected", 1);
    row.kind_version = 2;
    row.data = sm::encode_opened(
        &AssistantMessageOpenedFact::new(attempt("pat_selected"), step(1), message("msg_selected"))
            .with_selection(selection.clone()),
    );
    let mut history = MessageHistory::default();
    history.apply(row.borrow())?;
    history.apply(HistoryRow {
        seq: 2,
        kind_name: "session.model.selected",
        kind_version: 1,
        data: &serde_json::json!({"model":{"providerID":"openai","id":"later-model"}}),
    })?;
    let messages = history.messages()?;
    assert_eq!(messages[0].selection.as_ref(), Some(&selection));
    let mut replay = MessageHistory::default();
    replay.apply(row.borrow())?;
    assert_eq!(replay.messages()?, messages);
    row.data["selection"]["model"] = Value::Null;
    // The row does not decode, so it is never folded with a defaulted model:
    // it is left out and named, whatever the fold's refusal policy is.
    let mut missing = MessageHistory::default();
    let _ = missing.apply(row.borrow());
    assert!(
        missing.messages()?.is_empty(),
        "missing durable identity became a default model"
    );
    assert_eq!(
        missing.omissions().len(),
        1,
        "the undecodable opening vanished without a marker"
    );
    Ok(())
}

pub(super) fn appended(seq: u64, msg: &str, ordinal: u64, kind: ContentKind) -> Row {
    Row {
        seq,
        kind_name: "session.content.appended",
        kind_version: 1,
        data: sm::encode_appended(&match ContentAppendedFact::new(
            message(msg),
            NormalizedContentItem::new(ContentOrdinal::new(ordinal), kind, reference(ordinal + 1)),
            CaptureDisposition::Complete,
        ) {
            Ok(fact) => fact,
            Err(error) => panic!("the fixture fact must be constructible: {error}"),
        }),
    }
}

pub(super) fn ended(seq: u64, number: u32) -> Row {
    Row {
        seq,
        kind_name: "session.step.ended",
        kind_version: 2,
        data: session_step_ended::encode(&session_step_ended::StepEndedFact::new(
            step(number),
            ProviderCompletionDisposition::AssertedComplete,
        )),
    }
}

pub(super) fn failed(seq: u64, number: u32) -> Row {
    let fact = match session_step_failed::StepFailedFact::new(
        step(number),
        StepTerminalReason::Interrupted {
            by: grapt_types::InterruptionSource::User,
        },
    ) {
        Ok(fact) => fact,
        Err(error) => panic!("interruption is a failure reason: {error}"),
    };
    Row {
        seq,
        kind_name: "session.step.failed",
        kind_version: 2,
        data: match session_step_failed::encode(&fact) {
            Ok(data) => data,
            Err(error) => panic!("the fixture fact must encode: {error}"),
        },
    }
}

pub(super) fn committed(seq: u64) -> CommittedPosition {
    let event = match format!("evt_terminal_{seq}").parse() {
        Ok(event) => event,
        Err(error) => panic!("terminal event identity must be valid: {error}"),
    };
    let offset = match i64::try_from(seq) {
        Ok(offset) => offset,
        Err(_) => panic!("terminal sequence must fit the deterministic timestamp"),
    };
    CommittedPosition {
        event,
        seq,
        created_ms: 1_786_060_800_000_i64.saturating_add(offset),
    }
}

pub(super) fn fold(rows: &[Row]) -> Result<MessageHistory, HistoryError> {
    let mut history = MessageHistory::default();
    for row in rows {
        history.apply(row.borrow())?;
    }
    Ok(history)
}

pub(super) fn folded(rows: &[Row]) -> MessageHistory {
    match fold(rows) {
        Ok(history) => history,
        Err(error) => panic!("these rows must fold: {error}"),
    }
}

pub(super) fn messages(history: &MessageHistory) -> Vec<AssistantMessage> {
    match history.messages() {
        Ok(messages) => messages,
        Err(error) => panic!("this history must render: {error}"),
    }
}

/// The success cut: one Step's message, its interleaved content in ordinal
/// order, and its one terminal.
#[test]
fn one_step_folds_to_one_message_with_ordered_content_and_one_terminal() {
    let rows = [
        opened(0, "msg_a", "pat_1", 1),
        appended(1, "msg_a", 0, ContentKind::Reasoning),
        appended(2, "msg_a", 1, ContentKind::Text),
        ended(3, 1),
    ];
    let history = folded(&rows);
    let messages = messages(&history);
    assert_eq!(messages.len(), 1);
    let only = &messages[0];
    assert_eq!(only.message, message("msg_a"));
    assert_eq!(only.step, step(1));
    assert_eq!(only.opened_at_seq, 0);
    let content = match &only.content {
        Some(content) => content,
        None => panic!("the message has content"),
    };
    // Reasoning kept its position BEFORE the text. A per-kind array shape
    // would have lost exactly this.
    assert_eq!(content.len(), 2);
    assert_eq!(content.items()[0].item.kind(), &ContentKind::Reasoning);
    assert_eq!(content.items()[1].item.kind(), &ContentKind::Text);
    assert_eq!(
        only.terminal,
        Some(StepTerminal::Ended {
            completion: ProviderCompletionDisposition::AssertedComplete
        })
    );
    assert_eq!(history.orphaned_items(), 0);
}

/// Items may reach the fold out of order — nothing promises the store's append
/// order matches ordinal order — and the rendered sequence is still exact.
#[test]
fn content_arriving_out_of_order_still_renders_in_ordinal_order() {
    let rows = [
        opened(0, "msg_a", "pat_1", 1),
        appended(1, "msg_a", 1, ContentKind::Text),
        appended(2, "msg_a", 0, ContentKind::Reasoning),
    ];
    let messages = messages(&folded(&rows));
    let content = match &messages[0].content {
        Some(content) => content,
        None => panic!("the message has content"),
    };
    assert_eq!(content.items()[0].item.ordinal(), ContentOrdinal::FIRST);
    assert_eq!(content.items()[0].item.kind(), &ContentKind::Reasoning);
}

/// REPLAY IS A FUNCTION. Folding the same rows twice produces the same
/// history, and the second fold opens no provider stream because the fold has
/// no provider to open — it consumes rows and nothing else.
#[test]
fn folding_the_same_rows_twice_yields_an_equal_history() {
    let rows = [
        opened(0, "msg_a", "pat_1", 1),
        appended(1, "msg_a", 0, ContentKind::Text),
        ended(2, 1),
        opened(3, "msg_b", "pat_2", 2),
        appended(4, "msg_b", 0, ContentKind::Reasoning),
        failed(5, 2),
    ];
    let first = messages(&folded(&rows));
    let second = messages(&folded(&rows));
    assert_eq!(first, second);
    assert_eq!(first.len(), 2);
    // Two Steps, two terminals, and the failure did NOT become a success.
    assert!(matches!(
        first[0].terminal,
        Some(StepTerminal::Ended { .. })
    ));
    assert!(matches!(
        first[1].terminal,
        Some(StepTerminal::Failed { .. })
    ));
}

/// A Step can fail before the provider yields its first fragment. The terminal
/// remains a Session fact even though there is no assistant message on which a
/// message-shaped projection could hang it.
#[test]
fn terminal_commit_is_projected_without_an_assistant_message() {
    let row = failed(7, 1);
    let mut history = MessageHistory::default();
    history
        .apply_committed(row.borrow(), Some(committed(7)))
        .unwrap_or_else(|error| panic!("terminal-only history must fold: {error}"));

    assert!(messages(&history).is_empty());
    assert_eq!(
        history.latest_terminal_committed(),
        Some(&committed(7)),
        "the typed terminal position must not depend on AssistantMessage existence"
    );
}

/// The resurrection cut. A second durable terminal for one Step is refused by
/// name — not absorbed as an update, and not reported as ordinary history.
#[test]
fn a_second_terminal_for_one_step_is_refused_as_a_resurrection() {
    let rows = [opened(0, "msg_a", "pat_1", 1), ended(1, 1), ended(2, 1)];
    assert_eq!(
        fold(&rows).err(),
        Some(HistoryError::ResurrectedTerminal { step: 1, seq: 2 })
    );
}

/// A failure after a success is the same defect wearing a different hat: the
/// terminal is settled, and the SECOND kind does not make it a fresh one.
#[test]
fn a_failure_terminal_cannot_follow_a_success_terminal_for_one_step() {
    let rows = [opened(0, "msg_a", "pat_1", 1), ended(1, 1), failed(2, 1)];
    assert_eq!(
        fold(&rows).err(),
        Some(HistoryError::ResurrectedTerminal { step: 1, seq: 2 })
    );
}

/// Two items claiming one position is corruption. Silently keeping the last
/// would report a transcript the model never produced.
#[test]
fn two_content_items_at_one_ordinal_are_refused() {
    let rows = [
        opened(0, "msg_a", "pat_1", 1),
        appended(1, "msg_a", 0, ContentKind::Text),
        appended(2, "msg_a", 0, ContentKind::Reasoning),
    ];
    assert_eq!(
        fold(&rows).err(),
        Some(HistoryError::DuplicateOrdinal {
            message: "msg_a".to_owned(),
            ordinal: 0,
        })
    );
}

/// A gap in the ordinals cannot build one ordered sequence. It is refused at
/// render, where the whole item set is finally known.
#[test]
fn a_gap_in_the_ordinals_refuses_to_render_as_one_sequence() {
    let rows = [
        opened(0, "msg_a", "pat_1", 1),
        appended(1, "msg_a", 0, ContentKind::Text),
        appended(2, "msg_a", 2, ContentKind::Text),
    ];
    let history = folded(&rows);
    assert_eq!(
        history.messages().err(),
        Some(HistoryError::SparseContent {
            message: "msg_a".to_owned(),
        })
    );
}

/// Reopening one message identity is refused: the identity is the message, and
/// a second opening would make its attempt and Step ambiguous.
#[test]
fn reopening_one_message_is_refused() {
    let rows = [
        opened(0, "msg_a", "pat_1", 1),
        opened(1, "msg_a", "pat_2", 2),
    ];
    assert_eq!(
        fold(&rows).err(),
        Some(HistoryError::ReopenedMessage {
            message: "msg_a".to_owned(),
            seq: 1,
        })
    );
}

/// Head compaction may remove an opening fact while its items remain. That is
/// MEASURED, not refused — the same posture `SessionProjection::seedless`
/// takes for a lifecycle update without its seed.
#[test]
fn an_item_without_its_opening_fact_is_counted_not_refused() {
    let rows = [appended(0, "msg_gone", 0, ContentKind::Text)];
    let history = folded(&rows);
    assert_eq!(history.orphaned_items(), 1);
    // It is counted, and it does NOT invent a message: the rendered history is
    // empty because no message was opened in the folded range.
    assert!(messages(&history).is_empty());
}

/// An unsettled Step reports `None`, never a success. This is the distinction
/// a `done: bool` cannot carry.
#[test]
fn an_unsettled_step_reports_no_terminal_rather_than_a_success() {
    let rows = [
        opened(0, "msg_a", "pat_1", 1),
        appended(1, "msg_a", 0, ContentKind::Text),
    ];
    let messages = messages(&folded(&rows));
    assert_eq!(messages[0].terminal, None);
}

/// An opened message that produced nothing is a real state, distinct from a
/// message with content.
#[test]
fn an_opened_message_with_no_items_has_no_sequence() {
    let rows = [opened(0, "msg_a", "pat_1", 1)];
    let messages = messages(&folded(&rows));
    assert_eq!(messages[0].content, None);
}

/// Usage observations accumulate in order rather than overwriting, so a
/// terminal truth is visibly a LATER fact than a stream estimate.
#[test]
fn usage_observations_accumulate_in_observation_order() {
    let terminal = UsageObservedFact::new(
        attempt("pat_1"),
        AttemptUsage::observed(
            reported(14),
            reported(11),
            reported(7),
            reported(3),
            reported(2),
            reported(1),
            reported(21),
            UsageObservationPhase::Terminal,
        ),
    );
    let rows = [
        Row {
            seq: 0,
            kind_name: "session.usage.observed",
            kind_version: 1,
            data: serde_json::json!({
                "attempt": "pat_1",
                "usage": {
                    "outputTokens": 3,
                    "phase": "during-stream",
                    "provenance": "harness-estimated",
                    "policy": "pol_1"
                }
            }),
        },
        Row {
            seq: 1,
            kind_name: "session.usage.observed",
            kind_version: 2,
            data: sm::encode_observed(&terminal),
        },
    ];
    let history = folded(&rows);
    let observed = history.usage_for(&attempt("pat_1"));
    assert_eq!(observed.len(), 2);
    assert_eq!(observed[0].phase(), UsageObservationPhase::DuringStream);
    assert_eq!(observed[1].phase(), UsageObservationPhase::Terminal);
    // The estimate did NOT become the reported truth, and the absent input
    // field did not become a zero that the terminal then double-charged.
    assert!(observed[0].input_tokens().is_none());
    assert_eq!(
        observed[0]
            .input_tokens_ambiguous()
            .and_then(UsageField::value),
        None
    );
    assert_eq!(
        observed[1].input_tokens().and_then(UsageField::value),
        Some(11)
    );
    assert_eq!(
        observed[0].output_tokens().provenance(),
        grapt_types::PolicyVersion::new("pol_1")
            .map(|policy| UsageProvenance::HarnessEstimated { policy })
            .as_ref()
    );
    let restarted = folded(&rows);
    assert_eq!(restarted.usage_for(&attempt("pat_1")), observed);
    assert!(history.usage_for(&attempt("pat_other")).is_empty());
}

/// The next ordinal comes from DURABLE truth, so a restarted owner resumes at
/// the true position instead of overwriting ordinal zero.
#[test]
fn the_next_ordinal_resumes_from_what_is_durable() {
    let rows = [
        opened(0, "msg_a", "pat_1", 1),
        appended(1, "msg_a", 0, ContentKind::Text),
        appended(2, "msg_a", 1, ContentKind::Text),
    ];
    let history = folded(&rows);
    assert_eq!(
        history.next_ordinal(&message("msg_a")),
        ContentOrdinal::new(2)
    );
    // A message with nothing durable starts at the first ordinal.
    assert_eq!(
        history.next_ordinal(&message("msg_fresh")),
        ContentOrdinal::FIRST
    );
}

/// Rows belonging to other clusters pass through untouched. The log is one
/// family-tagged union and this fold owns five kinds.
#[test]
fn rows_this_fold_does_not_own_are_ignored() {
    let rows = [
        Row {
            seq: 0,
            kind_name: "session.input.admitted",
            kind_version: 1,
            data: serde_json::json!({"anything": true}),
        },
        // The ORACLE's v1 spellings are not this fold's kinds either: reading
        // them here would double-count a migrated history against the
        // Rust-native facts written beside it.
        Row {
            seq: 1,
            kind_name: "session.step.ended",
            kind_version: 1,
            data: serde_json::json!({"anything": true}),
        },
        Row {
            seq: 2,
            kind_name: "message.part.updated",
            kind_version: 1,
            data: serde_json::json!({"anything": true}),
        },
    ];
    let history = folded(&rows);
    assert!(messages(&history).is_empty());
    assert_eq!(history.orphaned_items(), 0);
}

/// An undecodable row names its kind and its sequence rather than being
/// skipped. A fold that skipped it would report a shorter history as a
/// complete one.
#[test]
fn an_undecodable_owned_row_names_its_kind_and_sequence() {
    let rows = [Row {
        seq: 4,
        kind_name: "session.content.appended",
        kind_version: 1,
        data: serde_json::json!({"message": "msg_a"}),
    }];
    let history = match fold(&rows) {
        Ok(history) => history,
        Err(error) => panic!("an undecodable row must be an omission: {error}"),
    };
    let [omission] = history.omissions() else {
        panic!(
            "expected exactly one omission, got {:?}",
            history.omissions()
        );
    };
    assert_eq!(omission.kind, "session.content.appended");
    assert_eq!(omission.seq, 4);
    assert_eq!(omission.class, HistoryOmissionClass::Undecodable);
    // A payload that only names its message and nothing else cannot attribute
    // the gap, so it is reported unattached rather than guessed at.
    assert_eq!(omission.message, None);
}

/// The SQL/string boundary the unit fold above cannot reach.
///
/// `MessageHistory::apply` is handed a decoded `Value` and a split kind, so
/// every witness that calls `fold()` starts AFTER the three steps that used to
/// swallow a row: splitting the stored `type` into a name and a version,
/// parsing the stored `data` as JSON, and reading the stored `seq`. Those steps
/// live in `SessionHistoryStore::fold`, and until this helper existed no
/// witness went through them at all — which is why every one of them could
/// `continue` past a row this fold OWNS and hand the caller a shorter
/// transcript reported as a successful read.
///
/// The table mirrors the governed `event` table's columns for exactly the
/// coordinates the fold's own query names. It is hand-built rather than
/// obtained from `EventStore` because the store's write gate refuses the
/// corrupt rows these witnesses exist to fold — a row no legal write can
/// produce is precisely the durable corruption a read model must survive.
pub(super) fn stored(rows: &[(i64, &str, &str)]) -> rusqlite::Connection {
    let conn = match rusqlite::Connection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => panic!("an in-memory connection must open: {error}"),
    };
    if let Err(error) = conn.execute_batch(
        "CREATE TABLE event (
            id TEXT PRIMARY KEY,
            aggregate_family TEXT NOT NULL,
            aggregate_id TEXT NOT NULL,
            seq INTEGER NOT NULL,
            created INTEGER NOT NULL,
            type TEXT NOT NULL,
            data TEXT
        );",
    ) {
        panic!("the event table must create: {error}");
    }
    for (index, (seq, kind, data)) in rows.iter().enumerate() {
        if let Err(error) = conn.execute(
            "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data)
             VALUES (?1, 'session', 'ses_df0a', ?2, 0, ?3, ?4)",
            rusqlite::params![format!("evt_{index}"), seq, kind, data],
        ) {
            panic!("the row must insert: {error}");
        }
    }
    conn
}

pub(super) fn append_stored_tool_call(conn: &rusqlite::Connection, ordinal: usize) {
    let call = match ToolCallId::new(format!("call_page_{ordinal}")) {
        Some(call) => call,
        None => panic!("the static tool-call identity is non-empty"),
    };
    let ordinal_u64 = match u64::try_from(ordinal) {
        Ok(value) => value,
        Err(_) => panic!("the bounded fixture ordinal fits u64"),
    };
    let fact = match ContentAppendedFact::new(
        message("msg_tool_page"),
        NormalizedContentItem::new(
            ContentOrdinal::new(ordinal_u64),
            ContentKind::Tool { call },
            reference(ordinal_u64.saturating_add(1)),
        ),
        CaptureDisposition::Complete,
    ) {
        Ok(fact) => fact,
        Err(error) => panic!("the bounded tool fixture must be valid: {error}"),
    };
    let data = sm::encode_appended(&fact).to_string();
    let seq = match i64::try_from(ordinal) {
        Ok(value) => value,
        Err(_) => panic!("the bounded fixture sequence fits i64"),
    };
    if let Err(error) = conn.execute(
        "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data)
         VALUES (?1, 'session', 'ses_df0a', ?2, ?2, 'session.content.appended.1', ?3)",
        rusqlite::params![format!("evt_tool_page_{ordinal}"), seq, data],
    ) {
        panic!("the bounded tool row must insert: {error}");
    }
}

pub(super) fn session() -> SessionId {
    match "ses_df0a".parse() {
        Ok(session) => session,
        Err(error) => panic!("ses_df0a is a valid session identity: {error}"),
    }
}

#[test]
fn tool_call_page_accepts_exactly_its_bound_and_observes_the_first_over_bound_row() {
    pub(super) const RATIFIED_TOOL_CALL_ROWS: usize = crate::MAX_TOOL_CALL_PAGE_ROWS;
    let conn = stored(&[]);
    for ordinal in 0..RATIFIED_TOOL_CALL_ROWS {
        append_stored_tool_call(&conn, ordinal);
    }
    let page = match SessionHistoryStore::new(&conn).tool_call_page(&session()) {
        Ok(page) => page,
        Err(error) => panic!("the exact page at its bound must be readable: {error}"),
    };
    assert_eq!(page.rows().len(), RATIFIED_TOOL_CALL_ROWS);
    assert_eq!(page.rows()[0].call().as_str(), "call_page_0");
    assert_eq!(
        page.rows()[RATIFIED_TOOL_CALL_ROWS - 1].call().as_str(),
        format!("call_page_{}", RATIFIED_TOOL_CALL_ROWS - 1)
    );

    append_stored_tool_call(&conn, RATIFIED_TOOL_CALL_ROWS);
    assert!(
        matches!(
            SessionHistoryStore::new(&conn).tool_call_page(&session()),
            Err(HistoryReadError::ToolCallPageTooLong { .. })
        ),
        "the first over-bound durable tool call was not observed"
    );
}

/// A stored row this fold OWNS whose data is not JSON is a named omission.
///
/// It was silently skipped once: the caller received a transcript with the
/// content item missing and no indication anything had gone wrong, which is
/// the same lie as reporting a truncated capture as complete. The cure is a
/// marker, not a refused read: one bad row costs that item and every consumer
/// can see the gap.
#[test]
fn an_owned_row_whose_data_is_not_json_is_a_named_omission() {
    let conn = stored(&[(0, "session.content.appended.1", "{not json")]);
    let history = match SessionHistoryStore::new(&conn).fold(&session()) {
        Ok(history) => history,
        Err(error) => panic!("one unreadable row must not refuse the read: {error}"),
    };
    let [omission] = history.omissions() else {
        panic!(
            "expected exactly one omission, got {:?}",
            history.omissions()
        );
    };
    assert_eq!(omission.seq, 0);
    assert_eq!(omission.kind, "session.content.appended");
    assert_eq!(omission.class, HistoryOmissionClass::Unclassified);
    assert!(
        omission.detail.contains("not JSON"),
        "got: {}",
        omission.detail
    );
    assert!(history.messages().is_ok());
}

#[test]
fn a_corrupt_v2_usage_leaf_is_named_instead_of_disappearing() {
    let conn = stored(&[(
        0,
        "session.usage.observed.2",
        r#"{"attempt":"pat_1","usage":{"inputFreshTokens":{"count":8,"provenance":"provider-derived"},"phase":"terminal"}}"#,
    )]);
    let history = match SessionHistoryStore::new(&conn).fold(&session()) {
        Ok(history) => history,
        Err(error) => panic!("one unreadable usage row must not refuse the read: {error}"),
    };
    let [omission] = history.omissions() else {
        panic!(
            "expected exactly one omission, got {:?}",
            history.omissions()
        );
    };
    assert_eq!(omission.seq, 0);
    assert_eq!(omission.kind, "session.usage.observed");
    assert_eq!(omission.class, HistoryOmissionClass::Undecodable);
    assert!(
        omission.detail.contains("rule"),
        "the corrupt derived leaf failed for the wrong reason: {}",
        omission.detail
    );
}

/// A row belonging to no kind this fold owns still skips.
///
/// This is the other half of the rule and it has to be witnessed separately:
/// a fold that refused EVERY unreadable row would refuse the inbox, permission
/// and campaign clusters that legitimately share the log, so "unowned kinds may
/// be ignored" would have been traded for a read model that cannot read.
#[test]
fn an_unowned_row_is_still_ignored_however_broken_it_is() {
    let conn = stored(&[
        (0, "inbox.delivered.1", "{not json"),
        (1, "campaign.transitioned", "also not json"),
    ]);
    match SessionHistoryStore::new(&conn).fold(&session()) {
        Ok(history) => assert_eq!(history.orphaned_items(), 0),
        Err(error) => panic!("unowned rows are not this fold's business: {error}"),
    }
}

/// A stored sequence below zero is corruption rather than sequence zero.
///
/// Mapping it to zero — as an earlier revision did — placed the row at the HEAD
/// of the fold, so a corrupt sequence read as the first fact the Session ever
/// wrote and could displace the real opening.
#[test]
fn a_negative_stored_sequence_on_an_owned_row_is_corruption() {
    let conn = stored(&[(-3, "session.usage.observed.1", "{}")]);
    match SessionHistoryStore::new(&conn).fold(&session()) {
        Err(HistoryReadError::Corrupt { seq, reason, .. }) => {
            assert_eq!(seq, -3);
            assert!(reason.contains("-3"), "got: {reason}");
        }
        other => panic!("expected corruption, got {other:?}"),
    }
}

/// An owned kind stored with no numeric version is corruption.
///
/// Both readings are refused: `session.step.ended` splits into a name this fold
/// does not own and a "version" that is not a number, so a check that looked
/// only at the split reading would let the row skip.
#[test]
fn an_owned_kind_with_no_numeric_version_is_a_named_omission() {
    for stored_type in ["session.step.ended", "session.content.appended.v1"] {
        let conn = stored(&[(0, stored_type, "{}")]);
        let history = match SessionHistoryStore::new(&conn).fold(&session()) {
            Ok(history) => history,
            Err(error) => panic!("{stored_type} must not refuse the read: {error}"),
        };
        let [omission] = history.omissions() else {
            panic!(
                "{stored_type}: expected one omission, got {:?}",
                history.omissions()
            );
        };
        assert_eq!(omission.class, HistoryOmissionClass::Unclassified);
        assert!(
            omission.detail.contains("version"),
            "{stored_type}: got {}",
            omission.detail
        );
    }
}

/// An owned NAME at a version this fold does not read still passes through.
///
/// The corruption rule is about rows that cannot be READ AT ALL — a type with
/// no numeric version, data that is not JSON, a sequence below zero. It is
/// deliberately not a rule about version NUMBERS: the oracle's v1 spellings
/// share these names, and refusing them would break every migrated history.
/// Witnessed here rather than left implicit because the corruption rule is one
/// mistaken generalization away from swallowing them.
#[test]
fn an_owned_name_at_another_era_s_version_is_still_ignored() {
    let conn = stored(&[
        (2, "session.content.appended.9", "{}"),
        (3, "session.step.ended.1", "{}"),
    ]);
    match SessionHistoryStore::new(&conn).fold(&session()) {
        Ok(history) => assert_eq!(history.orphaned_items(), 0),
        Err(error) => panic!("another era's versions are not corruption: {error}"),
    }
}

/// Usage is keyed by the WHOLE attempt identity, not by anything near it.
///
/// The map was keyed by `String` and reached through `as_str()`, so the read
/// model had a second, unvalidated way to name an attempt — and no witness
/// distinguished "looked it up" from "returned whatever was there", because
/// every usage witness folded exactly one attempt. Two attempts is the
/// smallest population that can tell those apart.
#[test]
fn usage_is_keyed_by_the_whole_attempt_identity() {
    let observation = |at: &str, output: u64| Row {
        seq: 0,
        kind_name: "session.usage.observed",
        kind_version: 2,
        data: sm::encode_observed(&UsageObservedFact::new(
            attempt(at),
            AttemptUsage::observed(
                UsageField::ABSENT,
                UsageField::ABSENT,
                reported(output),
                UsageField::ABSENT,
                UsageField::ABSENT,
                UsageField::ABSENT,
                UsageField::ABSENT,
                UsageObservationPhase::Terminal,
            ),
        )),
    };
    let history = folded(&[observation("pat_1", 3), observation("pat_2", 9)]);
    let only = |at: &str| match history.usage_for(&attempt(at)) {
        [single] => single.output_tokens().value(),
        other => panic!("{at} has exactly one observation, got {}", other.len()),
    };
    assert_eq!(only("pat_1"), Some(3));
    assert_eq!(only("pat_2"), Some(9));
    // And an attempt with no observations answers with nothing rather than
    // with somebody else's.
    assert!(history.usage_for(&attempt("pat_3")).is_empty());
}

#[test]
fn retry_usage_follows_the_committed_block_binding_without_stealing_another_attempt() {
    let binding = Row {
        seq: 2,
        kind_name: "session.content-block.opened",
        kind_version: 1,
        data: sm::encode_block_opened(&sm::ContentBlockOpenedFact::new(
            attempt("pat_retry"),
            message("msg_retry"),
            0,
            sm::ContentBlockKind::Text,
        )),
    };
    let usage = |seq, at, output| Row {
        seq,
        kind_name: "session.usage.observed",
        kind_version: 2,
        data: sm::encode_observed(&UsageObservedFact::new(
            attempt(at),
            AttemptUsage::observed(
                UsageField::ABSENT,
                UsageField::ABSENT,
                reported(output),
                UsageField::ABSENT,
                UsageField::ABSENT,
                UsageField::ABSENT,
                UsageField::ABSENT,
                UsageObservationPhase::Terminal,
            ),
        )),
    };
    let rows = [
        opened(0, "msg_retry", "pat_original", 1),
        opened(1, "msg_other", "pat_other", 2),
        binding,
        usage(3, "pat_retry", 9),
        usage(4, "pat_other", 17),
    ];
    let history = folded(&rows);
    let rendered = messages(&history);
    assert_eq!(rendered[0].attempt, attempt("pat_original"));
    assert_eq!(rendered[0].usage_attempt, attempt("pat_retry"));
    assert_eq!(
        rendered[0].usage.len(),
        1,
        "retry usage lost despite explicit durable binding"
    );
    assert_eq!(rendered[0].usage[0].output_tokens().value(), Some(9));
    assert_eq!(rendered[1].usage[0].output_tokens().value(), Some(17));
    assert_eq!(
        messages(&folded(&rows)),
        rendered,
        "replay changed retry attribution"
    );
    assert_eq!(history.usage_attempts().count(), 2);

    let mut crossed = folded(&rows);
    let wrong = sm::encode_block_opened(&sm::ContentBlockOpenedFact::new(
        attempt("pat_retry"),
        message("msg_other"),
        1,
        sm::ContentBlockKind::Text,
    ));
    assert!(
        crossed
            .apply(HistoryRow {
                seq: 5,
                kind_name: "session.content-block.opened",
                kind_version: 1,
                data: &wrong
            })
            .is_err(),
        "one retry was attributed to two messages"
    );
}
