//! SUSTAINED-WORK-REPAIR-01 J02: a typed execution failure is recorded and
//! published through fixed product-authored text. A row THIS build did not
//! write — a future name, a future wording — is still projected verbatim:
//! the reader refuses structure, not vocabulary.

use grapt_session::{create_session, execution, SessionSeed};
use grapt_store::EventStore;
use grapt_types::{
    ClockAuthority, DeterministicAuthorities, ExecutionId, IdAuthority, IdToken, Timestamp,
};
use std::cell::Cell;

#[derive(Debug, Default)]
struct Authorities(Cell<u64>);
impl ClockAuthority for Authorities {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1234)
    }
}
impl IdAuthority for Authorities {
    fn mint(&self) -> IdToken {
        self.0.set(self.0.get() + 1);
        IdToken::new(format!("executionfailureclass{}", self.0.get()))
            .unwrap_or_else(|| panic!("fixture ID refused"))
    }
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn swr_typed_failure_classes_are_recorded_verbatim_and_validated_on_read(
) -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join(format!(
        "grapt-execution-failure-class-{}.sqlite",
        std::process::id()
    ));
    assert!(
        !path.exists(),
        "refusing to overwrite existing witness data"
    );
    let source = Authorities::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open(&path, "execution-failure-class-witness")?;
    let seed = SessionSeed::new(
        grapt_types::ProjectId::new("prj_failure_class").ok_or("project ID refused")?,
        "/work/failure-class",
        "failure-class",
    )?;
    let session = create_session(&mut store, &authorities, &seed, &mut [])?.session;
    let classes = [
        (
            "exe_budget",
            execution::Failure::StepBudgetExhausted {
                policy: grapt_types::PolicyVersion::new("sustained-step-budget-v1")
                    .ok_or("Step budget policy refused")?,
                spent: 256,
                granted: 256,
            },
            "StepBudgetExhausted",
        ),
        (
            "exe_context",
            execution::Failure::ContextLimit,
            "ContextLimit",
        ),
        (
            "exe_generic",
            execution::Failure::Generic,
            "ExecutionFailed",
        ),
        (
            "exe_continuity",
            execution::Failure::Continuity(execution::ContinuityFailure::Capability),
            "ContextLimit",
        ),
    ];
    let mut settled = Vec::new();
    for (raw, failure, name) in classes {
        let id = ExecutionId::new(raw).ok_or("execution ID refused")?;
        execution::begin(&mut store, &authorities, &session, &id)?;
        let terminal = execution::settle_failed(&mut store, &authorities, &session, &id, failure)?;
        let recorded = terminal
            .failure
            .clone()
            .ok_or("a failed execution has no public failure")?;
        assert_eq!(recorded.name, name);
        assert!(recorded.message.len() <= 512);
        assert_eq!(
            execution::read(store.connection(), &session, &id)?.get(1),
            Some(&terminal),
            "the stored class did not read back verbatim"
        );
        settled.push(terminal);
    }
    let budget = settled[0].failure.clone().ok_or("budget failure absent")?;
    assert!(
        budget.message.contains("256 of 256 Steps"),
        "{}",
        budget.message
    );
    assert!(
        budget.message.contains("sustained-step-budget-v1"),
        "{}",
        budget.message
    );
    assert!(
        budget.message.contains("follow-up prompt"),
        "{}",
        budget.message
    );
    let generic = settled[2].failure.clone().ok_or("generic failure absent")?;
    assert_eq!(generic.message, execution::FAILURE_MESSAGE);
    assert!(settled[3]
        .failure
        .as_ref()
        .ok_or("continuity recovery absent")?
        .message
        .contains("Rebind the Session"));
    assert_eq!(execution::failures(store.connection(), &session)?, settled);

    // The ordinary settle keeps the generic class.
    let plain = ExecutionId::new("exe_plain").ok_or("execution ID refused")?;
    execution::begin(&mut store, &authorities, &session, &plain)?;
    let plain = execution::settle(
        &mut store,
        &authorities,
        &session,
        &plain,
        execution::Outcome::Failed,
    )?;
    assert_eq!(plain.failure, Some(execution::Failure::Generic.public()));

    // A repeated settlement returns the recorded class; it cannot relabel history.
    let budget_id = ExecutionId::new("exe_budget").ok_or("execution ID refused")?;
    let repeated = execution::settle_failed(
        &mut store,
        &authorities,
        &session,
        &budget_id,
        execution::Failure::ContextLimit,
    )?;
    assert_eq!(repeated, settled[0]);

    // Success and interruption carry no failure.
    let success = ExecutionId::new("exe_success").ok_or("execution ID refused")?;
    execution::begin(&mut store, &authorities, &session, &success)?;
    let success = execution::settle(
        &mut store,
        &authorities,
        &session,
        &success,
        execution::Outcome::Succeeded,
    )?;
    assert_eq!(success.failure, None);

    // Tolerant readers: a reworded message is the future's vocabulary. The
    // row stays projected, and the STORED text is what a transcript reader
    // sees — never a refusal, never the product wording in its place.
    drop(store);
    let damaged = rusqlite::Connection::open(&path)?;
    damaged.execute(
        "UPDATE event SET data = json_set(data, '$.error.message', 'private provider text')
         WHERE json_extract(data, '$.error.type') = 'ContextLimit'",
        [],
    )?;
    let reworded = execution::failures(&damaged, &session)?
        .into_iter()
        .find(|observation| {
            observation
                .failure
                .as_ref()
                .is_some_and(|failure| failure.name == "ContextLimit")
        })
        .ok_or("the reworded row must still be projected")?;
    let reworded = reworded
        .failure
        .as_ref()
        .ok_or("the projected row lost its failure")?;
    assert_eq!(
        reworded.message, "private provider text",
        "a future row publishes its stored text"
    );

    // An unknown stored class is projected under its stored name too.
    damaged.execute(
        "UPDATE event SET data = replace(data, '\"ContextLimit\"', '\"ArbitraryProviderText\"')
         WHERE type = 'session.execution.failed.1'",
        [],
    )?;
    let future_named = execution::failures(&damaged, &session)?
        .into_iter()
        .find(|observation| {
            observation
                .failure
                .as_ref()
                .is_some_and(|failure| failure.name == "ArbitraryProviderText")
        })
        .ok_or("a future class name must still be projected")?;
    let future_named = future_named
        .failure
        .as_ref()
        .ok_or("the projected row lost its failure")?;
    assert_eq!(future_named.message, "private provider text");
    assert!(
        !future_named.is_known_name(),
        "the stored name is untrusted, so the display layer must say so"
    );

    // Structural damage is still corruption: a blank message is not a row.
    damaged.execute(
        "UPDATE event SET data = json_set(data, '$.error.message', '')
         WHERE json_extract(data, '$.error.type') = 'ArbitraryProviderText'",
        [],
    )?;
    assert!(
        execution::failures(&damaged, &session).is_err(),
        "a blank message is corruption, not future vocabulary"
    );
    drop(damaged);
    std::fs::remove_file(path)?;
    Ok(())
}
