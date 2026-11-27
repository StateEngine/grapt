//! SC-REL-11: a location discovery that times out never parks a turn. The
//! eight-hour Windows soak on 21953b9c lost its 118th cycle this way: Git
//! discovery ran on every turn, one run outlasted its deadline, and the
//! prompt was left queued with nothing scheduled to run it.
use super::*;

/// One `grapt run` turn on `session` saying `prompt`.
fn turn(root: &Path, session: &str, prompt: &str) -> super::cli_continuity::CliRun {
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

#[test]
fn a_timed_out_location_discovery_still_runs_the_turn() {
    let _journey = process_journey_custody();
    let root = root("location-discovery-timeout");
    prepare(&root, 200_000);
    let directory = PathBuf::from(format!("{}-project", root.display()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap_or_else(|error| panic!("{error}"));
    let provider = ProviderFixture::start(vec![
        Script::SuccessText("first answered"),
        Script::SuccessText("second answered"),
    ]);
    // The first turn's discovery times out twice before it succeeds. Every
    // discovery after that times out, so a later turn that discovered again
    // instead of reusing its Session's location would be parked, as the
    // soak's was.
    let mut child = process_fixture::spawn_with(
        &root,
        provider.address,
        &[("GRAPT_TESTKIT_LOCATION_DISCOVERY_FAULTS", "1,2,4,5,6,7,8,9")],
    );
    let address = await_ready(&root, &mut child);
    let body = serde_json::json!({"location": {"directory": directory}});
    let session = json(address, "POST", "/api/session", &body.to_string())["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("created session had no id"))
        .to_owned();
    let first = turn(&root, &session, "first");
    let second = turn(&root, &session, "second");
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

    for (run, name) in [(&first, "first"), (&second, "second")] {
        assert_eq!(
            run.code,
            Some(0),
            "the {name} turn did not run: {}",
            run.stderr
        );
        assert!(
            !run.stderr.contains("Not run yet"),
            "the {name} turn was parked: {}",
            run.stderr
        );
    }
    assert_eq!(requests, 2, "both prompts were answered once each");
    let rows = history["data"]
        .as_array()
        .unwrap_or_else(|| panic!("history is not a page: {history}"));
    assert!(
        !rows.iter().any(|row| row["metadata"]["notRunYet"] == true),
        "a turn was recorded as not run yet: {history}"
    );
}
