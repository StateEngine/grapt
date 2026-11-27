//! Round-trip witnesses against the ACTUAL exported DTOs.
//!
//! These are the witnesses review blocker 1 asked for. Each encodes through the
//! real codec and decodes through it again, so a key that drifted from the
//! contract cannot survive — `decode_request` refuses it.
//!
//! The mutants are the important half. Each reverts one key or identity to the
//! competing handwritten shape the previous revision used, and requires the
//! real decoder to refuse it. Those are exactly the payloads that used to pass
//! every in-crate test while being incompatible with the admitted surface.

use super::*;

#[test]
fn mixed_files_keep_order_without_inlining_bytes_or_paths() -> Result<(), Box<dyn std::error::Error>>
{
    let row = assistant_tool_message_in(
        "msg_000000000001",
        "execute",
        "call_000000000001",
        ToolStateDouble::Completed {
            input: "{}".to_owned(),
            output: "BEFORE".to_owned(),
        },
    )?;
    let page = String::from_utf8(encode_messages(vec![row], None)?)?;
    let target = "{\"type\":\"text\",\"text\":\"BEFORE\"}";
    assert_eq!(page.matches(target).count(), 1);
    let replacement = r#"{"type":"text","text":"BEFORE"},{"type":"file","uri":"data:image/png;base64,PRIVATE_BYTES","mime":"image/png","name":"/sensitive/location/art_opaque"},{"type":"text","text":"AFTER"}"#;
    let page = page.replace(target, replacement);
    for _ in 0..2 {
        let decoded = decode_messages(page.as_bytes())?;
        let DurableMessage::Assistant { items, .. } = &decoded.messages[0] else {
            panic!("assistant absent");
        };
        let MessageItem::Tool(tool) = &items[0] else {
            panic!("tool absent");
        };
        let DurableToolState::Completed { output, .. } = &tool.state else {
            panic!("final tool absent");
        };
        assert_eq!(output, "BEFORE\nattachment image/png art_opaque\nAFTER");
        assert!(!output.contains("PRIVATE_BYTES") && !output.contains("sensitive"));
    }
    Ok(())
}

#[test]
fn codemode_progress_survives_history_and_refuses_malformed_rows(
) -> Result<(), Box<dyn std::error::Error>> {
    let row = assistant_tool_message("msg_000000000001", "execute")?;
    let page = String::from_utf8(encode_messages(vec![row], None)?)?;
    let target = "\"metadata\":{}";
    assert_eq!(page.matches(target).count(), 1);
    let with_calls = |calls: serde_json::Value| {
        page.replace(
            target,
            &format!("\"metadata\":{}", serde_json::json!({"toolCalls":calls})),
        )
    };
    // Preserve the codec's discriminant-first ordering outside the open metadata.
    let decoded = decode_messages(
        with_calls(serde_json::json!([
        {"tool":"fixture.add","status":"running"}]))
        .as_bytes(),
    )?;
    let DurableMessage::Assistant { items, .. } = &decoded.messages[0] else {
        panic!("assistant absent");
    };
    let MessageItem::Tool(tool) = &items[0] else {
        panic!("tool absent");
    };
    assert_eq!(tool.progress.len(), 1);
    assert_eq!(tool.progress[0].tool(), "fixture.add");
    for calls in [
        serde_json::json!(null),
        serde_json::json!([{"tool":"","status":"running"}]),
        serde_json::json!(vec![serde_json::json!({"tool":"x","status":"running"}); 65]),
    ] {
        assert!(decode_messages(with_calls(calls).as_bytes()).is_err());
    }
    Ok(())
}

fn an_id() -> SessionWireId {
    match session_id("ses_000000000001") {
        Ok(id) => id,
        Err(error) => panic!("a valid session id must parse: {error}"),
    }
}

fn one_page() -> Vec<u8> {
    let info = match session_info(&an_id(), "first", "/work", None) {
        Ok(info) => info,
        Err(error) => panic!("session info: {error}"),
    };
    match encode_sessions(vec![info]) {
        Ok(bytes) => bytes,
        Err(error) => panic!("encode: {error}"),
    }
}

#[test]
fn a_session_page_round_trips_through_the_real_codec() {
    let rows = match decode_sessions(&one_page()) {
        Ok(rows) => rows,
        Err(error) => panic!("decode: {error}"),
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "first");
}

