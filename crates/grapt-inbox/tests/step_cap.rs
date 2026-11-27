use grapt_inbox::{AdmitOutcome, AdmitRequest, Inbox, PromoteScope, UserInputOptions};
use grapt_store::{AggregateId, EventStore};
use grapt_types::{DeliveryMode, EventId, MessageId, SessionId};
use std::num::NonZeroU32;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn step_cap_commits_with_the_prompt_survives_restart_and_cannot_be_erased_by_retry() -> Result {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _removed = std::fs::remove_dir_all(&self.0);
        }
    }
    let root = Scratch(std::env::temp_dir().join(format!(
        "step-cap-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )));
    std::fs::create_dir(&root.0)?;
    let path = root.0.join("inbox.sqlite");
    let session: SessionId = "ses_step_cap".parse()?;
    let input: MessageId = "msg_step_cap".parse()?;
    let event: EventId = "evt_step_cap".parse()?;
    let request = || AdmitRequest {
        event_id: &event,
        session: &session,
        input: &input,
        text: "go",
        delivery: DeliveryMode::Queue,
        resume: false,
        created_ms: 1,
    };
    let images = [grapt_types::ArtifactRef::new(
        grapt_types::ArtifactId::new("cap-image").ok_or("image identity")?,
        12,
        grapt_types::ArtifactDigest::new("cap-image-digest").ok_or("image digest")?,
    )];
    let options = || UserInputOptions {
        images: &images,
        origin: grapt_inbox::UserInputOrigin::SeatStartup,
        max_steps: NonZeroU32::new(2),
    };
    let mut inbox = Inbox::new(EventStore::open(&path, "step-cap")?)?;
    assert!(
        inbox.admit_with_options(request(), options()).is_err(),
        "a lost writer cannot admit the input or its cap"
    );
    assert!(inbox.pending_inputs(&session)?.is_empty());
    inbox
        .store_mut()
        .elect_writer(&AggregateId::Session(session.clone()))?;
    inbox.admit_with_options(request(), options())?;
    drop(inbox); // committed before its reply was observed
    let mut store = EventStore::open(&path, "step-cap-restart")?;
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    let mut inbox = Inbox::new(store)?;
    assert_eq!(
        grapt_inbox::admission::options::max_steps(
            inbox.pending_inputs(&session)?[0].metadata.as_ref()
        )?,
        NonZeroU32::new(2)
    );
    assert!(inbox.admit(request()).is_err());
    assert!(inbox
        .admit_with_options(
            request(),
            UserInputOptions {
                max_steps: NonZeroU32::new(3),
                ..options()
            }
        )
        .is_err());
    assert!(matches!(
        inbox.admit_with_options(request(), options())?,
        AdmitOutcome::Reconciled { .. }
    ));
    let promoted: EventId = "evt_step_cap_promoted".parse()?;
    inbox.promote_next(
        &session,
        PromoteScope::InputAtIdle,
        &mut || promoted.clone(),
        2,
    )?;
    assert!(inbox.admit(request()).is_err());
    drop(inbox);
    let recovered = Inbox::new(EventStore::open(&path, "step-cap-promoted")?)?;
    let rows = recovered.promoted_messages(&session, 10)?;
    assert_eq!(
        grapt_inbox::admission::options::max_steps(rows[0].metadata.as_ref())?,
        NonZeroU32::new(2)
    );
    assert_eq!(
        grapt_inbox::user_image_references(rows[0].metadata.as_ref())?,
        images
    );
    assert_eq!(
        rows[0]
            .metadata
            .as_ref()
            .and_then(|value| value.get("origin")),
        Some(&serde_json::json!("seat-startup"))
    );
    for invalid in [
        serde_json::json!(0),
        serde_json::json!(-1),
        serde_json::json!(1.5),
        serde_json::json!(null),
        serde_json::json!(4294967296_u64),
    ] {
        assert!(grapt_inbox::admission::options::max_steps(Some(
            &serde_json::json!({"maxSteps":invalid})
        ))
        .is_err());
    }
    Ok(())
}
