//! A prompt's response acknowledges its admitted input; it does not wait for
//! MCP startup, which a user may configure to take far longer than any
//! client's request allowance.
#[cfg(target_os = "linux")]
use super::client_allowance::{now, CLI_REQUEST_ALLOWANCE};
use super::*;

/// Declare `config`'s MCP servers in the service root's own grapt.jsonc,
/// the user's: a project's local server is never started.
#[cfg(target_os = "linux")]
fn declare_user_mcp(root: &Path, config: &serde_json::Value) {
    let path = root.join("grapt.jsonc");
    let mut user: serde_json::Value = serde_json::from_slice(
        &fs::read(&path).unwrap_or_else(|error| panic!("fixture config read: {error}")),
    )
    .unwrap_or_else(|error| panic!("fixture config decode: {error}"));
    user["mcp"] = config["mcp"].clone();
    fs::write(&path, user.to_string()).unwrap_or_else(|error| panic!("fixture config: {error}"));
}

#[test]
#[cfg(target_os = "linux")]
fn a_slow_mcp_startup_does_not_fail_a_prompt_that_ran() {
    let _journey = process_journey_custody();
    let root = root("prompt-admission");
    prepare(&root, 200_000);
    // The user's own config declares one MCP server that never answers its
    // handshake, with a startup phase longer than the CLI's allowance (a
    // project may not start a local server).
    let directory = PathBuf::from(format!("{}-slow-mcp", root.display()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap_or_else(|error| panic!("{error}"));
    let startup = CLI_REQUEST_ALLOWANCE + Duration::from_secs(5);
    // A duration unique to this run, so its process can be found by name.
    let marker = format!("120.{}", std::process::id());
    let config = serde_json::json!({"mcp": {"slow": {
        "type": "local",
        "command": ["/usr/bin/sleep", marker],
        "timeout": {"startup": startup.as_millis() as u64, "catalog": 1_000},
    }}});
    declare_user_mcp(&root, &config);
    let provider = ProviderFixture::start(vec![Script::SuccessText("ran after a slow MCP")]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let body = serde_json::json!({"location": {"directory": directory}});
    let session = json(address, "POST", "/api/session", &body.to_string())["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("created session had no id"))
        .to_owned();

    let started = now();
    let service = root.to_string_lossy().into_owned();
    let run = super::cli_continuity::cli(
        &root,
        &[
            "run",
            "--service",
            &service,
            "--session",
            &session,
            "--prompt",
            "go",
            "--jsonl",
        ],
    );
    let elapsed = started.elapsed();
    // The server whose startup timed out is killed and reaped, not left on
    // the box: wait a bounded while for it to go.
    let reaped_by = now() + Duration::from_secs(10);
    let mut sleeper = running_with_argument(&marker);
    while !sleeper.is_empty() && now() < reaped_by {
        thread::sleep(Duration::from_millis(100));
        sleeper = running_with_argument(&marker);
    }

    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    let _ = fs::remove_dir_all(&directory);

    assert_eq!(
        run.code,
        Some(0),
        "the CLI reported a failure for a prompt that ran: {}",
        run.stderr
    );
    assert!(
        run.stdout.contains("ran after a slow MCP"),
        "the answer never reached the CLI: {}",
        run.stdout
    );
    assert!(
        sleeper.is_empty(),
        "the timed-out MCP server was left running: {sleeper:?}"
    );
    // The witness only means something if the slow startup was really on
    // the prompt's path.
    assert!(
        elapsed >= CLI_REQUEST_ALLOWANCE,
        "the MCP startup did not delay the turn: {elapsed:?}"
    );
}

/// Pids of live processes with `argument` among their arguments.
#[cfg(target_os = "linux")]
fn running_with_argument(argument: &str) -> Vec<u32> {
    running_with_argument_in(Path::new("/proc"), argument)
        .unwrap_or_else(|error| panic!("the process evidence is unreadable: {error}"))
}

/// The same, read from `proc`. Being unable to read the process table is an
/// error, never "none running": a cleanup witness must not pass on evidence
/// it could not see. A process that exits mid-scan is simply gone.
#[cfg(target_os = "linux")]
fn running_with_argument_in(proc: &Path, argument: &str) -> Result<Vec<u32>, String> {
    let entries = fs::read_dir(proc).map_err(|error| format!("{}: {error}", proc.display()))?;
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("{}: {error}", proc.display()))?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let cmdline = entry.path().join("cmdline");
        match fs::read(&cmdline) {
            Ok(arguments) => {
                if arguments
                    .split(|byte| *byte == 0)
                    .any(|part| part == argument.as_bytes())
                {
                    found.push(pid);
                }
            }
            // Exited between the listing and the read (ESRCH is 3).
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    || error.raw_os_error() == Some(3) => {}
            Err(error) => return Err(format!("{}: {error}", cmdline.display())),
        }
    }
    Ok(found)
}

#[test]
#[cfg(target_os = "linux")]
fn unreadable_process_evidence_refuses_rather_than_reporting_none() {
    let proc = std::env::temp_dir().join(format!("grapt-fake-proc-{}", std::process::id()));
    let _ = fs::remove_dir_all(&proc);
    assert!(
        running_with_argument_in(&proc, "x").is_err(),
        "a missing process table read as none running"
    );
    fs::create_dir_all(proc.join("124")).unwrap_or_else(|error| panic!("{error}"));
    fs::write(proc.join("124/cmdline"), b"/usr/bin/sleep\0marker\0")
        .unwrap_or_else(|error| panic!("{error}"));
    // A pid whose process has gone: tolerated.
    fs::create_dir_all(proc.join("125")).unwrap_or_else(|error| panic!("{error}"));
    let seen = running_with_argument_in(&proc, "marker");
    // A cmdline that cannot be read: refused.
    fs::create_dir_all(proc.join("126/cmdline")).unwrap_or_else(|error| panic!("{error}"));
    let unreadable = running_with_argument_in(&proc, "marker");
    let _ = fs::remove_dir_all(&proc);
    assert_eq!(seen, Ok(vec![124]));
    assert!(
        unreadable.is_err(),
        "an unreadable cmdline read as not running: {unreadable:?}"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn a_chain_refusal_still_answers_the_prompt_in_its_own_words() {
    let _journey = process_journey_custody();
    let root = root("prompt-refusal");
    prepare(&root, 200_000);
    // A configuration the chain refuses before any provider call.
    let directory = PathBuf::from(format!("{}-bad-compaction", root.display()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap_or_else(|error| panic!("{error}"));
    fs::write(directory.join("grapt.json"), r#"{"compaction":"bad"}"#)
        .unwrap_or_else(|error| panic!("fixture config: {error}"));
    let provider = ProviderFixture::start(Vec::new());
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let body = serde_json::json!({"location": {"directory": directory}});
    let session = json(address, "POST", "/api/session", &body.to_string())["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("created session had no id"))
        .to_owned();

    let started = now();
    let service = root.to_string_lossy().into_owned();
    let run = super::cli_continuity::cli(
        &root,
        &[
            "run",
            "--service",
            &service,
            "--session",
            &session,
            "--prompt",
            "go",
            "--jsonl",
        ],
    );
    let elapsed = started.elapsed();

    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    let _ = fs::remove_dir_all(&directory);

    assert!(
        run.code.is_some_and(|code| code != 0),
        "a refused prompt exited {:?}: {}",
        run.code,
        run.stdout
    );
    assert!(
        run.stderr.contains("compaction must be an object"),
        "the refusal lost its own words: {}",
        run.stderr
    );
    assert!(
        elapsed < CLI_REQUEST_ALLOWANCE,
        "the refusal was not prompt: {elapsed:?}"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn an_interrupt_during_a_slow_mcp_startup_ends_the_turn_promptly() {
    let _journey = process_journey_custody();
    let root = root("prompt-interrupt");
    prepare(&root, 200_000);
    let directory = PathBuf::from(format!("{}-slow-mcp", root.display()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap_or_else(|error| panic!("{error}"));
    let startup = CLI_REQUEST_ALLOWANCE + Duration::from_secs(5);
    let marker = format!("121.{}", std::process::id());
    let config = serde_json::json!({"mcp": {"slow": {
        "type": "local",
        "command": ["/usr/bin/sleep", marker],
        "timeout": {"startup": startup.as_millis() as u64, "catalog": 1_000},
    }}});
    declare_user_mcp(&root, &config);
    let provider = ProviderFixture::start(vec![Script::SuccessText("must never be asked")]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let body = serde_json::json!({"location": {"directory": directory}});
    let session = json(address, "POST", "/api/session", &body.to_string())["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("created session had no id"))
        .to_owned();

    // The prompt is acknowledged while MCP startup is still running, and the
    // Session is active from then on.
    let prompt = serde_json::json!({"text": "go"});
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &prompt.to_string(),
    );
    let active_by = now() + Duration::from_secs(10);
    let is_active = || {
        json(address, "GET", "/api/session/active", "")["data"]
            .as_object()
            .is_some_and(|active| active.contains_key(&session))
    };
    while !is_active() {
        assert!(
            now() < active_by,
            "the acknowledged prompt never showed as active"
        );
        thread::sleep(Duration::from_millis(20));
    }
    thread::sleep(Duration::from_secs(1));
    let interrupted_at = now();
    let (status, _) = request(
        address,
        "POST",
        &format!("/api/session/{session}/interrupt"),
        "{}",
    );
    let settled_by = interrupted_at + Duration::from_secs(5);
    while is_active() && now() < settled_by {
        thread::sleep(Duration::from_millis(20));
    }
    let settled = interrupted_at.elapsed();
    let still_active = is_active();
    let reaped_by = now() + Duration::from_secs(10);
    let mut sleeper = running_with_argument(&marker);
    while !sleeper.is_empty() && now() < reaped_by {
        thread::sleep(Duration::from_millis(100));
        sleeper = running_with_argument(&marker);
    }
    let calls = provider.calls.load(Ordering::SeqCst);

    child.stop_after_witness(&root);
    let store = EventStore::open(&root.join("database.sqlite3"), "prompt-interrupt-evidence")
        .unwrap_or_else(|error| panic!("evidence store is unreadable: {error}"));
    let interrupted: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
             AND type LIKE 'session.execution.interrupted.%'",
            [session.as_str()],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("evidence query failed: {error}"));
    drop(store);
    drop(child);
    process_fixture::cleanup(&root);
    let _ = fs::remove_dir_all(&directory);

    assert_eq!(status, 204, "the interrupt was refused");
    assert!(
        !still_active,
        "the turn was still active {settled:?} after the interrupt"
    );
    assert_eq!(interrupted, 1, "the turn did not end as interrupted");
    assert_eq!(calls, 0, "the provider was asked after the interrupt");
    assert!(
        sleeper.is_empty(),
        "the interrupted MCP startup left its server running: {sleeper:?}"
    );
}

/// The dry run (BACKLOG "Dry run, pre-Train-51"): a client whose prompt
/// reply timed out cannot know whether it was admitted. Resubmitting the SAME
/// input id is an exact retry: the product answers it with the original
/// admission and runs the work once.
#[test]
fn a_prompt_resubmitted_with_its_id_runs_once() {
    let _journey = process_journey_custody();
    let root = root("prompt-resubmit-same-id");
    prepare(&root, 200_000);
    let provider = ProviderFixture::start(vec![
        Script::SuccessText("answered once"),
        Script::SuccessText("answered twice"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let body = r#"{"id":"msg_resubmitted_once","text":"do the task once"}"#;
    let path = format!("/api/session/{session}/prompt");
    let first = json(address, "POST", &path, body);
    permission_residuals::wait_session_idle(address, &session);
    let second = json(address, "POST", &path, body);
    permission_residuals::wait_session_idle(address, &session);
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=50"),
        "",
    );
    let calls = provider.calls.load(Ordering::SeqCst);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    assert_eq!(first["data"]["id"], "msg_resubmitted_once", "{first}");
    assert_eq!(
        second["data"]["admittedSeq"], first["data"]["admittedSeq"],
        "the retry was admitted anew: {first} then {second}"
    );
    assert_eq!(calls, 1, "the retried input ran again: {history}");
    let rows = history["data"]
        .as_array()
        .unwrap_or_else(|| panic!("history is not a page: {history}"));
    let users = rows.iter().filter(|row| row["type"] == "user").count();
    assert_eq!(users, 1, "the retry duplicated the user's input: {history}");
    assert!(
        !history.to_string().contains("answered twice"),
        "a second answer was produced: {history}"
    );
}

/// A client-chosen input id orders nothing. Listings
/// follow the durable sequence, so a prompt the client named (here an id that
/// sorts before every server-minted one) lists after the server-minted prompt
/// it followed.
#[test]
fn a_client_named_prompt_lists_after_the_server_named_one_before_it() {
    let _journey = process_journey_custody();
    let root = root("prompt-client-id-order");
    prepare(&root, 200_000);
    let provider = ProviderFixture::start(vec![
        Script::SuccessText("first answer"),
        Script::SuccessText("second answer"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let path = format!("/api/session/{session}/prompt");
    let first = json(address, "POST", &path, r#"{"text":"first, server named"}"#);
    permission_residuals::wait_session_idle(address, &session);
    json(
        address,
        "POST",
        &path,
        r#"{"id":"msg_0000client","text":"second, client named"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=50"),
        "",
    );
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    let server_named = first["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("the first prompt has no id: {first}"));
    assert!(
        "msg_0000client" < server_named,
        "the witness id must sort first: {server_named}"
    );
    let users: Vec<&str> = history["data"]
        .as_array()
        .unwrap_or_else(|| panic!("history is not a page: {history}"))
        .iter()
        .filter(|row| row["type"] == "user")
        .filter_map(|row| row["id"].as_str())
        .collect();
    assert_eq!(users, [server_named, "msg_0000client"], "{history}");
}