#[test]
fn the_encoded_page_carries_the_contract_keys_not_the_invented_ones() {
    // The exact defect. The previous revision emitted `{"sessions": [...]}`;
    // the contract is `{"data": [...], "cursor": {...}}`.
    let bytes = one_page();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("\"data\""), "contract key `data`: {text}");
    assert!(text.contains("\"cursor\""), "contract key `cursor`: {text}");
    assert!(
        !text.contains("\"sessions\""),
        "the invented key must not appear: {text}"
    );
}

#[test]
fn mutant_the_invented_session_shape_is_refused_by_the_real_decoder() {
    // MUTANT: exactly the payload the previous revision produced. If this ever
    // decodes, the client has drifted back to a parallel vocabulary.
    let invented = br#"{"sessions":[{"id":"ses_000000000001","title":"first"}]}"#;
    match decode_sessions(invented) {
        Err(WireError::Codec { .. }) => {}
        other => panic!("the invented session shape must be refused, got {other:?}"),
    }
}

#[test]
fn mutant_a_page_missing_its_required_cursor_is_refused() {
    // `cursor` is REQUIRED. A shape carrying only `data` is the halfway
    // mutation between the invention and the contract, and must also fail.
    match decode_sessions(br#"{"data":[]}"#) {
        Err(WireError::Codec { .. }) => {}
        other => panic!("a page without its required cursor must be refused, got {other:?}"),
    }
}

#[test]
fn a_session_response_round_trips() {
    let info = match session_info(&an_id(), "solo", "/work", None) {
        Ok(info) => info,
        Err(error) => panic!("session info: {error}"),
    };
    let bytes = match encode_session(info) {
        Ok(bytes) => bytes,
        Err(error) => panic!("encode: {error}"),
    };
    match decode_session(&bytes) {
        Ok(row) => assert_eq!(row.title, "solo"),
        Err(error) => panic!("decode: {error}"),
    }
}

#[test]
fn mutant_a_bare_session_object_is_refused() {
    // The previous revision returned `{"id": ..., "title": ...}` directly from
    // session-create. The contract wraps it as `{"data": {...}}`.
    match decode_session(br#"{"id":"ses_000000000001","title":"solo"}"#) {
        Err(WireError::Codec { .. }) => {}
        other => panic!("a bare session object must be refused, got {other:?}"),
    }
}

#[test]
fn a_prompt_request_is_validated_against_the_real_dto_before_it_leaves() {
    let bytes = match encode_prompt("hello") {
        Ok(bytes) => bytes,
        Err(error) => panic!("encode: {error}"),
    };
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("\"text\""), "contract key: {text}");
    match decode_prompt(&bytes) {
        Ok(decoded) => assert_eq!(decoded, "hello"),
        Err(error) => panic!("decode: {error}"),
    }
}

#[test]
fn mutant_a_prompt_without_its_required_text_is_refused() {
    match decode_prompt(br#"{}"#) {
        Err(WireError::Codec { .. }) => {}
        other => panic!("`text` is required and its absence must be refused, got {other:?}"),
    }
}

#[test]
fn mutant_a_prompt_using_the_wrong_key_is_refused_at_the_client() {
    // `{"prompt": ...}` instead of `{"text": ...}`. The validation happens
    // before the bytes leave, so this is caught here rather than as a 400 from
    // a server the client would then have to interpret.
    match decode_prompt(br#"{"prompt":"hello"}"#) {
        Err(WireError::Codec { .. }) => {}
        other => panic!("a wrong-key prompt must be refused, got {other:?}"),
    }
}

#[test]
fn an_interrupt_and_a_session_create_validate_as_contract_objects() {
    // The session-create half now carries the SELECTION, because a create
    // request without one is a session created against the server's defaults.
    // The all-absent shape it used to assert is exactly what this client must
    // no longer send, so the witness follows the contract rather than being
    // kept alive against it.
    let Ok(agent) = agent_id("agent-build") else {
        panic!("agent id")
    };
    let Ok(model) = model_id("model-fast") else {
        panic!("model id")
    };
    let Ok(provider) = provider_id("double") else {
        panic!("provider id")
    };
    let create = encode_session_create(&agent, &model, &provider, "/work/project-a");
    for encoded in [
        encode_interrupt(),
        encode_interrupt_and_resume(None),
        create,
    ] {
        let Ok(bytes) = encoded else {
            panic!("a contract request must validate")
        };
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.starts_with('{') && text.ends_with('}'),
            "a contract request encodes as an object: {text}"
        );
    }
}

