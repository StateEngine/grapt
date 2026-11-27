//! SESSION-RECENT-PARITY-01 J02: three successive compactions through the
//! production path. Each committed compaction publishes the frozen oracle's
//! bounded `recent` value on the live `session.compaction.ended` notice,
//! projects the same value into its completed history row, and reproduces it
//! byte-exactly after a fresh-process restart. The first window is the
//! oracle's legitimately empty branch; the second and third pin hand-verified
//! non-empty literals, so a constant-empty `recent` cannot release this lane.
//!
//! Hand derivation of the expected values (frozen oracle, KEEP 8_000 tokens):
//! each window's conversation at its compaction is [rebuild-text, user, one
//! settled assistant message with two 60_000-byte reads]. That totals well
//! under 8_000 tokens, so the walk keeps every entry, the split pulls back to
//! the user, and the published recent is [user, settled message]. In the first
//! window the user is the first entry, so the split stays at zero, the head is
//! empty, no previous recent exists, and the oracle's summarize-recent branch
//! publishes the only legitimate empty value.

use super::compaction_continuity::{compact_once, compact_once_raw, compactions};
use super::*;

const WINDOW_BYTES: usize = 60_000;
const FIRST_WINDOW: [(&str, &str); 2] =
    [("call_srp_a1", "rp-a-1.txt"), ("call_srp_a2", "rp-a-2.txt")];
const SECOND_WINDOW: [(&str, &str); 2] =
    [("call_srp_b1", "rp-b-1.txt"), ("call_srp_b2", "rp-b-2.txt")];
const THIRD_WINDOW: [(&str, &str); 2] =
    [("call_srp_c1", "rp-c-1.txt"), ("call_srp_c2", "rp-c-2.txt")];
const SECOND_PROMPT: &str = "read the second window";
const THIRD_PROMPT: &str = "read the third window";
const F3_FIRST_WINDOW: [(&str, &str); 2] = [
    ("call_srp_f3_a1", "rp-f3-a-1.txt"),
    ("call_srp_f3_a2", "rp-f3-a-2.txt"),
];
const F3_SECOND_WINDOW: [(&str, &str); 2] = [
    ("call_srp_f3_b1", "rp-f3-b-1.txt"),
    ("call_srp_f3_b2", "rp-f3-b-2.txt"),
];
const F3_BOUNDARY_PROMPT: &str = "read the boundary window";
const SUMMARY_REQUEST: &str = "Output exactly the Markdown structure shown inside <template>";

/// Appends the UTF-16 units of a plain-text fragment to the derived vector.
fn push_units(units: &mut Vec<u16>, text: &str) {
    for unit in text.encode_utf16() {
        units.push(unit);
    }
}

/// One settled read: the call line plus the result truncated at 2_000 UTF-16
/// units with the oracle's newline marker.
fn read_part(prefix: &str, name: &str, pattern: char) -> String {
    format!(
        "[Assistant tool call]: read({{\"path\":\"{prefix}-{name}.txt\"}})\n[Tool result]: {}\n[truncated]",
        numbered_prefix(pattern as u8, WINDOW_BYTES, 2_000)
    )
}

