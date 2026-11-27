//! Real product 4xx reasons, discovery hints, and service-owned file lifetime.
use super::*;

/// Drive one refused turn whose provider body carries private text, then read
/// the public history row the reader is shown and assert it does not contain it.
fn failed_turn_hides(body: &'static str, forbidden: &str, label: &str) {
    let _journey = process_journey_custody();
    let root = root(label);
    prepare(&root, 200_000);
    let provider = ProviderFixture::start(vec![Script::Refused { status: 400, body }]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":"msg_private_refusal","text":"Say hello.",
            "delivery":"queue","resume":true})
        .to_string(),
    );
    permission_residuals::wait_session_idle(address, &session);
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=50"),
        "",
    );
    let rows = history["data"]
        .as_array()
        .unwrap_or_else(|| panic!("history is not a page: {history}"))
        .iter()
        .filter(|row| row["metadata"]["executionFailure"] == true)
        .filter_map(|row| row["text"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    assert!(
        !rows.is_empty(),
        "the failed execution must have a public row"
    );
    for row in rows {
        assert!(
            !row.contains(forbidden),
            "private provider text reached the reader: {row}"
        );
    }
}

#[test]
fn a_refused_turn_hides_an_escaped_windows_home() {
    failed_turn_hides(
        r#"{"error":{"code":"invalid_request_error","message":"cannot read C:\\Users\\private_profile\\project\\file"}}"#,
        "private_profile",
        "private-refusal-home",
    );
}

#[test]
fn a_refused_turn_hides_a_query_value() {
    failed_turn_hides(
        r#"{"error":{"code":"invalid_request_error","message":"failed URL https://example.test/?session_id=private-session-marker"}}"#,
        "private-session-marker",
        "private-refusal-query",
    );
}

/// The public row a reader sees, one test per punctuation a reviewer raised.
/// Every character here is legal inside a query value, so the value must not
/// survive the mask; separate cases keep a failure naming the character that
/// broke instead of the first one in a list.
#[test]
fn review_error_refused_turn_hides_a_query_value_with_a_comma() {
    failed_turn_hides(
        r#"{"error":{"code":"invalid_request_error","message":"failed URL https://example.test/?opaque=prefix,private-query-tail"}}"#,
        "private-query-tail",
        "private-refusal-query-comma",
    );
}

#[test]
fn review_error_refused_turn_hides_a_query_value_with_a_parenthesis() {
    failed_turn_hides(
        r#"{"error":{"code":"invalid_request_error","message":"failed URL https://example.test/?opaque=prefix)private-query-tail"}}"#,
        "private-query-tail",
        "private-refusal-query-parenthesis",
    );
}

#[test]
fn review_error_refused_turn_hides_a_query_value_with_a_semicolon() {
    failed_turn_hides(
        r#"{"error":{"code":"invalid_request_error","message":"https://example.test/?opaque=prefix;private-query-tail"}}"#,
        "private-query-tail",
        "private-refusal-query-semicolon",
    );
}

#[test]
fn review_error_refused_turn_hides_a_query_value_with_a_percent_escape() {
    failed_turn_hides(
        r#"{"error":{"code":"invalid_request_error","message":"https://example.test/?opaque=prefix%2Cprivate-query-tail"}}"#,
        "private-query-tail",
        "private-refusal-query-escape",
    );
}

#[test]
fn review_error_refused_turn_hides_a_query_value_with_an_apostrophe() {
    failed_turn_hides(
        r#"{"error":{"code":"invalid_request_error","message":"https://example.test/?opaque=prefix'private-query-tail"}}"#,
        "private-query-tail",
        "private-refusal-query-apostrophe",
    );
}

fn wait_saved(path: &Path, expected: &str) -> String {
    let wait = PollDeadline::new();
    while wait.pending() {
        match fs::read_to_string(path) {
            Ok(text) if text.contains(expected) => return text,
            Ok(_) => (), // The prior last-error record until atomic replacement.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => panic!("error-text record unreadable: {error}"),
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "last-error record was not saved ({wait}): {}",
        path.display()
    );
}

fn provider_notices(history: &serde_json::Value) -> Vec<&serde_json::Value> {
    history["data"]
        .as_array()
        .unwrap_or_else(|| panic!("history is not a page: {history}"))
        .iter()
        .filter(|row| row["metadata"]["providerErrorDetail"] == true)
        .collect()
}
fn assert_no_vendor_rows(root: &Path, forbidden: &[&str]) {
    let store = EventStore::open(&root.join("database.sqlite3"), "error-text-evidence")
        .unwrap_or_else(|error| panic!("missing durable evidence: {error}"));
    for text in forbidden {
        let count: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM event WHERE instr(data, ?1) > 0",
                [text],
                |row| row.get(0),
            )
            .unwrap_or_else(|error| panic!("event scan failed: {error}"));
        assert_eq!(count, 0, "vendor text or credential entered a durable row");
    }
}

#[test]
fn failed_body_preserves_last_error_and_valid_empty_body_reports_saved() {
    let _journey = process_journey_custody();
    let root = root("error-text-body-failed");
    prepare(&root, 200_000);
    let mut conf =
        fs::read_to_string(root.join("service.conf")).unwrap_or_else(|error| panic!("{error}"));
    conf.push_str("provider_error_text=true\n");
    fs::write(root.join("service.conf"), conf).unwrap_or_else(|error| panic!("{error}"));
    let path = root.join("logs/provider-error.txt");
    fs::create_dir(root.join("logs")).unwrap_or_else(|error| panic!("{error}"));
    fs::write(&path, "preceding valid diagnostic").unwrap_or_else(|error| panic!("{error}"));
    let provider = ProviderFixture::start(vec![
        Script::TruncatedRefusal { status: 400 },
        Script::Refused {
            status: 400,
            body: "",
        },
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    for index in 0..2 {
        json(address, "POST", &format!("/api/session/{session}/prompt"),
            &serde_json::json!({"id":format!("msg_error_body_{index}"),"text":"Say hello.","delivery":"queue","resume":true}).to_string());
        permission_residuals::wait_session_idle(address, &session);
        let history = json(
            address,
            "GET",
            &format!("/api/session/{session}/message?order=asc&limit=50"),
            "",
        );
        let notices = provider_notices(&history);
        assert_eq!(notices.len(), index + 1);
        let notice = notices[index]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("missing notice text"));
        let saved = fs::read_to_string(&path).unwrap_or_else(|error| panic!("{error}"));
        if index == 0 {
            assert!(notice.contains("could not be saved"), "{notice}");
            assert_eq!(saved, "preceding valid diagnostic");
            let log = wait_saved(
                &root.join("product-stderr-0.log"),
                "provider error-text body was unavailable",
            );
            assert!(log.contains("HTTP 400") && !log.contains("details saved to"));
        } else {
            assert!(notice.contains("details saved to"), "{notice}");
            assert_eq!(saved, "");
        }
    }
    let diagnostic = wait_saved(&root.join("diagnostics/provider-errors.jsonl"), "400");
    let attempts: Vec<serde_json::Value> = diagnostic
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|error| panic!("{error}")))
        .collect();
    assert_eq!(attempts.len(), 2);
    assert!(attempts.iter().all(|attempt| attempt["status"] == 400));
    assert_no_vendor_rows(&root, &["preceding valid diagnostic", "provider-secret"]);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn opt_in_saves_only_last_masked_error_and_survives_product_teardown() {
    let _journey = process_journey_custody();
    for enabled in [false, true] {
        let root = root(if enabled {
            "error-text-on"
        } else {
            "error-text-off"
        });
        prepare(&root, 200_000);
        if enabled {
            let mut conf = fs::read_to_string(root.join("service.conf"))
                .unwrap_or_else(|e| panic!("config read: {e}"));
            conf.push_str("provider_error_text=true\n");
            fs::write(root.join("service.conf"), conf)
                .unwrap_or_else(|e| panic!("config write: {e}"));
        }
        let provider = ProviderFixture::start(vec![
            Script::Refused {
                status: 400,
                body: "first vendor body provider-secret sk-other-secret",
            },
            Script::Refused {
                status: 400,
                body: "second vendor body provider-secret",
            },
        ]);
        let mut child = spawn(&root, provider.address);
        let address = await_ready(&root, &mut child);
        let session = create(address);
        let path = root.join("logs/provider-error.txt");
        for (index, expected) in ["first vendor body", "second vendor body"]
            .iter()
            .enumerate()
        {
            let body = serde_json::json!({"id":format!("msg_error_text_{index}"), "text":"Say hello.", "delivery":"queue", "resume":true});
            json(
                address,
                "POST",
                &format!("/api/session/{session}/prompt"),
                &body.to_string(),
            );
            permission_residuals::wait_session_idle(address, &session);
            let history = json(
                address,
                "GET",
                &format!("/api/session/{session}/message?order=asc&limit=50"),
                "",
            );
            let events = history.to_string();
            assert_no_vendor_rows(&root, &["provider-secret", "sk-other-secret"]);
            let notices = provider_notices(&history);
            // A notice is the opt-in file's own record. With the opt-in off the
            // run records none — the failure row carries the cause instead —
            // so the count follows the flag rather than always counting.
            assert_eq!(
                notices.len(),
                if enabled { index + 1 } else { 0 },
                "notices follow the opt-in: {history}"
            );
            assert!(!events.contains("provider-secret") && !events.contains("sk-other-secret"));
            // The failure detail states the masked reason with or without the flag.
            let detail = wait_saved(
                &root.join("product-stderr-0.log"),
                &format!("HTTP 400: {expected}"),
            );
            assert!(!detail.contains("provider-secret") && !detail.contains("sk-other-secret"));
            if enabled {
                assert!(
                    events.contains("provider-error.txt"),
                    "missing file-path hint: {events}"
                );
                let saved = wait_saved(&path, expected);
                assert!(
                    saved.len() <= 512
                        && !saved.contains("provider-secret")
                        && !saved.contains("sk-other-secret")
                );
                if index == 1 {
                    assert!(!saved.contains("first vendor body"));
                }
            } else {
                // The row shows the cause by default and never advertises the
                // setting; nothing is written to a file that is not opted in.
                assert!(
                    !events.contains("provider_error_text=true"),
                    "the failure row must not carry the opt-in hint: {events}"
                );
                assert!(!path.exists());
            }
        }
        child.stop_after_witness(&root);
        drop(child);
        if enabled {
            let saved = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("record lost after teardown: {e}"));
            assert!(saved.contains("second vendor body") && !saved.contains("provider-secret"));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(&path)
                        .unwrap_or_else(|e| panic!("{e}"))
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
        }
        process_fixture::cleanup(&root);
    }
}

