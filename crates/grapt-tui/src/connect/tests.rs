use super::*;
use std::sync::mpsc;
type TestResult = Result<(), Box<dyn std::error::Error>>;

fn controller() -> (Connect, mpsc::Receiver<Request>, mpsc::SyncSender<Reply>) {
    let (requests, incoming) = mpsc::sync_channel(2);
    let (outgoing, replies) = mpsc::sync_channel(4);
    (
        Connect {
            worker: Some(Worker { requests, replies }),
            first: true,
            ..Default::default()
        },
        incoming,
        outgoing,
    )
}
fn providers() -> Result<Vec<IntegrationInfoDto>, serde_json::Error> {
    serde_json::from_value(serde_json::json!([
        {"id":"openai","name":"OpenAI","methods":[{"type":"oauth","id":"chatgpt-browser","label":"ChatGPT browser"},{"type":"oauth","id":"chatgpt-device","label":"ChatGPT device"},{"type":"key"}],"connections":[]},
        {"id":"anthropic","name":"Anthropic","methods":[{"type":"key"},{"type":"oauth","id":"forbidden-subscription","label":"Subscription"}],"connections":[]}
    ]))
}
fn press(connect: &mut Connect, key: KeyCode) {
    connect.key(KeyEvent::new(key, KeyModifiers::NONE));
}

#[test]
fn fresh_profile_accepts_keys_in_the_dialog_and_existing_connections_skip_it() -> TestResult {
    let (connect, incoming, outgoing) = controller();
    let mut ui = crate::ui::Ui::new(false);
    ui.connect = connect;
    ui.composer.insert_str("keep my draft");
    outgoing.send(Reply::Catalog(providers()?, false))?;
    assert_eq!(ui.connect.poll(), (true, false));
    assert!(ui.connect.visible());
    ui.connect.paste("Anthropic");
    press(&mut ui.connect, KeyCode::Enter);
    assert!(ui
        .connect
        .pane
        .as_ref()
        .ok_or("pane")?
        .choices()
        .iter()
        .all(|c| !matches!(c, Choice::OAuth(_))));
    crate::keys::paste("test-private-key\n", &mut ui);
    assert_eq!(crate::keys::draft(&ui), "keep my draft");
    press(&mut ui.connect, KeyCode::Tab);
    ui.connect.paste("Work");
    press(&mut ui.connect, KeyCode::Enter);
    let Request::Key(id, body) = incoming.try_recv()? else {
        return Err("expected key connect".into());
    };
    assert_eq!(id.as_str(), "anthropic");
    assert_eq!(body.key.as_str(), "test-private-key");
    assert_eq!(body.label.as_present().map(|s| s.as_str()), Some("Work"));
    assert!(!format!("{body:?}").contains("test-private-key"));
    assert_eq!(crate::keys::draft(&ui), "keep my draft");
    outgoing.send(Reply::Catalog(providers()?, true))?;
    assert_eq!(ui.connect.poll(), (true, true));
    let (mut connected, existing_requests, reply) = controller();
    let mut rows = providers()?;
    let mut row = serde_json::to_value(&rows[0])?;
    row["connections"] =
        serde_json::json!([{"type":"credential","id":"cred_existing","label":"Saved"}]);
    rows[0] = serde_json::from_value(row)?;
    reply.send(Reply::Catalog(rows, false))?;
    connected.poll();
    assert!(!connected.visible());
    ui.connect = connected;
    ui.directory = Some("/work/current".into());
    let (commands, _requests) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _wakeups) = mpsc::sync_channel(1);
    let backend = crate::backend::Backend {
        commands,
        updates,
        wake,
    };
    crate::handle_key(
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
        &mut ui,
        &backend,
    )?;
    assert!(ui.connect.visible());
    assert!(
        matches!(existing_requests.try_recv()?, Request::List(Some(directory)) if directory == "/work/current")
    );
    assert_eq!(crate::keys::draft(&ui), "keep my draft");
    Ok(())
}

#[test]
fn escape_cancels_a_late_oauth_start_and_failed_cancellation_remains_visible() -> TestResult {
    let (mut connect, incoming, outgoing) = controller();
    outgoing.send(Reply::Catalog(providers()?, false))?;
    connect.poll();
    press(&mut connect, KeyCode::Enter);
    press(&mut connect, KeyCode::Enter);
    assert!(matches!(incoming.try_recv()?, Request::OAuth(_, _)));
    press(&mut connect, KeyCode::Esc);
    outgoing.send(Reply::Attempt(serde_json::from_value(serde_json::json!({
        "attemptID":"attempt-one","url":"https://example.test/sign-in","instructions":"Sign in", "mode":"auto","time":{"created":1,"expires":100}
    }))?))?;
    connect.poll();
    assert!(matches!(incoming.try_recv()?, Request::Cancel));
    outgoing.send(Reply::Failed("Cancellation not confirmed".into()))?;
    connect.poll();
    assert!(connect.visible());
    // Failure cannot trap terminal input; reopening retains the attempt.
    press(&mut connect, KeyCode::Esc);
    assert!(!connect.visible());
    assert!(connect
        .notice
        .as_ref()
        .is_some_and(|s| s.contains("not confirmed")));
    connect.open(None);
    assert!(connect.visible());
    press(&mut connect, KeyCode::Esc);
    assert!(matches!(incoming.try_recv()?, Request::Cancel));
    outgoing.send(Reply::Cancelled)?;
    assert_eq!(connect.poll(), (true, true));
    assert!(!connect.visible());
    Ok(())
}

#[test]
fn aws_chain_retry_retains_the_explicit_profile_after_refusal() -> TestResult {
    let (mut connect, incoming, outgoing) = controller();
    let providers = serde_json::from_value(serde_json::json!([
        {"id":"amazon-bedrock", "name":"Bedrock", "methods":[{"type":"key"}], "connections":[]}
    ]))?;
    outgoing.send(Reply::Catalog(providers, false))?;
    connect.poll();
    press(&mut connect, KeyCode::Enter);
    press(&mut connect, KeyCode::Enter);
    connect.paste("work");
    for _ in 0..3 {
        press(&mut connect, KeyCode::Tab);
    }
    connect.paste("fixture-model");
    press(&mut connect, KeyCode::Tab);
    connect.paste("us-east-1");
    for attempt in 0..2 {
        press(&mut connect, KeyCode::Enter);
        let Request::Key(id, body) = incoming.try_recv()? else {
            return Err("expected AWS key-connect request".into());
        };
        assert_eq!(id.as_str(), "amazon-bedrock");
        assert!(body.key.as_str().is_empty());
        let native = body
            .bedrock
            .as_present()
            .ok_or("Bedrock coordinates absent")?;
        let Some(grapt_api::u3a::BedrockAuthRequestDto::AwsChain { profile }) =
            native.auth.as_present()
        else {
            return Err("AWS selection absent".into());
        };
        assert_eq!(
            profile.as_present().map(|s| s.as_str()),
            Some("work"),
            "attempt {attempt} changed the credential source"
        );
        if attempt == 0 {
            outgoing.send(Reply::Failed(
                grapt_api::u3a::INTEGRATION_AWS_CREDENTIALS_MESSAGE.into(),
            ))?;
            connect.poll();
        }
    }
    Ok(())
}
