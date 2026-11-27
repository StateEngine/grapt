//! Only reported endpoint fields become metadata facts, additively.
use super::*;
use serde_json::json;

fn base() -> Value {
    json!({"id":"vendor/model","name":"vendor/model","metadata_source":"provider-listing",
        "limit":{"context":null,"output":null}})
}

#[test]
fn reported_fields_become_metadata_and_unreported_fields_stay_absent() -> Result<(), String> {
    let mut metadata = base();
    apply(
        &mut metadata,
        &json!({"id":"vendor/model","max_model_len":500000,
            "context_length":131072,"max_completion_tokens":16384,
            "supported_parameters":["tools","temperature"],
            "input_modalities":["text","image"],"output_modalities":["text"]}),
    )?;
    // The context length is validated by apply but combined in admit_listing.
    assert!(metadata["limit"]["context"].is_null());
    assert_eq!(metadata["limit"]["output"], 16384);
    assert_eq!(metadata["tool_call"], true);
    assert_eq!(metadata["modalities"]["input"], json!(["text", "image"]));
    assert_eq!(metadata["modalities"]["output"], json!(["text"]));

    let mut bare = base();
    apply(&mut bare, &json!({"id":"vendor/model"}))?;
    assert_eq!(bare, base(), "an unreported field became a fact");
    Ok(())
}

#[test]
fn a_parameter_list_without_tools_is_a_reported_lack_of_tools() -> Result<(), String> {
    let mut metadata = base();
    apply(
        &mut metadata,
        &json!({"id":"vendor/model","supported_parameters":["temperature"]}),
    )?;
    assert_eq!(metadata["tool_call"], false);
    Ok(())
}

#[test]
fn an_unreadable_optional_field_stays_unknown_instead_of_refusing() -> Result<(), String> {
    // A reported field no consumer can use is not a reason to refuse a listing
    // that was valid before enrichment: that field alone stays unknown.
    for row in [
        json!({"context_length":0}),
        json!({"context_length":"131072"}),
        json!({"max_completion_tokens":-1}),
        json!({"max_completion_tokens":u64::MAX}),
        json!({"supported_parameters":"tools"}),
        json!({"supported_parameters":["tools",7]}),
        json!({"input_modalities":[]}),
        json!({"output_modalities":["text",7]}),
    ] {
        let mut metadata = base();
        apply(&mut metadata, &row)?;
        assert_eq!(metadata, base(), "malformed row became a fact: {row}");
    }
    Ok(())
}

#[test]
fn reporting_one_modality_side_never_erases_the_other() -> Result<(), String> {
    let mut metadata = json!({"id":"vendor/model","limit":{"context":32000,"output":1024},
        "modalities":{"input":["text"],"output":["text"]}});
    apply(&mut metadata, &json!({"input_modalities":["text","image"]}))?;
    assert_eq!(metadata["modalities"]["input"], json!(["text", "image"]));
    assert_eq!(
        metadata["modalities"]["output"],
        json!(["text"]),
        "a reported input erased the known output"
    );
    Ok(())
}

#[test]
fn a_malformed_limit_neither_panics_nor_refuses_here() -> Result<(), String> {
    // This limit is a local catalogue's own shape; admitting it is the caller's
    // named refusal. Enrichment must not panic on it and must not refuse, since
    // its input row is remote.
    let mut metadata = json!({"id":"vendor/model","limit":"unreadable"});
    apply(&mut metadata, &json!({"max_completion_tokens":1024}))?;
    assert_eq!(
        metadata["limit"], "unreadable",
        "a remote row rewrote the limit"
    );
    Ok(())
}
