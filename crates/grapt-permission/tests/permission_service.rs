//! R3-b Permission witnesses (through the final review round): the
//! asymmetric blocker laws, the distinct settlement-authority paths, waiter
//! semantics, exact-once settlement, and literal frozen-fixture execution.

use grapt_permission::{OrdinaryReply, PermissionError, PermissionRequest, PermissionService};
use grapt_store::{AggregateId, EventStore};
use grapt_types::{
    CasOutcome, EventId, PermissionId, PermissionSuspensionReason, ProjectId, RuntimeGeneration,
    SessionId, Waiter,
};
use rusqlite::Connection;
#[path = "../../../test-support/evidence.rs"]
mod evidence;
#[path = "../../../test-support/fixtures.rs"]
mod fixtures;
use std::path::Path;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn minter(prefix: &'static str) -> impl FnMut() -> EventId {
    let mut n = 0usize;
    move || {
        n += 1;
        match format!("evt_{prefix}{n}").parse() {
            Ok(id) => id,
            Err(e) => panic!("mint: {e}"),
        }
    }
}

fn project() -> ProjectId {
    match ProjectId::new("prj_r3b") {
        Some(p) => p,
        None => panic!("project id"),
    }
}

/// Elect this store instance as the writer for every session the caller will
/// write. Election is EXPLICIT by contract: no append path elects on a caller's
/// behalf, so a store handed to the service unelected refuses every durable
/// transition. A runtime owner does exactly this before it starts writing.
fn elect(store: &mut EventStore, sessions: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    for raw in sessions {
        store.elect_writer(&AggregateId::Session(raw.parse()?))?;
    }
    Ok(())
}

fn service_for(sessions: &[&str]) -> Result<PermissionService, Box<dyn std::error::Error>> {
    let mut store = EventStore::open_in_memory()?;
    elect(&mut store, sessions)?;
    Ok(PermissionService::new(store)?)
}

/// The durable-file counterpart. Every reopen mints a NEW owner, so it elects
/// again and the epoch advances — an ownership transfer is precisely what a
/// service replacement is.
fn service_at(
    db: &Path,
    sessions: &[&str],
) -> Result<PermissionService, Box<dyn std::error::Error>> {
    let mut store = EventStore::open(db, "0.0.1")?;
    elect(&mut store, sessions)?;
    Ok(PermissionService::new(store)?)
}

fn ev(raw: &str) -> Result<EventId, Box<dyn std::error::Error>> {
    Ok(raw.parse()?)
}

fn fallback_authority() -> &'static grapt_types::AuthoritySource {
    // The corrected vocabulary requires a default-policy decision to name its
    // exact policy version; a bare `Fallback` marker no longer exists.
    static FALLBACK: std::sync::OnceLock<grapt_types::AuthoritySource> = std::sync::OnceLock::new();
    FALLBACK.get_or_init(|| grapt_types::AuthoritySource::DefaultPolicy {
        policy: match grapt_types::PolicyVersion::new("default") {
            Some(policy) => policy,
            None => unreachable!("non-empty policy version"),
        },
    })
}

/// Create-without-waiter admission with default resources/authority.
fn create(
    svc: &mut PermissionService,
    event_id: &EventId,
    id: &PermissionId,
    session: &SessionId,
    action: &str,
) -> Result<(), PermissionError> {
    let resources = ["res".to_owned()];
    let request = PermissionRequest {
        action,
        resources: &resources,
        authority: fallback_authority(),
    };
    svc.create(event_id, id, session, &request, 0)
}

fn status(
    svc: &PermissionService,
    id: &PermissionId,
) -> Result<String, Box<dyn std::error::Error>> {
    Ok(svc.store().connection().query_row(
        "SELECT status FROM permission_request WHERE id = ?1",
        [id.as_str()],
        |r| r.get(0),
    )?)
}

fn event_count(svc: &PermissionService) -> Result<i64, Box<dyn std::error::Error>> {
    Ok(svc
        .store()
        .connection()
        .query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))?)
}

