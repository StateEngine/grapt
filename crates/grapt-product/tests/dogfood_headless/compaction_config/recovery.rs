//! Summary failure recovery through the public prompt route.
use super::*;

/// An optional older product exercises a real downgrade using the same fixture.
fn restart_retained_reader(root: &Path, provider: SocketAddr) -> process_fixture::ManagedChild {
    let Some(product) = std::env::var_os("GRAPT_TEST_RETAINED_READER") else {
        return spawn(root, provider);
    };
    let mut command = grapt_service::launcher_environment::command(product);
    command
        .args(["serve", "--service"])
        .arg(root)
        .args(["--port", "0"])
        .env("GRAPT_TEST_CATALOG_ROOT", root)
        .env("GRAPT_TEST_HOME", root.join("home"))
        .env("GRAPT_CONFIG_DIR", root.join("global-config"))
        .env("GRAPT_LIVE_HEADLESS_LOOPBACK", format!("http://{provider}"));
    grapt_service::launcher_environment::scrub(&mut command);
    process_fixture::spawn_captured(root, &mut command)
}

#[test]
fn a_text_only_warm_summary_keeps_its_retained_context_after_restart(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("summary-plan-restart");
    prepare(&root, 120_000);
    context_quality::configure_anthropic(&root, 1_000_000);
    for name in ["model-catalog.json", "anthropic-account-models.json"] {
        let path = root.join(name);
        fs::write(
            &path,
            fs::read_to_string(&path)?.replace("claude-live", "claude-opus-5-5"),
        )?;
    }
    set_compaction(
        &root,
        serde_json::json!({"buffer": 1_000_000 - 32_000 - 150_000}),
    );
    fs::write(root.join("seed.txt"), text_lines(b'x', 36_000))?;
    let usage = |input: u64| {
        serde_json::json!({
            "input_tokens": input, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0
        })
    };
    let provider = ProviderFixture::start(vec![
        context_quality::anthropic_read("toolu_summary_seed", "seed.txt", usage(200_000)),
        context_quality::anthropic_text(
            "## Objective\n- preserve the retained rollback marker",
            usage(200_000),
        ),
        context_quality::anthropic_text("compacted answer", usage(1000)),
        context_quality::anthropic_text("continued after restart", usage(1000)),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let (status, _) = request(
        address,
        "POST",
        &format!("/api/session/{session}/model"),
        r#"{"model":{"providerID":"anthropic","id":"claude-opus-5-5"}}"#,
    );
    assert_eq!(status, 204);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_summary_seed","text":"read the seed","delivery":"queue","resume":true}"#,
    );
    compaction_continuity::approve(address, &session, "toolu_summary_seed");
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    let bodies = locked(&provider.bodies).clone();
    let coding: serde_json::Value = serde_json::from_str(&bodies[0])?;
    let summary: serde_json::Value = serde_json::from_str(&bodies[1])?;
    assert!(
        !coding["thinking"].is_null(),
        "coding did not enable thinking"
    );
    assert!(summary["thinking"].is_null(), "summary retained thinking");
    assert!(
        bodies[1].contains(SUMMARY_REQUEST),
        "no warm summary was sent"
    );
    assert_eq!(summary_plans(&root)[0].0, serde_json::json!("warm"));
    child.stop_after_witness(&root);
    drop(child);

    let mut restarted = restart_retained_reader(&root, provider.address);
    let address = await_ready(&root, &mut restarted);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_summary_resume","text":"continue the task","delivery":"queue","resume":true}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    let bodies = locked(&provider.bodies).clone();
    assert_eq!(bodies.len(), 4, "restart did not reach the provider");
    assert!(
        bodies[3].contains("preserve the retained rollback marker"),
        "restart did not load retained context"
    );
    restarted.stop_after_witness(&root);
    drop(restarted);
    process_fixture::cleanup(&root);
    Ok(())
}

#[test]
fn a_shape_refused_summary_recovers_on_the_next_input() {
    let (_, triggers, calls, history, summary_calls, stderr) = journey_sized(
        "config-summary-shape-recovery",
        Some(serde_json::json!({"buffer": BUFFER})),
        vec![
            seed_read(),
            Script::Refused {
                status: 400,
                body: r#"{"error":{"type":"invalid_request_error","message":"invalid request shape"}}"#,
            },
            Script::SuccessText("## Objective\n- read the seed files"),
            Script::SuccessText("recovered answer"),
        ],
        false,
        READ_BYTES,
        Some("continue the task"),
        None,
    );
    assert_eq!((calls, summary_calls), (4, 2), "{stderr}");
    assert_eq!(triggers, vec!["policyThreshold".to_owned()]);
    assert!(
        history["data"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| {
                row["type"] == "assistant"
                    && row["content"].as_array().is_some_and(|parts| {
                        parts.iter().any(|part| {
                            part["type"] == "text" && part["text"] == "recovered answer"
                        })
                    })
            })),
        "the later input did not finish: {history}"
    );
}