#[test]
fn a_failed_save_is_loud_without_replacing_the_provider_failure() {
    let _journey = process_journey_custody();
    let root = root("error-text-save-failed");
    prepare(&root, 200_000);
    let mut conf = fs::read_to_string(root.join("service.conf")).unwrap_or_else(|e| panic!("{e}"));
    conf.push_str("provider_error_text=true\n");
    fs::write(root.join("service.conf"), conf).unwrap_or_else(|e| panic!("{e}"));
    fs::write(root.join("logs"), "unrelated existing file").unwrap_or_else(|e| panic!("{e}"));
    let provider = ProviderFixture::start(vec![Script::Refused {
        status: 400,
        body: "private provider-secret",
    }]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let body = serde_json::json!({"id":"msg_error_text_io", "text":"Say hello.", "delivery":"queue", "resume":true});
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &body.to_string(),
    );
    permission_residuals::wait_session_idle(address, &session);
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=50"),
        "",
    );
    let notices = provider_notices(&history);
    assert_eq!(notices.len(), 1);
    assert!(notices[0]["text"]
        .as_str()
        .is_some_and(|text| text.contains("could not be saved")));
    assert_no_vendor_rows(&root, &["private provider-secret"]);
    let log = root.join("product-stderr-0.log");
    let diagnostic = wait_saved(&log, "provider error-text save failed");
    assert!(!diagnostic.contains("details saved to") && !diagnostic.contains("provider-secret"));
    let original = wait_saved(&log, "HTTP 400");
    assert!(original.contains("the provider returned HTTP 400"));
    child.stop_after_witness(&root);
    drop(child);
    assert_eq!(
        fs::read_to_string(root.join("logs")).unwrap_or_else(|e| panic!("{e}")),
        "unrelated existing file"
    );
    process_fixture::cleanup(&root);
}

