//! A connection publishes only models it can run on the wire it checked.
use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn ids(listing: &[u8]) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let listing: Value = serde_json::from_slice(listing)?;
    Ok(listing["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["id"].as_str().map(str::to_owned))
        .collect())
}

#[test]
fn a_mixed_endpoint_publishes_only_the_models_on_its_checked_wire() -> TestResult {
    let provider = KeyProvider::from_id("snowflake-cortex")?.ok_or("mixed endpoint")?;
    let listing = serde_json::to_vec(&serde_json::json!({"data":[
        {"id":"claude-test","max_model_len":200_000},
        {"id":"chat-test","max_model_len":128_000},
    ]}))?;
    let messages = provider.for_model("claude-test");
    let published = messages.supported_listing(&listing)?;
    assert_eq!(ids(&published)?, ["claude-test"]);
    assert!(!messages.runs(&published, "chat-test")?);
    let chat = provider.for_model("chat-test");
    assert_eq!(ids(&chat.supported_listing(&listing)?)?, ["chat-test"]);
    Ok(())
}

#[test]
fn a_row_without_tools_or_text_output_is_not_capable() -> TestResult {
    let provider = KeyProvider::from_id("ollama")?.ok_or("local preset")?;
    let row = |extra: Value| {
        let mut row = serde_json::json!({"id":"local-test"});
        if let (Some(row), Some(extra)) = (row.as_object_mut(), extra.as_object()) {
            row.extend(extra.clone());
        }
        row
    };
    assert!(provider.capable(&row(serde_json::json!({})))?);
    assert!(provider.capable(&row(serde_json::json!({"supported_parameters":["tools"]})))?);
    assert!(!provider.capable(&row(
        serde_json::json!({"supported_parameters":["temperature"]})
    ))?);
    assert!(!provider.capable(&row(serde_json::json!({"output_modalities":["embedding"]})))?);
    assert!(!provider.capable(&row(
        serde_json::json!({"architecture":{"output_modalities":["embedding"]}})
    ))?);
    Ok(())
}

#[test]
fn an_endpoint_listing_drops_the_rows_that_say_they_cannot_run() -> TestResult {
    let provider = KeyProvider::from_id("ollama")?.ok_or("local preset")?;
    let listing = serde_json::to_vec(&serde_json::json!({"data":[
        {"id":"chat-test","max_model_len":65_536},
        {"id":"plain-test","max_model_len":65_536,"supported_parameters":["temperature"]},
        {"id":"embed-test","max_model_len":8_192,"architecture":{"output_modalities":["embedding"]}},
    ]}))?;
    assert_eq!(ids(&provider.supported_listing(&listing)?)?, ["chat-test"]);
    Ok(())
}

#[test]
fn only_a_credential_variable_is_read_from_the_environment() {
    for name in ["TEST_API_KEY", "TEST_APIKEY", "TEST_TOKEN", "TEST_PAT"] {
        assert!(credential_name(name), "{name}");
    }
    for name in [
        "TEST_ACCOUNT_ID",
        "TEST_BASE_URL",
        "TEST_RESOURCE_NAME",
        "TEST_HOST",
    ] {
        assert!(!credential_name(name), "{name}");
    }
}
