//! J01: a primary account that declares an API-key protocol family and the
//! models.dev catalog source composed with it.

use std::path::Path;

use grapt_platform::{
    composed_catalogs_advice, ConfigPublicationError, PublishedConfig, COMPOSED_CATALOGS,
};
use grapt_provider::{CredentialPlacement, ProtocolFamily};

use super::shared::*;

/// The public catalog for both API-key providers: two Anthropic models, two
/// OpenAI models, every one with a context limit.
fn catalog() -> &'static str {
    r#"{
  "anthropic":{"id":"anthropic","name":"Anthropic","npm":"@ai-sdk/anthropic","models":{
    "claude-sonnet-4-5":{"id":"claude-sonnet-4-5","name":"Claude Sonnet 4.5","family":"claude-sonnet","limit":{"context":200000,"output":64000}},
    "claude-haiku-4-5":{"id":"claude-haiku-4-5","name":"Claude Haiku 4.5","family":"claude-haiku","limit":{"context":200000,"output":64000}}
  }},
  "openai":{"id":"openai","name":"OpenAI","npm":"@ai-sdk/openai","models":{
    "gpt-5.6":{"id":"gpt-5.6","name":"GPT-5.6","family":"gpt-5","limit":{"context":400000,"output":128000}},
    "gpt-5.6-mini":{"id":"gpt-5.6-mini","name":"GPT-5.6 mini","family":"gpt-5-mini","limit":{"context":400000,"output":128000}}
  }}
}"#
}

/// Write an API-key primary: `provider` with `family`, `source`, `model`,
/// and `listing` as its discovery snapshot.
fn write_api_key_profile(
    root: &Path,
    provider: &str,
    family: Option<&str>,
    source: &str,
    model: &str,
    listing: &str,
) -> TestResult {
    let family = family.map_or_else(String::new, |family| {
        format!(r#""protocol_family":"{family}","#)
    });
    std::fs::write(
        root.join("grapt.jsonc"),
        format!(
            r#"{{
  "store":{{"authority":"open-ready"}},
  "product":{{"build":"build-j01"}},
  "catalog":{{"source":"{source}"}},
  "provider":{{
    "id":"{provider}",{family}"model":"{model}","base_model":"{model}","family":"family-a",
    "route":"route-a","account":"account-a",
    "credential_version":"credential-v1","policy_version":"df0-policy-v1"
  }}
}}"#
        ),
    )?;
    std::fs::write(root.join("provider.credential"), "sentinel-api-key\n")?;
    std::fs::write(root.join("model-catalog.json"), catalog())?;
    std::fs::write(root.join("account-models.json"), listing)?;
    Ok(())
}

/// Anthropic `GET /v1/models?limit=1000`: one described model and one the
/// catalog does not describe.
const ANTHROPIC_LISTING: &str = r#"{"data":[
  {"type":"model","id":"claude-sonnet-4-5","display_name":"Claude Sonnet 4.5","created_at":"2025-09-29T00:00:00Z"},
  {"type":"model","id":"claude-undescribed","display_name":"Undescribed","created_at":"2025-01-01T00:00:00Z"}
],"has_more":false,"first_id":"claude-sonnet-4-5","last_id":"claude-undescribed"}"#;

/// OpenAI `GET /v1/models`: chat models plus an embedding model models.dev
/// does not describe.
const OPENAI_LISTING: &str = r#"{"object":"list","data":[
  {"id":"gpt-5.6","object":"model","created":1,"owned_by":"openai"},
  {"id":"text-embedding-3-large","object":"model","created":1,"owned_by":"openai"},
  {"id":"gpt-5.6-mini","object":"model","created":1,"owned_by":"openai"}
]}"#;

