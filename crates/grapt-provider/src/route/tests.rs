//! Witnesses for endpoint precedence, endpoint refusal, and catalog-backed
//! model selection.
//!
//! Each test below states the defect it would catch, because a precedence test
//! that only asserts "the override won" passes just as happily against an
//! implementation with no precedence at all — one that always returns the
//! override and ignores everything else.

use super::*;
use grapt_types::{BaseModelId, ModelCatalogRecord, ModelCatalogVersion, ModelFamilyId};

fn provider(name: &str) -> ProviderId {
    match ProviderId::new(name) {
        Some(provider) => provider,
        None => panic!("test provider id"),
    }
}

fn model_id(name: &str) -> ModelId {
    match ModelId::new(name) {
        Some(model) => model,
        None => panic!("test model id"),
    }
}

fn route() -> ModelRouteId {
    match ModelRouteId::new("route-1") {
        Some(route) => route,
        None => panic!("test route id"),
    }
}

fn catalog_with(rows: &[(&str, &str)]) -> ModelCatalog {
    let records = rows
        .iter()
        .map(|(vendor, model)| {
            let (base, family) = match (BaseModelId::new(*model), ModelFamilyId::new(*vendor)) {
                (Some(base), Some(family)) => (base, family),
                _ => panic!("test catalog coordinates"),
            };
            ModelCatalogRecord::new(provider(vendor), model_id(model), base, family, None)
        })
        .collect();
    let version = match ModelCatalogVersion::new("test-catalog") {
        Some(version) => version,
        None => panic!("test catalog version"),
    };
    match ModelCatalog::load(version, records) {
        Some(catalog) => catalog,
        None => panic!("test catalog"),
    }
}

// ---------------------------------------------------------------------------
// Precedence
// ---------------------------------------------------------------------------

#[test]
fn every_level_wins_when_it_is_the_highest_present() {
    // The three assertions together are what make this a precedence test.
    // Asserting only the first would pass against an implementation that
    // returns the override and never reads the other two levels at all.
    let family = ProtocolFamily::OpenAiResponses;

    let all_three = EndpointConfig::new()
        .with_configured_profile("https://profile.example")
        .with_attempt_override("https://override.example");
    let resolved = match all_three.resolve(family) {
        Ok(resolved) => resolved,
        Err(error) => panic!("resolution refused: {error}"),
    };
    assert_eq!(resolved.base(), "https://override.example");
    assert_eq!(resolved.source(), EndpointSource::AttemptOverride);

    let profile_only = EndpointConfig::new().with_configured_profile("https://profile.example");
    let resolved = match profile_only.resolve(family) {
        Ok(resolved) => resolved,
        Err(error) => panic!("resolution refused: {error}"),
    };
    assert_eq!(resolved.base(), "https://profile.example");
    assert_eq!(resolved.source(), EndpointSource::ConfiguredProfile);

    let nothing = EndpointConfig::new();
    let resolved = match nothing.resolve(family) {
        Ok(resolved) => resolved,
        Err(error) => panic!("resolution refused: {error}"),
    };
    assert_eq!(resolved.base(), "https://api.openai.com/v1");
    assert_eq!(resolved.source(), EndpointSource::FamilyDefault);
}

#[test]
fn the_winning_level_is_reported_not_inferred_from_the_value() {
    // The defect: a caller that configures the profile to the SAME string as
    // the built-in default. A `source` derived by comparing the resolved base
    // against the default would call this `FamilyDefault` and tell an operator
    // their configuration was ignored. It was not.
    let config = EndpointConfig::new().with_configured_profile("https://api.openai.com/v1");
    let resolved = match config.resolve(ProtocolFamily::OpenAiResponses) {
        Ok(resolved) => resolved,
        Err(error) => panic!("resolution refused: {error}"),
    };
    assert_eq!(resolved.base(), "https://api.openai.com/v1");
    assert_eq!(
        resolved.source(),
        EndpointSource::ConfiguredProfile,
        "provenance must come from which level supplied the value, never from the value"
    );
}