#[test]
fn esc_names_the_observed_execution_and_stop_only_stays_the_default(
) -> Result<(), Box<dyn std::error::Error>> {
    let observed = execution_id("exe_esc_observed")?;
    let stop: grapt_api::u3a::InterruptRequestDto =
        decode_request(&encode_interrupt()?, limits()?)?;
    assert!(stop.resume_pending().as_present().is_none());
    let esc: grapt_api::u3a::InterruptRequestDto =
        decode_request(&encode_interrupt_and_resume(Some(&observed))?, limits()?)?;
    assert_eq!(esc.resume_pending().as_present(), Some(&true));
    assert_eq!(esc.execution_id().as_value(), Some(&observed));
    Ok(())
}

#[test]
fn an_identity_that_is_not_a_session_id_is_refused_at_the_edge() {
    // Identity is parsed ONCE, here. A client that kept raw strings could hold
    // two spellings of one session and render them as two.
    match session_id("") {
        Err(WireError::Invalid { what, .. }) => assert_eq!(what, "session id"),
        other => panic!("an empty identity must be refused, got {other:?}"),
    }
}

#[test]
fn a_parsed_identity_round_trips_to_the_same_text() {
    let id = an_id();
    assert_eq!(id.as_inner().to_string(), "ses_000000000001");
}

// ---------------------------------------------------------------------------
// Message list
// ---------------------------------------------------------------------------

fn a_page(offers_next: bool) -> Vec<u8> {
    let user = match user_message("msg_000000000001", "hello") {
        Ok(m) => m,
        Err(error) => panic!("user message: {error}"),
    };
    let assistant = match assistant_message(
        "msg_000000000002",
        &[
            MessageItem::Reasoning("considering".to_owned()),
            MessageItem::Text("echo hello".to_owned()),
        ],
        Some(DurableFinish::Stop),
        Some((12, 7)),
        None,
    ) {
        Ok(m) => m,
        Err(error) => panic!("assistant message: {error}"),
    };
    match encode_messages(vec![user, assistant], offers_next.then_some("more")) {
        Ok(bytes) => bytes,
        Err(error) => panic!("encode: {error}"),
    }
}

#[test]
fn a_message_page_round_trips_through_the_real_codec() -> Result<(), Box<dyn std::error::Error>> {
    let page = match decode_messages(&a_page(false)) {
        Ok(page) => page,
        Err(error) => panic!("decode: {error}"),
    };
    assert_eq!(page.messages.len(), 2);
    assert_eq!(
        page.messages[0],
        DurableMessage::User {
            text: "hello".to_owned()
        }
    );
    assert_eq!(
        page.messages[1],
        DurableMessage::Assistant {
            items: vec![
                MessageItem::Reasoning("considering".to_owned()),
                MessageItem::Text("echo hello".to_owned()),
            ],
            finish: Some(DurableFinish::Stop),
            usage: Some((12, 7)),
            cache: Box::default(),
            footer: Some(Box::new(AssistantFooter {
                message: MessageWireId::parse("msg_000000000002".into()).ok_or("message id")?,
                model: grapt_api::u3a::ModelRefDto::new(
                    Required::new(WireId::parse("m1".into()).ok_or("model id")?),
                    Required::new(WireId::parse("p1".into()).ok_or("provider id")?),
                    Optional::absent(),
                ),
                created_ms: Some(1_700_000_000_000),
                completed_ms: None,
                execution_started_ms: None,
                outcome: None,
                input: Some(12),
                output: Some(7),
                cache_read: None,
                estimated_usd: None,
                usage_warning: None,
            })),
            error: None,
        }
    );
    assert!(
        page.next.is_none(),
        "an absent next cursor offers no further page"
    );
    assert_eq!(page.unsupported, 0);
    Ok(())
}

#[test]
fn the_encoded_message_page_carries_the_contract_keys_not_the_invented_ones() {
    // The previous revision emitted `{"messages": [...]}` with a hand-rolled
    // `durable` block per row. The contract is `{"data": [...], "cursor": ...}`
    // carrying the real message union.
    let bytes = a_page(false);
    let text = String::from_utf8_lossy(&bytes);
    assert_eq!(
        text,
        // `tokens` is present because `a_page` measures usage. The literal that
        // arrived with the accepted backend predates this encoder's durable
        // token accounting, so it described a page this crate no longer emits.
        r#"{"data":[{"type":"user","id":"msg_000000000001","time":{"created":1700000000000.0},"text":"hello"},{"type":"assistant","id":"msg_000000000002","time":{"created":1700000000000.0},"agent":"agent_double","model":{"id":"m1","providerID":"p1"},"content":[{"type":"reasoning","text":"considering","time":{"created":1700000000000.0}},{"type":"text","text":"echo hello"}],"finish":"stop","tokens":{"input":12.0,"output":7.0,"reasoning":0.0,"cache":{"read":0.0,"write":0.0}}}],"cursor":{}}"#
    );
    assert!(text.contains("\"data\""), "contract key `data`: {text}");
    assert!(text.contains("\"cursor\""), "contract key `cursor`: {text}");
    assert!(
        !text.contains("\"messages\""),
        "the invented key must not appear: {text}"
    );
}

