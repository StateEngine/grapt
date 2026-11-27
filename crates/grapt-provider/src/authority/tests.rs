//! The nine `prepared_request` witnesses, RE-AUTHORED rather than restored.
//!
//! `tests/pending/MANIFEST.tsv` preserved these bodies verbatim and hash-locked
//! them, but they cannot be restored as bytes: their preamble imports
//! `PreparedPhysicalAttemptRequest`, `PreparedRequestError` and `ProviderRequest`,
//! all deleted with the seam they tested. What is preserved is each scenario's
//! CLAIM, re-expressed against the replacement. Anyone diffing these against the
//! `.pending` bodies should expect different code asserting the same law.
//!
//! They live in-crate because [`ProviderAttemptCarrier::bind`] is crate-private.
//! The originals were integration tests over a public `prepare`; sealing that
//! entry is a strengthening, and its price is that the witnesses move inside.

use super::*;
use crate::credential::{CredentialLease, CredentialPlacement};
use crate::request::{
    AnthropicMessagesRequest, ChatGptCodexRequest, InputMessage, MaxOutputTokens, MessageRole,
    OpenAiResponsesRequest, ThinkingConfig, ToolChoice, ToolDefinition,
};
use crate::route::ProviderEndpoint;
use grapt_policy::{PhysicalAttemptAuthoritySnapshot, ToolVisibility};
use grapt_types::{
    AccountId, BaseModelId, CapabilityId, CredentialVersion, EffectClass, EgressAuthority,
    FilesystemAuthority, ModelCatalog, ModelCatalogRecord, ModelCatalogVersion, ModelFamilyId,
    ModelId, ModelRegistry, ModelRouteId, PhysicalAttemptId, PolicyVersion, ProviderId,
    RealizedEnforcement, ResolvedCapability, ResolvedModelTarget, RoutingRequest, Secret,
};

const PROVIDER: &str = "openai";
const MODEL: &str = "gpt-5-codex";
const SECRET_MATERIAL: &str = "token-value-never-printed";

fn ids() -> (ProviderId, ModelId) {
    let (Some(provider), Some(model)) = (ProviderId::new(PROVIDER), ModelId::new(MODEL)) else {
        panic!("test ids must be non-empty")
    };
    (provider, model)
}

fn route() -> ModelRouteId {
    match ModelRouteId::new("route_test") {
        Some(route) => route,
        None => panic!("test route must be non-empty"),
    }
}

/// One row for the test coordinates. `version` lets a witness mint a SECOND,
/// independent catalog spelling the same text — which is what a rogue release is.
fn catalog(version: &str) -> ModelCatalog {
    let (provider, model) = ids();
    let (Some(base), Some(family), Some(version)) = (
        BaseModelId::new(MODEL),
        ModelFamilyId::new(PROVIDER),
        ModelCatalogVersion::new(version),
    ) else {
        panic!("test catalog coordinates must be non-empty")
    };
    let record = ModelCatalogRecord::new(provider, model, base, family, None);
    match ModelCatalog::load(version, vec![record]) {
        Some(catalog) => catalog,
        None => panic!("test catalog must load"),
    }
}

fn issue_from(catalog: ModelCatalog) -> ResolvedModelTarget {
    let (provider, model) = ids();
    let (Some(account), Some(credential_version), Some(policy)) = (
        AccountId::new("acct_test"),
        CredentialVersion::new("7"),
        PolicyVersion::new("policy_test"),
    ) else {
        panic!("test routing coordinates must be non-empty")
    };
    let registry = ModelRegistry::install(catalog);
    match registry.issue(
        &provider,
        &model,
        None,
        RoutingRequest {
            route: route(),
            account,
            credential_version,
            policy,
        },
    ) {
        Ok(target) => target,
        Err(refusal) => panic!("the test catalog must issue its own row: {refusal:?}"),
    }
}

fn tool_capability() -> CapabilityId {
    CapabilityId::derive(&ResolvedCapability::new(
        EffectClass::ToolInvoke,
        None,
        FilesystemAuthority::new(Vec::new(), Vec::new(), Vec::new()),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    ))
}