#[test]
fn refusals_show_status_code_and_bounded_reason_by_default() {
    let _journey = process_journey_custody();
    // No opt-in in this run's service.conf: the row must still carry the cause.
    // Only a terminal refusal can settle as a failure row inside a test's
    // bound: 429 and 5xx are retry classes, so the run keeps retrying. Their
    // row text is asserted at the adapter seam, through the same renderer this
    // headless case proves end to end at 400.
    for (index, (status, code, message, body)) in [
        (400u16, "invalid_request_error", "the request shape was rejected",
         r#"{"error":{"type":"invalid_request_error","code":"invalid_request_error","message":"the request shape was rejected"}}"#),
    ]
    .into_iter()
    .enumerate()
    {
        let root = root(&format!("error-text-default-{status}"));
        prepare(&root, 200_000);
        let provider = ProviderFixture::start(vec![Script::Refused { status, body }]);
        let mut child = spawn(&root, provider.address);
        let address = await_ready(&root, &mut child);
        let session = create(address);
        let prompt = serde_json::json!({
            "id": format!("msg_refusal_default_{index}"),
            "text": "Say hello.",
            "delivery": "queue",
            "resume": true,
        });
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            &prompt.to_string(),
        );
        permission_residuals::wait_session_idle(address, &session);
        let history = json(
            address,
            "GET",
            &format!("/api/session/{session}/message?order=asc&limit=50"),
            "",
        );
        let rows = history["data"]
            .as_array()
            .unwrap_or_else(|| panic!("history is not a page: {history}"))
            .iter()
            .filter(|row| row["metadata"]["executionFailure"] == true)
            .filter_map(|row| row["text"].as_str())
            .collect::<Vec<_>>();
        assert!(
            rows.iter().any(|text| text.contains(&format!("HTTP {status}"))),
            "the failure row must state the status for {status}: {history}"
        );
        assert!(
            rows.iter().any(|text| text.contains(code)),
            "the failure row must name the vendor's code for {status}: {history}"
        );
        assert!(
            rows.iter().any(|text| text.contains(message)),
            "the failure row must quote the vendor's reason for {status}: {history}"
        );
        // The default path never advertises the opt-in, and writes no file.
        assert!(!history.to_string().contains("provider_error_text=true"));
        assert!(!root.join("logs/provider-error.txt").exists());
        child.stop_after_witness(&root);
        drop(child);
        process_fixture::cleanup(&root);
    }
}

