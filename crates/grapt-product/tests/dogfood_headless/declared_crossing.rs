//! SC-COMPACT-02: promoted inputs that carry a Session past the request's
//! 4 MiB byte bound trigger a structural compaction, and the compactor's own
//! read must hold the window that trigger admits. Before, the recent
//! projection charged the same window against the same 4 MiB and refused it,
//! so automatic compaction paused and the Session could not continue.

use super::*;

/// The request's 4 MiB byte bound (`MAX_CONTEXT_BYTES`): journeys cross it
/// with real promoted inputs, each under the prompt cap.
pub(super) const DECLARED_CONTEXT_BYTES: usize = 4 * 1024 * 1024;
/// One promoted input. Sixty-four of them sit 64,512 bytes under the bound,
/// room for the framing of the request before the crossing; the sixty-fifth
/// crosses it.
pub(super) const DECLARED_INPUT_BYTES: usize = 64_528;
pub(super) const DECLARED_INPUTS: usize = DECLARED_CONTEXT_BYTES / DECLARED_INPUT_BYTES + 1;
const _: () = assert!(DECLARED_INPUT_BYTES <= grapt_api::u3a::limits::MAX_PROMPT_TEXT_BYTES);
const _: () = assert!((DECLARED_INPUTS - 1) * DECLARED_INPUT_BYTES <= DECLARED_CONTEXT_BYTES);
const _: () = assert!(DECLARED_INPUTS * DECLARED_INPUT_BYTES > DECLARED_CONTEXT_BYTES);
/// A model window no request under the byte bound presses, so the trigger
/// is the structural bound, never token pressure.
pub(super) const DECLARED_CONTEXT_WINDOW: u64 = 16_000_000;

/// `label`, then padding to exactly [`DECLARED_INPUT_BYTES`].
pub(super) fn declared_input(label: &str) -> String {
    let mut text = format!("{label} ");
    text.extend(std::iter::repeat_n('x', DECLARED_INPUT_BYTES - text.len()));
    text
}

const SUMMARY_REQUEST: &str = "Output exactly the Markdown structure shown inside <template>";

/// Durable payloads of one event type, in order.
fn events(root: &Path, kind: &str) -> Vec<serde_json::Value> {
    let connection = rusqlite::Connection::open(root.join("database.sqlite3"))
        .unwrap_or_else(|error| panic!("evidence store is unreadable: {error}"));
    let mut query = connection
        .prepare("SELECT data FROM event WHERE type = ?1 ORDER BY seq")
        .unwrap_or_else(|error| panic!("event query failed: {error}"));
    query
        .query_map([kind], |row| row.get::<_, String>(0))
        .unwrap_or_else(|error| panic!("event read failed: {error}"))
        .map(|data| {
            serde_json::from_str(&data.unwrap_or_else(|error| panic!("row failed: {error}")))
                .unwrap_or_else(|error| panic!("event data is not JSON: {error}"))
        })
        .collect()
}

#[test]
fn promoted_inputs_past_the_byte_bound_compact_reopen_and_continue() {
    let _journey = process_journey_custody();
    let root = root("declared-crossing");
    prepare(&root, 700_000);
    prepare_model_inventory(&root, DECLARED_CONTEXT_WINDOW);
    let mut scripts = vec![Script::Success; DECLARED_INPUTS - 1];
    // The crossing's summary and rebuilt attempt, then one ordinary attempt
    // after a restart.
    scripts.extend([
        Script::SuccessText(OBJECTIVE_SUMMARY),
        Script::Success,
        Script::Success,
    ]);
    let provider = ProviderFixture::start(scripts);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = json(address, "POST", "/api/session", "{}");
    let id = session["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("missing Session id"))
        .to_owned();
    let prompt = |address, input: &str, text: &str| {
        json(
            address,
            "POST",
            &format!("/api/session/{id}/prompt"),
            &format!(r#"{{"id":"{input}","text":"{text}","delivery":"queue","resume":true}}"#),
        );
    };
    // A queued prompt acknowledges input admission, not execution.
    let wait_for_completion = |address, expected| {
        let called = permission_residuals::wait_until(
            client_allowance::CLI_REQUEST_ALLOWANCE,
            permission_residuals::SETTLE_POLL,
            || provider.calls.load(Ordering::SeqCst) >= expected,
        );
        assert!(called.is_some(), "provider call {expected} did not start");
        permission_residuals::wait_session_idle(address, &id);
        assert_eq!(provider.calls.load(Ordering::SeqCst), expected);
    };
    for index in 0..DECLARED_INPUTS {
        prompt(
            address,
            &format!("msg_crossing_{index:03}"),
            &declared_input(&format!("promoted input {index}")),
        );
        let expected = if index + 1 < DECLARED_INPUTS {
            index + 1
        } else {
            DECLARED_INPUTS + 1
        };
        wait_for_completion(address, expected);
    }
    let bodies = locked(&provider.bodies).clone();
    assert!(
        bodies[..DECLARED_INPUTS - 1]
            .iter()
            .all(|body| !body.contains(SUMMARY_REQUEST)),
        "a request under the byte bound compacted"
    );
    assert!(
        bodies[DECLARED_INPUTS - 1].contains(SUMMARY_REQUEST),
        "the crossing input skipped the actual compactor"
    );
    let current = format!("promoted input {} ", DECLARED_INPUTS - 1);
    assert!(
        bodies[DECLARED_INPUTS].contains("Model-written continuation summary")
            && bodies[DECLARED_INPUTS].contains(&current),
        "the rebuilt request lost its labelled summary or the crossing input"
    );
    let ended = events(&root, "session.compaction.ended.2");
    assert_eq!(ended.len(), 1, "not exactly one compaction: {ended:?}");
    assert_eq!(ended[0]["trigger"], "structuralBound");
    assert!(events(&root, "session.compaction.failed.1").is_empty());
    let path = format!("/api/session/{id}/message");
    let live = compaction_continuity::compactions(&json(address, "GET", &path, ""));
    assert_eq!(live.len(), 1, "public history omitted the compaction");
    assert_eq!(live[0]["status"], "completed");
    child.stop_after_witness(&root);
    drop(child);

    // Reopen: public history replays the committed lineage through the same
    // projection, and reproduces the value it published.
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let reopened = compaction_continuity::compactions(&json(address, "GET", &path, ""));
    assert_eq!(
        reopened, live,
        "the replayed compaction diverged after reopen"
    );
    prompt(address, "msg_crossing_after", "continue after the crossing");
    wait_for_completion(address, DECLARED_INPUTS + 2);
    let bodies = locked(&provider.bodies).clone();
    let after = &bodies[DECLARED_INPUTS + 1];
    assert!(
        !after.contains(SUMMARY_REQUEST)
            && after.contains("Model-written continuation summary")
            && after.contains("continue after the crossing"),
        "the Session did not continue on its compacted context"
    );
    assert_eq!(events(&root, "session.compaction.ended.2").len(), 1);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}
