//! R3-b Form witnesses: the Permission CONTRAST (answer/cancel succeed after
//! suspension), terminal immutability, reason-required corruption, and
//! execution of the frozen `form-suspend-settle-after-replacement` scenario.

use grapt_form::{FormError, FormService};
use grapt_store::{AggregateId, EventStore};
use grapt_types::{CasOutcome, EventId, FormId, FormSuspensionReason, SessionId};
use serde_json::json;
#[path = "../../../test-support/fixtures.rs"]
mod fixtures;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fields() -> serde_json::Value {
    json!([{"key": "name", "required": true, "type": "string"}])
}

#[test]
fn catalog_keeps_suspended_fields_and_refuses_missing_creation_truth() -> TestResult {
    let mut svc = service_for(&["ses_catalog"])?;
    let session: SessionId = "ses_catalog".parse()?;
    let id: FormId = "frm_catalog".parse()?;
    let spec = json!([{"key":"choice", "type":"multiselect", "options":[{"value":"tabs\tand\nlines", "label":"Keep exact text"}]}]);
    assert!(grapt_form::catalog::pending_forms(svc.store())?.is_empty());
    svc.create(
        &ev("evt_catalog_create")?,
        &id,
        &session,
        "Choose",
        &spec,
        1,
    )?;
    let initial = grapt_form::catalog::pending_forms(svc.store())?;
    assert_eq!(initial.len(), 1);
    assert_eq!(initial[0].info["fields"], spec);
    assert_eq!(
        grapt_form::catalog::pending_forms_for(svc.store(), Some(&session))?,
        initial
    );
    assert!(
        grapt_form::catalog::pending_forms_for(svc.store(), Some(&"ses_other".parse()?))?
            .is_empty()
    );
    svc.suspend(
        &ev("evt_catalog_suspend")?,
        &id,
        FormSuspensionReason::ProcessLoss,
        2,
    )?;
    let reopened = grapt_form::catalog::pending_forms(svc.store())?;
    assert_eq!(reopened[0].info, initial[0].info);
    assert_eq!(
        reopened[0].suspended,
        Some(FormSuspensionReason::ProcessLoss)
    );
    svc.store().connection().execute(
        "UPDATE form_request SET id = 'frm_missing_creation' WHERE id = 'frm_catalog'",
        [],
    )?;
    let error = grapt_form::catalog::pending_forms(svc.store())
        .err()
        .ok_or("missing creation was silently accepted")?;
    assert!(
        error.to_string().contains("creation event is missing"),
        "{error}"
    );
    Ok(())
}

#[test]
fn catalog_terminal_rows_stay_absent_and_corrupt_status_does_not_mean_empty() -> TestResult {
    let mut svc = service_for(&["ses_catalog_terminal"])?;
    let session: SessionId = "ses_catalog_terminal".parse()?;
    let id: FormId = "frm_catalog_terminal".parse()?;
    svc.create(
        &ev("evt_catalog_terminal_create")?,
        &id,
        &session,
        "Choose",
        &fields(),
        1,
    )?;
    svc.cancel(&ev("evt_catalog_cancel")?, &id, 2)?;
    assert!(grapt_form::catalog::pending_forms(svc.store())?.is_empty());
    svc.store().connection().execute(
        "UPDATE form_request SET status = 'unknown' WHERE id = ?1",
        [id.as_str()],
    )?;
    assert!(grapt_form::catalog::pending_forms(svc.store()).is_err());
    Ok(())
}

/// A service whose store has already been elected writer for every session the
/// test will write to. Election is done here, on the bare store, because the
/// append path never elects on a caller's behalf — an unelected writer is
/// refused with `NotElected` rather than silently taking the aggregate.
fn service_for(sessions: &[&str]) -> Result<FormService, Box<dyn std::error::Error>> {
    let mut store = EventStore::open_in_memory()?;
    for raw in sessions {
        store.elect_writer(&AggregateId::Session(raw.parse()?))?;
    }
    Ok(FormService::new(store)?)
}