#[test]
fn mutant_the_invented_message_shape_is_refused_by_the_real_decoder() {
    // MUTANT: exactly the payload the previous revision produced, durable
    // block and all. The real decoder must refuse it.
    let invented = br#"{"messages":[{"type":"session.text.started","durable":{"aggregateID":"ses_000000000001","seq":1,"version":1},"data":{}}]}"#;
    match decode_messages(invented) {
        Err(WireError::Codec { .. }) => {}
        other => panic!("the invented message shape must be refused, got {other:?}"),
    }
}

#[test]
fn a_page_carrying_a_cursor_offers_it_rather_than_declaring_a_prefix() {
    // The property blocker 2 depends on, corrected by the real product.
    //
    // A client that went live on a prefix would advertise a complete transcript
    // it does not have. But a cursor's PRESENCE is not a prefix: the real
    // product answers a whole two-message session with every message and a
    // `next` token that returns zero rows. So the page reports the cursor it
    // was offered, and `Client::reconcile` decides completeness by FOLLOWING
    // it — which is a claim only the follow can make.
    let page = match decode_messages(&a_page(true)) {
        Ok(page) => page,
        Err(error) => panic!("decode: {error}"),
    };
    assert_eq!(
        page.next.as_deref(),
        Some("more"),
        "the cursor the server offered is carried, not collapsed into a boolean"
    );
    assert_eq!(
        page.messages.len(),
        2,
        "the rows it did carry are still readable"
    );
}

#[test]
fn complete_and_truncated_pages_differ_only_in_the_cursor() {
    // Both directions of the same fact, so a decoder that hard-coded either
    // answer fails one of them.
    let complete = match decode_messages(&a_page(false)) {
        Ok(page) => page,
        Err(error) => panic!("decode: {error}"),
    };
    let truncated = match decode_messages(&a_page(true)) {
        Ok(page) => page,
        Err(error) => panic!("decode: {error}"),
    };
    assert_eq!(complete.messages, truncated.messages);
    assert!(complete.next.is_none());
    assert!(truncated.next.is_some());
}

#[test]
fn a_system_row_is_rendered_because_it_carries_the_failure_explanation() {
    // This row USED to be the example of something the client would not draw.
    // It is drawn now, deliberately: against a real provider the product wrote
    // "Execution failed: The execution stopped before completing. Inspect the
    // Session before retrying." into a `system` row, and the client showed the
    // user nothing at all — the actionable error J03 asks it to present.
    let payload = br#"{"data":[{"type":"system","id":"msg_000000000003","time":{"created":1.0},"text":"Execution failed: inspect the Session before retrying."}],"cursor":{}}"#;
    let page = match decode_messages(payload) {
        Ok(page) => page,
        Err(error) => panic!("a system row must decode: {error}"),
    };
    assert_eq!(
        page.messages,
        vec![DurableMessage::System {
            text: "Execution failed: inspect the Session before retrying.".to_owned()
        }],
        "the product's own account of the failure must reach the transcript"
    );
    assert_eq!(
        page.unsupported, 0,
        "it is presented, so nothing is unsupported"
    );
    assert_eq!(page.rows, 1);
}