fn authority_over(
    target: ResolvedModelTarget,
    tools: &[(&str, bool)],
) -> PhysicalAttemptAuthoritySnapshot {
    let mut builder = PhysicalAttemptAuthoritySnapshot::builder();
    if builder.model(target).is_err() {
        panic!("a registry-issued target must enter its own authority");
    }
    for (name, visible) in tools {
        let Some(tool) = ToolName::new((*name).to_owned()) else {
            panic!("test tool name must be non-empty")
        };
        let visibility = ToolVisibility {
            tool,
            required: tool_capability(),
            visible: *visible,
        };
        if builder.tool(visibility).is_err() {
            panic!("test tool visibility must be admissible");
        }
    }
    match builder.build() {
        Ok(snapshot) => snapshot,
        Err(error) => panic!("test authority must build: {error:?}"),
    }
}

fn authority(tools: &[(&str, bool)]) -> PhysicalAttemptAuthoritySnapshot {
    authority_over(issue_from(catalog("test-catalog")), tools)
}

/// One registry, issuing twice.
///
/// Two separate catalog LOADS are two separate snapshots and therefore two
/// different issuances, so a sibling built that way is not the sibling these
/// witnesses need. Only one registry issuing twice produces two distinct targets
/// that are genuinely the same issuance.
fn siblings_of_one_issuance() -> (ResolvedModelTarget, ResolvedModelTarget) {
    let (provider, model) = ids();
    let registry = ModelRegistry::install(catalog("test-catalog"));
    let issue = || {
        let (Some(account), Some(credential_version), Some(policy)) = (
            AccountId::new("acct_test"),
            CredentialVersion::new("7"),
            PolicyVersion::new("policy_test"),
        ) else {
            panic!("test routing coordinates must be non-empty")
        };
        match registry.issue(
            &provider,
            &model,
            None,
            RoutingRequest {
                route: route(),
                account,
                credential_version,
                policy,
            },
        ) {
            Ok(target) => target,
            Err(refusal) => panic!("one registry must issue twice: {refusal:?}"),
        }
    };
    let first = issue();
    let second = issue();
    (first, second)
}

fn attempt() -> PhysicalAttemptId {
    match PhysicalAttemptId::new("pat_r3f") {
        Some(attempt) => attempt,
        None => panic!("test attempt id must be non-empty"),
    }
}

fn endpoint() -> ProviderEndpoint {
    match crate::route::EndpointConfig::new()
        .with_attempt_override("https://chatgpt.com/backend-api/codex")
        .resolve(crate::request::ProtocolFamily::ChatGptCodex)
        .map(|resolved| resolved.bind(crate::request::ProtocolFamily::ChatGptCodex, route()))
    {
        Ok(endpoint) => endpoint,
        Err(error) => panic!("test endpoint: {error}"),
    }
}

fn lease() -> CredentialLease {
    let (Some(account), Some(version)) = (AccountId::new("acct_test"), CredentialVersion::new("7"))
    else {
        panic!("test credential coordinates must be non-empty")
    };
    match CredentialLease::issue(
        account,
        version,
        CredentialPlacement::BearerAuthorization,
        Secret::new(SECRET_MATERIAL.to_owned()),
    ) {
        Ok(lease) => lease,
        Err(error) => panic!("test lease: {error}"),
    }
}

fn tool_def(name: &str) -> ToolDefinition {
    ToolDefinition {
        input_format: crate::ToolInputFormat::Json,
        name: name.to_owned(),
        description: None,
        input_schema: "{}".to_owned(),
    }
}

fn message() -> InputMessage {
    InputMessage {
        role: MessageRole::User,
        content: "Hello".into(),
    }
}

fn bind_codex(
    authority: PhysicalAttemptAuthoritySnapshot,
    tools: Vec<ToolDefinition>,
    choice: Option<ToolChoice>,
) -> Result<
    ProviderAttemptCarrier<ProviderEndpoint, CredentialLease, ChatGptCodexRequest>,
    CarrierRefusal,
> {
    let request = ChatGptCodexRequest {
        model: authority.model().model_ref().clone(),
        input: vec![message().into()],
        prompt_cache_key: None,
        tools,
        web_search: None,
        tool_choice: choice,
        reasoning: Default::default(),
        turn: Default::default(),
    };
    ProviderAttemptCarrier::bind(attempt(), authority, endpoint(), lease(), request)
}

// ---------------------------------------------------------------------------
// The nine scenarios.
// ---------------------------------------------------------------------------

