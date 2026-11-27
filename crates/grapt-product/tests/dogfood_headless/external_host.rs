//! External control reads the durable key without replaying an uncertain input.
use super::*;

#[test]
#[cfg_attr(not(windows), expect(clippy::cognitive_complexity, reason = "legacy"))]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn external_host_reads_keyed_admission_and_fences_native_notices(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("external-host-control");
    prepare(&root, 30_000);
    #[cfg(unix)]
    {
        // Hold real async preparation AFTER execution custody was acquired.
        let peer = r#"
import json,sys,time
from pathlib import Path
base=Path(sys.argv[1])
for _ in range(32):
    line=sys.stdin.readline(65537)
    if not line: break
    if len(line)>65536: raise ValueError('fixture frame bound')
    value=json.loads(line); method=value.get('method')
    if 'id' not in value: continue
    if method=='initialize':
        result={'protocolVersion':value['params']['protocolVersion'],
                'serverInfo':{'name':'notice-gate','version':'1'},'capabilities':{'tools':{}}}
    elif method=='tools/list':
        base.with_suffix('.entered').write_text('ready')
        for tick in range(2000):
            if base.with_suffix('.release').exists(): break
            time.sleep(0.01)
        else: raise ValueError('preparation gate not released')
        result={'tools':[]}
    else: raise ValueError('unexpected fixture request')
    print(json.dumps({'jsonrpc':'2.0','id':value['id'],'result':result}),flush=True)
"#;
        let config = root.join("grapt.jsonc");
        let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&config)?)?;
        value["mcp"] = serde_json::json!({"notice-gate":{"type":"local",
            "command":["python3","-c",peer,root.join("preparation")]}});
        fs::write(config, value.to_string())?;
    }
    let provider = ProviderFixture::start(vec![Script::SuccessText("completed")]);
    let mut command = grapt_service::launcher_environment::command(shipped_cli::product_binary());
    command
        .args(["serve-service", "--service"])
        .arg(&root)
        .args(["--port", "0", "--external-host"])
        .env(
            "GRAPT_LIVE_HEADLESS_LOOPBACK",
            format!("http://{}", provider.address),
        )
        .env("GRAPT_TEST_CATALOG_ROOT", &root)
        .env("GRAPT_TEST_HOME", &root)
        .env("GRAPT_CONFIG_DIR", root.join("global-config"));
    let mut child = process_fixture::spawn_captured(&root, &mut command);
    let address = await_ready(&root, &mut child);
    let created = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({
            "location":{"directory":root}
        })
        .to_string(),
    );
    let session = created["data"]["id"].as_str().ok_or("created Session id")?;
    let input = "msg_host_initial";
    let prompt = format!("/api/session/{session}/prompt");
    let readback = format!("/api/session/{session}/input/{input}");
    let absent = json(address, "GET", &readback, "");
    assert!(absent["data"].is_null());
    let admission = json(
        address,
        "POST",
        &prompt,
        &serde_json::json!({
            "id":input,"text":"initial work","delivery":"queue","resume":false,
            "metadata":{"origin":"seat-startup"}
        })
        .to_string(),
    );
    let accepted = json(address, "GET", &readback, "");
    assert_eq!(
        accepted["data"]["admittedSeq"],
        admission["data"]["admittedSeq"]
    );
    assert_eq!(accepted["data"]["id"], input);
    assert_eq!(accepted["data"]["sessionID"], session);
    assert_eq!(accepted["data"]["text"], "initial work");
    assert_eq!(
        json(
            address,
            "GET",
            &format!("/api/session/{session}/pending"),
            ""
        )["data"]
            .as_array()
            .ok_or("pending input rows")?
            .len(),
        1
    );

    let notice_path = format!("/api/session/{session}/stateengine/notice");
    let unbound = json(address, "GET", &notice_path, "");
    assert_eq!(unbound["status"], "rebind-required");
    let (status, _) = request(
        address,
        "PUT",
        &format!("/api/session/{session}/instructions/entries/stateengine-identity"),
        &serde_json::json!({"value":{
            "project_id":"fixture-project","agent_id":"fixture-seat","scope":"project"
        }})
        .to_string(),
    );
    assert_eq!(status, 204);
    let (status, _) = request(
        address,
        "PUT",
        &format!("/api/session/{session}/stateengine/capability"),
        r#"{"capability":"synthetic-fixture-capability"}"#,
    );
    assert_eq!(status, 204);
    let binding = json(address, "GET", &notice_path, "");
    assert_eq!(binding["status"], "ready");
    let service = &binding["service"];
    let mut notice = serde_json::json!({
        "id":"msg_host_notice","targetID":binding["targetID"],
        "binding":binding["binding"],"messages":["msg-external-fixture"],
        "incarnation":service["incarnation"],
        "serviceInstance":"foreign-instance","generation":service["generation"],
        "registrationVersion":service["registrationVersion"]
    });
    let (status, _) = request(address, "POST", &notice_path, &notice.to_string());
    assert_eq!(status, 409);
    let notice_readback = format!("/api/session/{session}/input/msg_host_notice");
    assert!(json(address, "GET", &notice_readback, "")["data"].is_null());
    notice["serviceInstance"] = service["serviceInstance"].clone();
    notice["targetID"] = serde_json::json!("evt_foreign_target");
    let (status, _) = request(address, "POST", &notice_path, &notice.to_string());
    assert_eq!(status, 409);
    assert!(json(address, "GET", &notice_readback, "")["data"].is_null());
    notice["targetID"] = binding["targetID"].clone();
    let offered = json(address, "POST", &notice_path, &notice.to_string());
    assert_eq!(offered["data"]["kind"], "synthetic");
    #[cfg(unix)]
    {
        let wait = PollDeadline::within(Duration::from_secs(10));
        while !root.join("preparation.entered").exists() && wait.pending() {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            root.join("preparation.entered").exists(),
            "notice never reached async preparation"
        );
    }
    let replayed = json(address, "POST", &notice_path, &notice.to_string());
    assert_eq!(
        replayed["data"]["admittedSeq"],
        offered["data"]["admittedSeq"]
    );
    let observed = json(address, "GET", &notice_readback, "");
    assert_eq!(observed["data"]["id"], "msg_host_notice");
    assert_eq!(
        observed["data"]["admittedSeq"],
        offered["data"]["admittedSeq"]
    );
    let (status, _) = request(
        address,
        "PUT",
        &format!("/api/session/{session}/stateengine/capability"),
        r#"{"capability":"replacement-fixture-capability"}"#,
    );
    assert_eq!(status, 204);
    #[cfg(unix)]
    {
        fs::write(root.join("preparation.release"), "")?;
        permission_residuals::wait_session_idle(address, session);
        assert!(
            locked(&provider.bodies).is_empty(),
            "revoked notice woke unrelated queued input after preparation"
        );
        let pending = json(
            address,
            "GET",
            &format!("/api/session/{session}/pending"),
            "",
        );
        assert!(pending["data"]
            .as_array()
            .ok_or("pending rows")?
            .iter()
            .any(|row| row["id"] == input));
    }
    let (status, _) = request(address, "POST", &notice_path, &notice.to_string());
    assert_eq!(
        status, 409,
        "an old notice crossed a capability target replacement"
    );
    assert_eq!(
        json(address, "GET", &readback, "")["data"]["admittedSeq"],
        accepted["data"]["admittedSeq"]
    );
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}