#[test]
fn compaction_status_is_presented_without_claiming_an_unobserved_completion() {
    for (status, fields, expected) in [
        (
            "completed",
            serde_json::json!({"summary":"Quantity fix and four passing tests.","recent":"msg_recent"}),
            "Compaction completed.\n### Summary\n\nQuantity fix and four passing tests.",
        ),
        // Both estimates: the first line says what the compaction did.
        (
            "completed",
            serde_json::json!({"summary":"Quantity fix.","recent":"msg_recent",
                "metadata":{"tokenEstimates":{"before":"1200","after":"350","basis":"estimated"}}}),
            "Context compacted: 1200 → 350 tokens.\n### Summary\n\nQuantity fix.",
        ),
        // Only one of them: nothing is claimed about the other.
        (
            "completed",
            serde_json::json!({"summary":"Quantity fix.","recent":"msg_recent",
                "metadata":{"tokenEstimates":{"before":"1200","basis":"estimated"}}}),
            "Compaction completed.\n### Summary\n\nQuantity fix.",
        ),
        (
            "running",
            serde_json::json!({"summary":"provisional summary", "recent":"msg_recent"}),
            "Compaction running.",
        ),
        (
            "failed",
            serde_json::json!({"error":{"type":"provider","message":"Account unavailable"}}),
            "Compaction failed: Account unavailable",
        ),
    ] {
        // Both discriminants precede the remaining fields, matching the
        // production serializer's tag-first compaction union.
        let fields = fields.to_string();
        let fields = fields
            .strip_prefix('{')
            .and_then(|value| value.strip_suffix('}'))
            .unwrap_or_else(|| panic!("fixture fields object"));
        let payload = format!(
            r#"{{"data":[{{"type":"compaction","status":"{status}","id":"msg_compaction","time":{{"created":1.0}},"reason":"manual",{fields}}}],"cursor":{{}}}}"#
        );
        let page = decode_messages(payload.as_bytes())
            .unwrap_or_else(|error| panic!("compaction DTO {status} must decode: {error}"));
        assert_eq!(page.unsupported, 0);
        let Some(DurableMessage::Compaction { info, text }) = page.messages.first() else {
            panic!("compaction must retain its typed identity and phase");
        };
        assert_eq!(info.id, page.ids[0]);
        assert!(text.starts_with(expected));
        assert_eq!(info.phase == CompactionPhase::Running, status == "running");
    }
}

#[test]
fn an_unsupported_union_member_keeps_its_identity_and_is_counted() {
    // A member this client genuinely cannot present yet. It must not vanish:
    // the transcript names what it is holding, and the count says how much it
    // is not drawing. What it must NOT do is imply the history is incomplete —
    // the row was read.
    let payload = br#"{"data":[{"type":"skill","id":"msg_000000000004","time":{"created":1.0},"skill":"fixture","name":"fixture","text":"whatever"}],"cursor":{}}"#;
    match decode_messages(payload) {
        Ok(page) => {
            assert_eq!(
                page.messages,
                vec![DurableMessage::Unsupported { kind: "skill" }],
                "an unsupported row keeps its identity rather than disappearing"
            );
            assert_eq!(page.unsupported, 1, "and it IS counted");
            assert_eq!(page.rows, 1, "and the raw page still says one row arrived");
        }
        // If the frozen union spells this member differently, refusing is also
        // acceptable - what is NOT acceptable is silently returning an empty,
        // complete-looking page.
        Err(WireError::Codec { .. }) => {}
        Err(other) => panic!("unexpected refusal shape: {other}"),
    }
}

#[test]
fn a_page_of_only_unsupported_rows_is_not_an_empty_page() {
    // The exhaustion signal. `messages` is what survived PRESENTATION, so a
    // page carrying only undrawable rows decodes to zero messages. A walk that
    // read that as "no more history" stopped while the server was still
    // offering a cursor, truncating the transcript and then blaming the rows it
    // had discarded. `rows` is the raw count and is what exhaustion is judged
    // on.
    let payload = br#"{"data":[{"type":"synthetic","id":"msg_000000000005","time":{"created":1.0},"text":"a"},{"type":"synthetic","id":"msg_000000000006","time":{"created":1.0},"text":"b"}],"cursor":{"next":"c2"}}"#;
    let Ok(page) = decode_messages(payload) else {
        return;
    };
    assert_eq!(page.rows, 2, "two rows arrived on the wire");
    assert!(
        page.rows > 0,
        "a page that carried rows must never look exhausted"
    );
    assert_eq!(
        page.next.as_deref(),
        Some("c2"),
        "and it still offers a cursor"
    );
}

#[test]
fn a_form_state_round_trips_through_the_real_codec_in_all_four_members() {
    // All four, because the union's whole purpose is that suspended and
    // cancelled are DIFFERENT answers. A round trip over only the two that
    // carry no payload would leave exactly the members with payloads unproven.
    let mut answer = std::collections::BTreeMap::new();
    answer.insert("branch".to_owned(), FormValue::Text("main".to_owned()));
    answer.insert("dry_run".to_owned(), FormValue::Flag(false));
    for state in [
        FormState::Pending,
        FormState::Cancelled,
        FormState::Answered(answer),
        FormState::Suspended(FormSuspension::Shutdown),
        FormState::Suspended(FormSuspension::ProcessLoss),
    ] {
        let Ok(bytes) = encode_form_state(&state) else {
            panic!("a contract state must encode: {state:?}")
        };
        match decode_form_state(&bytes) {
            Ok(decoded) => assert_eq!(decoded, state, "round trip changed the state"),
            Err(error) => panic!("decode {state:?}: {error}"),
        }
    }
}

