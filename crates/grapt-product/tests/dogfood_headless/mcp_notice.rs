//! A turn that cannot use its MCP configuration or servers says so, once, in
//! the Session's history; the notice is the user's, never the model's.
use super::*;

/// The notice for a declaration with no type in `config`: the refusal names
/// the document that holds it.
fn refused(config: &Path) -> String {
    format!(
        "MCP unavailable this turn: MCP configuration (it was refused: \
         MCP declaration requires a local or remote type: {})",
        config.display()
    )
}
const CLEARED: &str = "MCP servers are available again.";

#[test]
fn reconnect_replaces_only_the_named_server_without_dispatching_a_tool(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("mcp-reconnect");
    prepare(&root, 120_000);
    let peer =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/mcp-producer-version-peer.py");
    let mut servers = serde_json::Map::new();
    for name in ["first", "other"] {
        let directory = root.join(name);
        fs::create_dir_all(&directory)?;
        fs::write(directory.join("generation"), "old")?;
        servers.insert(
            name.into(),
            serde_json::json!({
                "type": "local", "command": ["python3", peer, directory]
            }),
        );
    }
    servers.insert(
        "disabled".into(),
        serde_json::json!({
            "type": "local", "command": ["not-started"], "disabled": true
        }),
    );
    let config = root.join("global-config");
    fs::create_dir_all(&config)?;
    fs::write(
        config.join("grapt.json"),
        serde_json::to_vec(&serde_json::json!({"mcp": {"servers": servers}}))?,
    )?;
    let provider = ProviderFixture::start(vec![]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let catalog = json(address, "GET", "/api/mcp", "");
    let connected = |catalog: &serde_json::Value, name: &str| {
        catalog["data"].as_array().is_some_and(|rows| {
            rows.iter()
                .any(|row| row["name"] == name && row["status"]["status"] == "connected")
        })
    };
    assert!(
        connected(&catalog, "first") && connected(&catalog, "other"),
        "{catalog}"
    );
    let first = fs::read_to_string(root.join("first/starts"))?;
    let other = fs::read_to_string(root.join("other/starts"))?;
    let (status, body) = request(address, "POST", "/api/mcp/first/reconnect", "");
    assert_eq!(status, 204, "{}", String::from_utf8_lossy(&body));
    let catalog = json(address, "GET", "/api/mcp", "");
    assert!(
        connected(&catalog, "first") && connected(&catalog, "other"),
        "{catalog}"
    );
    let replaced = fs::read_to_string(root.join("first/starts"))?;
    assert_eq!(first.lines().count(), 1);
    assert_eq!(replaced.lines().count(), 2);
    assert_ne!(replaced.lines().next(), replaced.lines().nth(1));
    assert_eq!(fs::read_to_string(root.join("other/starts"))?, other);
    for name in ["missing", "disabled"] {
        assert_eq!(
            request(address, "POST", &format!("/api/mcp/{name}/reconnect"), "").0,
            400
        );
    }
    for name in ["first", "other"] {
        assert!(!root.join(name).join("calls").exists());
    }
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}

/// One `grapt run` turn on `session`, in text mode, which shows notices.
fn turn(root: &Path, session: &str) -> super::cli_continuity::CliRun {
    let service = root.to_string_lossy().into_owned();
    super::cli_continuity::cli(
        root,
        &[
            "run",
            "--service",
            &service,
            "--session",
            session,
            "--prompt",
            "go",
        ],
    )
}

fn create_in(address: SocketAddr, directory: &Path) -> String {
    let body = serde_json::json!({"location": {"directory": directory}});
    json(address, "POST", "/api/session", &body.to_string())["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("created session had no id"))
        .to_owned()
}

fn notice_rows(root: &Path, session: &str) -> i64 {
    let store = EventStore::open(&root.join("database.sqlite3"), "mcp-notice-evidence")
        .unwrap_or_else(|error| panic!("evidence store is unreadable: {error}"));
    store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
             AND type = 'session.notice.recorded.1'",
            [session],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("evidence query failed: {error}"))
}

#[test]
fn a_refused_mcp_config_is_announced_once_kept_from_the_model_and_cleared_once() {
    let _journey = process_journey_custody();
    let root = root("mcp-notice");
    prepare(&root, 200_000);
    let directory = PathBuf::from(format!("{}-project", root.display()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap_or_else(|error| panic!("{error}"));
    let config = directory.join("grapt.json");
    // A declaration with no type: refused, so no MCP tool is published.
    fs::write(
        &config,
        r#"{"mcp":{"notes":{"command":["/usr/bin/true"]}}}"#,
    )
    .unwrap_or_else(|error| panic!("fixture config: {error}"));
    let provider =
        ProviderFixture::start((1..=5).map(|_| Script::SuccessText("answered")).collect());
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create_in(address, &directory);

    let first = turn(&root, &session);
    let second = turn(&root, &session);
    child.stop_after_witness(&root);
    drop(child);
    // The comparison is with the durable log: a restart repeats nothing.
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let restarted = turn(&root, &session);
    fs::write(&config, "{}").unwrap_or_else(|error| panic!("fixture config: {error}"));
    let fixed = turn(&root, &session);
    let after_fix = turn(&root, &session);
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=50"),
        "",
    );
    let bodies = provider
        .bodies
        .lock()
        .map(|bodies| bodies.clone())
        .unwrap_or_default();

    child.stop_after_witness(&root);
    drop(child);
    let recorded = notice_rows(&root, &session);
    process_fixture::cleanup(&root);
    let _ = fs::remove_dir_all(&directory);

    for run in [&first, &second, &restarted, &fixed, &after_fix] {
        assert_eq!(run.code, Some(0), "a turn failed: {}", run.stderr);
        assert!(run.stdout.contains("answered"), "no answer: {}", run.stdout);
    }
    assert!(
        first
            .stderr
            .contains(&format!("[system] {}", refused(&config))),
        "the refused config was not announced: {}",
        first.stderr
    );
    for (label, run) in [("second", &second), ("restarted", &restarted)] {
        assert!(
            !run.stderr.contains("[system]"),
            "the {label} turn repeated an unchanged notice: {}",
            run.stderr
        );
    }
    assert!(
        fixed.stderr.contains(&format!("[system] {CLEARED}")),
        "the fix was not announced: {}",
        fixed.stderr
    );
    assert!(
        !after_fix.stderr.contains("[system]"),
        "the cleared notice was repeated: {}",
        after_fix.stderr
    );
    assert_eq!(recorded, 2, "one notice for the breakage, one for its fix");
    let notices: Vec<&str> = history["data"]
        .as_array()
        .unwrap_or_else(|| panic!("history is not a page: {history}"))
        .iter()
        .filter(|row| row["type"] == "system" && row["metadata"]["mcpUnavailable"] == true)
        .filter_map(|row| row["text"].as_str())
        .collect();
    assert_eq!(notices, [refused(&config).as_str(), CLEARED], "{history}");
    // Every turn after the first was planned over a history holding the
    // notice; none of them showed it to the model.
    assert_eq!(bodies.len(), 5, "one provider request a turn");
    for (index, body) in bodies.iter().enumerate() {
        for text in [
            "MCP unavailable",
            "requires a local or remote type",
            CLEARED,
        ] {
            assert!(
                !body.contains(text),
                "provider request {index} carried the notice text {text:?}"
            );
        }
    }
}

#[test]
fn a_server_that_misses_its_startup_budget_is_named_with_the_phase() {
    let _journey = process_journey_custody();
    let root = root("mcp-notice-timeout");
    prepare(&root, 200_000);
    let directory = PathBuf::from(format!("{}-project", root.display()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap_or_else(|error| panic!("{error}"));
    let marker = format!("122.{}", std::process::id());
    // A project may not start a local server, so the slow one is the user's:
    // declared in the service root's own grapt.jsonc.
    let user_config = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(
        &fs::read(&user_config).unwrap_or_else(|error| panic!("fixture config read: {error}")),
    )
    .unwrap_or_else(|error| panic!("fixture config decode: {error}"));
    config["mcp"] = serde_json::json!({"slow": {
        "type": "local",
        "command": ["/usr/bin/sleep", marker],
        "timeout": {"startup": 1_000, "catalog": 1_000},
    }});
    fs::write(&user_config, config.to_string())
        .unwrap_or_else(|error| panic!("fixture config: {error}"));
    let provider = ProviderFixture::start(vec![Script::SuccessText("answered")]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create_in(address, &directory);

    let run = turn(&root, &session);

    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    let _ = fs::remove_dir_all(&directory);

    assert_eq!(run.code, Some(0), "the turn failed: {}", run.stderr);
    assert!(
        run.stderr.contains(
            "[system] MCP unavailable this turn: slow (the handshake did not complete \
             within its startup budget)"
        ),
        "the timed-out server was not named with its phase: {}",
        run.stderr
    );
}

/// A cloned repository may not make SE run a command (ruling
/// Q2): its local server is never started, and the turn names it.
#[cfg(unix)]
#[test]
fn a_project_local_server_is_not_started_and_is_named() {
    let _journey = process_journey_custody();
    let root = root("mcp-notice-project-local");
    prepare(&root, 200_000);
    let directory = PathBuf::from(format!("{}-project", root.display()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap_or_else(|error| panic!("{error}"));
    let started = directory.join("started");
    let config = serde_json::json!({"mcp": {"repo": {
        "type": "local",
        "command": ["/bin/sh", "-c", format!("touch '{}'", started.display())],
    }}});
    fs::write(directory.join("grapt.json"), config.to_string())
        .unwrap_or_else(|error| panic!("fixture config: {error}"));
    let provider = ProviderFixture::start(vec![Script::SuccessText("answered")]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create_in(address, &directory);

    let run = turn(&root, &session);

    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    let ran = started.exists();
    let _ = fs::remove_dir_all(&directory);

    assert_eq!(run.code, Some(0), "the turn failed: {}", run.stderr);
    assert!(
        run.stderr.contains(
            "[system] MCP unavailable this turn: repo (MCP servers from project config \
             require approval; use /mcp approve <server> or add them to your user config)"
        ),
        "the blocked server was not named: {}",
        run.stderr
    );
    assert!(!ran, "the project's local server was started");
}

/// An unreadable source is named on both paths (review H2). One the
/// Session's own sources hold queues the prompt, named. One only the
/// service's plugin sources hold (its own `.grapt`) reaches MCP
/// publication, where plugins refresh first: it is the Session's notice, and
/// its repair is announced.
#[test]
fn an_unreadable_source_is_named_on_both_paths_and_its_repair_cleared() {
    let _journey = process_journey_custody();
    let root = root("mcp-notice-unreadable-source");
    prepare(&root, 200_000);
    let directory = PathBuf::from(format!("{}-project", root.display()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap_or_else(|error| panic!("{error}"));
    let provider =
        ProviderFixture::start((1..=4).map(|_| Script::SuccessText("answered")).collect());
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create_in(address, &directory);
    // A directory where a document belongs is not a regular file. The
    // service root's own `.grapt` is not an ancestor of the project.
    let plugins_only = root.join(".grapt").join("grapt.json");
    fs::create_dir_all(&plugins_only).unwrap_or_else(|error| panic!("{error}"));
    let noticed = turn(&root, &session);
    fs::remove_dir(&plugins_only).unwrap_or_else(|error| panic!("{error}"));
    let repaired = turn(&root, &session);
    // The fixture's global config directory is in every reader's sources.
    // Last: a blocked prompt stays queued for the Session's next execution.
    let shared = root.join("global-config").join("grapt.json");
    fs::create_dir_all(&shared).unwrap_or_else(|error| panic!("{error}"));
    let refused = turn(&root, &session);
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=50"),
        "",
    );

    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    let _ = fs::remove_dir(&shared);
    let _ = fs::remove_dir_all(&directory);

    assert_ne!(refused.code, Some(0), "the prompt ran: {}", refused.stdout);
    // Each refusal names the exact source it could not read.
    let irregular = |path: &Path| {
        format!(
            "catalog configuration is not a regular file: {}",
            path.display()
        )
    };
    assert!(
        refused
            .stderr
            .contains(&format!("Not run yet: {}", irregular(&shared))),
        "the unreadable shared source was not named: {}",
        refused.stderr
    );
    // The durable history is the record; each turn's text shows its own.
    let notices: Vec<&str> = history["data"]
        .as_array()
        .unwrap_or_else(|| panic!("history is not a page: {history}"))
        .iter()
        .filter(|row| row["type"] == "system" && row["metadata"]["mcpUnavailable"] == true)
        .filter_map(|row| row["text"].as_str())
        .collect();
    let seen = format!(
        "notices {notices:?}\nnoticed: {}\nrepaired: {}",
        noticed.stderr, repaired.stderr
    );
    assert_eq!(notices.len(), 2, "{seen}");
    assert_eq!(
        notices[0],
        format!(
            "MCP unavailable this turn: plugins (they could not be refreshed: {})",
            irregular(&plugins_only)
        ),
        "the unreadable plugin source was not named: {seen}"
    );
    assert_eq!(notices[1], CLEARED, "{seen}");
    assert!(
        noticed.stderr.contains(&format!("[system] {}", notices[0])),
        "the noticed turn did not show it: {seen}"
    );
    assert!(
        repaired.stderr.contains(&format!("[system] {CLEARED}")),
        "the repair was not announced: {seen}"
    );
    for run in [&noticed, &repaired] {
        assert_eq!(run.code, Some(0), "a turn failed: {}", run.stderr);
    }
}

/// ACP shows the notice live, as product text, ahead of the turn's answer:
/// it is committed before the promotion ACP reads history on.
#[cfg(unix)]
#[test]
fn an_acp_prompt_shows_the_notice_before_its_answer() {
    use super::acp_process::{now, AcpObserved, AcpProcess, ACP_FRAMES};
    let _journey = process_journey_custody();
    let root = root("mcp-notice-acp");
    prepare(&root, 200_000);
    let directory = PathBuf::from(format!("{}-project", root.display()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap_or_else(|error| panic!("{error}"));
    let config = directory.join("grapt.json");
    fs::write(
        &config,
        r#"{"mcp":{"notes":{"command":["/usr/bin/true"]}}}"#,
    )
    .unwrap_or_else(|error| panic!("fixture config: {error}"));
    let provider = ProviderFixture::start(vec![Script::SuccessText("answered")]);
    let deadline = now() + Duration::from_secs(60);
    let mut acp = AcpProcess::start(&root, provider.address, ACP_FRAMES);
    let mut observed = AcpObserved::default();
    let response = (|| {
        acp.request(
            1,
            "initialize",
            serde_json::json!({"protocolVersion": 1, "clientCapabilities": {}}),
            deadline,
            &mut observed,
        )?;
        let created = acp.request(
            2,
            "session/new",
            serde_json::json!({"cwd": directory, "mcpServers": []}),
            deadline,
            &mut observed,
        )?;
        let session = created["sessionId"]
            .as_str()
            .ok_or_else(|| format!("ACP session creation refused: {created}"))?
            .to_owned();
        acp.request(
            3,
            "session/prompt",
            serde_json::json!({"sessionId": session, "prompt": [{"type": "text", "text": "go"}]}),
            deadline,
            &mut observed,
        )
    })();
    acp.cleanup();
    let bodies = locked(&provider.bodies).clone();
    process_fixture::cleanup(&root);
    let _ = fs::remove_dir_all(&directory);

    let response = response.unwrap_or_else(|error| panic!("the ACP prompt failed: {error}"));
    assert_eq!(response["stopReason"], "end_turn", "{response}");
    let notice = observed
        .texts
        .iter()
        .position(|text| text == &format!("[system] {}", refused(&config)))
        .unwrap_or_else(|| panic!("ACP never showed the notice: {:?}", observed.texts));
    let answer = observed
        .texts
        .iter()
        .position(|text| text.contains("answered"))
        .unwrap_or_else(|| panic!("ACP never showed the answer: {:?}", observed.texts));
    assert!(
        notice < answer,
        "the notice came after the answer: {:?}",
        observed.texts
    );
    assert_eq!(observed.updates[notice], "agent_message_chunk");
    for body in &bodies {
        assert!(
            !body.contains("MCP unavailable"),
            "the model was shown the notice"
        );
    }
}

/// One `grapt run` turn on `session` saying `prompt`.
fn turn_saying(root: &Path, session: &str, prompt: &str) -> super::cli_continuity::CliRun {
    let service = root.to_string_lossy().into_owned();
    super::cli_continuity::cli(
        root,
        &[
            "run",
            "--service",
            &service,
            "--session",
            session,
            "--prompt",
            prompt,
        ],
    )
}

// ApiClientError::Status/StatusDetail render this phrase; a run wraps it
// with "API client refused the operation". Session IDs are ordinary text.
fn cli_conflict(stderr: &str) -> bool {
    stderr.contains("server returned HTTP status 409 Conflict")
}

#[test]
fn a_cli_acceptance_id_containing_409_is_not_a_conflict_refusal() {
    let accepted = "prompt accepted for ses_f40973db0123456789\n\
        grapt: Not run yet: catalog configuration is not a regular file\n";
    assert!(
        !cli_conflict(accepted),
        "a session ID was mistaken for a refusal"
    );
    for refused in [
        "grapt: API client refused the operation: server returned HTTP status 409 Conflict\n",
        "grapt: API client refused the operation: server returned HTTP status 409 Conflict: this Session already has four admitted wake owners\n",
    ] {
        assert!(cli_conflict(refused), "a rendered HTTP conflict was missed: {refused}");
    }
}

/// Slice P: a prompt whose turn is blocked before it
/// begins is still admitted, never refused; the turn says it has not run
/// yet and exits non-zero; once fixed, the next turn answers the queued
/// prompt first and then its own.
#[test]
fn a_blocked_prompt_is_queued_with_a_notice_and_answered_after_the_fix() {
    let _journey = process_journey_custody();
    let root = root("mcp-notice-not-run-yet");
    prepare(&root, 200_000);
    let directory = PathBuf::from(format!("{}-project", root.display()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap_or_else(|error| panic!("{error}"));
    let provider = ProviderFixture::start(vec![
        Script::SuccessText("first answered"),
        Script::SuccessText("second answered"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create_in(address, &directory);
    // A directory where a document belongs: every reader's source.
    let shared = root.join("global-config").join("grapt.json");
    fs::create_dir_all(&shared).unwrap_or_else(|error| panic!("{error}"));
    let blocked = turn_saying(&root, &session, "first");
    fs::remove_dir(&shared).unwrap_or_else(|error| panic!("{error}"));
    let fixed = turn_saying(&root, &session, "second");
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=50"),
        "",
    );
    let requests = provider
        .bodies
        .lock()
        .map(|bodies| bodies.len())
        .unwrap_or_default();

    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    let _ = fs::remove_dir_all(&directory);

    assert_ne!(blocked.code, Some(0), "a queued prompt ran as a success");
    assert!(
        !cli_conflict(&blocked.stderr),
        "the blocked prompt was refused: {}",
        blocked.stderr
    );
    assert!(
        blocked.stderr.contains("Not run yet: ")
            && blocked
                .stderr
                .contains("catalog configuration is not a regular file")
            && blocked
                .stderr
                .contains("Your message is queued and runs on the next turn once this is fixed."),
        "the block was not shown: {}",
        blocked.stderr
    );
    assert_eq!(
        fixed.code,
        Some(0),
        "the fixed turn failed: {}",
        fixed.stderr
    );
    assert_eq!(requests, 2, "both prompts were answered once each");
    let texts: Vec<String> = history["data"]
        .as_array()
        .unwrap_or_else(|| panic!("history is not a page: {history}"))
        .iter()
        .filter(|row| row["type"] == "user" || row["type"] == "assistant")
        .map(|row| {
            let text = row["text"].as_str().map(str::to_owned).unwrap_or_else(|| {
                row["content"]
                    .as_array()
                    .and_then(|parts| parts.iter().find_map(|part| part["text"].as_str()))
                    .unwrap_or_default()
                    .to_owned()
            });
            format!("{}:{text}", row["type"].as_str().unwrap_or_default())
        })
        .collect();
    assert_eq!(
        texts,
        [
            "user:first",
            "assistant:first answered",
            "user:second",
            "assistant:second answered"
        ],
        "the queued prompt was not answered first: {history}"
    );
}

/// Review P-H2: even when the not-run-yet notice
/// cannot be written, an admitted prompt is never refused. The acceptance
/// itself carries the reason, the run shows it and exits non-zero, no
/// durable notice exists, and the next turn after the repair answers both.
#[test]
fn a_blocked_prompt_whose_notice_cannot_be_written_is_still_accepted() {
    let _journey = process_journey_custody();
    let root = root("mcp-notice-not-run-yet-no-notice");
    prepare(&root, 200_000);
    let directory = PathBuf::from(format!("{}-project", root.display()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap_or_else(|error| panic!("{error}"));
    let provider = ProviderFixture::start(vec![
        Script::SuccessText("first answered"),
        Script::SuccessText("second answered"),
    ]);
    let mut child = process_fixture::spawn_with(
        &root,
        provider.address,
        &[("GRAPT_TESTKIT_FAIL_NOTICE_APPEND", "1")],
    );
    let address = await_ready(&root, &mut child);
    let session = create_in(address, &directory);
    let shared = root.join("global-config").join("grapt.json");
    fs::create_dir_all(&shared).unwrap_or_else(|error| panic!("{error}"));
    let blocked = turn_saying(&root, &session, "first");
    fs::remove_dir(&shared).unwrap_or_else(|error| panic!("{error}"));
    let fixed = turn_saying(&root, &session, "second");
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=50"),
        "",
    );
    let requests = provider
        .bodies
        .lock()
        .map(|bodies| bodies.len())
        .unwrap_or_default();

    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    let _ = fs::remove_dir_all(&directory);

    assert_ne!(blocked.code, Some(0), "a queued prompt ran as a success");
    assert!(
        !cli_conflict(&blocked.stderr),
        "an admitted prompt was refused: {}",
        blocked.stderr
    );
    assert!(
        blocked.stderr.contains("Not run yet: ")
            && blocked
                .stderr
                .contains("catalog configuration is not a regular file"),
        "the acceptance did not carry the reason: {}",
        blocked.stderr
    );
    let rows = history["data"]
        .as_array()
        .unwrap_or_else(|| panic!("history is not a page: {history}"));
    assert!(
        !rows.iter().any(|row| row["metadata"]["notRunYet"] == true),
        "the notice append was not refused: {history}"
    );
    assert_eq!(
        fixed.code,
        Some(0),
        "the fixed turn failed: {}",
        fixed.stderr
    );
    assert_eq!(requests, 2, "both prompts were answered once each");
}

/// J03: a refused MCP config is still an error on the
/// status route, as the oracle's invalid config is, and the refusal names the
/// document to edit and the field at fault. A valid config is answered
/// exactly as before.
#[test]
fn a_refused_mcp_config_names_its_file_on_the_status_route() {
    let _journey = process_journey_custody();
    let root = root("mcp-status-refusal");
    prepare(&root, 200_000);
    let config = root.join("grapt.json");
    // `command` misspelt: the declaration has none.
    fs::write(
        &config,
        r#"{"mcp":{"notes":{"type":"local","comand":["/usr/bin/true"]}}}"#,
    )
    .unwrap_or_else(|error| panic!("fixture config: {error}"));
    let provider = ProviderFixture::start(Vec::new());
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let (refused_status, refused_body) = request(address, "GET", "/api/mcp", "");
    fs::write(
        &config,
        r#"{"mcp":{"notes":{"type":"local","command":["/usr/bin/true"],"disabled":true}}}"#,
    )
    .unwrap_or_else(|error| panic!("fixture config: {error}"));
    let (valid_status, valid_body) = request(address, "GET", "/api/mcp", "");
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    let refused = String::from_utf8_lossy(&refused_body);
    assert_eq!(refused_status, 400, "{refused}");
    assert!(
        refused.contains("local MCP command must be an array"),
        "the refusal did not name the field: {refused}"
    );
    assert!(
        refused.contains(&config.display().to_string()),
        "the refusal did not name {}: {refused}",
        config.display()
    );
    let valid: serde_json::Value = serde_json::from_slice(&valid_body)
        .unwrap_or_else(|error| panic!("status page is not JSON: {error}"));
    assert_eq!(valid_status, 200, "{valid}");
    assert_eq!(
        valid["data"].to_string(),
        r#"[{"name":"notes","status":{"status":"disabled"}}]"#
    );
}

#[test]
#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
fn project_mcp_approval_binds_the_reviewed_declaration_and_can_be_withdrawn(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("mcp-project-approval");
    prepare(&root, 120_000);
    let directory = PathBuf::from(format!("{}-project", root.display()));
    fs::create_dir_all(&directory)?;
    fs::write(directory.join("generation"), "old")?;
    let peer =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/mcp-producer-version-peer.py");
    let project_path = directory.join("grapt.jsonc");
    let mut declaration = serde_json::json!({"mcp":{"servers":{"repo":{
        "type":"local","command":["python3",peer,directory]
    }}}});
    declaration["mcpProjectApprovals"] = serde_json::json!({});
    declaration["mcpProjectApprovals"][directory.to_string_lossy().as_ref()] =
        serde_json::json!({"repo":{"fingerprint":"0".repeat(64),"enabled":true}});
    fs::write(&project_path, serde_json::to_vec(&declaration)?)?;
    let config = root.join("global-config");
    fs::create_dir_all(&config)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&config, fs::Permissions::from_mode(0o700))?;
    }
    let user_path = config.join("grapt.jsonc");
    fs::write(&user_path, b"{}")?;
    let provider = ProviderFixture::start(vec![]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create_in(address, &directory);
    let catalog_route = format!("/api/mcp?session={session}");
    let preview_route = format!("/api/mcp/repo/approval?session={session}");
    let write_route = format!("/api/mcp/repo/configuration?session={session}");
    let state = |expected: &str| {
        let catalog = json(address, "GET", &catalog_route, "");
        assert!(
            catalog["data"].as_array().is_some_and(|rows| rows
                .iter()
                .any(|row| row["name"] == "repo" && row["status"]["status"] == expected)),
            "{catalog}"
        );
    };
    state("failed");
    assert!(!directory.join("starts").exists());
    let before = fs::read(&user_path)?;
    for body in [
        serde_json::json!({"action":"set_enabled","enabled":true}),
        serde_json::json!({"action":"approve_project","fingerprint":"0".repeat(64)}),
    ] {
        assert_eq!(
            request(address, "POST", &write_route, &body.to_string()).0,
            400
        );
        assert_eq!(fs::read(&user_path)?, before);
    }
    let preview = json(address, "GET", &preview_route, "");
    assert_eq!(preview["approved"], false);
    let old_fingerprint = preview["fingerprint"]
        .as_str()
        .ok_or("preview fingerprint")?
        .to_owned();
    declaration["mcpProjectApprovals"][directory.to_string_lossy().as_ref()]["repo"]
        ["fingerprint"] = old_fingerprint.clone().into();
    fs::write(&project_path, serde_json::to_vec(&declaration)?)?;
    state("failed");
    assert!(
        !directory.join("starts").exists(),
        "the project approved its own server"
    );
    let approve = |fingerprint: &str| {
        request(
            address,
            "POST",
            &write_route,
            &serde_json::json!({"action":"approve_project","fingerprint":fingerprint}).to_string(),
        )
    };
    assert_eq!(approve(&old_fingerprint).0, 204);
    state("connected");
    let first = fs::read_to_string(directory.join("starts"))?;
    declaration["mcp"]["servers"]["repo"]["command"]
        .as_array_mut()
        .ok_or("command")?
        .push("changed".into());
    fs::write(&project_path, serde_json::to_vec(&declaration)?)?;
    state("failed");
    assert_eq!(fs::read_to_string(directory.join("starts"))?, first);
    let before = fs::read(&user_path)?;
    assert_eq!(approve(&old_fingerprint).0, 400);
    assert_eq!(fs::read(&user_path)?, before);
    let preview = json(address, "GET", &preview_route, "");
    let fingerprint = preview["fingerprint"].as_str().ok_or("new fingerprint")?;
    assert_ne!(fingerprint, old_fingerprint);
    assert_eq!(approve(fingerprint).0, 204);
    state("connected");
    for (enabled, expected) in [(false, "disabled"), (true, "connected")] {
        assert_eq!(
            request(
                address,
                "POST",
                &write_route,
                &serde_json::json!({"action":"set_enabled","enabled":enabled}).to_string()
            )
            .0,
            204
        );
        state(expected);
    }
    let profile_path = root.join("grapt.jsonc");
    let mut profile: serde_json::Value = serde_json::from_slice(&fs::read(&profile_path)?)?;
    let user: serde_json::Value = serde_json::from_slice(&fs::read(&user_path)?)?;
    profile["mcpProjectApprovals"] = user["mcpProjectApprovals"].clone();
    fs::write(&profile_path, serde_json::to_vec(&profile)?)?;
    assert_eq!(
        request(
            address,
            "POST",
            &write_route,
            r#"{"action":"revoke_project_approval"}"#
        )
        .0,
        204
    );
    state("failed");
    assert!(!directory.join("calls").exists());
    child.stop_after_witness(&root);
    drop(child);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let catalog = json(address, "GET", &catalog_route, "");
    assert!(
        catalog["data"].as_array().is_some_and(|rows| rows
            .iter()
            .any(|row| row["name"] == "repo" && row["status"]["status"] == "failed")),
        "an older profile approval resurrected after revoke and restart: {catalog}"
    );
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    fs::remove_dir_all(directory)?;
    Ok(())
}