/// 1 — all three provider families bind under a matching authority.
#[test]
fn all_three_provider_families_prepare_under_a_matching_authority() {
    let a = authority(&[]);
    let model = a.model().model_ref().clone();
    assert!(
        ProviderAttemptCarrier::bind(
            attempt(),
            a,
            endpoint(),
            lease(),
            OpenAiResponsesRequest {
                model,
                input: vec![message().into()],
                max_output_tokens: MaxOutputTokens::new(256),
                prompt_cache_key: None,
                tools: Vec::new(),
                web_search: None,
                tool_choice: None,
            }
        )
        .is_ok(),
        "openai responses binds under its own authority"
    );

    let a = authority(&[]);
    let model = a.model().model_ref().clone();
    assert!(
        ProviderAttemptCarrier::bind(
            attempt(),
            a,
            endpoint(),
            lease(),
            AnthropicMessagesRequest {
                web_search: None,
                uncached_system_suffix: None,
                effort: crate::EffortPolicy::default(),
                model,
                messages: vec![message().into()],
                max_tokens: match MaxOutputTokens::new(256) {
                    Some(tokens) => tokens,
                    None => panic!("a positive token bound is valid"),
                },
                instruction_mode: crate::AnthropicInstructionMode::CurrentPrefix,
                thinking: Some(ThinkingConfig::Enabled { budget_tokens: 128 }),
                prompt_cache: crate::AnthropicPromptCache::Off,
                tools: Vec::new(),
                tool_choice: None,
            }
        )
        .is_ok(),
        "anthropic messages binds under its own authority"
    );

    assert!(
        bind_codex(authority(&[]), Vec::new(), None).is_ok(),
        "chatgpt codex binds under its own authority"
    );
}

/// 2 — a model or variant mismatch is rejected as a mismatch.
#[test]
fn a_model_or_variant_mismatch_is_rejected() {
    let a = authority(&[]);
    let (provider, _) = ids();
    let (Some(other), Some(base), Some(family), Some(version)) = (
        ModelId::new("gpt-5-other"),
        BaseModelId::new("gpt-5-other"),
        ModelFamilyId::new(PROVIDER),
        ModelCatalogVersion::new("test-catalog"),
    ) else {
        panic!("test coordinates must be non-empty")
    };
    let record = ModelCatalogRecord::new(provider.clone(), other.clone(), base, family, None);
    let Some(other_catalog) = ModelCatalog::load(version, vec![record]) else {
        panic!("other catalog must load")
    };
    let Some(entry) = other_catalog.entry(&provider, &other, None) else {
        panic!("other entry")
    };
    let request = ChatGptCodexRequest {
        model: ModelRef::from_catalog(entry),
        input: vec![message().into()],
        prompt_cache_key: None,
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
        reasoning: Default::default(),
        turn: Default::default(),
    };
    assert_eq!(
        ProviderAttemptCarrier::bind(attempt(), a, endpoint(), lease(), request).err(),
        Some(CarrierRefusal::RequestNamesAnotherModel),
        "a different model is refused as a different model"
    );
}

/// 3 — a request from another catalog cannot ride a genuine authority.
///
/// This is the witness that caught a real defect in this carrier's first draft,
/// which compared models with the provenance-blind equality operator. The rogue
/// ref spells the SAME provider, model and variant, so equality accepts it; only
/// the issuance question refuses it. The refusal has its own variant so a caller
/// is not sent hunting for a spelling mistake that is not there.
#[test]
fn a_request_from_another_catalog_cannot_ride_a_genuine_authority() {
    let a = authority(&[]);
    let (provider, model) = ids();
    let rogue = catalog("rogue-catalog");
    let Some(entry) = rogue.entry(&provider, &model, None) else {
        panic!("rogue entry")
    };
    let rogue_ref = ModelRef::from_catalog(entry);

    assert_eq!(
        rogue_ref,
        *a.model().model_ref(),
        "the rogue ref must be EQUAL by text, or this witness proves nothing"
    );

    let request = ChatGptCodexRequest {
        model: rogue_ref,
        input: vec![message().into()],
        prompt_cache_key: None,
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
        reasoning: Default::default(),
        turn: Default::default(),
    };
    assert_eq!(
        ProviderAttemptCarrier::bind(attempt(), a, endpoint(), lease(), request).err(),
        Some(CarrierRefusal::RequestFromAnotherCatalog),
        "same text, different issuance, refused"
    );
}

/// 4 — a hidden or unlisted tool is rejected, and both are one refusal.
#[test]
fn a_hidden_or_unlisted_tool_is_rejected() {
    assert_eq!(
        bind_codex(
            authority(&[("search", false)]),
            vec![tool_def("search")],
            None
        )
        .err(),
        Some(CarrierRefusal::HiddenTool {
            name: "search".to_owned()
        }),
        "offered but marked not visible"
    );
    assert_eq!(
        bind_codex(authority(&[]), vec![tool_def("search")], None).err(),
        Some(CarrierRefusal::HiddenTool {
            name: "search".to_owned()
        }),
        "offered but absent from the authority altogether"
    );
}

