//! Public pending and admitted-event reads preserve large native provenance.
use super::*;
use serde_json::{json, Value};

type Prepared = (
    Root,
    Arc<ServerApplication>,
    SessionId,
    grapt_types::MessageId,
    u64,
);

fn metadata(native: bool, count: usize) -> Value {
    let messages: Vec<_> = (0..count)
        .map(|n| {
            json!({
                "local_message":format!("mailbox-{n}"),
                "source_message":format!("source-{n}"),
                "local_admission":n+1,
                "digest":"a".repeat(64)
            })
        })
        .collect();
    let mut value = json!({"messages":messages,"origin":"input-contract"});
    if native {
        value["native_mail_wake"] = json!(1);
    }
    value
}

async fn prepare(metadata: &Value) -> Result<Prepared, Box<dyn std::error::Error>> {
    let (root, application, _driver, session) = fixture(None).await?;
    let input = "msg_native_public_contract".parse()?;
    let event = application.mint_event()?;
    application.ensure_session_writer(&session)?;
    let outcome = application
        .lock_inbox()
        .admit_synthetic(grapt_inbox::SyntheticAdmitRequest {
            event_id: &event,
            session: &session,
            input: &input,
            text: "Read the authenticated inbox",
            description: Some("Native inbox wake"),
            metadata: Some(metadata),
            delivery: grapt_types::DeliveryMode::Queue,
            resume: true,
            created_ms: 1,
        })?;
    let grapt_inbox::AdmitOutcome::Admitted { seq, .. } = outcome else {
        return Err("fixture input was not admitted".into());
    };
    Ok((root, application, session, input, seq))
}

fn durable_data(
    application: &ServerApplication,
    session: &SessionId,
    seq: u64,
) -> Result<String, Box<dyn std::error::Error>> {
    let inbox = application.lock_inbox();
    Ok(inbox.store().connection().query_row(
        "SELECT data FROM event WHERE aggregate_family='session' AND aggregate_id=?1 AND seq=?2",
        (session.as_str(), i64::try_from(seq)?),
        |row| row.get(0),
    )?)
}

#[tokio::test]
async fn public_pending_read_survives_a_full_native_batch_without_changing_provenance() -> TestResult
{
    let metadata = metadata(true, 64);
    let (_root, application, session, input, seq) = prepare(&metadata).await?;
    let before = durable_data(&application, &session, seq)?;
    let pending = application
        .pending_list(crate::dogfood::decode(json!(session.as_str()))?)
        .map_err(|error| format!("pending native input unreadable: {error:?}"))?;
    let encoded = serde_json::to_vec(&pending)?;
    let decoded: grapt_api::u3a::SessionPendingListResponseDto = serde_json::from_slice(&encoded)?;
    let value = serde_json::to_value(decoded)?;
    assert_eq!(value["data"].as_array().ok_or("pending rows")?.len(), 1);
    assert_eq!(value["data"][0]["id"], input.as_str());
    assert_eq!(value["data"][0]["data"]["metadata"]["messageCount"], 64);
    assert_eq!(durable_data(&application, &session, seq)?, before);
    let retained = application.lock_inbox().pending_inputs(&session)?;
    assert_eq!(
        retained.first().ok_or("retained pending input")?.metadata,
        Some(metadata)
    );
    Ok(())
}

#[tokio::test]
async fn public_admitted_event_survives_a_full_native_batch_without_changing_provenance(
) -> TestResult {
    let metadata = metadata(true, 64);
    let (_root, application, session, _input, seq) = prepare(&metadata).await?;
    let before = durable_data(&application, &session, seq)?;
    let event = crate::dogfood::input_events::project(&application.lock_inbox(), &session, seq)
        .map_err(|error| format!("committed native input unreadable: {error}"))?;
    assert!(matches!(
        &event,
        grapt_api::u3a::DogfoodEvent::InputAdmitted(_)
    ));
    let encoded = serde_json::to_vec(&event)?;
    #[derive(serde::Deserialize)]
    struct EnvelopeData {
        data: Box<serde_json::value::RawValue>,
    }
    let envelope: EnvelopeData = serde_json::from_slice(&encoded)?;
    let _decoded: grapt_api::u3a::InputAdmittedData = serde_json::from_str(envelope.data.get())?;
    crate::dogfood::event_hub()?.publish(&event)?;
    assert_eq!(durable_data(&application, &session, seq)?, before);
    let retained = application.lock_inbox().pending_inputs(&session)?;
    assert_eq!(
        retained.first().ok_or("retained pending input")?.metadata,
        Some(metadata)
    );
    Ok(())
}

#[tokio::test]
async fn public_input_metadata_keeps_valid_native_and_unrelated_values_exact() -> TestResult {
    for native in [false, true] {
        let metadata = metadata(native, 1);
        let (_root, application, session, _input, seq) = prepare(&metadata).await?;
        let pending = application
            .pending_list(crate::dogfood::decode(json!(session.as_str()))?)
            .map_err(|error| format!("small pending metadata unreadable: {error:?}"))?;
        let value = serde_json::to_value(pending)?;
        assert_eq!(value["data"][0]["data"]["metadata"], metadata);
        let event =
            crate::dogfood::input_events::project(&application.lock_inbox(), &session, seq)?;
        let value = serde_json::to_value(event)?;
        assert_eq!(value["data"]["input"]["data"]["metadata"], metadata);
    }
    Ok(())
}

#[tokio::test]
async fn public_input_metadata_does_not_relax_unrelated_metadata_limits() -> TestResult {
    let metadata = metadata(false, 64);
    let (_root, application, session, _input, seq) = prepare(&metadata).await?;
    let before = durable_data(&application, &session, seq)?;
    assert!(application
        .pending_list(crate::dogfood::decode(json!(session.as_str()))?)
        .is_err());
    assert!(
        crate::dogfood::input_events::project(&application.lock_inbox(), &session, seq).is_err()
    );
    assert_eq!(durable_data(&application, &session, seq)?, before);
    Ok(())
}
