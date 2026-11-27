//! Presentation provenance commits, retries and promotes under the existing owner.
use grapt_inbox::{AdmitOutcome, AdmitRequest, Inbox, PromoteScope, UserInputOrigin};
use grapt_store::{AggregateId, EventStore};
use grapt_types::{DeliveryMode, EventId, MessageId, SessionId};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[test]
fn startup_origin_is_atomic_and_cannot_change_on_retry_before_or_after_promotion() -> Result {
    let session: SessionId = "ses_startup_origin".parse()?;
    let input: MessageId = "msg_startup_origin".parse()?;
    let event: EventId = "evt_startup_origin".parse()?;
    let mut store = EventStore::open_in_memory()?;
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    let mut inbox = Inbox::new(store)?;
    let request = || AdmitRequest {
        event_id: &event,
        session: &session,
        input: &input,
        text: "PRIVATE_SEAT_PACKET",
        delivery: DeliveryMode::Queue,
        resume: false,
        created_ms: 1,
    };
    assert!(matches!(
        inbox.admit_with_origin(request(), &[], UserInputOrigin::SeatStartup)?,
        AdmitOutcome::Admitted { .. }
    ));
    let pending = inbox.pending_inputs(&session)?;
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].metadata,
        Some(serde_json::json!({"origin":"seat-startup"}))
    );
    assert!(
        inbox.admit(request()).is_err(),
        "a human retry cannot erase startup provenance"
    );
    assert!(matches!(
        inbox.admit_with_origin(request(), &[], UserInputOrigin::SeatStartup)?,
        AdmitOutcome::Reconciled { .. }
    ));
    let promoted: EventId = "evt_startup_promoted".parse()?;
    inbox.promote_next(
        &session,
        PromoteScope::InputAtIdle,
        &mut || promoted.clone(),
        2,
    )?;
    let rows = inbox.promoted_messages(&session, 10)?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].metadata, pending[0].metadata);
    assert!(
        inbox.admit(request()).is_err(),
        "promoted retry cannot erase startup provenance"
    );
    assert!(matches!(
        inbox.admit_with_origin(request(), &[], UserInputOrigin::SeatStartup)?,
        AdmitOutcome::Reconciled { .. }
    ));
    Ok(())
}

#[test]
fn lost_writer_refuses_the_origin_and_prompt_together() -> Result {
    let session: SessionId = "ses_startup_refusal".parse()?;
    let input: MessageId = "msg_startup_refusal".parse()?;
    let event: EventId = "evt_startup_refusal".parse()?;
    let mut inbox = Inbox::new(EventStore::open_in_memory()?)?;
    assert!(inbox
        .admit_with_origin(
            AdmitRequest {
                event_id: &event,
                session: &session,
                input: &input,
                text: "packet",
                delivery: DeliveryMode::Queue,
                resume: false,
                created_ms: 1
            },
            &[],
            UserInputOrigin::SeatStartup
        )
        .is_err());
    assert!(inbox.pending_inputs(&session)?.is_empty());
    Ok(())
}

#[test]
fn committed_startup_survives_lost_reply_and_restart_without_becoming_human() -> Result {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _removed = std::fs::remove_dir_all(&self.0);
        }
    }
    let root = Scratch(std::env::temp_dir().join(format!(
        "grapt-startup-origin-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )));
    std::fs::create_dir(&root.0)?;
    let path = root.0.join("inbox.sqlite");
    let session: SessionId = "ses_startup_restart".parse()?;
    let input: MessageId = "msg_startup_restart".parse()?;
    let event: EventId = "evt_startup_restart".parse()?;
    let request = || AdmitRequest {
        event_id: &event,
        session: &session,
        input: &input,
        text: "PRIVATE_SEAT_PACKET",
        delivery: DeliveryMode::Queue,
        resume: false,
        created_ms: 1,
    };
    let mut store = EventStore::open(&path, "startup-origin")?;
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    let mut inbox = Inbox::new(store)?;
    inbox.admit_with_origin(request(), &[], UserInputOrigin::SeatStartup)?;
    drop(inbox); // Admission committed but its reply/execution never arrived.
    let mut store = EventStore::open(&path, "startup-origin-restart")?;
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    let mut recovered = Inbox::new(store)?;
    assert!(matches!(
        recovered.admit_with_origin(request(), &[], UserInputOrigin::SeatStartup)?,
        AdmitOutcome::Reconciled { .. }
    ));
    assert!(recovered.admit(request()).is_err());
    let promoted: EventId = "evt_startup_restart_promoted".parse()?;
    recovered.promote_next(
        &session,
        PromoteScope::InputAtIdle,
        &mut || promoted.clone(),
        2,
    )?;
    drop(recovered);
    let recovered = Inbox::new(EventStore::open(&path, "startup-promoted-restart")?)?;
    let rows = recovered.promoted_messages(&session, 10)?;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].metadata,
        Some(serde_json::json!({"origin":"seat-startup"}))
    );
    assert!(recovered.pending_inputs(&session)?.is_empty());
    Ok(())
}
