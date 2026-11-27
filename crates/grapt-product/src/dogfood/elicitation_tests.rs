use super::super::mcp::channel::ServerRequestHandler;
use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn startup_suspends_an_orphaned_durable_form_without_recreating_its_wait() -> TestResult {
    let producer = producer()?;
    let mut live = producer.connect("/elicitation-witness", "greeter")?;
    let request = request()?;
    let mut wait = live.answer(&request, None);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(1), &mut wait)
            .await
            .is_err()
    );
    drop(wait);
    // Discard volatile ownership without a finalizer, leaving the durable
    // input the startup reconciler must consume.
    let id = live.pending.take().ok_or("no pending form to orphan")?;
    drop(live);
    assert_eq!(
        global_form(producer.lock().store(), &id)?
            .ok_or("form")?
            .state,
        FormState::Pending
    );
    recover(
        producer.lock().store_mut(),
        producer.ids.as_ref(),
        producer.clock.as_ref(),
    )?;
    assert_eq!(
        global_form(producer.lock().store(), &id)?
            .ok_or("form")?
            .state,
        FormState::Suspended(FormSuspensionReason::ProcessLoss)
    );
    let next = producer.connect("/elicitation-witness", "greeter")?;
    assert!(next.pending.is_none());
    Ok(())
}

/// The database failing under a form's settlement fails startup instead of
/// leaving the form unsettled behind a warning.
#[tokio::test]
async fn a_database_failure_settling_a_lost_form_fails_startup() -> TestResult {
    let path = std::env::temp_dir().join(format!(
        "grapt-form-storage-failure-{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let producer = producer_on(grapt_store::EventStore::open(&path, "form-storage")?)?;
    let mut live = producer.connect("/elicitation-witness", "greeter")?;
    let request = request()?;
    let mut wait = live.answer(&request, None);
    let _pending = tokio::time::timeout(std::time::Duration::from_millis(1), &mut wait).await;
    drop(wait);
    let id = live.pending.take().ok_or("no pending form to orphan")?;
    drop(live);
    // An engine error on the settling UPDATE, from outside the store.
    rusqlite::Connection::open(&path)?.execute_batch(
        "CREATE TRIGGER fail_form_settlement BEFORE UPDATE ON form_request
         BEGIN SELECT * FROM absent_table; END;",
    )?;
    let result = recover(
        producer.lock().store_mut(),
        producer.ids.as_ref(),
        producer.clock.as_ref(),
    );
    assert!(
        result.is_err(),
        "startup passed over a database failure: {result:?}"
    );
    assert_eq!(
        global_form(producer.lock().store(), &id)?
            .ok_or("form")?
            .state,
        FormState::Pending
    );
    drop(producer);
    std::fs::remove_file(&path)?;
    Ok(())
}

fn producer() -> Result<FormProducer, Box<dyn std::error::Error>> {
    producer_on(grapt_store::EventStore::open_in_memory()?)
}

fn producer_on(
    mut store: grapt_store::EventStore,
) -> Result<FormProducer, Box<dyn std::error::Error>> {
    FormService::initialize(&mut store)?;
    Ok(FormProducer {
        hooks: None,
        inbox: Arc::new(Mutex::new(Inbox::new(store)?)),
        ids: Arc::new(OsRandomIds::seed()?),
        clock: Arc::new(SystemClock),
        hub: super::super::event_hub()?,
    })
}

fn request() -> Result<grapt_mcp::incoming::ServerRequest, Box<dyn std::error::Error>> {
    grapt_mcp::incoming::server_request(grapt_mcp::McpBounds::conservative()?,
        br#"{"jsonrpc":"2.0","id":"ask-name","method":"elicitation/create","params":{"message":"Name for the greeting","requestedSchema":{"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}}}"#)?
        .ok_or_else(|| "request missing".into())
}

#[tokio::test]
async fn durable_reply_and_cancel_resume_only_the_live_wait() -> TestResult {
    for cancel in [false, true] {
        let producer = producer()?;
        let mut live = producer.connect("/elicitation-witness", "greeter")?;
        let request = request()?;
        let mut wait = live.answer(&request, None);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(1), &mut wait)
                .await
                .is_err()
        );
        let rows = grapt_form::global_catalog::pending_global_forms(producer.lock().store())?;
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.info["sessionID"], "global");
        let event = producer.event()?;
        let outcome = if cancel {
            FormService::cancel_global_on(
                producer.lock().store_mut(),
                &event,
                &row.id,
                &row.binding,
                1,
            )?
        } else {
            FormService::answer_global_on(
                producer.lock().store_mut(),
                &event,
                &row.id,
                &row.binding,
                &json!({"name":"Ada"}),
                1,
            )?
        };
        assert_eq!(outcome, grapt_types::CasOutcome::Applied);
        let answer = tokio::time::timeout(std::time::Duration::from_secs(1), wait).await??;
        assert_eq!(answer["action"], if cancel { "cancel" } else { "accept" });
        drop(live);
        assert!(!matches!(
            global_form(producer.lock().store(), &row.id)?
                .ok_or("form")?
                .state,
            FormState::Suspended(_)
        ));
    }
    Ok(())
}

