//! Operate the shipped command against facts produced by the governed store.
use grapt_event::session_message::{
    AssistantMessageOpenedFact, AssistantSelection, ContentBlockKind, ContentBlockOpenedFact,
    UsageObservedFact,
};
use grapt_store::{
    AggregateId, AttemptRecordCeiling, EventStore, ReserveInput, TerminalCoordinates,
};
use grapt_types::*;
use std::num::NonZeroU32;
use std::path::Path;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[expect(clippy::too_many_lines, reason = "legacy")]
fn fixture(path: &Path) -> TestResult {
    let mut store = EventStore::open(&path.join("database.sqlite3"), "lua-fixture")?;
    let session: SessionId = "ses_lua_fixture".parse()?;
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    let writer = store.fencing_token(&AggregateId::Session(session.clone()))?;
    let attempt = PhysicalAttemptId::new("pat_lua_first").ok_or("attempt")?;
    let unknown = PhysicalAttemptId::new("pat_lua_reclaimed").ok_or("attempt")?;
    let step = StepNumber::new(1).ok_or("step")?;
    let bound = NonZeroU32::new(4096).ok_or("bound")?;
    let receipt = DurableAttemptReceipt::reserved(
        BoundedAttemptId::new(unknown.clone(), bound).map_err(|e| format!("{e:?}"))?,
        BoundedSessionId::new(session.clone(), bound).map_err(|e| format!("{e:?}"))?,
        step,
        AttemptOrdinal::new(2).ok_or("ordinal")?,
        ReservedMarker::new(Timestamp::from_millis(100)),
    );
    let terminal = TerminalCoordinates::new("evt_lua_terminal".parse()?, 100);
    store.reserve_attempt_receipt(&ReserveInput {
        receipt: &receipt,
        writer,
        terminal: &terminal,
        ceiling: AttemptRecordCeiling::new(bound),
    })?;
    let receipt_only = DurableAttemptReceipt::reserved(
        BoundedAttemptId::new(
            PhysicalAttemptId::new("pat_receipt_only").ok_or("attempt")?,
            bound,
        )
        .map_err(|e| format!("{e:?}"))?,
        BoundedSessionId::new(session.clone(), bound).map_err(|e| format!("{e:?}"))?,
        StepNumber::new(2).ok_or("step")?,
        AttemptOrdinal::new(1).ok_or("ordinal")?,
        ReservedMarker::new(Timestamp::from_millis(300)),
    );
    let pending_terminal = TerminalCoordinates::new("evt_pending_terminal".parse()?, 300);
    store.reserve_attempt_receipt(&ReserveInput {
        receipt: &receipt_only,
        writer,
        terminal: &pending_terminal,
        ceiling: AttemptRecordCeiling::new(bound),
    })?;
    let unavailable = PhysicalAttemptId::new("pat_lua_unavailable").ok_or("attempt")?;
    for (id, ordinal) in [(&attempt, 1), (&unavailable, 3)] {
        let receipt = DurableAttemptReceipt::reserved(
            BoundedAttemptId::new(id.clone(), bound).map_err(|e| format!("{e:?}"))?,
            BoundedSessionId::new(session.clone(), bound).map_err(|e| format!("{e:?}"))?,
            step,
            AttemptOrdinal::new(ordinal).ok_or("ordinal")?,
            ReservedMarker::new(Timestamp::from_millis(100)),
        );
        store.reserve_attempt_receipt(&ReserveInput {
            receipt: &receipt,
            writer,
            terminal: &terminal,
            ceiling: AttemptRecordCeiling::new(bound),
        })?;
    }
    let tx = store.governed_transaction()?;
    for (id, at) in [
        (&attempt, 100),
        (&unknown, 100),
        (&unavailable, 100),
        (receipt_only.attempt(), 300),
    ] {
        EventStore::permit_attempt_send_within(
            &tx,
            id,
            writer,
            AttemptRecordCeiling::new(bound),
            RequestSentMarker::new(Timestamp::from_millis(at)),
        )?;
    }
    tx.commit()?.discard();
    let tx = store.governed_transaction()?;
    let opened = AssistantMessageOpenedFact::new(attempt.clone(), step, "msg_lua".parse()?)
        .with_selection(AssistantSelection::new(
            AgentId::new("fixture").ok_or("agent")?,
            ProviderId::new("fixture").ok_or("provider")?,
            ModelId::new("deterministic").ok_or("model")?,
            None,
        ));
    grapt_store::session_message::record_opened(
        &tx,
        &session,
        &"evt_lua_open".parse()?,
        writer,
        200,
        &opened,
        &mut [],
    )?;
    let block = ContentBlockOpenedFact::new(
        attempt.clone(),
        "msg_lua".parse()?,
        0,
        ContentBlockKind::Text,
    );
    grapt_store::session_message::record_block_opened(
        &tx,
        &session,
        &"evt_lua_block".parse()?,
        writer,
        240,
        &block,
        &mut [],
    )?;
    let field = |value| TokenCount::new(value).map(UsageField::reported);
    let complete = AttemptUsage::observed(
        field(110)?,
        field(100)?,
        field(10)?,
        field(4)?,
        field(7)?,
        field(3)?,
        field(120)?,
        UsageObservationPhase::Terminal,
    );
    for (id, who) in [
        ("evt_lua_usage", attempt.clone()),
        ("evt_lua_duplicate", attempt),
        ("evt_lua_unbound", unknown.clone()),
    ] {
        grapt_store::session_message::record_observed(
            &tx,
            &session,
            &id.parse()?,
            writer,
            250,
            &UsageObservedFact::new(who, complete.clone()),
            &mut [],
        )?;
    }
    let ended = grapt_event::session_step_ended::StepEndedFact::new(
        step,
        ProviderCompletionDisposition::AssertedComplete,
    );
    grapt_store::step_ended::record(
        &tx,
        &session,
        terminal.event(),
        writer,
        100,
        &ended,
        &mut [],
    )?;
    EventStore::settle_attempt_ordinary_terminal_within(
        &tx,
        &unknown,
        writer,
        AttemptRecordCeiling::new(bound),
        SettledMarker::new(Timestamp::from_millis(300)),
    )?;
    EventStore::settle_attempt_ordinary_terminal_within(
        &tx,
        &unavailable,
        writer,
        AttemptRecordCeiling::new(bound),
        SettledMarker::new(Timestamp::from_millis(300)),
    )?;
    tx.commit()?.discard();
    assert!(
        store
            .attempt_receipt(&unknown, AttemptRecordCeiling::new(bound))?
            .is_none(),
        "real lifecycle failed to reclaim receipt"
    );
    Ok(())
}