/// 5 — a duplicate or empty tool name is rejected.
#[test]
fn a_duplicate_or_empty_tool_name_is_rejected() {
    assert_eq!(
        bind_codex(
            authority(&[("search", true)]),
            vec![tool_def("search"), tool_def("search")],
            None
        )
        .err(),
        Some(CarrierRefusal::MalformedToolName {
            name: "search".to_owned()
        }),
        "a duplicate puts two definitions under one name"
    );
    assert_eq!(
        bind_codex(authority(&[("search", true)]), vec![tool_def("")], None).err(),
        Some(CarrierRefusal::MalformedToolName {
            name: String::new()
        }),
        "an empty name names nothing"
    );
}

/// 6 — a named tool choice must name an offered, visible tool.
#[test]
fn named_tool_choice_must_name_an_offered_visible_tool() {
    assert!(
        bind_codex(
            authority(&[("search", true)]),
            vec![tool_def("search")],
            Some(ToolChoice::Named {
                tool: "search".to_owned()
            })
        )
        .is_ok(),
        "naming an offered visible tool binds"
    );
    assert_eq!(
        bind_codex(
            authority(&[("search", true)]),
            vec![tool_def("search")],
            Some(ToolChoice::Named {
                tool: "other".to_owned()
            })
        )
        .err(),
        Some(CarrierRefusal::ToolChoiceNotOffered {
            name: "other".to_owned()
        }),
        "naming a tool the request does not offer is refused"
    );
}

/// 7 — a required tool choice needs at least one visible tool.
#[test]
fn required_tool_choice_needs_at_least_one_visible_tool() {
    assert_eq!(
        bind_codex(authority(&[]), Vec::new(), Some(ToolChoice::Required)).err(),
        Some(CarrierRefusal::ToolChoiceUnsatisfiable),
        "requiring a call while offering nothing is unsatisfiable by construction"
    );
    assert!(
        bind_codex(
            authority(&[("search", true)]),
            vec![tool_def("search")],
            Some(ToolChoice::Required)
        )
        .is_ok(),
        "requiring a call with one visible tool offered binds"
    );
}

/// 8 — custody is derived internally and is stable.
///
/// The original asserted the receipt equalled one an identical authority derives
/// on its own, and that the caller never supplies it. Custody is the replacement:
/// no part of it is caller-supplied, it is minted only by into_parts, and it
/// answers the same question the same way twice.
#[test]
fn the_receipt_is_derived_internally_and_stable() {
    let a = authority(&[]);
    let generation = a.model().issuance_generation();
    let Ok(carrier) = bind_codex(a, Vec::new(), None) else {
        panic!("test coordinates must bind")
    };
    let (custody, _endpoint, _lease, _request) = carrier.into_parts();

    assert_eq!(custody.attempt(), &attempt(), "custody names its attempt");
    assert_eq!(
        custody.issuance_generation(),
        generation,
        "custody carries the issuance it was bound under"
    );
    assert!(custody.issuance_is_current(), "the issuance is live");
    assert_eq!(
        custody.issuance_generation(),
        custody.issuance_generation(),
        "stable across calls"
    );
}

/// 9 — secret material is absent from the bound carrier and from custody.
#[test]
fn secret_material_is_absent_from_the_prepared_request_and_receipt() {
    let Ok(carrier) = bind_codex(authority(&[]), Vec::new(), None) else {
        panic!("test coordinates must bind")
    };
    let (custody, _endpoint, _lease, _request) = carrier.into_parts();
    let rendered = format!("{custody:?}");
    assert!(
        !rendered.contains(SECRET_MATERIAL),
        "custody Debug must not reach the lease: {rendered}"
    );
    assert!(
        !rendered.contains(MODEL),
        "custody Debug must not print the model either: {rendered}"
    );
}

// ---------------------------------------------------------------------------
// Custody, which the nine do not fully cover.
// ---------------------------------------------------------------------------