#[test]
fn external_host_clean_restart_changes_authenticated_service_identity(
) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(windows)]
    {
        super::resident_console::in_private_console(
            "restart",
            "external_host::external_host_clean_restart_changes_authenticated_service_identity",
            clean_restart,
        )
    }
    #[cfg(not(windows))]
    {
        let _journey = process_journey_custody();
        clean_restart()
    }
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn clean_restart() -> Result<(), Box<dyn std::error::Error>> {
    // Keep the helper alive when the console-wide interrupt reaches it too.
    #[cfg(windows)]
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    #[cfg(windows)]
    let _entered_runtime = runtime.enter();
    #[cfg(windows)]
    let _interrupt = tokio::signal::windows::ctrl_c()?;
    let root = root("external-host-clean-restart");
    prepare(&root, 30_000);
    let provider = ProviderFixture::start(vec![Script::SuccessText("completed")]);
    let spawn = || {
        let mut command =
            grapt_service::launcher_environment::command(shipped_cli::product_binary());
        command
            .args(["serve-service", "--service"])
            .arg(&root)
            .args(["--port", "0", "--external-host"])
            .env(
                "GRAPT_LIVE_HEADLESS_LOOPBACK",
                format!("http://{}", provider.address),
            )
            .env("GRAPT_TEST_CATALOG_ROOT", &root)
            .env("GRAPT_TEST_HOME", &root)
            .env("GRAPT_CONFIG_DIR", root.join("global-config"));
        process_fixture::spawn_captured(&root, &mut command)
    };
    let mut first = spawn();
    let address = await_ready(&root, &mut first);
    let status = grapt_service::launcher_environment::command(shipped_cli::cli_binary())
        .args(["status", "--service"])
        .arg(&root)
        .arg("--jsonl")
        .env("GRAPT_TEST_HOME", &root)
        .env("GRAPT_CONFIG_DIR", root.join("global-config"))
        .output()?;
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    assert!(!status.stdout.is_empty());
    for line in std::str::from_utf8(&status.stdout)?.lines() {
        let _: serde_json::Value = serde_json::from_str(line)?;
    }
    let created = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({
            "location":{"directory":root}
        })
        .to_string(),
    );
    let session = created["data"]["id"].as_str().ok_or("created Session id")?;
    let binding = format!("/api/session/{session}/stateengine/notice");
    let before = json(address, "GET", &binding, "");
    #[cfg(unix)]
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(first.id())?),
        nix::sys::signal::Signal::SIGTERM,
    )?;
    // The Windows helper and this service alone share a freshly owned console.
    // A forced process kill would not exercise graceful registration retirement.
    #[cfg(windows)]
    winconsole::console::generate_ctrl_event(false, None)?;
    let wait = PollDeadline::within(Duration::from_secs(10));
    let status = loop {
        if let Some(status) = first.try_wait()? {
            break status;
        }
        if !wait.pending() {
            return Err("service did not stop gracefully".into());
        }
        thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success(), "service stop was not graceful: {status}");
    drop(first);
    assert!(
        !root.join("registration").exists(),
        "service did not retire registration"
    );
    let mut second = spawn();
    let address = await_ready(&root, &mut second);
    let after = json(address, "GET", &binding, "");
    assert_ne!(
        before["service"], after["service"],
        "clean restart reused control identity"
    );
    let stale = serde_json::json!({
        "id":"msg_stale_restart_notice", "targetID":"evt_stale_target",
        "binding":"a".repeat(64), "messages":["source-stale-notice"],
        "incarnation":before["service"]["incarnation"],
        "serviceInstance":after["service"]["serviceInstance"],
        "generation":after["service"]["generation"],
        "registrationVersion":after["service"]["registrationVersion"]
    });
    let (status, body) = request(address, "POST", &binding, &stale.to_string());
    assert_eq!(status, 409);
    assert!(std::str::from_utf8(&body)?.contains("another service incarnation"));
    second.stop_after_witness(&root);
    drop(second);
    process_fixture::cleanup(&root);
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn external_host_releases_only_the_observed_stop_without_replacement_input(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("external-host-explicit-resume");
    prepare(&root, 30_000);
    let provider = ProviderFixture::start(vec![
        Script::SuccessText("notice consumed"),
        Script::SuccessText("initial completed"),
    ]);
    let mut command = grapt_service::launcher_environment::command(shipped_cli::product_binary());
    command
        .args(["serve-service", "--service"])
        .arg(&root)
        .args(["--port", "0", "--external-host"])
        .env(
            "GRAPT_LIVE_HEADLESS_LOOPBACK",
            format!("http://{}", provider.address),
        )
        .env("GRAPT_TEST_CATALOG_ROOT", &root)
        .env("GRAPT_TEST_HOME", &root)
        .env("GRAPT_CONFIG_DIR", root.join("global-config"));
    let mut child = process_fixture::spawn_captured(&root, &mut command);
    let address = await_ready(&root, &mut child);
    let created = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({"location":{"directory":root}}).to_string(),
    );
    let session = created["data"]["id"].as_str().ok_or("created Session id")?;
    bound_session_tools::bind_config(address, &root, session);
    let notice_path = format!("/api/session/{session}/stateengine/notice");
    let stop = format!("/api/session/{session}/interrupt");
    assert_eq!(request(address, "POST", &stop, "{}").0, 204);
    // A keyed resident startup cannot silently release the human stop.
    let initial_path = format!("/api/session/{session}/input/msg_resume_initial");
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_resume_initial","text":"initial work","delivery":"queue","resume":false,"metadata":{"origin":"seat-startup"}}"#,
    );
    let initial = json(address, "GET", &initial_path, "");
    let held = json(address, "GET", &notice_path, "");
    assert_eq!(held["status"], "held");
    let resume_path = format!("/api/session/{session}/stateengine/resume");
    let release = serde_json::json!({
        "holdID":held["holdID"],"targetID":held["targetID"],
        "binding":held["binding"],"service":held["service"]
    });
    let ready = json(address, "POST", &resume_path, &release.to_string());
    assert_eq!(ready["status"], "ready");
    assert!(ready["holdID"].is_null());
    assert_eq!(ready["targetID"], held["targetID"]);
    assert_eq!(ready["binding"], held["binding"]);
    assert_eq!(
        json(address, "GET", &initial_path, "")["data"],
        initial["data"]
    );
    let pending_path = format!("/api/session/{session}/pending");
    let pending = json(address, "GET", &pending_path, "");
    let rows = pending["data"].as_array().ok_or("pending rows")?;
    assert_eq!(rows.len(), 1, "resume admitted replacement input");
    assert_eq!(rows[0]["id"], "msg_resume_initial");
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        0,
        "release started execution"
    );
    // The next real stop has its own fence: neither an old release nor a stale
    // service/target can reopen it, even when capability custody is replaced.
    assert_eq!(request(address, "POST", &stop, "{}").0, 204);
    assert_eq!(
        request(address, "POST", &resume_path, &release.to_string()).0,
        409
    );
    assert_eq!(json(address, "GET", &notice_path, "")["status"], "held");
    let second = json(address, "GET", &notice_path, "");
    let mut release = serde_json::json!({
        "holdID":second["holdID"],"targetID":second["targetID"],
        "binding":second["binding"],"service":second["service"]
    });
    assert_eq!(request(address, "POST", &stop, "{}").0, 204);
    assert_eq!(
        request(address, "POST", &resume_path, &release.to_string()).0,
        409,
        "a stale release cleared a newer stop while already held"
    );
    let second = json(address, "GET", &notice_path, "");
    release["holdID"] = second["holdID"].clone();
    release["service"]["incarnation"] = serde_json::json!("foreign-incarnation");
    assert_eq!(
        request(address, "POST", &resume_path, &release.to_string()).0,
        409
    );
    release["service"] = second["service"].clone();
    bound_session_tools::bind_config(address, &root, session);
    assert_eq!(
        request(address, "POST", &resume_path, &release.to_string()).0,
        409
    );
    let current = json(address, "GET", &notice_path, "");
    assert_eq!(
        current["status"], "held",
        "capability rebind released the stop"
    );
    release["holdID"] = current["holdID"].clone();
    release["targetID"] = current["targetID"].clone();
    json(address, "POST", &resume_path, &release.to_string());
    let notice = serde_json::json!({
        "id":"msg_notice_after_resume","targetID":current["targetID"],
        "binding":current["binding"],"messages":["source-after-explicit-resume"],
        "incarnation":current["service"]["incarnation"],
        "serviceInstance":current["service"]["serviceInstance"],
        "generation":current["service"]["generation"],
        "registrationVersion":current["service"]["registrationVersion"]
    });
    let accepted = json(address, "POST", &notice_path, &notice.to_string());
    assert_eq!(accepted["data"]["kind"], "synthetic");
    wait_calls(&provider.calls, 2);
    permission_residuals::wait_session_idle(address, session);
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    );
    assert!(
        history["data"]
            .as_array()
            .ok_or("history rows")?
            .iter()
            .any(|row| {
                row["type"] == "assistant"
                    && row["content"].as_array().is_some_and(|parts| {
                        parts.iter().any(|part| part["text"] == "notice consumed")
                    })
            }),
        "resumed notice was not consumed"
    );
    assert!(locked(&provider.bodies)
        .iter()
        .any(|body| body.contains("source-after-explicit-resume")));
    assert_eq!(
        json(address, "GET", &initial_path, "")["data"],
        initial["data"]
    );
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}
