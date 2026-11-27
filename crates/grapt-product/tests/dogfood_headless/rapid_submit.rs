//! Rapid submissions on one busy Session.
//!
//! A wake owner parked behind a running turn used to spend the Session's
//! execution allowance while it waited, so the fifth rapid submit was refused
//! with a 409 before its input was ever admitted. The allowance now belongs to
//! the running owner, so a burst is admitted and every submit settles.
use super::*;

const RAPID: usize = 8;

#[test]
fn rapid_submits_on_a_busy_session_are_admitted_and_all_settle() {
    let _journey = process_journey_custody();
    let root = root("rapid-submit");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 200_000);
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    // The first turn is held open so the rest of the burst arrives while it is
    // still running, which is the load that exhausted the allowance.
    let mut scripts = vec![Script::HeldOutput {
        bytes: 64,
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    }];
    scripts.extend((0..RAPID).map(|_| Script::SuccessText("rapid answer")));
    let provider = ProviderFixture::start(scripts);
    let mut service = spawn(&root, provider.address);
    let address = await_ready(&root, &mut service);
    let session = create(address);
    let prompt_path = format!("/api/session/{session}/prompt");

    let first = serde_json::json!({
        "id": "msg_rapid_0",
        "text": "first turn",
        "delivery": "queue",
        "resume": true,
    });
    let (status, body) = request(address, "POST", &prompt_path, &first.to_string());
    assert_eq!(
        status,
        200,
        "first prompt: {}",
        String::from_utf8_lossy(&body)
    );
    entered.wait();

    // The burst arrives while the first turn is still running.
    for ordinal in 1..=RAPID {
        let submit = serde_json::json!({
            "id": format!("msg_rapid_{ordinal}"),
            "text": format!("rapid submit {ordinal}"),
            "delivery": "queue",
            "resume": true,
        });
        let (status, body) = request(address, "POST", &prompt_path, &submit.to_string());
        assert_eq!(
            status,
            200,
            "rapid submit {ordinal} was refused: {}",
            String::from_utf8_lossy(&body)
        );
    }
    release.wait();

    // Every submitted input is admitted exactly once and the Session settles.
    permission_residuals::wait_session_idle(address, &session);
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    );
    let admitted = history["data"]
        .as_array()
        .unwrap_or_else(|| panic!("history page absent"))
        .iter()
        .filter(|row| row["type"] == "user")
        .count();
    assert_eq!(
        admitted,
        RAPID + 1,
        "an admitted rapid submit was lost from history"
    );
}