fn ev(raw: &str) -> Result<EventId, Box<dyn std::error::Error>> {
    Ok(raw.parse()?)
}

fn row(
    svc: &FormService,
    id: &FormId,
) -> Result<(String, Option<String>), Box<dyn std::error::Error>> {
    Ok(svc.store().connection().query_row(
        "SELECT status, suspended_reason FROM form_request WHERE id = ?1",
        [id.as_str()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

#[test]
fn a_suspended_form_can_still_be_answered_keeping_history() -> TestResult {
    // THE CONTRAST with Permission (which would return NotLive here).
    let mut svc = service_for(&["ses_f"])?;
    let session: SessionId = "ses_f".parse()?;
    let id: FormId = "frm_a".parse()?;
    svc.create(&ev("evt_c")?, &id, &session, "T", &fields(), 0)?;
    assert_eq!(
        svc.suspend(&ev("evt_s")?, &id, FormSuspensionReason::Shutdown, 0)?,
        CasOutcome::Applied
    );
    // Answer AFTER suspension SUCCEEDS.
    assert_eq!(
        svc.answer(&ev("evt_r")?, &id, &json!({"name": "Beta"}), 0)?,
        CasOutcome::Applied
    );
    let (status, reason) = row(&svc, &id)?;
    assert_eq!(status, "answered");
    assert_eq!(
        reason.as_deref(),
        Some("shutdown"),
        "suspension kept as history"
    );
    Ok(())
}

#[test]
fn a_suspended_form_can_still_be_cancelled() -> TestResult {
    let mut svc = service_for(&["ses_f"])?;
    let session: SessionId = "ses_f".parse()?;
    let id: FormId = "frm_x".parse()?;
    svc.create(&ev("evt_c")?, &id, &session, "T", &fields(), 0)?;
    svc.suspend(&ev("evt_s")?, &id, FormSuspensionReason::ProcessLoss, 0)?;
    assert_eq!(svc.cancel(&ev("evt_x")?, &id, 0)?, CasOutcome::Applied);
    let (status, reason) = row(&svc, &id)?;
    assert_eq!(status, "cancelled");
    assert_eq!(reason.as_deref(), Some("process-loss"));
    Ok(())
}

#[test]
fn terminal_forms_are_immutable() -> TestResult {
    let mut svc = service_for(&["ses_f"])?;
    let session: SessionId = "ses_f".parse()?;
    let id: FormId = "frm_t".parse()?;
    svc.create(&ev("evt_c")?, &id, &session, "T", &fields(), 0)?;
    svc.answer(&ev("evt_r")?, &id, &json!({"name": "x"}), 0)?;
    let events_after: i64 =
        svc.store()
            .connection()
            .query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))?;
    // Second answer, a cancel, and a suspend all see a terminal row.
    assert_eq!(
        svc.answer(&ev("evt_r2")?, &id, &json!({"name": "y"}), 0)?,
        CasOutcome::AlreadyTerminal
    );
    assert_eq!(
        svc.cancel(&ev("evt_x")?, &id, 0)?,
        CasOutcome::AlreadyTerminal
    );
    assert_eq!(
        svc.suspend(&ev("evt_s")?, &id, FormSuspensionReason::Shutdown, 0)?,
        CasOutcome::AlreadyTerminal
    );
    let events_now: i64 =
        svc.store()
            .connection()
            .query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))?;
    assert_eq!(events_now, events_after, "terminal form admitted no writes");
    Ok(())
}

#[test]
fn corrupt_suspended_form_without_reason_is_typed_corruption() -> TestResult {
    let mut svc = service_for(&["ses_f"])?;
    let session: SessionId = "ses_f".parse()?;
    let id: FormId = "frm_c".parse()?;
    svc.create(&ev("evt_c")?, &id, &session, "T", &fields(), 0)?;
    svc.store().connection().execute(
        "UPDATE form_request SET status = 'suspended', suspended_reason = NULL WHERE id = ?1",
        [id.as_str()],
    )?;
    assert!(matches!(
        svc.answer(&ev("evt_r")?, &id, &json!({}), 0),
        Err(FormError::Corruption { .. })
    ));
    Ok(())
}