/// A target from a superseded issuance is refused, and named as stale.
///
/// The check the registry made expressible. Its own witness, because the nine all bind a
/// live issuance and none of them would notice the check disappearing — which the
/// mutation harness proved by surviving a row that disabled it.
#[test]
fn a_superseded_issuance_is_refused_as_stale() {
    let (provider, model) = ids();
    let mut registry = ModelRegistry::install(catalog("test-catalog"));
    let (Some(account), Some(credential_version), Some(policy)) = (
        AccountId::new("acct_test"),
        CredentialVersion::new("7"),
        PolicyVersion::new("policy_test"),
    ) else {
        panic!("test routing coordinates must be non-empty")
    };
    let Ok(target) = registry.issue(
        &provider,
        &model,
        None,
        RoutingRequest {
            route: route(),
            account,
            credential_version,
            policy,
        },
    ) else {
        panic!("the test catalog must issue its own row")
    };

    // Retire the release the target was minted from.
    let (Some(base), Some(family), Some(next)) = (
        BaseModelId::new(MODEL),
        ModelFamilyId::new(PROVIDER),
        ModelCatalogVersion::new("test-catalog-2"),
    ) else {
        panic!("successor coordinates must be non-empty")
    };
    let record = ModelCatalogRecord::new(provider, model, base, family, None);
    // ORDER IS THE WITNESS. The authority is built while the issuance is still
    // live — the authority builder refuses a stale target outright, so a graph
    // over one cannot even be constructed — and the release is retired
    // afterwards. That is the only window this check defends: the target was
    // current when the graph was built and is not current when the attempt binds.
    // Checking at graph-build time cannot see it, because that check already ran.
    let a = authority_over(target, &[]);
    assert!(
        a.model().is_current_issuance(),
        "the graph must be built while live, or the builder would refuse it first"
    );
    assert!(
        registry.supersede(next, vec![record]).is_some(),
        "a well-formed successor must supersede"
    );
    assert!(
        !a.model().is_current_issuance(),
        "the carried target must now be stale, or this witness proves nothing"
    );

    let model_ref = a.model().model_ref().clone();
    let request = ChatGptCodexRequest {
        model: model_ref,
        input: vec![message().into()],
        prompt_cache_key: None,
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
        reasoning: Default::default(),
        turn: Default::default(),
    };
    let refusal = ProviderAttemptCarrier::bind(attempt(), a, endpoint(), lease(), request).err();
    assert!(
        matches!(refusal, Some(CarrierRefusal::StaleIssuance { .. })),
        "a withdrawn release is refused as STALE, not as a coordinate disagreement: {refusal:?}"
    );
}

/// An endpoint issued for another route is refused.
#[test]
fn an_endpoint_for_another_route_is_refused() {
    let Some(other) = ModelRouteId::new("route_other") else {
        panic!("route id")
    };
    let Ok(foreign) = crate::route::EndpointConfig::new()
        .with_attempt_override("https://chatgpt.com/backend-api/codex")
        .resolve(crate::request::ProtocolFamily::ChatGptCodex)
        .map(|resolved| resolved.bind(crate::request::ProtocolFamily::ChatGptCodex, other))
    else {
        panic!("foreign endpoint")
    };
    let a = authority(&[]);
    let request = ChatGptCodexRequest {
        model: a.model().model_ref().clone(),
        input: vec![message().into()],
        prompt_cache_key: None,
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
        reasoning: Default::default(),
        turn: Default::default(),
    };
    assert_eq!(
        ProviderAttemptCarrier::bind(attempt(), a, foreign, lease(), request).err(),
        Some(CarrierRefusal::EndpointServesAnotherRoute),
        "the endpoint must serve the route the authority resolved"
    );
}

/// A lease carrying another account or credential version is refused.
#[test]
fn a_lease_for_another_account_or_credential_version_is_refused() {
    let (Some(account), Some(rotated)) = (AccountId::new("acct_test"), CredentialVersion::new("8"))
    else {
        panic!("credential coordinates")
    };
    let Ok(rotated_lease) = CredentialLease::issue(
        account,
        rotated,
        CredentialPlacement::BearerAuthorization,
        Secret::new(SECRET_MATERIAL.to_owned()),
    ) else {
        panic!("rotated lease")
    };
    let a = authority(&[]);
    let request = ChatGptCodexRequest {
        model: a.model().model_ref().clone(),
        input: vec![message().into()],
        prompt_cache_key: None,
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
        reasoning: Default::default(),
        turn: Default::default(),
    };
    assert_eq!(
        ProviderAttemptCarrier::bind(attempt(), a, endpoint(), rotated_lease, request).err(),
        Some(CarrierRefusal::LeaseCarriesAnotherCredentialVersion),
        "a rotated credential is a different resolved target"
    );

    let (Some(other_account), Some(version)) =
        (AccountId::new("acct_other"), CredentialVersion::new("7"))
    else {
        panic!("credential coordinates")
    };
    let Ok(other_lease) = CredentialLease::issue(
        other_account,
        version,
        CredentialPlacement::BearerAuthorization,
        Secret::new(SECRET_MATERIAL.to_owned()),
    ) else {
        panic!("other lease")
    };
    let a = authority(&[]);
    let request = ChatGptCodexRequest {
        model: a.model().model_ref().clone(),
        input: vec![message().into()],
        prompt_cache_key: None,
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
        reasoning: Default::default(),
        turn: Default::default(),
    };
    assert_eq!(
        ProviderAttemptCarrier::bind(attempt(), a, endpoint(), other_lease, request).err(),
        Some(CarrierRefusal::LeaseCarriesAnotherAccount),
        "another account is another resolved target"
    );
}