#[test]
fn mutant_a_form_state_whose_tag_is_not_first_is_refused_by_the_real_decoder() {
    // The MUTANT for the ordering `encode_form_state` maintains deliberately.
    //
    // `tagged_union!` dispatches on the LEADING key so it never has to buffer a
    // map to find the tag. That makes key order load-bearing on this one shape,
    // which is exactly the kind of constraint a later "tidy-up" through
    // `serde_json::json!` would silently break — `json!` sorts, and `answer`
    // and `reason` both sort ahead of `status`.
    //
    // If this ever DECODES, the codec stopped being tag-first and the explicit
    // spelling in `encode_form_state` is free to go. Until then it is required,
    // and this is the witness that says so.
    for sorted in [
        br#"{"data":{"answer":{"branch":"main"},"status":"answered"}}"#.as_slice(),
        br#"{"data":{"reason":"shutdown","status":"suspended"}}"#.as_slice(),
    ] {
        match decode_form_state(sorted) {
            Err(WireError::Codec { .. }) => {}
            other => panic!("a tag-last state must be refused, got {other:?}"),
        }
    }
}

#[test]
fn typed_numeric_and_list_answers_round_trip_without_coercion(
) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = br#"{"answer":{"integer":18446744073709551615,"fraction":-1.25,"many":["a","b"],"empty":[]}}"#;
    let answer = decode_form_reply(bytes)?;
    assert!(
        matches!(answer.get("integer"), Some(FormValue::Number(number)) if number.as_u64() == Some(u64::MAX))
    );
    assert_eq!(
        answer.get("many"),
        Some(&FormValue::Texts(vec!["a".into(), "b".into()]))
    );
    assert_eq!(answer.get("empty"), Some(&FormValue::Texts(vec![])));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&encode_form_reply(&answer)?)?,
        serde_json::from_slice::<serde_json::Value>(bytes)?
    );
    let state = FormState::Answered(answer);
    assert_eq!(decode_form_state(&encode_form_state(&state)?)?, state);
    for invalid in [
        br#"{"answer":{"many":["a",1]}}"#.as_slice(),
        br#"{"answer":{"number":null}}"#.as_slice(),
        br#"{"answer":{"number":1e999}}"#.as_slice(),
    ] {
        assert!(decode_form_reply(invalid).is_err());
    }
    Ok(())
}

#[test]
fn a_tool_permission_names_the_call_it_holds() {
    let page = |source: serde_json::Value| {
        let mut row = serde_json::json!({
            "id": "per_held",
            "sessionID": "ses_held",
            "action": "fs.create",
            "resources": ["/work/answer.txt"],
            "status": "pending"
        });
        if !source.is_null() {
            row["source"] = source;
        }
        serde_json::json!({
            "location": {
                "directory": "/work",
                "project": {"id": "prj_double", "directory": "/work"}
            },
            "data": [row],
            "hasMore": false
        })
        .to_string()
    };
    let held = decode_permissions(
        page(serde_json::json!({"type": "tool", "messageID": "msg_held", "callID": "call_held"}))
            .as_bytes(),
    );
    let Ok(held) = held else {
        panic!("tool ask refused: {held:?}");
    };
    assert_eq!(
        held[0].call.as_ref().map(|call| call.spelled()),
        Some("call_held")
    );
    let Ok(plain) = decode_permissions(page(serde_json::Value::Null).as_bytes()) else {
        panic!("plain ask refused");
    };
    assert!(
        plain[0].call.is_none(),
        "an ask with no tool source holds no call"
    );
}

