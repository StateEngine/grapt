//! Resolved preset reads and selection during a real running execution.
use super::*;

fn select(address: SocketAddr, session: &str, preset: &str) {
    let (status, body) = request(
        address,
        "POST",
        &format!("/api/session/{session}/preset"),
        &serde_json::json!({"preset":preset}).to_string(),
    );
    assert_eq!(status, 204, "{}", String::from_utf8_lossy(&body));
}

fn resolved(address: SocketAddr, session: &str) -> serde_json::Value {
    json(
        address,
        "GET",
        &format!("/api/session/{session}/preset"),
        "",
    )["data"]
        .clone()
}

fn event_count(root: &Path) -> i64 {
    let database = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap_or_else(|error| panic!("preset evidence database unreadable: {error}"));
    database
        .query_row("SELECT COUNT(*) FROM event", [], |row| row.get(0))
        .unwrap_or_else(|error| panic!("preset event count unreadable: {error}"))
}

#[test]
#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
fn preset_read_distinguishes_selection_from_config_and_writes_nothing() {
    let _journey = process_journey_custody();
    let root = root("preset-resolved-read");
    prepare(&root, 200_000);
    let project = root.join("project");
    fs::create_dir(&project).unwrap_or_else(|error| panic!("project: {error}"));
    let provider = ProviderFixture::start(Vec::new());
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({"location":{"directory":project}}).to_string(),
    )["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("created Session lacks id"))
        .to_owned();
    let initial = event_count(&root);
    assert_eq!(
        resolved(address, &session),
        serde_json::json!({"selected":null,"effective":"maintainer"})
    );
    let config = root.join("service.conf");
    let mut profile =
        fs::read_to_string(&config).unwrap_or_else(|error| panic!("profile: {error}"));
    profile.push_str("permission_preset=author\n");
    fs::write(&config, &profile).unwrap_or_else(|error| panic!("profile: {error}"));
    assert_eq!(
        resolved(address, &session),
        serde_json::json!({"selected":null,"effective":"author"})
    );
    fs::write(
        project.join("grapt.json"),
        r#"{"permission_preset":"inspect"}"#,
    )
    .unwrap_or_else(|error| panic!("project config: {error}"));
    assert_eq!(
        resolved(address, &session),
        serde_json::json!({"selected":null,"effective":"inspect"})
    );
    assert_eq!(
        event_count(&root),
        initial,
        "a preset GET changed durable truth"
    );
    select(address, &session, "maintainer");
    let selected = event_count(&root);
    assert_eq!(
        selected,
        initial + 1,
        "selection must commit exactly one event"
    );
    assert_eq!(
        resolved(address, &session),
        serde_json::json!({"selected":"maintainer","effective":"maintainer"})
    );
    assert_eq!(
        fs::read_to_string(&config).unwrap_or_else(|error| panic!("profile: {error}")),
        profile,
        "selection rewrote profile defaults"
    );
    assert_eq!(
        fs::read_to_string(project.join("grapt.json"))
            .unwrap_or_else(|error| panic!("project: {error}")),
        r#"{"permission_preset":"inspect"}"#,
        "selection rewrote repository defaults"
    );
    let (status, _) = request(address, "GET", "/api/session/ses_missing/preset", "");
    assert_eq!(
        status, 404,
        "missing Session was displayed as default trust"
    );
    let (status, _) = request(
        address,
        "GET",
        &format!("/api/session/{session}/preset?unknown=1"),
        "",
    );
    assert_eq!(status, 400);
    // A directory where a config file belongs is an actual read failure,
    // rather than absence or an unknown preset spelling.
    fs::remove_file(project.join("grapt.json")).unwrap_or_else(|error| panic!("config: {error}"));
    fs::create_dir(project.join("grapt.json")).unwrap_or_else(|error| panic!("config: {error}"));
    let (status, body) = request(
        address,
        "GET",
        &format!("/api/session/{session}/preset"),
        "",
    );
    assert_eq!(
        status,
        400,
        "unreadable config became apparent maintainer trust: {}",
        String::from_utf8_lossy(&body)
    );
    assert!(
        String::from_utf8_lossy(&body).contains("grapt.json"),
        "read error lost its config path"
    );
    assert_eq!(
        event_count(&root),
        selected,
        "resolved or failed reads appended events"
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        0,
        "display started an execution"
    );
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}

fn tools(body: &str) -> std::collections::BTreeSet<String> {
    let request: serde_json::Value =
        serde_json::from_str(body).unwrap_or_else(|error| panic!("provider request: {error}"));
    request["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("provider tool list absent"))
        .iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .unwrap_or_else(|| panic!("tool name absent"))
                .to_owned()
        })
        .collect()
}

#[test]
fn selection_during_execution_applies_only_to_the_next_execution() {
    let _journey = process_journey_custody();
    let root = root("preset-running-selection");
    prepare(&root, 200_000);
    fs::write(root.join("ordinary.txt"), "ordinary repository input")
        .unwrap_or_else(|error| panic!("ordinary input: {error}"));
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let provider = ProviderFixture::start(vec![
        Script::HeldReads {
            calls: vec![("call_captured_preset", "ordinary.txt")],
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        },
        Script::Success,
        Script::Success,
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    select(address, &session, "maintainer");
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"read the ordinary input"}"#,
    );
    let _ = entered.wait();
    let running = tools(&locked(&provider.bodies)[0]);
    assert!(
        running.contains("write"),
        "maintainer execution was not offered write"
    );
    select(address, &session, "inspect");
    assert_eq!(
        resolved(address, &session),
        serde_json::json!({"selected":"inspect","effective":"inspect"})
    );
    let _ = release.wait();
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "current execution did not continue after its held tool"
    );
    assert_eq!(
        tools(&locked(&provider.bodies)[1]),
        running,
        "selection changed an execution's captured tool list"
    );
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"a later execution"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    let bodies = locked(&provider.bodies).clone();
    assert_eq!(bodies.len(), 3, "later execution was not observed");
    let next = tools(&bodies[2]);
    assert!(
        !next.contains("write"),
        "later execution resurrected a denied tool"
    );
    assert!(next.contains("read"), "later inspect execution lost read");
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}
