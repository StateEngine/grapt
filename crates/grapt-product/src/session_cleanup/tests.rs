use super::*;
use grapt_inbox::{AdmitRequest, Inbox};
use grapt_session::{create_session, execution, SessionListProjector, SessionSeed};
use grapt_types::{DeliveryMode, ExecutionId, IdToken, ProjectId};
use std::cell::Cell;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
pub(super) struct Clock(pub(super) i64);
impl ClockAuthority for Clock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0)
    }
}
#[derive(Debug, Default)]
pub(super) struct Ids(Cell<u32>);
impl IdAuthority for Ids {
    fn mint(&self) -> IdToken {
        self.0.set(self.0.get() + 1);
        match IdToken::new(format!("cleanup_{}", self.0.get())) {
            Some(token) => token,
            None => unreachable!("fixture tokens are nonempty"),
        }
    }
}
fn command(extra: &[&str]) -> Result<CleanupCommand> {
    let root = std::env::current_dir()?;
    let mut argv = vec![
        "--service",
        root.to_str().ok_or("UTF-8 root")?,
        "--cutoff",
        "100",
        "--current-session",
        "ses_current",
    ];
    argv.extend_from_slice(extra);
    Ok(CleanupCommand::parse(argv)?)
}
fn store() -> Result<EventStore> {
    let mut store = EventStore::open_in_memory()?;
    initialize(&mut store)?;
    Ok(store)
}
pub(super) fn initialize(store: &mut EventStore) -> Result {
    let tx = store.governed_transaction()?;
    grapt_instruction::InstructionService::ensure_tables_within(&tx)?;
    grapt_form::FormService::initialize_within(&tx)?;
    grapt_shell::schema::initialize_within(&tx).map_err(|error| error.to_string())?;
    grapt_subagent::SubagentService::ensure_schema_within(&tx)?;
    tx.commit()?.discard();
    Ok(())
}
pub(super) fn seed(
    store: &mut EventStore,
    ids: &Ids,
    id: &str,
    parent: Option<&SessionId>,
    time: i64,
) -> Result<SessionId> {
    let mut seed = SessionSeed::new(
        ProjectId::new("prj_cleanup").ok_or("project")?,
        "/cleanup-fixture",
        "Cleanup fixture",
    )?
    .with_requested_id(id.parse()?)?;
    if let Some(parent) = parent {
        seed = seed.with_parent(parent.clone());
    }
    Ok(create_session(
        store,
        &DeterministicAuthorities::new(&Clock(time), ids),
        &seed,
        &mut [&mut SessionListProjector],
    )?
    .session)
}

#[test]
fn argv_requires_explicit_scope_and_defaults_to_dry_run() -> Result {
    let dry = command(&[])?;
    assert!(!dry.applies());
    assert_eq!(dry.cutoff().as_millis(), 100);
    assert!(command(&["--apply"])?.applies());
    assert!(CleanupCommand::parse(["--cutoff-ms", "100"]).is_err());
    for extra in [
        ["--cutoff", "101"],
        ["--resume", "ses_other"],
        ["--service", "relative"],
    ] {
        assert!(command(&extra).is_err());
    }
    assert!(command(&["--apply", "--apply"]).is_err());
    Ok(())
}

#[test]
fn public_preview_retains_current_families_work_and_incomplete_executions() -> Result {
    let ids = Ids::default();
    let mut store = store()?;
    let old = seed(&mut store, &ids, "ses_old", None, 10)?;
    let parent = seed(&mut store, &ids, "ses_parent", None, 10)?;
    let child = seed(&mut store, &ids, "ses_child", Some(&parent), 10)?;
    grapt_session::delete_session(
        &mut store,
        &DeterministicAuthorities::new(&Clock(20), &ids),
        &child,
    )?;
    let current = seed(&mut store, &ids, "ses_current", None, 10)?;
    let recent = seed(&mut store, &ids, "ses_recent", None, 100)?;
    let active = seed(&mut store, &ids, "ses_active", None, 10)?;
    let done = seed(&mut store, &ids, "ses_done", None, 10)?;
    let pending = seed(&mut store, &ids, "ses_pending", None, 10)?;
    let auth = DeterministicAuthorities::new(&Clock(20), &ids);
    execution::begin(
        &mut store,
        &auth,
        &active,
        &ExecutionId::new("exe_active").ok_or("execution")?,
    )?;
    let execution = ExecutionId::new("exe_done").ok_or("execution")?;
    execution::begin(&mut store, &auth, &done, &execution)?;
    execution::settle(
        &mut store,
        &auth,
        &done,
        &execution,
        execution::Outcome::Succeeded,
    )?;
    let mut inbox = Inbox::new(store)?;
    inbox.admit(AdmitRequest {
        event_id: &"evt_pending".parse()?,
        session: &pending,
        input: &"msg_pending".parse()?,
        text: "keep this work",
        delivery: DeliveryMode::Queue,
        resume: false,
        created_ms: 20,
    })?;
    let connection = inbox.store().connection();
    let before = connection.total_changes();
    let report = command(&[])?.preview(connection)?;
    assert!(!report.applied);
    assert!(report
        .sessions
        .iter()
        .any(|row| row.session == old && row.disposition == CleanupDisposition::Eligible));
    assert!(report
        .sessions
        .iter()
        .any(|row| row.session == done && row.disposition == CleanupDisposition::Eligible));
    for row in report.sessions {
        if row.session == old || row.session == done {
            assert_eq!(row.disposition, CleanupDisposition::Eligible);
        } else if [&parent, &child, &current, &recent, &active, &pending].contains(&&row.session) {
            assert!(matches!(row.disposition, CleanupDisposition::Retained(_)));
        } else {
            return Err("unexpected session".into());
        }
    }
    assert_eq!(connection.total_changes(), before, "preview must not write");
    Ok(())
}

#[test]
fn public_preview_refuses_missing_owner_evidence() -> Result {
    let mut store = EventStore::open_in_memory()?;
    seed(&mut store, &Ids::default(), "ses_old", None, 10)?;
    assert!(matches!(
        command(&[])?.preview(store.connection()),
        Err(CleanupError::Evidence(_))
    ));
    Ok(())
}
