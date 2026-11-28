//! The same running product adopts a rotated source, with no hidden resend.
use super::*;

fn prompt(address: SocketAddr, session: &str, text: &str) {
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"text":text}).to_string(),
    );
}

fn history(address: SocketAddr, session: &str) -> String {
    json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=50"),
        "",
    )
    .to_string()
}

fn check_headers(provider: &ProviderFixture, keys: &[&str]) {
    let heads = locked(&provider.heads);
    assert_eq!(heads.len(), keys.len());
    for (head, key) in heads.iter().zip(keys) {
        let head = head.to_ascii_lowercase();
        let authentication: Vec<_> = head
            .lines()
            .filter(|line| line.starts_with("authorization:"))
            .collect();
        assert_eq!(authentication, [format!("authorization: bearer {key}")]);
    }
}

#[test]
fn a_running_service_uses_a_rotated_file_on_its_next_prompt() {
    let _journey = process_journey_custody();
    let root = root("credential-live-file");
    prepare(&root, 200_000);
    let provider = ProviderFixture::start(vec![
        Script::SuccessText("before renewal"),
        Script::SuccessText("after renewal"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    prompt(address, &session, "first request");
    permission_residuals::wait_session_idle(address, &session);
    fs::write(root.join("provider.credential"), "rotated-key")
        .unwrap_or_else(|error| panic!("rotate credential fixture: {error}"));
    prompt(address, &session, "second request in the same service");
    permission_residuals::wait_session_idle(address, &session);
    check_headers(&provider, &["provider-secret", "rotated-key"]);
    assert!(history(address, &session).contains("after renewal"));
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}

#[test]
fn a_401_after_external_rotation_retries_with_fresh_material_in_a_new_attempt() {
    let _journey = process_journey_custody();
    let root = root("credential-live-401");
    prepare(&root, 200_000);
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let provider = ProviderFixture::start(vec![
        Script::HeldRefusal {
            status: 401,
            entered: entered.clone(),
            release: release.clone(),
        },
        Script::SuccessText("recovered credentials"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    prompt(address, &session, "recover this same prompt");
    entered.wait();
    fs::write(root.join("provider.credential"), "rotated-key")
        .unwrap_or_else(|error| panic!("rotate credential fixture: {error}"));
    release.wait();
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    check_headers(&provider, &["provider-secret", "rotated-key"]);
    assert!(history(address, &session).contains("recovered credentials"));
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}

#[test]
fn a_403_after_rotation_does_not_retry_or_adopt_a_recovery_hint() {
    let _journey = process_journey_custody();
    let root = root("credential-live-403");
    prepare(&root, 200_000);
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let provider = ProviderFixture::start(vec![
        Script::HeldRefusal {
            status: 403,
            entered: entered.clone(),
            release: release.clone(),
        },
        Script::SuccessText("must not run"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    prompt(address, &session, "keep permission failure terminal");
    entered.wait();
    fs::write(root.join("provider.credential"), "rotated-key")
        .unwrap_or_else(|error| panic!("rotate credential fixture: {error}"));
    release.wait();
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    check_headers(&provider, &["provider-secret"]);
    assert!(!history(address, &session).contains("must not run"));
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}

#[test]
fn a_chatgpt_sign_in_rotation_recovers_in_the_running_headless_service(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("credential-live-sign-in");
    prepare(&root, 200_000);
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    config["provider"]["protocol_family"] = serde_json::json!("chatgpt-codex");
    config["provider"]["credential_version"] =
        serde_json::json!(grapt_login::chatgpt::SIGN_IN_CREDENTIAL_VERSION);
    fs::write(path, serde_json::to_vec(&config)?)?;
    // Long-valid stored tokens isolate reload from authority renewal. This
    // journey never needs the live token issuer; issuer grants have local unit fixtures.
    let mut tokens = grapt_login::chatgpt::Tokens {
        access: "oauth-before".into(),
        refresh: "unused-fixture-refresh".into(),
        account: "account-a".into(),
        expires_at: grapt_types::Timestamp::from_millis(8_000_000_000_000),
    };
    // The service reads the sealed sign-in through the same test keystore.
    let keystore = grapt_login::profile_security::isolate_test_keystore()?;
    let keystore = keystore.to_str().ok_or("test keystore path is not UTF-8")?;
    grapt_login::chatgpt::save(&root, &tokens)?;
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let provider = ProviderFixture::start(vec![
        Script::HeldRefusal {
            status: 401,
            entered: entered.clone(),
            release: release.clone(),
        },
        Script::SuccessText("recovered sign-in generation"),
    ]);
    let mut child = process_fixture::spawn_with(
        &root,
        provider.address,
        &[(
            grapt_login::profile_security::TEST_KEYSTORE_VARIABLE,
            keystore,
        )],
    );
    let address = await_ready(&root, &mut child);
    let session = create(address);
    prompt(
        address,
        &session,
        "reload the sign-in another process renewed",
    );
    entered.wait();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("chatgpt.lock"))?;
    assert!(
        lock.try_lock().is_ok(),
        "model request retained the sign-in lock"
    );
    tokens.access = "oauth-after".into();
    tokens.refresh = "rotated-fixture-refresh".into();
    grapt_login::chatgpt::save(&root, &tokens)?;
    drop(lock);
    release.wait();
    permission_residuals::wait_session_idle(address, &session);
    check_headers(&provider, &["oauth-before", "oauth-after"]);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert!(history(address, &session).contains("recovered sign-in generation"));
    assert_eq!(grapt_login::chatgpt::load(&root)?, Some(tokens));
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}