#[test]
fn a_permission_subtitle_names_the_tool_and_what_will_happen() {
    let page = |metadata: serde_json::Value| {
        let mut row = serde_json::json!({
            "id": "per_sub",
            "sessionID": "ses_sub",
            "action": "patch",
            "resources": ["/work/a", "/work/b"],
            "status": "pending"
        });
        if !metadata.is_null() {
            row["metadata"] = metadata;
        }
        serde_json::json!({
            "location": {
                "directory": "/work",
                "project": {"id": "prj_sub", "directory": "/work"}
            },
            "data": [row],
            "hasMore": false
        })
        .to_string()
    };
    let envelope = serde_json::json!({
        "tool": "patch",
        "policy": {"ownerFloor": false, "removesFiles": true},
        "consequences": [
            {"action": "fs.delete", "resource": "/work/a"},
            {"action": "fs.move", "resource": "/work/b"}
        ],
        "request": null
    });
    let asked = decode_permissions(page(envelope).as_bytes());
    let Ok(asked) = asked else {
        panic!("classified ask refused: {asked:?}");
    };
    assert_eq!(asked[0].title, "patch: /work/a, /work/b");
    assert_eq!(
        asked[0].subtitle,
        "tool patch · deletes /work/a, moves /work/b"
    );
    let verbs = serde_json::json!({
        "consequences": [
            {"action": "fs.create", "resource": "/work/new.txt"},
            {"action": "fs.overwrite", "resource": "/work/a"}
        ]
    });
    let Ok(modified) = decode_permissions(page(verbs.clone()).as_bytes()) else {
        panic!("create/overwrite ask refused: {verbs:?}");
    };
    assert_eq!(
        modified[0].subtitle,
        "tool patch · creates /work/new.txt, modifies /work/a"
    );
    let executable = "C:/Git/usr/bin/ssh.exe";
    let command = format!("{executable} -V {}", "argument ".repeat(256));
    let msys = serde_json::json!({"consequences": [
        {"action": "sandbox.msys_unconfined", "resource": executable},
        {"action": "shell", "resource": command}
    ]});
    let Ok(mut request) = serde_json::from_str::<serde_json::Value>(&page(msys)) else {
        panic!("MSYS permission fixture was not JSON");
    };
    request["data"][0]["action"] = serde_json::json!("sandbox.msys_unconfined");
    request["data"][0]["resources"] = serde_json::json!([executable, command]);
    let Ok(warning) = decode_permissions(request.to_string().as_bytes()) else {
        panic!("MSYS permission was not decoded");
    };
    assert!(warning[0]
        .subtitle
        .contains("without OS containment (including all children)"));
    assert!(!warning[0].subtitle.contains("all Windows shell commands"));
    assert!(warning[0].allow_always);
    assert!(warning[0]
        .title
        .starts_with("Runs without OS containment (including all children):"));
    assert!(warning[0].title.contains(executable));
    assert!(warning[0].title.ends_with(&command));
    request["data"][0]["resources"] = serde_json::json!([command, "windows-session-command-trees"]);
    let Ok(scoped) = decode_permissions(request.to_string().as_bytes()) else {
        panic!("Session command-tree permission was not decoded");
    };
    assert!(scoped[0]
        .subtitle
        .contains("all Windows shell commands and their children"));
    assert!(!scoped[0].allow_always);
    let plain = decode_permissions(page(serde_json::Value::Null).as_bytes());
    let Ok(plain) = plain else {
        panic!("unclassified ask refused: {plain:?}");
    };
    assert_eq!(plain[0].subtitle, "tool patch");
}

// ---------------------------------------------------------------------------
// MCP
// ---------------------------------------------------------------------------

fn mcp_location() -> crate::wire::ProjectRow {
    crate::wire::ProjectRow {
        id: crate::wire::project_id("prj_double_a")
            .unwrap_or_else(|error| panic!("witness project id must parse: {error}")),
        worktree: "/work/project-a".to_owned(),
        name: Some("Project A".to_owned()),
    }
}

/// One row per member of the frozen `Mcp.Status` union.
fn all_six_mcp_statuses() -> Vec<crate::wire::McpServerRow> {
    use crate::wire::McpServerRow;
    use crate::wire::McpServerStatus;
    vec![
        McpServerRow {
            tools: None,
            name: "fs".to_owned(),
            status: McpServerStatus::Connected,
            integration_id: None,
        },
        McpServerRow {
            tools: None,
            name: "search".to_owned(),
            status: McpServerStatus::Pending,
            integration_id: None,
        },
        McpServerRow {
            tools: None,
            name: "legacy".to_owned(),
            status: McpServerStatus::Disabled,
            integration_id: None,
        },
        McpServerRow {
            tools: None,
            name: "broken".to_owned(),
            status: McpServerStatus::Failed {
                error: "The configured MCP server is failed: spawn refused".to_owned(),
            },
            integration_id: None,
        },
        McpServerRow {
            tools: None,
            name: "github".to_owned(),
            status: McpServerStatus::NeedsAuth,
            integration_id: None,
        },
        McpServerRow {
            tools: None,
            name: "registry".to_owned(),
            status: McpServerStatus::NeedsRegistration {
                error: "no client id configured".to_owned(),
            },
            integration_id: Some("integ_0000000001".to_owned()),
        },
    ]
}

