//! Real transport shouts remain diagnostics beside exactly one JSON answer.
use super::{real_shouts, tests::decode_frame, E, MAX_REPLY};
use serde_json::json;

#[test]
fn seat05_real_behind_and_unreadable_notices_keep_the_single_answer() -> Result<(), E> {
    let fixtures = real_shouts();
    assert_eq!(
        fixtures.len(),
        4,
        "all real refusal/no-refusal forms are required"
    );
    for fixture in fixtures {
        let body = &fixture["answer"];
        for structured in [false, true] {
            let mut frame = json!({"content":[{"type":"text","text":body.to_string()},
                {"type":"text","text":fixture["text"]}]});
            if structured {
                frame["structuredContent"] = body.clone();
            }
            let mut actual = decode_frame(frame)?;
            let diagnostics = actual
                .as_object_mut()
                .ok_or(E::Contract)?
                .remove("grapt_transport_diagnostics")
                .ok_or(E::Contract)?;
            assert!(diagnostics.to_string().contains("stale_door:"));
            assert!(!diagnostics.to_string().contains("this MCP door"));
            assert_eq!(&actual, body);
        }
        let decoded = decode_frame(json!({"content":[{"type":"text","text":fixture["text"]}],
            "structuredContent":body}))?;
        assert!(decoded.get("grapt_transport_diagnostics").is_some());
    }
    Ok(())
}

#[test]
fn seat05_transport_prefix_never_supplies_a_body_or_displaces_ambiguity_checks() {
    let body = json!({"complete":true});
    let shout = "StateEngine: this MCP door warning includes private-DO-NOT-COPY";
    for frame in [
        json!({"content":[{"type":"text","text":shout}]}),
        json!({"content":[{"type":"text","text":shout},{"type":"text","text":body.to_string()}]}),
        json!({"content":[{"type":"text","text":shout},{"type":"text","text":body.to_string()}],"structuredContent":body}),
        json!({"content":[{"type":"text","text":body.to_string()},{"type":"text","text":body.to_string()},{"type":"text","text":shout}]}),
        json!({"content":[{"type":"text","text":body.to_string()},{"type":"text","text":shout}],"structuredContent":{"complete":false}}),
        json!({"content":[{"type":"text","text":body.to_string()},{"type":"text","text":"StateEngine:this MCP door"}]}),
        json!({"content":[{"type":"text","text":body.to_string()},{"type":"image","data":"AA==","mimeType":"image/png"},{"type":"text","text":shout}]}),
        json!({"content":[{"type":"text","text":"null"},{"type":"text","text":shout}]}),
        json!({"content":[{"type":"text","text":json!({"grapt_transport_diagnostics":[]}).to_string()},{"type":"text","text":shout}]}),
        json!({"content":[{"type":"text","text":"unrecognised text"},{"type":"text","text":shout}]}),
    ] {
        assert_eq!(decode_frame(frame), Err(E::Contract));
    }
    assert_eq!(
        decode_frame(
            json!({"isError":true,"content":[{"type":"text","text":body.to_string()},{"type":"text","text":shout}]})
        ),
        Err(E::Transport)
    );
    assert_eq!(
        decode_frame(
            json!({"content":[{"type":"text","text":body.to_string()},{"type":"text","text":format!("StateEngine: {}", "x".repeat(MAX_REPLY))}]})
        ),
        Err(E::Bounds)
    );
    let actual = decode_frame(
        json!({"content":[{"type":"text","text":body.to_string()},{"type":"text","text":shout}]}),
    )
    .unwrap_or_else(|error| panic!("diagnostic fixture was refused: {error:?}"));
    assert!(!actual.to_string().contains("private-DO-NOT-COPY"));
}