#[test]
fn direct_settlement_of_suspended_is_notlive_and_writes_nothing() -> TestResult {
    let mut svc = service_for(&["ses_p"])?;
    let session: SessionId = "ses_p".parse()?;
    let id: PermissionId = "per_p1".parse()?;
    create(&mut svc, &ev("evt_ask")?, &id, &session, "read")?;
    assert_eq!(
        svc.suspend(
            &ev("evt_sus")?,
            &id,
            PermissionSuspensionReason::ProcessLoss,
            0
        )?,
        CasOutcome::Applied
    );
    let before = event_count(&svc)?;
    // Every direct settlement path refuses a suspended request with NotLive.
    assert_eq!(
        svc.reply(&id, OrdinaryReply::Once, minter("r"), 0)?.primary,
        CasOutcome::NotLive
    );
    assert_eq!(
        svc.grant_session(&id, &ev("evt_gs")?, 0)?,
        CasOutcome::NotLive
    );
    assert_eq!(
        svc.grant_always(&id, Some(&project()), &ev("evt_ga")?, 0)?,
        CasOutcome::NotLive
    );
    assert_eq!(event_count(&svc)?, before, "NotLive wrote nothing");
    assert_eq!(status(&svc, &id)?, "suspended");
    Ok(())
}

#[test]
fn reject_cascade_settles_suspended_siblings_and_preserves_history() -> TestResult {
    // Both sessions are written to: the cascade's own session and the
    // cross-session sibling that must be left untouched.
    let mut svc = service_for(&["ses_p", "ses_other"])?;
    let session: SessionId = "ses_p".parse()?;
    let live: PermissionId = "per_live".parse()?;
    let sib_a: PermissionId = "per_sibA".parse()?;
    let sib_b: PermissionId = "per_sibB".parse()?;
    let other_session: SessionId = "ses_other".parse()?;
    let elsewhere: PermissionId = "per_elsewhere".parse()?;

    create(&mut svc, &ev("evt_l")?, &live, &session, "read")?;
    create(&mut svc, &ev("evt_a")?, &sib_a, &session, "read")?;
    create(&mut svc, &ev("evt_b")?, &sib_b, &session, "read")?;
    create(&mut svc, &ev("evt_e")?, &elsewhere, &other_session, "read")?;
    svc.suspend(
        &ev("evt_sa")?,
        &sib_a,
        PermissionSuspensionReason::Shutdown,
        0,
    )?;
    svc.suspend(
        &ev("evt_sb")?,
        &sib_b,
        PermissionSuspensionReason::ProcessLoss,
        0,
    )?;
    svc.suspend(
        &ev("evt_se")?,
        &elsewhere,
        PermissionSuspensionReason::Shutdown,
        0,
    )?;

    let outcome = svc.reply(&live, OrdinaryReply::Reject, minter("rc"), 0)?;
    assert_eq!(outcome.primary, CasOutcome::Applied);
    let mut cascaded: Vec<_> = outcome
        .cascaded
        .iter()
        .map(|c| c.request.as_str())
        .collect();
    cascaded.sort();
    assert_eq!(cascaded, vec!["per_sibA", "per_sibB"]);
    for (sib, reason) in [(&sib_a, "shutdown"), (&sib_b, "process-loss")] {
        let (st, susp): (String, Option<String>) = svc.store().connection().query_row(
            "SELECT status, suspended_reason FROM permission_request WHERE id = ?1",
            [sib.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        assert_eq!(st, "replied");
        assert_eq!(
            susp.as_deref(),
            Some(reason),
            "historical suspension preserved"
        );
    }
    assert_eq!(
        status(&svc, &elsewhere)?,
        "suspended",
        "cross-session sibling not swept"
    );
    Ok(())
}

#[test]
fn a_plain_once_reply_does_not_cascade() -> TestResult {
    let mut svc = service_for(&["ses_p"])?;
    let session: SessionId = "ses_p".parse()?;
    let live: PermissionId = "per_live".parse()?;
    let sib: PermissionId = "per_sib".parse()?;
    create(&mut svc, &ev("evt_l")?, &live, &session, "read")?;
    create(&mut svc, &ev("evt_s")?, &sib, &session, "read")?;
    svc.suspend(
        &ev("evt_ss")?,
        &sib,
        PermissionSuspensionReason::Shutdown,
        0,
    )?;
    let outcome = svc.reply(&live, OrdinaryReply::Once, minter("np"), 0)?;
    assert_eq!(outcome.primary, CasOutcome::Applied);
    assert!(outcome.cascaded.is_empty());
    assert_eq!(status(&svc, &sib)?, "suspended");
    Ok(())
}

#[test]
fn terminal_rows_are_immutable() -> TestResult {
    let mut svc = service_for(&["ses_p"])?;
    let session: SessionId = "ses_p".parse()?;
    let id: PermissionId = "per_t".parse()?;
    create(&mut svc, &ev("evt_ask")?, &id, &session, "read")?;
    assert_eq!(
        svc.reply(&id, OrdinaryReply::Once, minter("t1"), 0)?
            .primary,
        CasOutcome::Applied
    );
    let after_first = event_count(&svc)?;
    assert_eq!(
        svc.reply(&id, OrdinaryReply::Reject, minter("t2"), 0)?
            .primary,
        CasOutcome::AlreadyTerminal
    );
    assert_eq!(
        svc.grant_session(&id, &ev("evt_gs")?, 0)?,
        CasOutcome::AlreadyTerminal
    );
    assert_eq!(
        svc.suspend(
            &ev("evt_late")?,
            &id,
            PermissionSuspensionReason::Shutdown,
            0
        )?,
        CasOutcome::AlreadyTerminal
    );
    assert_eq!(
        event_count(&svc)?,
        after_first,
        "terminal row admitted no writes"
    );
    assert_eq!(status(&svc, &id)?, "replied");
    Ok(())
}

#[test]
fn corrupt_suspended_row_without_reason_is_typed_corruption() -> TestResult {
    let mut svc = service_for(&["ses_p"])?;
    let session: SessionId = "ses_p".parse()?;
    let id: PermissionId = "per_c".parse()?;
    create(&mut svc, &ev("evt_ask")?, &id, &session, "read")?;
    svc.store().connection().execute(
        "UPDATE permission_request SET status = 'suspended', suspended_reason = NULL WHERE id = ?1",
        [id.as_str()],
    )?;
    assert!(matches!(
        svc.reply(&id, OrdinaryReply::Once, minter("c"), 0),
        Err(PermissionError::Corruption { .. })
    ));
    Ok(())
}

#[test]
fn cancel_settles_pending_and_suspended_and_refuses_terminal() -> TestResult {
    let mut svc = service_for(&["ses_p"])?;
    let session: SessionId = "ses_p".parse()?;
    let a: PermissionId = "per_cp".parse()?;
    create(&mut svc, &ev("evt_a")?, &a, &session, "read")?;
    assert_eq!(svc.cancel(&ev("evt_ca")?, &a, 0)?, CasOutcome::Applied);
    assert_eq!(status(&svc, &a)?, "cancelled");
    let b: PermissionId = "per_cs".parse()?;
    create(&mut svc, &ev("evt_b")?, &b, &session, "read")?;
    svc.suspend(&ev("evt_sb")?, &b, PermissionSuspensionReason::Shutdown, 0)?;
    assert_eq!(svc.cancel(&ev("evt_cb")?, &b, 0)?, CasOutcome::Applied);
    assert_eq!(status(&svc, &b)?, "cancelled");
    assert_eq!(
        svc.cancel(&ev("evt_cc")?, &a, 0)?,
        CasOutcome::AlreadyTerminal
    );
    let ghost: PermissionId = "per_ghost".parse()?;
    assert_eq!(svc.cancel(&ev("evt_cg")?, &ghost, 0)?, CasOutcome::NotLive);
    Ok(())
}

#[test]
fn create_has_no_waiter_ask_attaches_a_live_one() -> TestResult {
    let mut svc = service_for(&["ses_p"])?;
    let session: SessionId = "ses_p".parse()?;
    let c: PermissionId = "per_create".parse()?;
    create(&mut svc, &ev("evt_c")?, &c, &session, "read")?;
    assert_eq!(svc.waiter(&c), Waiter::None, "create leaves no live waiter");
    let a: PermissionId = "per_ask".parse()?;
    let resources = ["res".to_owned()];
    let request = PermissionRequest {
        action: "read",
        resources: &resources,
        authority: fallback_authority(),
    };
    svc.ask(
        &ev("evt_a")?,
        &a,
        &session,
        &request,
        RuntimeGeneration::new(9),
        0,
    )?;
    assert_eq!(
        svc.waiter(&a),
        Waiter::Live(RuntimeGeneration::new(9)),
        "ask attaches the live waiter"
    );
    Ok(())
}

#[test]
fn attach_waiter_refuses_invalid_targets_and_transitions_clear_it() -> TestResult {
    let mut svc = service_for(&["ses_p"])?;
    let session: SessionId = "ses_p".parse()?;
    let ghost: PermissionId = "per_ghost".parse()?;
    assert!(matches!(
        svc.attach_waiter(&ghost, RuntimeGeneration::new(1)),
        Err(PermissionError::NotLiveWaiter { .. })
    ));
    let s: PermissionId = "per_ws".parse()?;
    create(&mut svc, &ev("evt_s")?, &s, &session, "read")?;
    svc.attach_waiter(&s, RuntimeGeneration::new(2))?;
    assert_eq!(svc.waiter(&s), Waiter::Live(RuntimeGeneration::new(2)));
    svc.suspend(&ev("evt_ss")?, &s, PermissionSuspensionReason::Shutdown, 0)?;
    assert_eq!(svc.waiter(&s), Waiter::None, "suspend cleared the waiter");
    assert!(matches!(
        svc.attach_waiter(&s, RuntimeGeneration::new(3)),
        Err(PermissionError::NotLiveWaiter { .. })
    ));
    let t: PermissionId = "per_wt".parse()?;
    create(&mut svc, &ev("evt_t")?, &t, &session, "read")?;
    svc.attach_waiter(&t, RuntimeGeneration::new(4))?;
    svc.reply(&t, OrdinaryReply::Once, minter("wt"), 0)?;
    assert_eq!(svc.waiter(&t), Waiter::None, "reply cleared the waiter");
    Ok(())
}

#[test]
fn empty_action_is_rejected_at_admission_with_zero_writes() -> TestResult {
    let mut svc = service_for(&["ses_p"])?;
    let session: SessionId = "ses_p".parse()?;
    let id: PermissionId = "per_empty".parse()?;
    assert!(matches!(
        create(&mut svc, &ev("evt_ask")?, &id, &session, ""),
        Err(PermissionError::InvalidAction { .. })
    ));
    assert_eq!(event_count(&svc)?, 0);
    Ok(())
}

#[test]
fn session_and_always_are_distinct_authority_paths() -> TestResult {
    let mut svc = service_for(&["ses_auth"])?;
    let session: SessionId = "ses_auth".parse()?;
    let always_id: PermissionId = "per_always".parse()?;
    create(&mut svc, &ev("evt_a1")?, &always_id, &session, "fs.read")?;
    assert_eq!(
        svc.grant_always(&always_id, Some(&project()), &ev("evt_ga")?, 0)?,
        CasOutcome::Applied
    );
    assert!(svc.has_saved_rule(&project(), "fs.read")?);

    let session_id: PermissionId = "per_session".parse()?;
    create(&mut svc, &ev("evt_s1")?, &session_id, &session, "fs.write")?;
    assert_eq!(
        svc.grant_session(&session_id, &ev("evt_gs")?, 0)?,
        CasOutcome::Applied
    );
    // The observable distinction is durability, not a policy query. `Session`
    // writes no project rule; `Always` does. The crate deliberately exposes no
    // "does this session allow X" answer keyed on a raw action string — that
    // would present an unresolved action as resolved authority, and the exact
    // capability it would need belongs to the later tool/policy resolution
    // owner.
    assert!(!svc.has_saved_rule(&project(), "fs.write")?);
    let rules: i64 =
        svc.store()
            .connection()
            .query_row("SELECT COUNT(*) FROM permission_rule", [], |r| r.get(0))?;
    assert_eq!(rules, 1, "only the Always rule is durable");
    Ok(())
}

#[test]
fn always_without_a_project_refuses_with_zero_writes() -> TestResult {
    let mut svc = service_for(&["ses_p"])?;
    let session: SessionId = "ses_p".parse()?;
    let id: PermissionId = "per_noproj".parse()?;
    create(&mut svc, &ev("evt_ask")?, &id, &session, "fs.read")?;
    let before = event_count(&svc)?;
    assert_eq!(
        svc.grant_always(&id, None, &ev("evt_ga")?, 0)?,
        CasOutcome::NotLive
    );
    assert_eq!(event_count(&svc)?, before, "no reply event written");
    assert_eq!(status(&svc, &id)?, "pending", "the request is NOT settled");
    let rules: i64 =
        svc.store()
            .connection()
            .query_row("SELECT COUNT(*) FROM permission_rule", [], |r| r.get(0))?;
    assert_eq!(rules, 0, "no rule written");
    Ok(())
}

#[test]
fn a_suspended_permission_survives_replacement_and_stays_notlive() -> TestResult {
    let dir = std::env::temp_dir().join(format!("grapt-perm-restart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("perm.db");
    let session: SessionId = "ses_p".parse()?;
    let id: PermissionId = "per_restart".parse()?;
    {
        let mut svc = service_at(&db, &["ses_p"])?;
        create(&mut svc, &ev("evt_ask")?, &id, &session, "read")?;
        svc.suspend(
            &ev("evt_sus")?,
            &id,
            PermissionSuspensionReason::ProcessLoss,
            0,
        )?;
    }
    // The replacement is a new owner and elects again; the epoch advances, which
    // is what a service replacement IS. The suspended row's NotLive refusal is
    // decided before any append, so it is unaffected either way.
    let mut reopened = service_at(&db, &["ses_p"])?;
    assert_eq!(status(&reopened, &id)?, "suspended");
    assert_eq!(
        reopened
            .reply(&id, OrdinaryReply::Once, minter("r"), 0)?
            .primary,
        CasOutcome::NotLive
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn contending_replies_settle_exactly_once() -> TestResult {
    use std::sync::{Arc, Barrier};
    let dir = std::env::temp_dir().join(format!("grapt-perm-race-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("perm.db");
    let session: SessionId = "ses_p".parse()?;
    let id: PermissionId = "per_race".parse()?;
    let aggregate = AggregateId::Session(session.clone());
    let owner = {
        let mut store = EventStore::open(&db, "0.0.1")?;
        store.elect_writer(&aggregate)?;
        let owner = store.owner().clone();
        let mut svc = PermissionService::new(store)?;
        create(&mut svc, &ev("evt_ask")?, &id, &session, "read")?;
        owner
    };
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for worker in 0..2 {
        let path = db.clone();
        let gate = Arc::clone(&barrier);
        let owner = owner.clone();
        let aggregate = aggregate.clone();
        handles.push(std::thread::spawn(move || -> Result<CasOutcome, String> {
            // The two contenders must be ONE owner across two connections. Two
            // separately-minted owners would fence rather than race: the second
            // election would transfer ownership and the first's reply would be
            // refused as stale, so the test would report "exactly once" for the
            // wrong reason. Sharing the owner makes both re-elections `Retained`
            // at the same epoch, leaving BEGIN IMMEDIATE as the only serializer
            // and the CAS as the only thing that can decide the winner.
            let mut store = EventStore::open_as(&path, "0.0.1", &owner)
                .map_err(|e: grapt_store::WriteError| e.to_string())?;
            store
                .elect_writer(&aggregate)
                .map_err(|e: grapt_store::WriteError| e.to_string())?;
            let mut svc = PermissionService::new(store).map_err(|e| e.to_string())?;
            let id: PermissionId = "per_race"
                .parse()
                .map_err(|e: grapt_types::IdError| e.to_string())?;
            let mut n = 0usize;
            let mint = move || {
                n += 1;
                match format!("evt_race{worker}_{n}").parse() {
                    Ok(x) => x,
                    Err(e) => panic!("mint: {e}"),
                }
            };
            gate.wait();
            svc.reply(&id, OrdinaryReply::Once, mint, 0)
                .map(|o| o.primary)
                .map_err(|e| e.to_string())
        }));
    }
    let mut applied = 0;
    let mut already = 0;
    for h in handles {
        match h.join() {
            Ok(Ok(CasOutcome::Applied)) => applied += 1,
            Ok(Ok(CasOutcome::AlreadyTerminal)) => already += 1,
            Ok(Ok(other)) => panic!("unexpected outcome {other:?}"),
            Ok(Err(e)) => panic!("reply failed: {e}"),
            Err(_) => panic!("thread panicked"),
        }
    }
    assert_eq!((applied, already), (1, 1), "exactly once");
    let conn = Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let replied: i64 = conn.query_row(
        "SELECT COUNT(*) FROM event WHERE type = 'permission.replied.1'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(replied, 1, "one durable replied event");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn waiter_defaults_to_none_and_does_not_survive_replacement() -> TestResult {
    let dir = std::env::temp_dir().join(format!("grapt-perm-waiter-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("perm.db");
    let session: SessionId = "ses_p".parse()?;
    let id: PermissionId = "per_w".parse()?;
    let mut svc = service_at(&db, &["ses_p"])?;
    create(&mut svc, &ev("evt_ask")?, &id, &session, "read")?;
    assert_eq!(svc.waiter(&id), Waiter::None);
    svc.attach_waiter(&id, RuntimeGeneration::new(7))?;
    assert_eq!(svc.waiter(&id), Waiter::Live(RuntimeGeneration::new(7)));
    drop(svc);
    let reopened = service_at(&db, &["ses_p"])?;
    assert_eq!(status(&reopened, &id)?, "pending");
    assert_eq!(
        reopened.waiter(&id),
        Waiter::None,
        "waiter did not survive replacement"
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
#[ignore = "evidence-bearing: needs the frozen clone; run with --include-ignored on the evidence machine"]
fn real_clone_permission_events_replay_to_parity() -> TestResult {
    let path = evidence::frozen_clone();
    let log = grapt_store::EventLog::open_read_only(&path)?;
    // Replay writes the projection rows directly, never through the service, so
    // this scratch store elects no writer.
    let svc = service_for(&[])?;
    let conn = svc.store().connection();
    let mut failures: Vec<String> = Vec::new();
    log.for_each_in_replay_order(|e| {
        match grapt_permission::PermissionEvent::decode(e.kind.name(), e.kind.version().value(), &e.data) {
            Ok(Some(grapt_permission::PermissionEvent::Asked { id, session, action, .. })) => {
                if let Err(err) = conn.execute(
                    "INSERT INTO permission_request (id, session_id, action, status, suspended_reason, reply)
                     VALUES (?1, ?2, ?3, 'pending', NULL, NULL)",
                    rusqlite::params![id.as_str(), session.as_str(), action],
                ) {
                    failures.push(format!("{id}: {err}"));
                }
            }
            Ok(Some(grapt_permission::PermissionEvent::Replied { request, reply, .. })) => {
                let _ = conn.execute(
                    "UPDATE permission_request SET status = 'replied', reply = ?2 WHERE id = ?1",
                    rusqlite::params![request.as_str(), grapt_permission::reply_as_str(reply)],
                );
            }
            Ok(Some(grapt_permission::PermissionEvent::Suspended { request, reason, .. })) => {
                let r = match reason {
                    PermissionSuspensionReason::Shutdown => "shutdown",
                    PermissionSuspensionReason::ProcessLoss => "process-loss",
                };
                let _ = conn.execute(
                    "UPDATE permission_request SET status = 'suspended', suspended_reason = ?2 WHERE id = ?1",
                    rusqlite::params![request.as_str(), r],
                );
            }
            Ok(Some(grapt_permission::PermissionEvent::Cancelled { request, .. })) => {
                let _ = conn.execute(
                    "UPDATE permission_request SET status = 'cancelled' WHERE id = ?1",
                    [request.as_str()],
                );
            }
            // Rule withdrawals do not change the request lifecycle projection.
            Ok(Some(grapt_permission::PermissionEvent::RuleRevoked(_))) | Ok(None) => {}
            Err(err) => failures.push(format!("{}: {err}", e.id.as_str())),
        }
    })?;
    assert!(
        failures.is_empty(),
        "permission replay failures: {failures:?}"
    );
    let replied_clean: i64 = conn.query_row(
        "SELECT COUNT(*) FROM permission_request WHERE status = 'replied' AND suspended_reason IS NULL",
        [],
        |r| r.get(0),
    )?;
    let replied_after_suspension: i64 = conn.query_row(
        "SELECT COUNT(*) FROM permission_request WHERE status = 'replied' AND suspended_reason = 'process-loss'",
        [],
        |r| r.get(0),
    )?;
    let cancelled: i64 = conn.query_row(
        "SELECT COUNT(*) FROM permission_request WHERE status = 'cancelled'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(
        (replied_clean, replied_after_suspension, cancelled),
        (31, 10, 2),
        "D1 distribution"
    );
    Ok(())
}

#[test]
fn executes_the_frozen_reject_cascade_fixture_with_literal_durable_data() -> TestResult {
    let path = fixtures::fixture_path(std::path::Path::new(
        "permission-suspend-notlive-reject-cascade/expected/durable.jsonl",
    ));
    let mut svc = service_for(&["ses_r0_permission"])?;
    let session: SessionId = "ses_r0_permission".parse()?;
    let first: PermissionId = "per_r0_first".parse()?;
    let second: PermissionId = "per_r0_second".parse()?;
    let third: PermissionId = "per_r0_third".parse()?;
    let fb = fallback_authority();
    let (ra, rb, rc) = (
        ["src/a.ts".to_owned()],
        ["src/b.ts".to_owned()],
        ["src/c.ts".to_owned()],
    );

    svc.create(
        &ev("evt_1")?,
        &first,
        &session,
        &PermissionRequest {
            action: "read",
            resources: &ra,
            authority: fb,
        },
        0,
    )?;
    svc.create(
        &ev("evt_2")?,
        &second,
        &session,
        &PermissionRequest {
            action: "read",
            resources: &rb,
            authority: fb,
        },
        0,
    )?;
    svc.suspend(
        &ev("evt_3")?,
        &first,
        PermissionSuspensionReason::Shutdown,
        0,
    )?;
    svc.suspend(
        &ev("evt_4")?,
        &second,
        PermissionSuspensionReason::Shutdown,
        0,
    )?;
    svc.create(
        &ev("evt_5")?,
        &third,
        &session,
        &PermissionRequest {
            action: "read",
            resources: &rc,
            authority: fb,
        },
        0,
    )?;
    let out = svc.reply(&third, OrdinaryReply::Reject, minter("rc"), 0)?;
    assert_eq!(out.primary, CasOutcome::Applied);

    let mine: Vec<(String, serde_json::Value)> = svc
        .store()
        .connection()
        .prepare("SELECT type, data FROM event ORDER BY seq")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .map(|row| {
            let (ty, raw) = row?;
            Ok::<_, Box<dyn std::error::Error>>((ty, serde_json::from_str(&raw)?))
        })
        .collect::<Result<_, _>>()?;

    let expected: Vec<(String, serde_json::Value)> = std::fs::read_to_string(&path)?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l)?;
            Ok::<_, Box<dyn std::error::Error>>((
                v["type"].as_str().ok_or("type")?.to_owned(),
                v["data"].clone(),
            ))
        })
        .collect::<Result<_, _>>()?;

    assert_eq!(
        mine, expected,
        "durable type + data must match the frozen fixture EXACTLY (no normalization)"
    );
    Ok(())
}
