use super::*;
use serde_json::json;
fn citation() -> serde_json::Value {
    json!({"type":"url_citation","url":"https://example.com/Case?q=private#section","title":"Example [reference]","start_index":0,"end_index":4})
}
#[test]
fn citation_events_and_aggregate_fallback_emit_one_visible_link_per_annotation() {
    let mut harness = Harness::new();
    let mut decoder = ResponsesDecoder::new();
    decoder.decode(
        r#"{"type":"response.output_text.delta","delta":"Fact"}"#,
        &mut harness.sink(),
    );
    decoder.decode(&json!({"type":"response.output_text.annotation.added","item_id":"msg_fixture","content_index":0,"annotation":citation()}).to_string(),&mut harness.sink());
    decoder.decode(
        r#"{"type":"response.output_text.done"}"#,
        &mut harness.sink(),
    );
    let message = json!({"type":"message","id":"msg_fixture","content":[{"type":"output_text","text":"Fact","annotations":[citation()]}]});
    decoder.decode(
        &json!({"type":"response.output_item.done","item":message}).to_string(),
        &mut harness.sink(),
    );
    decoder.decode(
        &json!({"type":"response.completed","response":{"output":[message]}}).to_string(),
        &mut harness.sink(),
    );
    let text = harness
        .fragments()
        .iter()
        .filter_map(|f| match f {
            AttemptFragment::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(
        text,
        "Fact [Source 1: Example reference](<https://example.com/Case?q=private#section>)"
    );
    assert!(matches!(
        harness.fragments().last(),
        Some(AttemptFragment::ProviderTerminal { .. })
    ));
    assert!(!harness
        .fragments()
        .iter()
        .any(|f| matches!(f, AttemptFragment::ToolCalled { .. })));
}
#[test]
fn completion_only_citations_are_preserved_and_unsafe_destinations_fail_closed() {
    for unsafe_destination in [false, true] {
        let mut harness = Harness::new();
        let mut decoder = ResponsesDecoder::new();
        let mut annotation = citation();
        if unsafe_destination {
            annotation["url"] = json!("javascript:alert(1)");
        }
        let message = json!({"type":"message","id":"msg_fixture","content":[{"type":"output_text","annotations":[annotation]}]});
        decoder.decode(
            &json!({"type":"response.completed","response":{"output":[message]}}).to_string(),
            &mut harness.sink(),
        );
        if unsafe_destination {
            assert_eq!(
                harness.terminal_class(),
                Some(&ProviderFailureClass::InvalidProviderOutput)
            );
            assert!(!harness
                .fragments()
                .iter()
                .any(|f| matches!(f, AttemptFragment::TextDelta { .. })));
        } else {
            assert!(harness.fragments().iter().any(
                |f| matches!(f,AttemptFragment::TextDelta{text,..} if text.contains("[Source 1:"))
            ));
        }
    }
}
