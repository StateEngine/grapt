//! Retained native wake provenance must fit the public history contract.
use super::*;
use serde_json::json;

#[tokio::test]
async fn retained_native_wake_history_survives_a_full_delivery_batch() -> TestResult {
    let (_root, application, _driver, session) = fixture(None).await?;
    let messages: Vec<_> = (0..64)
        .map(|n| {
            json!({
                "local_message":format!("mailbox-{n}"), "source_message":format!("source-{n}"),
                "local_admission":n+1, "digest":"a".repeat(64)
            })
        })
        .collect();
    let metadata = json!({"native_mail_wake":1,"binding":"fixture-binding","recipient":"coder","messages":messages});
    let input: grapt_types::MessageId = "msg_retained_native_history".parse()?;
    let text =
        "Native inbox message IDs: source-0 through source-63. Read the authenticated inbox.";
    application.ensure_session_writer(&session)?;
    let admitted = application.mint_event()?;
    let promoted = application.mint_event()?;
    {
        let mut inbox = application.lock_inbox();
        inbox.admit_synthetic(grapt_inbox::SyntheticAdmitRequest {
            event_id: &admitted,
            session: &session,
            input: &input,
            text,
            description: Some("Native inbox wake"),
            metadata: Some(&metadata),
            delivery: grapt_types::DeliveryMode::Queue,
            resume: true,
            created_ms: 1,
        })?;
        inbox
            .promote_next(
                &session,
                grapt_inbox::PromoteScope::InputAtIdle,
                &mut || promoted.clone(),
                2,
            )?
            .ok_or("native wake was not promoted")?;
    }
    let page = crate::dogfood::history::MessagePage::parse(crate::dogfood::decode(
        json!({"order":"asc","limit":"20"}),
    )?)
    .map_err(|e| format!("{e:?}"))?;
    let raw = application
        .history_json(&session, &page, grapt_api::u3a::MessageListView::Durable)
        .map_err(|e| format!("retained native history refused: {e:?}"))?;
    let public: grapt_api::u3a::SessionMessagesResponseDto = serde_json::from_str(&raw)?;
    let public = serde_json::to_value(public)?;
    assert_eq!(public["data"][0]["text"], text);
    assert_eq!(public["data"][0]["metadata"]["messageCount"], 64);
    assert_eq!(
        public["data"][0]["metadata"]["provenanceProjection"],
        "summary"
    );
    let retained = application.lock_inbox().promoted_input(&session, &input)?;
    let retained = application
        .lock_inbox()
        .promoted_message_at(&session, grapt_types::EventSeq::new(retained.seq))?
        .ok_or("retained metadata")?;
    assert_eq!(
        retained.metadata,
        Some(metadata),
        "history changed durable provenance"
    );
    Ok(())
}