/// The exact recent value the oracle publishes for [rebuild-text, user,
/// settled message]: the user line, a blank-line separator, then the message's
/// parts joined by newlines.
fn expected_tail(user_text: &str, prefix: &str, pattern: char) -> String {
    format!(
        "[User]: {user_text}\n\n{}\n{}",
        read_part(prefix, "1", pattern),
        read_part(prefix, "2", pattern)
    )
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn successive_compactions_publish_and_replay_the_oracle_recent_value() {
    let _journey = process_journey_custody();
    let root = root("rp-recent-parity");
    prepare(&root, 500_000);
    prepare_model_inventory(&root, 128_000);
    for (prefix, pattern) in [("a", b'a'), ("b", b'b'), ("c", b'c')] {
        for name in ["1", "2"] {
            fs::write(
                root.join(format!("rp-{prefix}-{name}.txt")),
                text_lines(pattern, WINDOW_BYTES),
            )
            .unwrap_or_else(|error| panic!("large fixture write failed: {error}"));
        }
    }
    let provider = ProviderFixture::start(vec![
        Script::ToolReads {
            calls: FIRST_WINDOW.to_vec(),
        },
        Script::Output(5_645),
        Script::SuccessText("A step done"),
        Script::ToolReads {
            calls: SECOND_WINDOW.to_vec(),
        },
        Script::Output(5_645),
        Script::SuccessText("B step done"),
        Script::ToolReads {
            calls: THIRD_WINDOW.to_vec(),
        },
        Script::Output(5_645),
        Script::SuccessText("C step done"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);

    let first = compact_once(
        address,
        &session,
        r#"{"id":"msg_srp_first","text":"read the first window","delivery":"queue","resume":true}"#,
        &FIRST_WINDOW,
    );
    let second = compact_once(
        address,
        &session,
        &format!(
            r#"{{"id":"msg_srp_second","text":"{SECOND_PROMPT}","delivery":"queue","resume":true}}"#
        ),
        &SECOND_WINDOW,
    );
    let third = compact_once(
        address,
        &session,
        &format!(
            r#"{{"id":"msg_srp_third","text":"{THIRD_PROMPT}","delivery":"queue","resume":true}}"#
        ),
        &THIRD_WINDOW,
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        9,
        "the three windows did not each run one coding attempt, one compaction summary and one rebuilt attempt"
    );
    let bodies = locked(&provider.bodies).clone();
    for (index, body) in bodies.iter().enumerate() {
        assert_eq!(
            body.contains(SUMMARY_REQUEST),
            index % 3 == 1,
            "request {index} is not the provider call the witness expects"
        );
    }

    // J01 divergence pin: a constant-empty `recent` is not parity. The first
    // window is the oracle's legitimately empty branch; the second and third
    // are hand-verified non-empty literals over the retained references.
    let expected_second = expected_tail(SECOND_PROMPT, "rp-b", 'b');
    let expected_third = expected_tail(THIRD_PROMPT, "rp-c", 'c');
    assert!(
        expected_second.contains("[truncated]"),
        "witness literal lost its truncation marker"
    );
    assert_eq!(
        first["data"]["recent"].as_str(),
        Some(""),
        "the first window is the oracle's legitimately empty branch: {first}"
    );
    assert_eq!(
        second["data"]["recent"].as_str(),
        Some(expected_second.as_str()),
        "the live recent diverges from the frozen oracle on the second window"
    );
    assert_eq!(
        third["data"]["recent"].as_str(),
        Some(expected_third.as_str()),
        "the live recent diverges from the frozen oracle on the chained third window"
    );

    let path = format!("/api/session/{session}/message");
    let page = json(address, "GET", &path, "");
    let rows = compactions(&page);
    assert_eq!(
        rows.len(),
        3,
        "public history omitted a committed compaction: {page}"
    );
    for (row, notice) in rows.iter().zip([&first, &second, &third]) {
        assert_eq!(row["status"], "completed");
        assert_eq!(row["reason"], "auto");
        assert_eq!(
            row["recent"], notice["data"]["recent"],
            "the live notice and public history disagree on recent"
        );
        assert!(
            row["summary"].as_str().is_some_and(|text| !text.is_empty()),
            "compaction row lost its summary"
        );
        assert_eq!(
            notice["id"].as_str().and_then(|id| id.strip_prefix("evt_")),
            row["id"].as_str().and_then(|id| id.strip_prefix("msg_")),
            "the live notice and public history name different compactions"
        );
        assert_eq!(notice["data"]["sessionID"], session.as_str());
    }
    let epochs = rows
        .iter()
        .map(|row| {
            row["metadata"]["epoch"]
                .as_u64()
                .unwrap_or_else(|| panic!("compaction row has no epoch: {row}"))
        })
        .collect::<Vec<_>>();
    assert_eq!(epochs[1], epochs[0] + 1, "context epochs are not monotonic");
    assert_eq!(epochs[2], epochs[1] + 1, "context epochs are not monotonic");

    child.stop_after_witness(&root);
    drop(child);

    let mut restarted = spawn(&root, provider.address);
    let address = await_ready(&root, &mut restarted);
    assert_eq!(
        json(address, "GET", &path, ""),
        page,
        "public history did not replay the compacted Session exactly after restart"
    );
    restarted.stop_after_witness(&root);
    drop(restarted);
    process_fixture::cleanup(&root);
}

/// J03: the truncation cut lands between the halves of an astral character.
/// The boundary file's first line is 1_996 'a's, one rocket (one UTF-16
/// surrogate pair) and three 'a's, then 60_000 bytes of 'a' lines; read
/// numbers it "1: ", so at 2_000 units the cut leaves a lone high surrogate in
/// the published recent. Every public path - the live frame, the message
/// list page, the single message, and a fresh-process restart - must carry
/// the frozen string wire form, never the internal unit object, and the
/// restart must replay the row byte-exactly.
#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn boundary_window_compaction_publishes_the_frozen_recent_wire_form() {
    let _journey = process_journey_custody();
    let root = root("rp-boundary-recent");
    prepare(&root, 500_000);
    prepare_model_inventory(&root, 128_000);
    for name in ["rp-f3-a-1.txt", "rp-f3-a-2.txt", "rp-f3-b-2.txt"] {
        fs::write(root.join(name), text_lines(b'a', WINDOW_BYTES))
            .unwrap_or_else(|error| panic!("fixture write failed: {error}"));
    }
    // A numbered read shows "1: " first, so 1_996 characters put the
    // rocket's high surrogate at unit 1_999 of the tool result; the line stays
    // at 2_000 characters, under the read's line cut, and the window's bytes
    // follow as ordinary lines.
    let mut boundary = vec![b'a'; 1_996];
    boundary.extend_from_slice("🚀".as_bytes());
    boundary.extend_from_slice(b"aaa\n");
    boundary.extend(text_lines(b'a', WINDOW_BYTES));
    assert!(
        boundary.len() < 64 * 1_024,
        "the boundary file must stay under the tool artifact bound"
    );
    fs::write(root.join("rp-f3-b-1.txt"), &boundary)
        .unwrap_or_else(|error| panic!("boundary fixture write failed: {error}"));
    let provider = ProviderFixture::start(vec![
        Script::ToolReads {
            calls: F3_FIRST_WINDOW.to_vec(),
        },
        Script::Output(5_645),
        Script::SuccessText("A step done"),
        Script::ToolReads {
            calls: F3_SECOND_WINDOW.to_vec(),
        },
        Script::Output(5_645),
        Script::SuccessText("B step done"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);

    let first = compact_once(
        address,
        &session,
        r#"{"id":"msg_srp_f3_first","text":"read the first window","delivery":"queue","resume":true}"#,
        &F3_FIRST_WINDOW,
    );
    assert_eq!(
        first["data"]["recent"].as_str(),
        Some(""),
        "the first window is the oracle's legitimately empty branch: {first}"
    );

    let observed = compact_once_raw(
        address,
        &session,
        &format!(
            r#"{{"id":"msg_srp_f3_boundary","text":"{F3_BOUNDARY_PROMPT}","delivery":"queue","resume":true}}"#
        ),
        &F3_SECOND_WINDOW,
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        6,
        "the two windows did not each run one coding attempt, one compaction summary and one rebuilt attempt"
    );
    let bodies = locked(&provider.bodies).clone();
    for (index, body) in bodies.iter().enumerate() {
        assert_eq!(
            body.contains(SUMMARY_REQUEST),
            index % 3 == 1,
            "request {index} is not the provider call the witness expects"
        );
    }

    // The live wire form: exactly one compaction frame, carrying the recent
    // as a JSON string whose truncation cut is the exact unit escape of the
    // lone high surrogate, never the raw-value token's debug form.
    let mut frames: Vec<String> = observed
        .lines()
        .filter(|line| {
            line.strip_prefix("data: ")
                .is_some_and(|encoded| encoded.contains("\"type\":\"session.compaction.ended\""))
        })
        .map(str::to_owned)
        .collect();
    assert_eq!(
        frames.len(),
        1,
        "the raw stream must carry exactly one compaction frame"
    );
    let frame = frames.remove(0);
    assert!(
        frame.contains(&format!(
            "[Tool result]: 1: {}\\ud83d\\n[truncated]",
            "a".repeat(1_996)
        )),
        "the live recent lost the exact unit escape at the truncation cut"
    );
    assert!(
        !frame.contains("$serde_json::private::RawValue"),
        "the live recent leaked the raw-value token form"
    );

    let path = format!("/api/session/{session}/message");
    let (status, page_bytes) = request(address, "GET", &path, "");
    assert_eq!(status, 200, "the boundary page request failed");
    let page_body = String::from_utf8_lossy(&page_bytes).into_owned();
    // The boundary row carries the frozen string wire form, whose lone-
    // surrogate cut escape no strict parser can re-read. The structural
    // assertions run on a copy with that exact escape neutralized; the
    // wire-form assertions stay on the raw body.
    let structural = page_body.replace("\\ud83d", "\\ufffd");
    let page = serde_json::from_str(&structural)
        .unwrap_or_else(|error| panic!("the boundary page was not JSON: {error}"));
    let rows = compactions(&page);
    assert_eq!(
        rows.len(),
        2,
        "public history omitted a committed compaction: {page}"
    );
    for row in &rows {
        assert_eq!(row["status"], "completed");
        assert_eq!(row["reason"], "auto");
    }
    assert_eq!(
        rows[0]["recent"], first["data"]["recent"],
        "the first window's live notice and public history disagree on recent"
    );

    // The hand-derived units: the user line, the blank-line separator, then
    // the two settled read pairs. The first result's 2_000-unit cut lands on
    // the high half of the rocket pair, so the public value is the frozen
    // string wire form with the lone surrogate escaped verbatim.
    let mut units: Vec<u16> = Vec::new();
    push_units(&mut units, "[User]: read the boundary window\n\n");
    push_units(
        &mut units,
        "[Assistant tool call]: read({\"path\":\"rp-f3-b-1.txt\"})\n[Tool result]: ",
    );
    push_units(&mut units, "1: ");
    units.extend(std::iter::repeat_n(0x0061, 1_996));
    units.push(0xD83D);
    push_units(&mut units, "\n[truncated]\n");
    push_units(
        &mut units,
        "[Assistant tool call]: read({\"path\":\"rp-f3-b-2.txt\"})\n[Tool result]: ",
    );
    push_units(&mut units, &numbered_prefix(b'a', WINDOW_BYTES, 2_000));
    push_units(&mut units, "\n[truncated]");
    let expected = grapt_api::u3a::Utf16Text::try_new(units)
        .unwrap_or_else(|error| panic!("hand-derived units exceed the dto bound: {error}"));
    assert!(
        expected.as_str().is_none(),
        "the hand-derived units must carry a lone surrogate"
    );
    // The exact wire fragment the frozen oracle emits at the cut.
    let mut fragment = String::from("[Tool result]: 1: ");
    fragment.extend(std::iter::repeat_n('a', 1_996));
    fragment.push_str("\\ud83d\\n[truncated]");
    assert!(
        page_body.contains(&fragment),
        "the public history row left the frozen string wire form"
    );
    assert!(
        !page_body.contains("secode.unit16"),
        "the unit object leaked onto the public history row"
    );

    // The single-message path serves the same row and the same wire form.
    let Some(message_id) = rows[1]["id"].as_str() else {
        panic!("the compaction row lost its id")
    };
    let get_path = format!("/api/session/{session}/message/{message_id}");
    let (status, get_bytes) = request(address, "GET", &get_path, "");
    assert_eq!(status, 200, "the boundary message request failed");
    let get_body = String::from_utf8_lossy(&get_bytes).into_owned();
    assert!(
        get_body.contains(&fragment),
        "the single message left the frozen string wire form"
    );

    child.stop_after_witness(&root);
    drop(child);

    let mut restarted = spawn(&root, provider.address);
    let address = await_ready(&root, &mut restarted);
    let (status, replay_bytes) = request(address, "GET", &path, "");
    assert_eq!(status, 200, "the replayed page request failed");
    assert_eq!(
        replay_bytes, page_bytes,
        "public history did not replay the boundary compaction exactly after restart"
    );
    restarted.stop_after_witness(&root);
    drop(restarted);
    process_fixture::cleanup(&root);
}