#[test]
fn executes_the_frozen_fixture_with_faithful_durable_data() -> TestResult {
    // Run the exact scenario and compare the FULL durable payload (type + data)
    // of every event against the frozen durable.jsonl — not kinds only.
    let path = fixtures::fixture_path(std::path::Path::new(
        "form-suspend-settle-after-replacement/expected/durable.jsonl",
    ));

    let mut svc = service_for(&["ses_r0_form"])?;
    let session: SessionId = "ses_r0_form".parse()?;
    let suspend_id: FormId = "frm_r0_suspend".parse()?;
    let cancel_id: FormId = "frm_r0_cancel".parse()?;
    svc.create(
        &ev("evt_c1")?,
        &suspend_id,
        &session,
        "R0 form fixture",
        &fields(),
        0,
    )?;
    svc.suspend(
        &ev("evt_s1")?,
        &suspend_id,
        FormSuspensionReason::Shutdown,
        0,
    )?;
    svc.answer(&ev("evt_a1")?, &suspend_id, &json!({"name": "Beta"}), 0)?;
    svc.create(
        &ev("evt_c2")?,
        &cancel_id,
        &session,
        "R0 form fixture",
        &fields(),
        0,
    )?;
    svc.cancel(&ev("evt_x1")?, &cancel_id, 0)?;

    let mine: Vec<(String, serde_json::Value)> = svc
        .store()
        .connection()
        .prepare("SELECT type, data FROM event ORDER BY seq")?
        .query_map([], |r| {
            let ty: String = r.get(0)?;
            let raw: String = r.get(1)?;
            Ok((ty, raw))
        })?
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
        "durable type + data must match the frozen fixture exactly"
    );
    Ok(())
}

/// A form id is unique across the WHOLE projection, not per session. The
/// session-scoped by-id read cannot see a collision in another session, so a
/// creation check built on it would admit a duplicate id and then fail inside
/// the durable insert. `form_id_exists` is the check that actually answers
/// "is this id taken".
#[test]
fn a_form_id_is_taken_across_every_session_not_only_its_own() -> TestResult {
    let mut svc = service_for(&["ses_one", "ses_two"])?;
    let one: SessionId = "ses_one".parse()?;
    let two: SessionId = "ses_two".parse()?;
    let id: FormId = "frm_shared".parse()?;

    assert!(!grapt_form::catalog::form_id_exists(svc.store(), &id)?);
    svc.create(&ev("evt_shared_c1")?, &id, &one, "One", &fields(), 1)?;
    assert!(grapt_form::catalog::form_id_exists(svc.store(), &id)?);

    // The OTHER session cannot see it by id, which is exactly why a
    // session-scoped existence check is the wrong check for creation.
    assert!(grapt_form::catalog::form_by_id(svc.store(), &two, &id)?.is_none());
    assert!(grapt_form::catalog::form_by_id(svc.store(), &one, &id)?.is_some());
    Ok(())
}

