//! Image references have the same atomic admission and exact-retry owner as text.
use grapt_inbox::{AdmitOutcome, AdmitRequest, Inbox, PromoteScope};
use grapt_store::{AggregateId, EventStore};
use grapt_types::{
    ArtifactDigest, ArtifactId, ArtifactRef, DeliveryMode, EventId, MessageId, SessionId,
};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
fn image(name: &str) -> Result<ArtifactRef> {
    Ok(ArtifactRef::new(
        ArtifactId::new(name).ok_or("artifact id")?,
        12,
        ArtifactDigest::new(format!("digest-{name}")).ok_or("digest")?,
    ))
}
#[test]
fn user_images_commit_and_promote_with_the_input_and_exact_retries_keep_the_same_references(
) -> Result {
    let session: SessionId = "ses_images".parse()?;
    let input: MessageId = "msg_images".parse()?;
    let event: EventId = "evt_images".parse()?;
    let mut store = EventStore::open_in_memory()?;
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    let mut inbox = Inbox::new(store)?;
    let images = [image("pixels-a")?];
    let request = || AdmitRequest {
        event_id: &event,
        session: &session,
        input: &input,
        text: "describe",
        delivery: DeliveryMode::Queue,
        resume: false,
        created_ms: 1,
    };
    assert!(matches!(
        inbox.admit_with_images(request(), &images)?,
        AdmitOutcome::Admitted { .. }
    ));
    let pending = inbox.pending_inputs(&session)?;
    assert_eq!(pending.len(), 1);
    assert_eq!(
        grapt_inbox::user_image_references(pending[0].metadata.as_ref())?,
        images
    );
    assert!(matches!(
        inbox.admit_with_images(request(), &images)?,
        AdmitOutcome::Reconciled { .. }
    ));
    assert!(inbox
        .admit_with_images(request(), &[image("different")?])
        .is_err());
    assert!(
        inbox.admit(request()).is_err(),
        "a retry cannot silently drop its original image"
    );
    let promoted: EventId = "evt_images_promoted".parse()?;
    inbox.promote_next(
        &session,
        PromoteScope::InputAtIdle,
        &mut || promoted.clone(),
        2,
    )?;
    let rows = inbox.promoted_messages(&session, 10)?;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        grapt_inbox::user_image_references(rows[0].metadata.as_ref())?,
        images
    );
    assert!(matches!(
        inbox.admit_with_images(request(), &images)?,
        AdmitOutcome::Reconciled { .. }
    ));
    assert!(inbox
        .admit_with_images(request(), &[image("different")?])
        .is_err());
    Ok(())
}
#[test]
fn lost_writer_refuses_pixels_and_leaves_no_pending_projection() -> Result {
    let session: SessionId = "ses_image_refusal".parse()?;
    let input: MessageId = "msg_image_refusal".parse()?;
    let event: EventId = "evt_image_refusal".parse()?;
    let mut inbox = Inbox::new(EventStore::open_in_memory()?)?;
    assert!(inbox
        .admit_with_images(
            AdmitRequest {
                event_id: &event,
                session: &session,
                input: &input,
                text: "describe",
                delivery: DeliveryMode::Queue,
                resume: false,
                created_ms: 1
            },
            &[image("private-pixels")?]
        )
        .is_err());
    assert!(inbox.pending_inputs(&session)?.is_empty());
    Ok(())
}