#[test]
fn a_lower_level_is_not_consulted_once_a_higher_one_is_present() {
    // A malformed profile must not fail an attempt whose override is what will
    // actually be used: validation happens when a level WINS, not eagerly.
    let config = EndpointConfig::new()
        .with_configured_profile("http://plaintext.example")
        .with_attempt_override("https://override.example");
    let resolved = match config.resolve(ProtocolFamily::OpenAiResponses) {
        Ok(resolved) => resolved,
        Err(error) => panic!("an unused malformed profile must not fail: {error}"),
    };
    assert_eq!(resolved.base(), "https://override.example");
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

#[test]
fn a_plaintext_endpoint_is_refused_at_every_level_that_can_supply_one() {
    // The override is the most likely place for a plaintext URL to enter, so
    // it is checked exactly as hard as the profile. Both directions asserted:
    // a guard present on one level only is a guard someone routes around.
    let by_override = EndpointConfig::new().with_attempt_override("http://plaintext.example");
    assert_eq!(
        by_override.resolve(ProtocolFamily::OpenAiResponses).err(),
        Some(RouteConfigError::EndpointNotHttps {
            level: EndpointSource::AttemptOverride
        })
    );

    let by_profile = EndpointConfig::new().with_configured_profile("http://plaintext.example");
    assert_eq!(
        by_profile.resolve(ProtocolFamily::OpenAiResponses).err(),
        Some(RouteConfigError::EndpointNotHttps {
            level: EndpointSource::ConfiguredProfile
        })
    );
}

#[test]
fn loopback_base_requires_exact_parsed_authority() {
    let accepted = LoopbackBase::parse("http://127.0.0.1:43210/")
        .unwrap_or_else(|error| panic!("exact loopback refused: {error}"));
    assert_eq!(accepted.as_str(), "http://127.0.0.1:43210");

    for malicious in [
        "http://127.0.0.1:1@example.com/",
        "http://127.0.0.1:43210@example.com/",
        "http://127.0.0.1:1@example.com:43210/",
        "http://127.0.0.1:43210/?redirect=https://example.com",
        "http://127.0.0.1:43210/v1?redirect=https://example.com",
        "http://127.0.0.1:43210/v1#fragment",
        "https://127.0.0.1:43210/v1",
        "http://localhost:43210/v1",
        "http://127.0.0.1/v1",
    ] {
        assert!(
            LoopbackBase::parse(malicious).is_err(),
            "parsed non-loopback authority was admitted: {malicious}"
        );
    }
}

#[test]
fn configured_http_requires_literal_loopback_without_authority_or_path_decorations() {
    for accepted in ["http://127.0.0.1:8000/v1", "http://[::1]:8000/v1"] {
        let resolved = EndpointConfig::new()
            .with_configured_profile(accepted)
            .resolve(ProtocolFamily::OpenAiCompatible)
            .unwrap_or_else(|error| panic!("local provider refused: {error}"));
        assert_eq!(resolved.base(), accepted);
        assert!(resolved.is_plaintext_loopback());
    }
    for refused in [
        "http://localhost:8000/v1",
        "http://remote.example/v1",
        "http://192.168.1.2/v1",
        "http://127.0.0.1:8000@remote.example/v1",
        "http://user@127.0.0.1:8000/v1",
        "http://[::1]:8000/v1?forward=remote",
        "http://127.0.0.1:8000/v1#fragment",
        "https://user@api.example/v1",
        "https:///",
        "file:///tmp/provider",
    ] {
        assert!(
            EndpointConfig::new()
                .with_attempt_override(refused)
                .resolve(ProtocolFamily::OpenAiCompatible)
                .is_err(),
            "endpoint admitted: {refused}"
        );
    }
}

#[test]
fn endpoint_byte_cap_precedes_parsing_and_whitespace_normalization() {
    let prefix = "http://127.0.0.1:8000/";
    let exact = format!("{prefix}{}", "a".repeat(2048 - prefix.len()));
    assert!(EndpointConfig::new()
        .with_attempt_override(&exact)
        .resolve(ProtocolFamily::OpenAiCompatible)
        .is_ok());
    for over_cap in [format!("{exact}a"), format!(" {exact}")] {
        assert!(EndpointConfig::new()
            .with_attempt_override(&over_cap)
            .resolve(ProtocolFamily::OpenAiCompatible)
            .is_err());
        assert!(LoopbackBase::parse(&over_cap).is_err());
    }
}

#[test]
fn a_loopback_base_carries_a_bounded_path_prefix() {
    // A standard vLLM or llama.cpp server serves its API under /v1; the base
    // must carry that prefix to the family path instead of dropping it.
    let v1 = LoopbackBase::parse("http://127.0.0.1:8000/v1")
        .unwrap_or_else(|error| panic!("loopback /v1 base refused: {error}"));
    assert_eq!(v1.as_str(), "http://127.0.0.1:8000/v1");

    let responses = ProviderEndpoint::loopback(&v1, ProtocolFamily::OpenAiResponses, route());
    assert_eq!(responses.url(), "http://127.0.0.1:8000/v1/responses");

    let compat = ProviderEndpoint::loopback(&v1, ProtocolFamily::OpenAiCompatible, route());
    assert_eq!(compat.url(), "http://127.0.0.1:8000/v1/chat/completions");

    // A trailing slash trims like the root path, so the family path never
    // doubles at the join.
    let slashed = LoopbackBase::parse("http://127.0.0.1:8000/v1/")
        .unwrap_or_else(|error| panic!("trailing-slash /v1 base refused: {error}"));
    assert_eq!(slashed.as_str(), "http://127.0.0.1:8000/v1");

    // A deeper prefix is preserved whole.
    let deep = LoopbackBase::parse("http://127.0.0.1:8000/llama/v1")
        .unwrap_or_else(|error| panic!("deep loopback prefix refused: {error}"));
    assert_eq!(deep.as_str(), "http://127.0.0.1:8000/llama/v1");

    // An authority-only base keeps the URLs exactly as they were before the
    // prefix existed.
    let authority = LoopbackBase::parse("http://127.0.0.1:43210")
        .unwrap_or_else(|error| panic!("authority-only loopback refused: {error}"));
    assert_eq!(authority.as_str(), "http://127.0.0.1:43210");
    let endpoint = ProviderEndpoint::loopback(&authority, ProtocolFamily::OpenAiResponses, route());
    assert_eq!(endpoint.url(), "http://127.0.0.1:43210/responses");
}

#[test]
fn an_empty_or_whitespace_endpoint_is_refused_rather_than_falling_through() {
    // Falling through to the next level would be the tempting behaviour and it
    // is wrong: an operator who set the profile to "" made a mistake, and
    // silently using the vendor default hides it until the bill arrives.
    let config = EndpointConfig::new().with_configured_profile("   ");
    assert_eq!(
        config.resolve(ProtocolFamily::OpenAiResponses).err(),
        Some(RouteConfigError::EndpointEmpty {
            level: EndpointSource::ConfiguredProfile
        })
    );
}

#[test]
fn the_openai_compatible_family_has_no_default_and_says_so() {
    // The family whose purpose is a caller-supplied endpoint must not acquire
    // a silent vendor winner when configuration is missing.
    assert_eq!(
        family_default_endpoint(ProtocolFamily::OpenAiCompatible),
        None
    );
    assert_eq!(
        EndpointConfig::new()
            .resolve(ProtocolFamily::OpenAiCompatible)
            .err(),
        Some(RouteConfigError::NoDefaultEndpoint)
    );

    // ...and it resolves normally once one is supplied. Grok is the supported
    // named instance of this family.
    let grok = EndpointConfig::new().with_configured_profile("https://api.x.ai/v1");
    let resolved = match grok.resolve(ProtocolFamily::OpenAiCompatible) {
        Ok(resolved) => resolved,
        Err(error) => panic!("a supplied compat endpoint must resolve: {error}"),
    };
    assert_eq!(resolved.base(), "https://api.x.ai/v1");
}

#[test]
fn a_trailing_slash_never_doubles_in_the_url() {
    let config = EndpointConfig::new().with_attempt_override("https://api.example.com/v1/");
    let resolved = match config.resolve(ProtocolFamily::AnthropicMessages) {
        Ok(resolved) => resolved,
        Err(error) => panic!("resolution refused: {error}"),
    };
    let endpoint = resolved.bind(ProtocolFamily::AnthropicMessages, route());
    assert_eq!(endpoint.url(), "https://api.example.com/v1/messages");
}

#[test]
fn each_family_appends_its_own_path() {
    let bases = [
        (ProtocolFamily::OpenAiResponses, "/responses"),
        (ProtocolFamily::ChatGptCodex, "/responses"),
        (ProtocolFamily::AnthropicMessages, "/messages"),
        (ProtocolFamily::OpenAiCompatible, "/chat/completions"),
    ];
    for (family, path) in bases {
        let endpoint = ResolvedEndpoint {
            cloudflare: None,
            base: "https://base.example".to_owned(),
            source: EndpointSource::FamilyDefault,
            messages_auth: crate::MessagesAuthMode::ApiKey,
        }
        .bind(family, route());
        assert_eq!(endpoint.url(), format!("https://base.example{path}"));
    }
}

#[test]
fn the_endpoint_retains_the_route_it_was_bound_to() {
    use crate::authority::RouteBoundEndpoint;
    let endpoint = ResolvedEndpoint {
        cloudflare: None,
        base: "https://base.example".to_owned(),
        source: EndpointSource::FamilyDefault,
        messages_auth: crate::MessagesAuthMode::ApiKey,
    }
    .bind(ProtocolFamily::OpenAiResponses, route());
    // The carrier compares THIS against the issued route. If bind dropped it
    // and the endpoint derived a route from the target instead, that check
    // would compare a value with itself.
    assert_eq!(endpoint.route().as_str(), "route-1");
}

// ---------------------------------------------------------------------------
// Model selection
// ---------------------------------------------------------------------------

#[test]
fn a_model_the_catalog_carries_is_selected_with_its_catalog_identity() {
    let catalog = catalog_with(&[("openai", "gpt-5"), ("anthropic", "claude-opus-5")]);
    let selected = match select_model(&catalog, &provider("openai"), &model_id("gpt-5"), None) {
        Ok(selected) => selected,
        Err(error) => panic!("a carried model must select: {error}"),
    };
    assert_eq!(selected.provider().as_str(), "openai");
    assert_eq!(selected.model().as_str(), "gpt-5");
    // The identity comes from the catalog release, not from the caller.
    assert_eq!(selected.catalog_version().as_str(), "test-catalog");
}

#[test]
fn a_model_the_catalog_does_not_carry_is_refused_before_any_adapter_exists() {
    // The alternative — building a ModelRef from asserted coordinates — would
    // let a typo reach the wire as a request for a model nobody declared.
    let catalog = catalog_with(&[("openai", "gpt-5")]);
    assert_eq!(
        select_model(&catalog, &provider("openai"), &model_id("gpt-5-typo"), None).err(),
        Some(RouteConfigError::ModelNotInCatalog {
            provider: "openai".to_owned(),
            model: "gpt-5-typo".to_owned(),
        })
    );
    // Right model, wrong vendor: the pair is the key, not either half.
    assert!(select_model(&catalog, &provider("anthropic"), &model_id("gpt-5"), None).is_err());
}
