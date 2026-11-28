//! Settles nested starts that process loss left without a terminal.
//!
//! A nested call running when its process died has a durable start and
//! nothing else, so whether its effect happened is unknown and the Session
//! refuses to continue past it. This owner records exactly that, at startup
//! for every Session the crash interrupted and otherwise only when asked
//! (`/reconcile`) for an idle Session: one unknown-effect terminal per
//! orphaned `(attempt, call)`, never success, never a retry, and never in place
//! of a terminal that already exists. An unreadable terminal row is not one:
//! it proves nothing, so its call is settled beside it and the row stays. The
//! live attempt owner cannot do this: reopening a past attempt would bind it
//! to a second assistant message.

use grapt_event::session_tool_terminal::ToolTerminalFact;
use grapt_event::AggregateId;
use grapt_store::write::EventStore;
use grapt_types::{
    PhysicalAttemptId, SessionId, ToolCallId, ToolFailureReason, ToolTerminalOutcome,
};

use crate::application::{Clock, EventIds};

/// Three active compositions of at most 64 nested effects each: a larger
/// unresolved set is not process loss this owner may settle.
pub const MAX_ORPHANED_NESTED: usize = 3 * 64;

/// Record an unknown effect for each orphaned nested start, in one
/// transaction. The caller holds the Session idle and elected, and read
/// `orphans` under the same store lock, so none has gained a terminal since.
pub fn reconcile_orphaned_nested(
    store: &mut EventStore,
    session: &SessionId,
    orphans: &[(PhysicalAttemptId, ToolCallId)],
    ids: &dyn EventIds,
    clock: &dyn Clock,
) -> Result<(), String> {
    reconcile_orphaned_nested_with_artifacts::<grapt_artifact::FilesystemArtifactStore>(
        store, session, orphans, ids, clock, None,
    )
}

/// Settle crash-orphaned nested tool runs and capture a bounded diagnostic
/// detail artifact when `artifacts` is provided so every failed terminal names
/// its cause.
pub fn reconcile_orphaned_nested_with_artifacts<A: grapt_artifact::ArtifactStore>(
    store: &mut EventStore,
    session: &SessionId,
    orphans: &[(PhysicalAttemptId, ToolCallId)],
    ids: &dyn EventIds,
    clock: &dyn Clock,
    artifacts: Option<&A>,
) -> Result<(), String> {
    if orphans.len() > MAX_ORPHANED_NESTED {
        return Err("unresolved nested effects exceed what reconciliation may settle".into());
    }
    if orphans.is_empty() {
        return Ok(());
    }
    let detail = artifacts.and_then(|artifact_store| {
        let bound = grapt_artifact::CaptureBound::new(2048, 2048)?;
        let message =
            b"nested tool run was interrupted by process loss and reconciled as unknown effect";
        artifact_store
            .capture(
                std::io::Cursor::new(message.as_slice()),
                bound,
                grapt_artifact::ArtifactKind::CapturedOutput,
                grapt_artifact::Sensitivity::Ordinary,
            )
            .ok()
            .and_then(|captured| {
                grapt_types::ToolTerminalArtifact::new(
                    captured.reference().clone(),
                    captured.disposition(),
                )
            })
    });
    let writer = store
        .fencing_token(&AggregateId::Session(session.clone()))
        .map_err(|error| format!("reconciliation has no Session writer: {error}"))?;
    let now = clock.now_ms();
    let tx = store
        .governed_transaction()
        .map_err(|error| format!("reconciliation transaction refused: {error}"))?;
    for (attempt, call) in orphans {
        let fact = ToolTerminalFact::new(
            attempt.clone(),
            call.clone(),
            ToolTerminalOutcome::Failed {
                reason: ToolFailureReason::EffectUnknown,
                detail: detail.clone(),
            },
        );
        grapt_store::tool_terminal::record(&tx, session, &ids.mint(), writer, now, &fact, &mut [])
            .map_err(|error| format!("reconciliation terminal refused: {error}"))?;
    }
    tx.commit()
        .map_err(|error| format!("reconciliation commit refused: {error}"))?
        .discard();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_artifact::ArtifactStore as _;

    #[derive(Debug)]
    struct FixedClock;
    impl Clock for FixedClock {
        fn now_ms(&self) -> i64 {
            1_786_060_800_000
        }
    }

    #[derive(Debug)]
    struct CounterIds(std::sync::atomic::AtomicU64);
    impl EventIds for CounterIds {
        fn mint(&self) -> grapt_types::EventId {
            let n = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            format!("evt_reconcile_{n:08}")
                .parse()
                .unwrap_or_else(|_| unreachable!("static event id"))
        }
    }

    #[derive(Debug)]
    struct FixedArtifactClock;
    impl grapt_types::ClockAuthority for FixedArtifactClock {
        fn now(&self) -> grapt_types::Timestamp {
            grapt_types::Timestamp::from_millis(1_786_060_800_000)
        }
    }

    #[test]
    fn reconcile_with_artifacts_attaches_named_cause_detail() -> Result<(), String> {
        let dir = tempfile::TempDir::new().map_err(|e| e.to_string())?;
        let mut store = EventStore::open_in_memory().map_err(|e| e.to_string())?;
        let session: SessionId = "ses_reconcile_detail".parse().map_err(|e| format!("{e}"))?;
        store
            .elect_writer(&AggregateId::Session(session.clone()))
            .map_err(|e| e.to_string())?;
        let artifacts = grapt_artifact::FilesystemArtifactStore::open(
            dir.path().join("artifacts"),
            b"reconcile-test-key".to_vec(),
            Box::new(FixedArtifactClock),
        )
        .map_err(|e| e.to_string())?;
        let attempt = PhysicalAttemptId::new("attempt_lost_1").ok_or("attempt")?;
        let call = ToolCallId::new("call_lost_1").ok_or("call")?;
        reconcile_orphaned_nested_with_artifacts(
            &mut store,
            &session,
            &[(attempt, call)],
            &CounterIds(std::sync::atomic::AtomicU64::new(0)),
            &FixedClock,
            Some(&artifacts),
        )?;
        let raw: String = store
            .connection()
            .query_row(
                "SELECT data FROM event WHERE aggregate_id = ?1 AND type = 'session.tool.terminal.1'",
                [session.as_str()],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        let value: serde_json::Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
        let fact = grapt_event::session_tool_terminal::decode(&value).map_err(|e| e.to_string())?;
        let ToolTerminalOutcome::Failed {
            reason: ToolFailureReason::EffectUnknown,
            detail: Some(detail),
        } = fact.outcome()
        else {
            return Err(format!(
                "expected EffectUnknown with detail, got {:?}",
                fact.outcome()
            ));
        };
        let budget = grapt_artifact::ReadBudget::new(2048).ok_or("read budget")?;
        let page = artifacts
            .fetch(detail.reference(), 0, budget)
            .map_err(|e| e.to_string())?;
        let text = std::str::from_utf8(page.bytes()).map_err(|e| e.to_string())?;
        assert!(text.contains("process loss"), "unexpected detail: {text}");
        Ok(())
    }
}