#[test]
fn the_mcp_list_round_trips_all_six_statuses() {
    let rows = all_six_mcp_statuses();
    let bytes = encode_mcp_servers(&mcp_location(), &rows)
        .unwrap_or_else(|error| panic!("the encoded list must validate as the real DTO: {error}"));
    let decoded = decode_mcp_servers(&bytes)
        .unwrap_or_else(|error| panic!("the encoded list must decode as the real DTO: {error}"));
    assert_eq!(
        decoded, rows,
        "encode then decode is the identity over all six statuses"
    );
}

#[test]
fn an_mcp_status_the_frozen_union_does_not_declare_is_refused() {
    // The encoder cannot spell a seventh status — it matches its own enum —
    // so the mutant takes the wire shape the drifted producer would send.
    let bytes = encode_mcp_servers(&mcp_location(), &all_six_mcp_statuses()[..1])
        .unwrap_or_else(|error| panic!("encode: {error}"));
    let page = String::from_utf8(bytes)
        .unwrap_or_else(|error| panic!("the encoded page must be utf8: {error:?}"));
    let mutated = page.replace(r#""status":"connected""#, r#""status":"reconnecting""#);
    assert_ne!(mutated, page, "the mutant must change the payload");
    assert!(
        decode_mcp_servers(mutated.as_bytes()).is_err(),
        "a status the frozen union does not declare must be refused"
    );
}

#[test]
fn a_failed_mcp_status_without_its_error_is_refused() {
    let bytes = encode_mcp_servers(&mcp_location(), &all_six_mcp_statuses()[3..4])
        .unwrap_or_else(|error| panic!("encode: {error}"));
    let page = String::from_utf8(bytes)
        .unwrap_or_else(|error| panic!("the encoded page must be utf8: {error:?}"));
    // The frozen object is encoded key-sorted, so `error` precedes `status`;
    // the mutant drops the field and the comma after it, leaving the valid
    // JSON a producer that omitted the required member would send.
    let mutated = page.replace(
        r#""error":"The configured MCP server is failed: spawn refused",""#,
        "",
    );
    assert_ne!(mutated, page, "the mutant must drop the error");
    assert!(
        decode_mcp_servers(mutated.as_bytes()).is_err(),
        "the frozen Failed member carries its error; a row without one is refused"
    );
}

#[test]
fn the_mcp_refusal_body_round_trips_and_a_foreign_body_is_refused() {
    let message = "invalid mcp config: /work/project-a/.grapt/mcp.json";
    let bytes = encode_invalid_request(message)
        .unwrap_or_else(|error| panic!("the refusal body must validate: {error}"));
    assert_eq!(
        decode_invalid_request_message(&bytes)
            .unwrap_or_else(|error| panic!("the refusal body must decode: {error}")),
        message
    );
    // A 400 body in a shape the frozen record does not declare is not a
    // refusal this client can read: the ingress keeps it a bare status.
    assert!(decode_invalid_request_message(b"{}").is_err());
    assert!(
        decode_invalid_request_message(b"{\"_tag\":\"SomethingElse\",\"message\":\"x\"}").is_err()
    );
    assert!(
        decode_invalid_request_message(b"{\"_tag\":\"InvalidRequestError\"}").is_err(),
        "the tag without its message is not the frozen error"
    );
}

#[test]
fn startup_origin_survives_the_wire_and_reconciliation_never_changes_it(
) -> Result<(), Box<dyn std::error::Error>> {
    let id =
        grapt_api::u3a::WireId::<grapt_types::MessageId>::parse("msg_startup_origin".to_owned())
            .ok_or("message id")?;
    let encoded = encode_seat_startup("packet", &id)?;
    let value: serde_json::Value = serde_json::from_slice(&encoded)?;
    assert_eq!(
        value["metadata"],
        serde_json::json!({"origin":"seat-startup"})
    );
    let history=br#"{"data":[{"type":"user","id":"msg_startup_origin","time":{"created":1},"text":"packet","metadata":{"origin":"seat-startup"}}],"cursor":{}}"#;
    let page = decode_messages(history)?;
    assert_eq!(
        page.messages,
        vec![DurableMessage::SeatStartup {
            text: "packet".into()
        }]
    );
    assert!(super::submission::history_holds_input(
        history, &id, "packet", true
    )?);
    assert!(!super::submission::history_holds_input(
        history, &id, "packet", false
    )?);
    Ok(())
}