/// The settled-inclusive by-id read.
///
/// The list projection answers "what is still blocking"; this answers "what did
/// this form end up as", which a client asks immediately after replying. The
/// two questions have different answers for the same form, and the whole point
/// of the new read is that the second one survives settlement.
#[test]
fn a_form_reads_back_by_id_after_it_settles() -> TestResult {
    let mut svc = service_for(&["ses_byid"])?;
    let session: SessionId = "ses_byid".parse()?;
    let answered: FormId = "frm_answered".parse()?;
    let cancelled: FormId = "frm_cancelled".parse()?;
    let spec = fields();

    // Absence is absence, not an empty form.
    assert!(grapt_form::catalog::form_by_id(svc.store(), &session, &answered)?.is_none());

    svc.create(
        &ev("evt_byid_c1")?,
        &answered,
        &session,
        "Answered",
        &spec,
        1,
    )?;
    svc.create(
        &ev("evt_byid_c2")?,
        &cancelled,
        &session,
        "Cancelled",
        &spec,
        2,
    )?;
    let pending = grapt_form::catalog::form_by_id(svc.store(), &session, &answered)?
        .ok_or("the created form must read back")?;
    assert_eq!(pending.state, grapt_form::catalog::FormState::Pending);
    assert_eq!(pending.info["fields"], spec);
    assert_eq!(pending.session, session);

    // A suspended form reports its reason, and stays readable.
    svc.suspend(
        &ev("evt_byid_suspend")?,
        &answered,
        FormSuspensionReason::Shutdown,
        3,
    )?;
    assert_eq!(
        grapt_form::catalog::form_by_id(svc.store(), &session, &answered)?
            .ok_or("a suspended form must read back")?
            .state,
        grapt_form::catalog::FormState::Suspended(FormSuspensionReason::Shutdown)
    );

    // Answering a SUSPENDED form settles it — the Permission contrast — and the
    // settled read hands the answer back rather than reporting suspension.
    let answer = json!({"name": "ada"});
    svc.answer(&ev("evt_byid_answer")?, &answered, &answer, 4)?;
    let settled = grapt_form::catalog::form_by_id(svc.store(), &session, &answered)?
        .ok_or("an answered form must still read back")?;
    assert_eq!(
        settled.state,
        grapt_form::catalog::FormState::Answered(answer),
        "the recorded answer must survive settlement"
    );
    assert_eq!(settled.state.status_str(), "answered");
    assert_eq!(
        settled.info, pending.info,
        "settling must not disturb the field specification"
    );
    // And it is gone from the unsettled projection, which is the whole reason
    // this read exists.
    assert!(
        grapt_form::catalog::pending_forms_for(svc.store(), Some(&session))?
            .iter()
            .all(|form| form.id != answered)
    );

    svc.cancel(&ev("evt_byid_cancel")?, &cancelled, 5)?;
    assert_eq!(
        grapt_form::catalog::form_by_id(svc.store(), &session, &cancelled)?
            .ok_or("a cancelled form must still read back")?
            .state,
        grapt_form::catalog::FormState::Cancelled
    );

    // A form belonging to another session is not readable through this one.
    assert!(
        grapt_form::catalog::form_by_id(svc.store(), &"ses_other".parse()?, &answered)?.is_none(),
        "a form was readable through a session that does not own it"
    );
    Ok(())
}

/// A row whose status, reason and answer contradict each other is corruption,
/// never a default state.
#[test]
fn a_contradictory_settled_row_refuses_rather_than_defaulting() -> TestResult {
    let mut svc = service_for(&["ses_corrupt"])?;
    let session: SessionId = "ses_corrupt".parse()?;
    let id: FormId = "frm_corrupt".parse()?;
    svc.create(
        &ev("evt_corrupt_c")?,
        &id,
        &session,
        "Corrupt",
        &fields(),
        1,
    )?;
    for statement in [
        // Answered with no recorded answer.
        "UPDATE form_request SET status = 'answered', answer = NULL",
        // Cancelled while carrying one.
        "UPDATE form_request SET status = 'cancelled', answer = '{\"name\":\"x\"}'",
        // Pending but suspended.
        "UPDATE form_request SET status = 'pending', suspended_reason = 'shutdown', answer = NULL",
        // An answer that is not JSON.
        "UPDATE form_request SET status = 'answered', answer = 'not json'",
    ] {
        svc.store().connection().execute(statement, [])?;
        assert!(
            grapt_form::catalog::form_by_id(svc.store(), &session, &id).is_err(),
            "a contradictory row was admitted by {statement}"
        );
    }
    Ok(())
}