fn run(root: &Path, prices: Option<&Path>) -> Result<std::process::Output, std::io::Error> {
    let mut command =
        grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"));
    command
        .arg("usage")
        .arg("--service")
        .arg(root)
        .args(["--session", "ses_lua_fixture"]);
    if let Some(prices) = prices {
        command.arg("--prices").arg(prices);
    }
    command.output()
}

#[test]
fn sc_acct01_terminal_unreported_attempts_complete_reconciliation_without_zero_tokens() -> TestResult
{
    let root = std::env::temp_dir().join(format!("acct01-complete-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    fixture(&root)?;
    {
        let mut store = EventStore::open(&root.join("database.sqlite3"), "acct01-close")?;
        let session: SessionId = "ses_lua_fixture".parse()?;
        store.elect_writer(&AggregateId::Session(session.clone()))?;
        let writer = store.fencing_token(&AggregateId::Session(session.clone()))?;
        let tx = store.governed_transaction()?;
        let ended = grapt_event::session_step_ended::StepEndedFact::new(
            StepNumber::new(2).ok_or("step")?,
            ProviderCompletionDisposition::AssertedComplete,
        );
        grapt_store::step_ended::record(
            &tx,
            &session,
            &"evt_pending_terminal".parse()?,
            writer,
            300,
            &ended,
            &mut [],
        )?;
        EventStore::settle_attempt_ordinary_terminal_within(
            &tx,
            &PhysicalAttemptId::new("pat_receipt_only").ok_or("attempt")?,
            writer,
            AttemptRecordCeiling::new(NonZeroU32::new(4096).ok_or("bound")?),
            SettledMarker::new(Timestamp::from_millis(400)),
        )?;
        tx.commit()?.discard();
    }
    let before = std::fs::read(root.join("database.sqlite3"))?;
    for _ in 0..2 {
        let output = run(&root, None)?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(report["session_totals"]["status"], "complete");
        assert_eq!(report["session_totals"]["dispatched_attempts"], 4);
        assert_eq!(report["session_totals"]["observed_usage_attempts"], 2);
        assert_eq!(report["session_totals"]["unavailable_usage_attempts"], 2);
        assert_eq!(
            report["session_totals"]["tokens"]["input_fresh"]["status"],
            "unavailable"
        );
        assert_eq!(report["observed_subtotals"]["input_fresh"]["tokens"], 200);
    }
    assert_eq!(before, std::fs::read(root.join("database.sqlite3"))?);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn lua_shipped_command_restarts_without_writes_or_invented_reclaimed_joins() -> TestResult {
    let root = std::env::temp_dir().join(format!("lua-product-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    fixture(&root)?;
    let policy = root.join("prices.json");
    std::fs::write(
        &policy,
        include_bytes!("../../../fixtures/live-usage-accounting/prices.json"),
    )?;
    let before = std::fs::read(root.join("database.sqlite3"))?;
    let first = run(&root, Some(&policy))?;
    assert!(
        first.status.success(),
        "command refused: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let second = run(&root, Some(&policy))?;
    assert!(second.status.success());
    assert_eq!(first.stdout, second.stdout, "restart accounting differs");
    assert_eq!(
        before,
        std::fs::read(root.join("database.sqlite3"))?,
        "report mutated database"
    );
    println!("{}", String::from_utf8_lossy(&first.stdout));
    let report: serde_json::Value = serde_json::from_slice(&first.stdout)?;
    let attempts = report["attempts"].as_array().ok_or("attempts")?;
    assert_eq!(
        attempts.len(),
        4,
        "dispatched attempts disappeared before publication or after reclamation"
    );
    assert_eq!(attempts[0]["cost"]["scope"], "observed-usage-only");
    assert_eq!(
        attempts[0]["cost"]["status"], "estimated",
        "estimated cost became billed"
    );
    assert_eq!(
        attempts[0]["usage"]["input_fresh"]["tokens"], 100,
        "duplicate usage charged twice"
    );
    assert_eq!(attempts[0]["latency"]["first_block_elapsed_ms"], 40);
    assert!(
        attempts[0]["latency"]["step_completed_ms"].is_null(),
        "reserved terminal time resurrected as completion"
    );
    assert!(
        attempts[1]["step"] == 1,
        "reclaimed no-block retry lost its durable receipt Step"
    );
    assert_eq!(
        attempts[1]["usage"]["input_fresh"]["tokens"], 100,
        "unbound attributable usage vanished"
    );
    assert_eq!(attempts[1]["cost"]["status"], "unavailable");
    assert_eq!(attempts[2]["usage_outcome"]["status"], "unavailable");
    assert_eq!(attempts[2]["usage"]["input_fresh"]["status"], "unavailable");
    assert_eq!(attempts[3]["usage_outcome"]["status"], "pending");
    assert_eq!(report["session_totals"]["status"], "incomplete");
    assert_eq!(report["session_totals"]["reason"], "attempt-not-terminal");
    assert_eq!(report["observed_subtotals"]["input_fresh"]["tokens"], 200);
    let invalid_policy = run(&root, Some(&root))?;
    assert!(!invalid_policy.status.success());
    assert!(String::from_utf8_lossy(&invalid_policy.stderr)
        .contains("session accounting refused: price policy must be a regular file"));
    let refusal: serde_json::Value = serde_json::from_slice(&invalid_policy.stdout)?;
    assert_eq!(
        refusal["schema"], "secode.session-accounting.v1",
        "accounting refusal carried another surface's receipt"
    );
    let missing = run(&root, None)?;
    assert!(missing.status.success());
    let missing: serde_json::Value = serde_json::from_slice(&missing.stdout)?;
    assert_eq!(
        missing["attempts"][0]["cost"]["reason"],
        "model-price-unavailable"
    );
    // Read-only SQLite reports can leave WAL shared-memory sidecars. Remove
    // the whole disposable fixture only after its read-only assertions pass.
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn lua_shipped_command_missing_corrupt_and_non_file_database_refuse() -> TestResult {
    let root = std::env::temp_dir().join(format!("lua-refusals-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    assert!(!run(&root, None)?.status.success());
    let db = root.join("database.sqlite3");
    assert!(!db.exists(), "read-only command created absent database");
    std::fs::write(&db, b"corrupt sqlite")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let original = std::fs::metadata(&db)?.permissions();
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o000))?;
        let unreadable = run(&root, None);
        std::fs::set_permissions(&db, original)?;
        let unreadable = unreadable?;
        assert!(
            !unreadable.status.success(),
            "unreadable database became empty accounting"
        );
        assert!(String::from_utf8_lossy(&unreadable.stderr).contains("open failed"));
    }
    assert!(!run(&root, None)?.status.success());
    assert_eq!(std::fs::read(&db)?, b"corrupt sqlite");
    std::fs::remove_file(&db)?;
    std::fs::create_dir(&db)?;
    assert!(!run(&root, None)?.status.success());
    std::fs::remove_dir(db)?;
    std::fs::remove_dir(root)?;
    Ok(())
}