#[test]
fn retry_failures_keep_exact_attempt_identity_and_do_not_share_capture() {
    let _journey = process_journey_custody();
    let root = root("error-text-retry");
    prepare(&root, 200_000);
    let mut conf =
        fs::read_to_string(root.join("service.conf")).unwrap_or_else(|error| panic!("{error}"));
    conf.push_str("provider_error_text=true\n");
    fs::write(root.join("service.conf"), conf).unwrap_or_else(|error| panic!("{error}"));
    let provider = ProviderFixture::start(vec![
        Script::Refused {
            status: 429,
            body: "retry first body provider-secret",
        },
        Script::Refused {
            status: 400,
            body: "retry second body provider-secret",
        },
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    json(address, "POST", &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":"msg_error_retry","text":"Say hello.","delivery":"queue","resume":true}).to_string());
    permission_residuals::wait_session_idle(address, &session);
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=50"),
        "",
    );
    let notices = provider_notices(&history);
    assert_eq!(notices.len(), 2, "two failed physical attempts: {history}");
    assert_eq!(
        notices[0]["metadata"]["executionID"],
        notices[1]["metadata"]["executionID"]
    );
    assert_ne!(
        notices[0]["metadata"]["attemptID"],
        notices[1]["metadata"]["attemptID"]
    );
    let saved = wait_saved(&root.join("logs/provider-error.txt"), "retry second body");
    assert!(!saved.contains("retry first body") && !saved.contains("provider-secret"));
    let diagnostic = wait_saved(&root.join("diagnostics/provider-errors.jsonl"), "400");
    let attempts: Vec<serde_json::Value> = diagnostic
        .lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("invalid refusal evidence: {error}"))
        })
        .collect();
    assert_eq!(attempts.len(), 2);
    for (notice, refusal) in notices.iter().zip(&attempts) {
        assert_eq!(
            notice["metadata"]["attemptID"], refusal["attempt"],
            "notice was attributed to another attempt"
        );
    }
    assert_eq!(attempts[0]["status"], 429);
    assert_eq!(attempts[1]["status"], 400);
    assert_no_vendor_rows(&root, &["provider-secret"]);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}
