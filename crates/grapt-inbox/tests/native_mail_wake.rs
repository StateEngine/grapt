//! Native wake inputs and source consumption share one governed commit.
use grapt_inbox::{AdmitRequest, Inbox, SyntheticAdmitRequest};
use grapt_store::{
    AggregateId, EventStore, MailBindingExpectation, RemoteMailWakeClaim, RemoteMailboxInput,
    RemoteMailboxScope,
};
use grapt_types::remote_mail::*;
use grapt_types::{
    DeliveryMode, InstructionKey, MailboxBody, MailboxCursor, ProtocolVersion, SeatMessageKind,
    SeatRecipientId, SessionId,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn restart_state_fixture(inbox: &Inbox, session: &SessionId, data: &str) -> TestResult {
    let aggregate = AggregateId::Session(session.clone());
    let rows = inbox.store().fixture_rows()?;
    rows.sequence_cursor(&aggregate, 2)?;
    rows.event_row(&grapt_store::fixture_rows::EventRow {
        aggregate: &aggregate,
        id: "evt_restart_fixture",
        seq: 2,
        created_ms: 2,
        versioned_type: "session.native-mail-state.1",
        data,
        writer_epoch: 1,
    })?;
    rows.commit()?;
    Ok(())
}

#[test]
fn restart_selection_isolates_a_malformed_session_from_healthy_sessions() -> TestResult {
    let (mut inbox, session) = fixture()?;
    let fingerprint = |raw: &str| -> Result<String, Box<dyn std::error::Error>> {
        Ok(MailBindingExpectation::new(
            InstructionKey::new("stateengine-identity").ok_or("key")?,
            raw.into(),
        )?
        .fingerprint())
    };
    inbox.select_native_mail_target(
        &session,
        &[],
        &fingerprint("bound")?,
        &"evt_selected".parse()?,
        1,
    )?;
    let damaged: SessionId = "ses_replacement_target".parse()?;
    restart_state_fixture(&inbox, &damaged, "{")?;
    let selected = inbox.selected_native_mail_sessions(64);
    assert!(
        selected.is_ok(),
        "one malformed session broke all restart selection: {selected:?}"
    );
    assert_eq!(selected?, vec![session]);
    Ok(())
}

#[test]
fn restart_selection_never_revives_an_older_target_behind_oversized_latest_state() -> TestResult {
    let (mut inbox, session) = fixture()?;
    let binding = MailBindingExpectation::new(
        InstructionKey::new("stateengine-identity").ok_or("key")?,
        "bound".into(),
    )?
    .fingerprint();
    inbox.select_native_mail_target(&session, &[], &binding, &"evt_selected".parse()?, 1)?;
    let oversized = serde_json::json!({
        "sessionID":session.as_str(),"held":false,"humanResume":false,
        "target":"evt_revoked","binding":null,"padding":"x".repeat(4096),
    })
    .to_string();
    restart_state_fixture(&inbox, &session, &oversized)?;
    assert!(
        inbox.selected_native_mail_sessions(64)?.is_empty(),
        "restart revived a historical target behind unreadable latest state"
    );
    Ok(())
}

fn scope() -> Result<RemoteMailboxScope, Box<dyn std::error::Error>> {
    Ok(RemoteMailboxScope {
        projects: ProjectPair::new(
            SourceProjectId::try_new("source")?,
            TargetProjectId::try_new("secode")?,
        ),
        principal: SourcePrincipalId::try_new("coder")?,
        recipient: SeatRecipientId::try_new("coder")?,
    })
}

fn fixture() -> Result<(Inbox, SessionId), Box<dyn std::error::Error>> {
    initialize(EventStore::open_in_memory()?)
}

fn initialize(mut store: EventStore) -> Result<(Inbox, SessionId), Box<dyn std::error::Error>> {
    let session: SessionId = "ses_native_wake".parse()?;
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    // The instruction owner is outside this crate. Supply its exact read seam.
    store.connection().execute_batch(
        "CREATE TABLE instruction_entry(session_id TEXT,key TEXT,value TEXT,removed INTEGER);
         INSERT INTO instruction_entry VALUES('ses_native_wake','stateengine-identity','bound',0);
         INSERT INTO instruction_entry VALUES('ses_replacement_target','stateengine-identity','bound',0);",
    )?;
    Ok((Inbox::new(store)?, session))
}

fn mail(inbox: &mut Inbox, id: &str, attention: MailAttention, settle: bool) -> TestResult {
    let scope = scope()?;
    let input = RemoteMailboxInput {
        scope: scope.clone(),
        protocol: ProtocolVersion::new("remote-seat-mail-transport/v1").ok_or("protocol")?,
        source_message: RemoteMailboxMessageId::try_new(id)?,
        source_binding: SourceBindingId::try_new("binding")?,
        source_cursor: ExclusiveSourceCursor::try_new(format!("cursor-{id}"))?,
        sender: SourcePrincipalId::try_new("pilot")?,
        // Deliberately quiet-sounding: attention, not kind, grants a wake.
        kind: SeatMessageKind::Done,
        body: MailboxBody::new("untrusted body")?,
        source_created: SourceTimestamp::try_new("2026-10-20T00:00:00Z")?,
        refs: Vec::new(),
        supersedes: None,
        superseded_by: None,
        archive: SourceArchiveDisposition::Active,
        attention,
    };
    let ack = inbox.store_mut().admit_remote_mailbox(&scope, &input)?.ack;
    if settle {
        inbox
            .store_mut()
            .settle_remote_mailbox_delivery(&scope, &ack, None)?;
    }
    Ok(())
}

fn claim(
    inbox: &Inbox,
    session: &SessionId,
) -> Result<RemoteMailWakeClaim, Box<dyn std::error::Error>> {
    let binding = MailBindingExpectation::new(
        InstructionKey::new("stateengine-identity").ok_or("key")?,
        "bound".into(),
    )?;
    inbox
        .store()
        .prepare_remote_mail_wake(session, &scope()?, binding, MailboxCursor::START)?
        .claim
        .ok_or_else(|| "expected pending wake claim".into())
}

fn admit(inbox: &mut Inbox, claim: &RemoteMailWakeClaim, name: &str) -> TestResult {
    admit_mode(inbox, claim, name, DeliveryMode::Queue)
}

fn admit_mode(
    inbox: &mut Inbox,
    claim: &RemoteMailWakeClaim,
    name: &str,
    delivery: DeliveryMode,
) -> TestResult {
    let event = format!("evt_{name}").parse()?;
    let input = format!("msg_{name}").parse()?;
    let metadata = claim.metadata();
    let text = claim
        .source_messages()
        .map(|id| id.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    inbox.admit_native_mail_wake(
        claim,
        SyntheticAdmitRequest {
            event_id: &event,
            session: claim.session(),
            input: &input,
            text: &text,
            description: Some("Native inbox wake"),
            metadata: Some(&metadata),
            delivery,
            resume: true,
            created_ms: 1,
        },
    )?;
    Ok(())
}

#[test]
fn native_mail_admission_and_recovery_preserve_a_large_pending_prompt() -> TestResult {
    let (mut inbox, session) = fixture()?;
    inbox.admit(AdmitRequest {
        event_id: &"evt_large_prompt".parse()?,
        session: &session,
        input: &"msg_large_prompt".parse()?,
        text: &"x".repeat(262145),
        delivery: DeliveryMode::Queue,
        resume: true,
        created_ms: 1,
    })?;
    mail(&mut inbox, "first", MailAttention::Wake, true)?;
    let frozen = claim(&inbox, &session)?;
    admit(&mut inbox, &frozen, "beside_large_prompt")?;
    assert!(inbox.pending_native_mail_wake(&session)?.is_some());
    Ok(())
}

#[test]
fn seat03_a_burst_is_one_input_and_replayed_claim_cannot_admit_twice() -> TestResult {
    let (mut inbox, session) = fixture()?;
    mail(&mut inbox, "first", MailAttention::Wake, true)?;
    mail(&mut inbox, "quiet", MailAttention::Quiet, true)?;
    mail(&mut inbox, "second", MailAttention::Wake, true)?;
    let frozen = claim(&inbox, &session)?;
    assert_eq!(
        frozen
            .source_messages()
            .map(|id| id.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    admit(&mut inbox, &frozen, "native_burst")?;
    let pending = inbox.pending_inputs(&session)?;
    assert_eq!(
        pending.len(),
        1,
        "burst started more than one canonical input"
    );
    assert_eq!(pending[0].text, "first\nsecond");
    assert_eq!(pending[0].metadata, Some(frozen.metadata()));
    assert_eq!(
        inbox
            .pending_native_mail_wake(&session)?
            .map(|input| input.input),
        Some(pending[0].input.clone())
    );
    assert!(admit(&mut inbox, &frozen, "duplicate_burst").is_err());
    assert_eq!(
        inbox.pending_inputs(&session)?.len(),
        1,
        "lost race committed a duplicate input"
    );
    Ok(())
}

#[test]
fn seat03_a_competing_capture_rolls_back_the_entire_wake_admission() -> TestResult {
    let (mut inbox, session) = fixture()?;
    mail(&mut inbox, "first", MailAttention::Wake, true)?;
    mail(&mut inbox, "second", MailAttention::Wake, true)?;
    let frozen = claim(&inbox, &session)?;
    let page = inbox
        .store()
        .read_remote_mailbox(&scope()?, MailboxCursor::START, 64, 262144)?;
    inbox
        .store_mut()
        .consume_remote_mailbox(&scope()?, page.records[0].ack().local_message())?;
    assert!(admit(&mut inbox, &frozen, "lost_capture_race").is_err());
    assert!(
        inbox.pending_inputs(&session)?.is_empty(),
        "input escaped the failed atomic claim"
    );
    let page = inbox
        .store()
        .read_remote_mailbox(&scope()?, MailboxCursor::START, 64, 262144)?;
    assert!(
        page.records[1].actionable(),
        "failed batch consumed an unrelated row"
    );
    Ok(())
}

#[test]
fn seat03_rebinding_before_commit_refuses_without_consuming_work() -> TestResult {
    let (mut inbox, session) = fixture()?;
    mail(&mut inbox, "first", MailAttention::Wake, true)?;
    let frozen = claim(&inbox, &session)?;
    inbox
        .store()
        .connection()
        .execute("UPDATE instruction_entry SET value='rebound'", [])?;
    assert!(admit(&mut inbox, &frozen, "rebound_wake").is_err());
    assert!(inbox.pending_inputs(&session)?.is_empty());
    let page = inbox
        .store()
        .read_remote_mailbox(&scope()?, MailboxCursor::START, 64, 262144)?;
    assert!(
        page.records[0].actionable(),
        "revoked binding consumed pending work"
    );
    Ok(())
}

#[test]
fn seat03_delivery_debt_cannot_be_claimed_as_a_wake() -> TestResult {
    let (mut inbox, session) = fixture()?;
    mail(&mut inbox, "unsettled", MailAttention::Wake, false)?;
    assert!(claim(&inbox, &session).is_err());
    assert!(inbox.pending_inputs(&session)?.is_empty());
    Ok(())
}

#[test]
fn seat03_later_mail_does_not_rewrite_the_admitted_burst() -> TestResult {
    let (mut inbox, session) = fixture()?;
    mail(&mut inbox, "first", MailAttention::Wake, true)?;
    let frozen = claim(&inbox, &session)?;
    mail(&mut inbox, "later", MailAttention::Wake, true)?;
    admit(&mut inbox, &frozen, "frozen_wake")?;
    assert_eq!(
        inbox
            .pending_native_mail_wake(&session)?
            .ok_or("wake")?
            .text,
        "first"
    );
    assert_eq!(
        claim(&inbox, &session)?
            .source_messages()
            .map(|id| id.as_str())
            .collect::<Vec<_>>(),
        ["later"]
    );
    Ok(())
}

#[test]
fn seat03_client_metadata_cannot_authorize_native_recovery_or_steal_a_claim() -> TestResult {
    let (mut inbox, session) = fixture()?;
    mail(&mut inbox, "first", MailAttention::Wake, true)?;
    let frozen = claim(&inbox, &session)?;
    let metadata = frozen.metadata();
    let event = "evt_client_metadata".parse()?;
    let input = "msg_collision".parse()?;
    inbox.admit_synthetic(SyntheticAdmitRequest {
        event_id: &event,
        session: &session,
        input: &input,
        text: "first",
        description: Some("Native inbox wake"),
        metadata: Some(&metadata),
        delivery: DeliveryMode::Queue,
        resume: false,
        created_ms: 1,
    })?;
    assert!(
        inbox.pending_native_mail_wake(&session)?.is_none(),
        "caller metadata granted automatic execution"
    );
    assert!(
        admit(&mut inbox, &frozen, "collision").is_err(),
        "ordinary input reconciled into a native claim"
    );
    assert!(
        claim(&inbox, &session)?.source_messages().next().is_some(),
        "collision consumed source work"
    );
    Ok(())
}

#[test]
fn seat03_commit_before_enqueue_recovers_the_same_input_after_reopen() -> TestResult {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _removed = std::fs::remove_dir_all(&self.0);
        }
    }
    let root = Scratch(std::env::temp_dir().join(format!(
        "grapt-seat03-commit-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )));
    std::fs::create_dir(&root.0)?;
    let path = root.0.join("inbox.sqlite");
    let (mut inbox, session) = initialize(EventStore::open(&path, "seat03-commit")?)?;
    mail(&mut inbox, "first", MailAttention::Wake, true)?;
    let frozen = claim(&inbox, &session)?;
    admit(&mut inbox, &frozen, "committed_not_enqueued")?;
    let before = inbox
        .pending_native_mail_wake(&session)?
        .ok_or("committed wake")?;
    drop(inbox); // The execution driver never received the committed input.
    let mut recovered = Inbox::new(EventStore::open(&path, "seat03-recovery")?)?;
    assert_eq!(recovered.pending_native_mail_wake(&session)?, Some(before));
    // Source replay still names the original consumed local row.
    mail(&mut recovered, "first", MailAttention::Wake, true)?;
    assert_eq!(recovered.pending_inputs(&session)?.len(), 1);
    let binding = MailBindingExpectation::new(
        InstructionKey::new("stateengine-identity").ok_or("key")?,
        "bound".into(),
    )?;
    assert!(recovered
        .store()
        .prepare_remote_mail_wake(&session, &scope()?, binding, MailboxCursor::START)?
        .claim
        .is_none());
    Ok(())
}

#[test]
fn seat03_an_explicit_stop_survives_reopen_without_losing_pending_input() -> TestResult {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _removed = std::fs::remove_dir_all(&self.0);
        }
    }
    let root = Scratch(std::env::temp_dir().join(format!(
        "grapt-seat03-held-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )));
    std::fs::create_dir(&root.0)?;
    let path = root.0.join("inbox.sqlite");
    let (mut inbox, session) = initialize(EventStore::open(&path, "seat03-commit")?)?;
    mail(&mut inbox, "first", MailAttention::Wake, true)?;
    let frozen = claim(&inbox, &session)?;
    admit(&mut inbox, &frozen, "committed_not_enqueued")?;
    let before = inbox.pending_inputs(&session)?;
    assert_eq!(inbox.hold_native_mail_wakes(&session, 2)?, 1);
    assert_eq!(
        inbox.hold_native_mail_wakes(&session, 3)?,
        0,
        "repeated stop appended another hold"
    );
    drop(inbox);
    let mut recovered = Inbox::new(EventStore::open(&path, "seat03-held-recovery")?)?;
    recovered
        .store_mut()
        .elect_writer(&AggregateId::Session(session.clone()))?;
    assert_eq!(
        recovered.pending_inputs(&session)?,
        before,
        "stop lost canonical pending input"
    );
    assert!(
        recovered.pending_native_mail_wake(&session)?.is_none(),
        "restart reauthorized explicitly cancelled work"
    );
    // Source replay still names the original consumed local row.
    mail(&mut recovered, "first", MailAttention::Wake, true)?;
    assert_eq!(recovered.pending_inputs(&session)?.len(), 1);
    let binding = MailBindingExpectation::new(
        InstructionKey::new("stateengine-identity").ok_or("key")?,
        "bound".into(),
    )?;
    assert!(recovered
        .store()
        .prepare_remote_mail_wake(&session, &scope()?, binding, MailboxCursor::START)?
        .claim
        .is_none());
    mail(&mut recovered, "later", MailAttention::Wake, true)?;
    let later = claim(&recovered, &session)?;
    assert!(admit(&mut recovered, &later, "after_stop").is_err());
    assert_eq!(recovered.pending_inputs(&session)?, before);
    assert!(claim(&recovered, &session)?
        .source_messages()
        .next()
        .is_some());
    Ok(())
}

#[test]
fn seat03_a_claim_cannot_admit_input_to_another_session() -> TestResult {
    let (mut inbox, session) = fixture()?;
    mail(&mut inbox, "first", MailAttention::Wake, true)?;
    let frozen = claim(&inbox, &session)?;
    let other: SessionId = "ses_native_foreign".parse()?;
    inbox
        .store_mut()
        .elect_writer(&AggregateId::Session(other.clone()))?;
    let event = "evt_native_foreign".parse()?;
    let input = "msg_native_foreign".parse()?;
    let metadata = frozen.metadata();
    assert!(
        inbox
            .admit_native_mail_wake(
                &frozen,
                SyntheticAdmitRequest {
                    event_id: &event,
                    session: &other,
                    input: &input,
                    text: "first",
                    description: None,
                    metadata: Some(&metadata),
                    delivery: DeliveryMode::Queue,
                    resume: true,
                    created_ms: 1,
                }
            )
            .is_err(),
        "native claim crossed its Session binding"
    );
    assert!(inbox.pending_inputs(&session)?.is_empty() && inbox.pending_inputs(&other)?.is_empty());
    assert!(claim(&inbox, &session)?.source_messages().next().is_some());
    Ok(())
}

#[test]
fn seat03_admission_cannot_replace_the_frozen_source_claim_metadata() -> TestResult {
    let (mut inbox, session) = fixture()?;
    mail(&mut inbox, "first", MailAttention::Wake, true)?;
    let frozen = claim(&inbox, &session)?;
    let event = "evt_native_metadata".parse()?;
    let input = "msg_native_metadata".parse()?;
    let metadata = serde_json::json!({"native_mail_wake":1,"messages":[]});
    assert!(
        inbox
            .admit_native_mail_wake(
                &frozen,
                SyntheticAdmitRequest {
                    event_id: &event,
                    session: &session,
                    input: &input,
                    text: "first",
                    description: None,
                    metadata: Some(&metadata),
                    delivery: DeliveryMode::Queue,
                    resume: true,
                    created_ms: 1,
                }
            )
            .is_err(),
        "admission rewrote its frozen source provenance"
    );
    assert!(inbox.pending_inputs(&session)?.is_empty());
    assert!(claim(&inbox, &session)?.source_messages().next().is_some());
    Ok(())
}

#[test]
fn seat03_strict_replay_keeps_a_cancelled_wake_pending() -> TestResult {
    let (mut inbox, session) = fixture()?;
    mail(&mut inbox, "first", MailAttention::Wake, true)?;
    let frozen = claim(&inbox, &session)?;
    admit(&mut inbox, &frozen, "held_replay")?;
    inbox.hold_native_mail_wakes(&session, 2)?;
    let mut query = inbox.store().connection().prepare(
        "SELECT seq,type,data FROM event WHERE aggregate_family='session'
         AND aggregate_id=?1 AND type IN
         ('session.input.admitted.1','session.input.native-wake-held.1')
         ORDER BY seq LIMIT 3",
    )?;
    let mut rows = query.query([session.as_str()])?;
    let mut fold = grapt_inbox::InboxFold::default();
    let mut count = 0;
    while let Some(row) = rows.next()? {
        let seq = u64::try_from(row.get::<_, i64>(0)?)?;
        let kind: String = row.get(1)?;
        let raw: String = row.get(2)?;
        let data = serde_json::from_str(&raw)?;
        if let Some(event) = grapt_inbox::InboxEvent::decode(
            kind.strip_suffix(".1").ok_or("stored kind version")?,
            1,
            &data,
        )? {
            fold.apply(event, seq);
        }
        count += 1;
    }
    assert_eq!(count, 2, "admission and hold did not both reach replay");
    assert_eq!(
        fold.pending.len(),
        1,
        "hold consumed the pending replay row"
    );
    assert!(fold.pending.contains_key(&"msg_held_replay".parse()?));
    assert!(fold.violations.is_empty());
    assert!(
        grapt_inbox::InboxEvent::decode(
            "session.input.native-wake-held",
            1,
            &serde_json::json!({"sessionID":"ses_native_wake","inputID":"invalid"})
        )
        .is_err(),
        "native hold replay accepted an invalid identity"
    );
    assert!(
        grapt_inbox::InboxEvent::decode(
            "session.input.native-wake-held",
            2,
            &serde_json::json!({"sessionID":"ses_native_wake","inputID":"msg_held_replay"})
        )
        .is_err(),
        "native hold replay accepted an unsupported version"
    );
    Ok(())
}

#[path = "native_mail_wake/state.rs"]
mod state;

#[path = "native_mail_wake/boundary.rs"]
mod boundary;
