//! Synthetic admission shares the live connection carrying interactive asks.
use super::*;
use grapt_inbox::SyntheticAdmitRequest;
use grapt_store::{EventStore, OsRandomIds, SystemClock};
use grapt_types::{DeliveryMode, DeterministicAuthorities, ProjectId};

#[test]
fn a_synthetic_input_does_not_disconnect_other_sessions_live_subscribers(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut store = EventStore::open_in_memory()?;
    let ids = OsRandomIds::seed()?;
    let clock = SystemClock;
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let seed = grapt_session::SessionSeed::new(
        ProjectId::new("prj_synthetic_stream").ok_or("project")?,
        "/work/synthetic-stream",
        "synthetic stream",
    )?;
    let session = grapt_session::create_session(&mut store, &authorities, &seed, &mut [])?.session;
    let mut inbox = Inbox::new(store)?;
    let event = "evt_synthetic_stream".parse()?;
    let input = "msg_synthetic_stream".parse()?;
    let metadata = serde_json::json!({"source":"native-mail"});
    let admitted = inbox.admit_synthetic(SyntheticAdmitRequest {
        event_id: &event,
        session: &session,
        input: &input,
        text: "Read the newly delivered mail",
        description: Some("New mail"),
        metadata: Some(&metadata),
        delivery: DeliveryMode::Steer,
        resume: false,
        created_ms: 1,
    })?;
    let grapt_inbox::AdmitOutcome::Admitted { seq, .. } = admitted else {
        return Err("synthetic admission did not commit".into());
    };
    let hub = crate::dogfood::event_hub()?;
    let subscriber = hub.subscribe_host()?;
    let _connected = subscriber
        .next_frame()?
        .ok_or("connection was not opened")?;
    publish(&inbox, &hub, &session, seq);
    assert!(
        subscriber.next_frame()?.is_some(),
        "publishing synthetic input closed the live connection used by other Sessions"
    );
    Ok(())
}