/// covers() requires BOTH the attempt and the issuance.
///
/// Two issue calls on one un-superseded registry produce two distinct targets
/// that are nonetheless the same issuance, so an issuance-only predicate would
/// answer yes for a sibling attempt. The attempt argument makes that unspellable.
#[test]
fn covers_requires_the_attempt_and_not_only_the_issuance() {
    // The sibling must share the issuance, or dropping the attempt check would
    // still refuse it and this witness would pass for the wrong reason. An
    // earlier draft built it from a second catalog LOAD, which is a different
    // issuance, and the mutation harness caught that the row survived.
    let (bound, sibling) = siblings_of_one_issuance();
    assert!(
        bound.is_same_issuance(&sibling),
        "the sibling must be the SAME issuance, or this witness proves nothing"
    );
    let a = authority_over(bound, &[]);
    let Ok(carrier) = bind_codex(a, Vec::new(), None) else {
        panic!("test coordinates must bind")
    };
    let (custody, _e, _l, _r) = carrier.into_parts();

    let Some(other_attempt) = PhysicalAttemptId::new("pat_other") else {
        panic!("attempt id")
    };
    assert!(
        !custody.covers(&other_attempt, &sibling),
        "a sibling attempt is not covered, however alike the issuance"
    );

    let foreign = issue_from(catalog("rogue-catalog"));
    assert!(
        !custody.covers(&attempt(), &foreign),
        "a foreign issuance is not covered, however alike the text"
    );
}

#[test]
fn native_search_must_be_visible_in_the_same_authority_as_function_tools() {
    use crate::authority::TargetedRequest;
    for anthropic in [false, true] {
        for visible in [None, Some(false), Some(true)] {
            let tools = visible
                .map(|visible| vec![("web_search", visible)])
                .unwrap_or_default();
            let authority = authority(&tools);
            let request = if anthropic {
                crate::ProviderRequest::AnthropicMessages(AnthropicMessagesRequest {
                    model: authority.model().model_ref().clone(),
                    messages: vec![message().into()],
                    max_tokens: MaxOutputTokens::new(1024).unwrap_or_else(|| panic!("bound")),
                    uncached_system_suffix: None,
                    effort: crate::EffortPolicy::default(),
                    thinking: None,
                    instruction_mode: crate::AnthropicInstructionMode::CurrentPrefix,
                    prompt_cache: crate::AnthropicPromptCache::Off,
                    tools: Vec::new(),
                    web_search: Some(
                        crate::AnthropicWebSearchSpec::live(1).unwrap_or_else(|_| panic!("uses")),
                    ),
                    tool_choice: None,
                })
            } else {
                crate::ProviderRequest::ChatGptCodex(ChatGptCodexRequest {
                    model: authority.model().model_ref().clone(),
                    input: vec![message().into()],
                    prompt_cache_key: None,
                    tools: Vec::new(),
                    web_search: Some(crate::WebSearchSpec::new(crate::WebSearchAccess::Cached)),
                    tool_choice: None,
                    reasoning: Default::default(),
                    turn: Default::default(),
                })
            };
            assert_eq!(request.offered_tools(), vec!["web_search"]);
            let result =
                ProviderAttemptCarrier::bind(attempt(), authority, endpoint(), lease(), request);
            if visible == Some(true) {
                assert!(result.is_ok());
            } else {
                assert_eq!(
                    result.err(),
                    Some(CarrierRefusal::HiddenTool {
                        name: "web_search".into()
                    })
                );
            }
        }
    }
}