#[test]
fn presentation_metadata_is_bounded_and_creation_rolls_back_with_its_projection() -> TestResult {
    use grapt_form::{catalog::form_by_id, service::FormPresentation};
    let mut store = EventStore::open_in_memory()?;
    FormService::initialize(&mut store)?;
    let session: SessionId = "ses_presented".parse()?;
    let id: FormId = "frm_presented".parse()?;
    let event = ev("evt_presented")?;
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    let fields = fields();
    for metadata in [json!([]), json!({"text":"x".repeat(4097)})] {
        assert!(matches!(
            FormService::create_presented_on(
                &mut store,
                &event,
                &id,
                &session,
                FormPresentation {
                    title: "Question",
                    fields: &fields,
                    metadata: Some(&metadata)
                },
                1
            ),
            Err(FormError::InvalidPresentation { .. })
        ));
        assert!(form_by_id(&store, &session, &id)?.is_none());
    }
    let metadata = json!({"kind":"request_user_input"});
    store.connection().execute_batch("CREATE TEMP TRIGGER refuse_presented BEFORE INSERT ON form_request BEGIN SELECT RAISE(ABORT, 'projection refused'); END")?;
    assert!(FormService::create_presented_on(
        &mut store,
        &event,
        &id,
        &session,
        FormPresentation {
            title: "Question",
            fields: &fields,
            metadata: Some(&metadata)
        },
        1
    )
    .is_err());
    assert!(form_by_id(&store, &session, &id)?.is_none());
    let events: i64 = store.connection().query_row(
        "SELECT count(*) FROM event WHERE id = ?1",
        [event.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(events, 0, "failed projection left a creation event");
    store
        .connection()
        .execute_batch("DROP TRIGGER refuse_presented")?;
    FormService::create_presented_on(
        &mut store,
        &event,
        &id,
        &session,
        FormPresentation {
            title: "Question",
            fields: &fields,
            metadata: Some(&metadata),
        },
        1,
    )?;
    let row = form_by_id(&store, &session, &id)?.ok_or("created question missing")?;
    assert_eq!(row.info["metadata"], metadata);
    assert_eq!(row.info["fields"], fields);
    Ok(())
}

#[test]
fn malformed_creation_json_skips_its_form_and_recovery_keeps_the_healthy_one() -> TestResult {
    let path = std::env::temp_dir().join(format!(
        "grapt-form-malformed-creation-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let mut store = EventStore::open(&path, "test")?;
    for session in ["ses_creation_broken", "ses_creation_healthy"] {
        store.elect_writer(&AggregateId::Session(session.parse()?))?;
    }
    let mut svc = FormService::new(store)?;
    for (form, session) in [
        ("frm_creation_broken", "ses_creation_broken"),
        ("frm_creation_healthy", "ses_creation_healthy"),
    ] {
        svc.create(
            &ev(&format!("evt_{form}"))?,
            &form.parse()?,
            &session.parse()?,
            "Name",
            &fields(),
            1,
        )?;
    }
    drop(svc);
    // The store refuses ungoverned writes; another writer's damage.
    rusqlite::Connection::open(&path)?.execute(
        "UPDATE event SET data = '{\"form\": not json' WHERE type = 'form.created.1'
           AND aggregate_id = 'ses_creation_broken'",
        [],
    )?;
    let store = EventStore::open(&path, "test")?;
    let recoverable = grapt_form::catalog::pending_forms_for_recovery(&store)?;
    let healthy: Vec<&str> = recoverable
        .forms
        .iter()
        .map(|form| form.id.as_str())
        .collect();
    assert_eq!(healthy, ["frm_creation_healthy"]);
    assert_eq!(recoverable.skipped.len(), 1);
    assert_eq!(
        recoverable.skipped[0].id.as_ref().map(FormId::as_str),
        Some("frm_creation_broken")
    );
    drop(store);
    std::fs::remove_file(&path)?;
    Ok(())
}