#[test]
fn p1_an_anthropic_api_key_primary_publishes_with_its_family_and_listed_models() -> TestResult {
    let root = scratch("j01-anthropic")?;
    ready_database(&root)?;
    write_api_key_profile(
        &root,
        "anthropic",
        Some("anthropic-messages"),
        "models.dev-anthropic",
        "claude-sonnet-4-5",
        ANTHROPIC_LISTING,
    )?;
    let published = PublishedConfig::load(&root)?;
    let primary = published.credentials();
    assert_eq!(primary.protocol_family(), ProtocolFamily::AnthropicMessages);
    assert_eq!(primary.placement(), CredentialPlacement::AnthropicApiKey);
    let inventory = published.inventory().ok_or("no inventory")?;
    let available: Vec<(&str, Option<u64>)> = inventory
        .available()
        .iter()
        .map(|model| (model.id().as_str(), model.context()))
        .collect();
    // Listed models remain visible; unknown context is explicit.
    assert_eq!(
        available,
        [
            ("claude-sonnet-4-5", Some(200_000)),
            ("claude-undescribed", None)
        ]
    );
    assert!(inventory.available()[0].variants().is_empty());
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn p1_an_openai_api_key_primary_preserves_listed_unknown_models() -> TestResult {
    let root = scratch("j01-openai")?;
    ready_database(&root)?;
    write_api_key_profile(
        &root,
        "openai",
        Some("openai-responses"),
        "models.dev-openai",
        "gpt-5.6",
        OPENAI_LISTING,
    )?;
    let published = PublishedConfig::load(&root)?;
    assert_eq!(
        published.credentials().protocol_family(),
        ProtocolFamily::OpenAiResponses
    );
    assert_eq!(
        published.credentials().placement(),
        CredentialPlacement::BearerAuthorization
    );
    let inventory = published.inventory().ok_or("no inventory")?;
    let ids: Vec<&str> = inventory
        .available()
        .iter()
        .map(|model| model.id().as_str())
        .collect();
    assert_eq!(ids, ["gpt-5.6", "text-embedding-3-large", "gpt-5.6-mini"]);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn p1_a_pair_outside_the_table_refuses_naming_the_family_and_every_composed_pair() -> TestResult {
    for (provider, family, source, model) in [
        (
            "anthropic",
            "anthropic-messages",
            "models.dev-openai",
            "claude-sonnet-4-5",
        ),
        (
            "openai",
            "openai-responses",
            "models.dev-anthropic",
            "gpt-5.6",
        ),
        (
            "anthropic",
            "anthropic-messages",
            "models.dev-chatgpt",
            "claude-sonnet-4-5",
        ),
        (
            "openai",
            "openai-responses",
            "models.dev-chatgpt",
            "gpt-5.6",
        ),
        ("openai", "chatgpt-codex", "models.dev-openai", "gpt-5.6"),
    ] {
        let root = scratch("j01-mismatch")?;
        write_api_key_profile(&root, provider, Some(family), source, model, OPENAI_LISTING)?;
        let error = PublishedConfig::load(&root)
            .err()
            .ok_or("a mismatched pair loaded")?;
        let message = error.to_string();
        let ConfigPublicationError::CatalogComposition {
            protocol_family, ..
        } = error
        else {
            return Err(format!("{provider}/{family}/{source}: {message}").into());
        };
        assert_eq!(protocol_family, family);
        assert!(message.ends_with(&composed_catalogs_advice()), "{message}");
        std::fs::remove_dir_all(root)?;
    }
    Ok(())
}

/// The advice is read from the table: every composed row is advertised, in
/// table order, and nothing else is. A row the advice omitted, or a pair it
/// named that the table does not compose, fails here.
#[test]
fn p1_the_advice_advertises_exactly_the_composed_table() -> TestResult {
    let advice = composed_catalogs_advice();
    let mut from = 0;
    for pair in COMPOSED_CATALOGS {
        let named = format!(
            "provider id {:?} with catalog source {:?} (protocol family {})",
            pair.provider, pair.source, pair.family
        );
        let at = advice[from..]
            .find(&named)
            .ok_or_else(|| format!("the advice omits {named}: {advice}"))?;
        from += at + named.len();
    }
    assert_eq!(
        advice.matches("provider id ").count(),
        COMPOSED_CATALOGS.len(),
        "the advice names a pair the table does not compose: {advice}"
    );
    Ok(())
}

#[test]
fn p1_an_absent_family_keeps_chatgpt_codex_and_an_unknown_one_refuses() -> TestResult {
    let root = scratch("j01-absent-family")?;
    ready_database(&root)?;
    write_api_key_profile(
        &root,
        "openai",
        None,
        "models.dev-chatgpt",
        "gpt-5.6",
        r#"{"models":[{"slug":"gpt-5.6","visibility":"list","context_window":272000,"supported_reasoning_levels":[]}]}"#,
    )?;
    assert_eq!(
        PublishedConfig::load(&root)?
            .credentials()
            .protocol_family(),
        ProtocolFamily::ChatGptCodex
    );
    std::fs::remove_dir_all(&root)?;
    let root = scratch("j01-unknown-family")?;
    write_api_key_profile(
        &root,
        "anthropic",
        Some("anthropic"),
        "models.dev-anthropic",
        "claude-sonnet-4-5",
        ANTHROPIC_LISTING,
    )?;
    assert!(matches!(
        PublishedConfig::load(&root),
        Err(ConfigPublicationError::ProtocolFamily(family)) if family == "anthropic"
    ));
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn p2_a_paged_empty_or_malformed_listing_refuses() -> TestResult {
    for listing in [
        r#"{"data":[{"id":"claude-sonnet-4-5"}],"has_more":true}"#,
        r#"{"data":[]}"#,
        r#"{"models":[{"slug":"claude-sonnet-4-5"}]}"#,
    ] {
        let root = scratch("j01-listing")?;
        write_api_key_profile(
            &root,
            "anthropic",
            Some("anthropic-messages"),
            "models.dev-anthropic",
            "claude-sonnet-4-5",
            listing,
        )?;
        assert!(
            matches!(
                PublishedConfig::load(&root),
                Err(ConfigPublicationError::Load(_))
            ),
            "{listing}"
        );
        std::fs::remove_dir_all(root)?;
    }
    Ok(())
}

/// Point the primary's credential at `reference` (`provider.credential`).
fn with_credential_reference(root: &Path, reference: &str) -> TestResult {
    let path = root.join("grapt.jsonc");
    let text = std::fs::read_to_string(&path)?;
    let text = text.replacen(
        r#""route":"route-a""#,
        &format!(r#""credential":"{reference}","route":"route-a""#),
        1,
    );
    std::fs::write(path, text)?;
    std::fs::remove_file(root.join("provider.credential"))?;
    Ok(())
}

#[test]
fn s3_an_env_reference_reads_the_named_variable_and_an_unset_one_refuses_by_name() -> TestResult {
    // A short variable every test environment sets: the reference resolves
    // without a credential file. No variable is set by the test (that would
    // race other tests reading the environment).
    let root = scratch("j01-env-present")?;
    ready_database(&root)?;
    write_api_key_profile(
        &root,
        "anthropic",
        Some("anthropic-messages"),
        "models.dev-anthropic",
        "claude-sonnet-4-5",
        ANTHROPIC_LISTING,
    )?;
    let present = if cfg!(windows) {
        "env:USERNAME"
    } else {
        "env:HOME"
    };
    with_credential_reference(&root, present)?;
    let published = PublishedConfig::load(&root)?;
    assert_eq!(
        published.credentials().storage(),
        grapt_types::CredentialStorageMode::ProcessEnvironment
    );
    let (_credential, _target) = credential_authority_and_target(&root)?;
    std::fs::remove_dir_all(&root)?;

    let root = scratch("j01-env-absent")?;
    ready_database(&root)?;
    write_api_key_profile(
        &root,
        "anthropic",
        Some("anthropic-messages"),
        "models.dev-anthropic",
        "claude-sonnet-4-5",
        ANTHROPIC_LISTING,
    )?;
    with_credential_reference(&root, "env:GRAPT_J01_NEVER_SET_3D9F")?;
    match platform_refusal(&root)? {
        grapt_platform::PlatformRefusal::Credential(
            grapt_platform::CredentialRefusal::EnvironmentAbsent { name },
        ) => {
            assert_eq!(name, "GRAPT_J01_NEVER_SET_3D9F");
        }
        other => {
            return Err(
                format!("expected the unset variable to refuse by name, got {other}").into(),
            )
        }
    }
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn s3_a_reference_that_is_not_a_plain_variable_name_refuses_without_echoing_it() -> TestResult {
    for reference in [
        "sk-ant-J01-PASTED-KEY-NOT-A-REFERENCE",
        "env:",
        "env:lower_case",
        "env:1LEADING_DIGIT",
        "env:A-B",
        "env:PATH;rm",
    ] {
        let root = scratch("j01-env-invalid")?;
        write_api_key_profile(
            &root,
            "anthropic",
            Some("anthropic-messages"),
            "models.dev-anthropic",
            "claude-sonnet-4-5",
            ANTHROPIC_LISTING,
        )?;
        with_credential_reference(&root, reference)?;
        let error = PublishedConfig::load(&root)
            .err()
            .ok_or("an invalid reference loaded")?;
        assert!(
            matches!(error, ConfigPublicationError::CredentialReference),
            "{reference}"
        );
        assert!(
            !error.to_string().contains("J01-PASTED-KEY"),
            "the configured value was echoed"
        );
        std::fs::remove_dir_all(root)?;
    }
    Ok(())
}

#[test]
fn loaded_provider_environment_names_survive_configuration_edits() -> TestResult {
    let root = scratch("shell-environment-grant")?;
    ready_database(&root)?;
    write_api_key_profile(
        &root,
        "anthropic",
        Some("anthropic-messages"),
        "models.dev-anthropic",
        "claude-sonnet-4-5",
        ANTHROPIC_LISTING,
    )?;
    let name = if cfg!(windows) { "USERNAME" } else { "HOME" };
    with_credential_reference(&root, &format!("env:{name}"))?;
    let owner = grapt_platform::PlatformOwner::prepare_with_authorities(
        &root,
        &FixedAuthorities,
        &FixedAuthorities,
    )?;
    let (store, targets, _build, _credential, _first_contact) =
        owner.into_provider_parts()?.into_components();
    std::fs::write(root.join("grapt.jsonc"), b"{}")?;
    assert_eq!(
        targets.credential_environment_names().collect::<Vec<_>>(),
        [name]
    );
    drop(store);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn sc_fr01_a_local_listing_needs_no_composed_catalog() -> TestResult {
    let root = scratch("sc-fr01-local-listing")?;
    ready_database(&root)?;
    write_api_key_profile(
        &root,
        "vllm",
        Some("openai-compatible"),
        "unused",
        "qwen3.8-27b",
        r#"{"data":[{"id":"qwen3.8-27b","max_model_len":200000}]}"#,
    )?;
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("grapt.jsonc"))?)?;
    config["catalog"] = serde_json::json!({});
    config["provider"]["family"] = "qwen3".into();
    config["provider"]["endpoint"] = "https://local.example/v1".into();
    std::fs::write(root.join("grapt.jsonc"), serde_json::to_vec(&config)?)?;
    std::fs::remove_file(root.join("model-catalog.json"))?;
    let published = PublishedConfig::load(&root)?;
    let inventory = published.inventory().ok_or("local inventory absent")?;
    let model = inventory.available().first().ok_or("local model absent")?;
    assert_eq!(model.context(), Some(200000));
    assert_eq!(model.provider().as_str(), "vllm");
    assert_eq!(model.metadata()["metadata_source"], "provider-listing");
    assert_eq!(model.metadata()["family"], "qwen3");
    assert_eq!(model.metadata()["family_source"], "configuration");
    drop(published);
    std::fs::write(root.join("model-catalog.json"), "unreadable JSON")?;
    let error = PublishedConfig::load(&root)
        .err()
        .ok_or("malformed catalog was hidden")?;
    assert!(error.to_string().contains("model-catalog.json"));
    std::fs::remove_file(root.join("model-catalog.json"))?;
    std::fs::remove_file(root.join("account-models.json"))?;
    let error = PublishedConfig::load(&root)
        .err()
        .ok_or("missing inventory was published")?;
    assert!(error.to_string().contains("account-models.json"));
    std::fs::remove_dir_all(root)?;
    Ok(())
}