#[tokio::test]
async fn dropping_the_live_call_suspends_without_inventing_a_user_cancel() -> TestResult {
    let producer = producer()?;
    let mut live = producer.connect("/elicitation-witness", "greeter")?;
    let request = request()?;
    let mut wait = live.answer(&request, None);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(1), &mut wait)
            .await
            .is_err()
    );
    drop(wait);
    drop(live);
    let rows = grapt_form::global_catalog::pending_global_forms(producer.lock().store())?;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].state,
        FormState::Suspended(FormSuspensionReason::Shutdown)
    );
    let event = producer.event()?;
    assert_eq!(
        FormService::answer_global_on(
            producer.lock().store_mut(),
            &event,
            &rows[0].id,
            &rows[0].binding,
            &json!({"name":"late"}),
            2
        )?,
        grapt_types::CasOutcome::Applied
    );
    let replacement = producer.connect("/elicitation-witness", "greeter")?;
    assert_ne!(replacement.connection, rows[0].binding.connection);
    assert!(replacement.pending.is_none());
    Ok(())
}

#[tokio::test]
async fn url_consent_uses_global_ownership_and_distinct_content_free_actions() -> TestResult {
    let bounds = grapt_mcp::McpBounds::conservative()?;
    let frame = br#"{"jsonrpc":"2.0","id":"rpc-7","method":"elicitation/create","params":{"mode":"url","message":"Connect manually","url":"https://example.test/connect?private=token","elicitationId":"external-9"}}"#;
    let request = grapt_mcp::incoming::server_request(bounds, frame)?.ok_or("request")?;
    for decision in ["accept", "decline", "cancel"] {
        let producer = producer()?;
        let mut live = producer.connect("/elicitation-witness", "greeter")?;
        live.protocol("2025-11-25");
        let mut wait = live.answer(&request, None);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(1), &mut wait)
                .await
                .is_err()
        );
        let row = grapt_form::global_catalog::pending_global_forms(producer.lock().store())?
            .into_iter()
            .next()
            .ok_or("pending URL form")?;
        assert!(row.binding.call.is_none());
        assert_ne!(row.binding.elicitation.as_str(), "external-9");
        assert_eq!(row.info["metadata"]["elicitationId"], "external-9");
        let event = producer.event()?;
        if decision == "cancel" {
            FormService::cancel_global_on(
                producer.lock().store_mut(),
                &event,
                &row.id,
                &row.binding,
                1,
            )?;
        } else {
            FormService::answer_global_on(
                producer.lock().store_mut(),
                &event,
                &row.id,
                &row.binding,
                &json!({"decision":decision}),
                1,
            )?;
        }
        let answer = tokio::time::timeout(std::time::Duration::from_secs(1), wait).await??;
        assert_eq!(answer, json!({"action":decision}));
        let wire: Value = serde_json::from_slice(&request.result(bounds, answer)?)?;
        assert_eq!(wire["id"], "rpc-7");
        drop(live);
        assert!(!matches!(
            global_form(producer.lock().store(), &row.id)?
                .ok_or("form")?
                .state,
            FormState::Suspended(_)
        ));
    }
    let producer = producer()?;
    let mut live = producer.connect("/elicitation-witness", "greeter")?;
    live.protocol("2025-06-18");
    assert!(live.answer(&request, None).await.is_err());
    assert!(grapt_form::global_catalog::pending_global_forms(producer.lock().store())?.is_empty());
    let mut startup = producer.connect_startup("/elicitation-witness", "greeter")?;
    assert!(startup.answer(&request, None).await.is_err());
    assert!(grapt_form::global_catalog::pending_global_forms(producer.lock().store())?.is_empty());
    Ok(())
}
