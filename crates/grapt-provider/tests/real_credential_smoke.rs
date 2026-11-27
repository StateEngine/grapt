//! Opt-in smoke commands against REAL providers with REAL credentials.
//!
//! # These make real, billable network requests
//!
//! Every test here is `#[ignore]`d AND gated on an environment variable that
//! must name a credential. Both guards are required, and neither is enough
//! alone:
//!
//! - `#[ignore]` keeps them out of `cargo test`, which is what CI and every
//!   gate in this repository run.
//! - The environment gate keeps them out of `cargo test -- --ignored`, which a
//!   developer might reasonably run to see what is being skipped, and which
//!   would otherwise start spending money the moment a credential happened to
//!   be exported.
//!
//! A test whose variable is unset SKIPS by returning, and says so. It does not
//! fail: an absent credential is not a defect, and a red suite would train
//! whoever runs this to ignore it.
//!
//! # Running one, deliberately
//!
//! ```text
//! GRAPT_SMOKE_ANTHROPIC_KEY=sk-ant-...  \
//!   cargo test -p grapt-provider --test real_credential_smoke \
//!   -- --ignored --nocapture anthropic
//!
//! GRAPT_SMOKE_OPENAI_KEY=sk-...         \
//!   cargo test -p grapt-provider --test real_credential_smoke \
//!   -- --ignored --nocapture openai_responses
//!
//! GRAPT_SMOKE_GROK_KEY=xai-...          \
//! GRAPT_SMOKE_GROK_BASE=https://api.x.ai/v1 \
//!   cargo test -p grapt-provider --test real_credential_smoke \
//!   -- --ignored --nocapture grok
//! ```
//!
//! `--nocapture` is worth passing: these print what they observed, and the
//! point of running one is to read that.
//!
//! # What a smoke command is FOR, and what it is not
//!
//! It is the only thing in this crate that can answer "does the wire mapping
//! match the real provider?" Every other witness drives bytes this crate
//! authored, and a fixture agreeing with the decoder that wrote it certifies
//! nothing about the provider — the fixture and the decoder share the author's
//! misunderstanding.
//!
//! It is NOT a gate. It is not in any profile, it is not run by any battery,
//! and a green run of one is not evidence that anything else is green. It is a
//! human-driven check, and its result belongs in a receipt a human wrote.
//!
//! # No credential is read from a store
//!
//! The variables below are read by THIS test file and injected through
//! `ProviderCredential::inject`, the same door production uses. This crate
//! still opens no credential store; the smoke command is the operator handing
//! it one.

use std::num::NonZeroUsize;
use std::time::Duration;

use grapt_policy::PhysicalAttemptAuthoritySnapshot;
use grapt_provider::fragment::AttemptFragment;
use grapt_provider::{
    AnthropicMessagesRequest, AttemptOpener, CodexDeadlines, CodexLimits, CredentialPlacement,
    EndpointConfig, InputMessage, MaxOutputTokens, MessageRole, OpenAiCompatibleRequest,
    OpenAiResponsesRequest, OpenerCredential, ProtocolFamily, ProviderRequest,
};
use grapt_types::{
    AccountId, BaseModelId, CredentialVersion, ModelCatalog, ModelCatalogRecord,
    ModelCatalogVersion, ModelFamilyId, ModelId, ModelRegistry, ModelRouteId, PhysicalAttemptId,
    PolicyVersion, ProviderId, RoutingRequest, Secret,
};

const ACCOUNT: &str = "smoke-account";
const CREDENTIAL_VERSION: &str = "smoke-1";

/// Read the gate variable, or explain the skip and return `None`.
fn gate(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        _ => {
            println!("SKIPPED: {name} is not set. This smoke command makes a real, billable request and will not run without an explicit credential.");
            None
        }
    }
}

fn route() -> ModelRouteId {
    match ModelRouteId::new("smoke-route") {
        Some(route) => route,
        None => panic!("smoke route id"),
    }
}

/// Build a one-row registry and issue the target for this smoke run.
///
/// The real issuance path, not a shortcut: a smoke command that bypassed the
/// carrier would be exercising a code path production does not have, and the
/// thing worth learning from a real request is what the REAL path does.
fn authority(vendor: &str, model_name: &str) -> PhysicalAttemptAuthoritySnapshot {
    let (provider, model, base, family, version) = match (
        ProviderId::new(vendor),
        ModelId::new(model_name),
        BaseModelId::new(model_name),
        ModelFamilyId::new(vendor),
        ModelCatalogVersion::new("smoke-catalog"),
    ) {
        (Some(p), Some(m), Some(b), Some(f), Some(v)) => (p, m, b, f, v),
        _ => panic!("smoke model coordinates"),
    };
    let record = ModelCatalogRecord::new(provider.clone(), model.clone(), base, family, None);
    let catalog = match ModelCatalog::load(version, vec![record]) {
        Some(catalog) => catalog,
        None => panic!("smoke catalog"),
    };
    let registry = ModelRegistry::install(catalog);

    let (account, credential_version, policy) = match (
        AccountId::new(ACCOUNT),
        CredentialVersion::new(CREDENTIAL_VERSION),
        PolicyVersion::new("smoke-policy"),
    ) {
        (Some(a), Some(c), Some(p)) => (a, c, p),
        _ => panic!("smoke routing coordinates"),
    };
    let target = match registry.issue(
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
        Err(refusal) => panic!("the smoke catalog must issue its own row: {refusal:?}"),
    };

    let mut builder = PhysicalAttemptAuthoritySnapshot::builder();
    if builder.model(target).is_err() {
        panic!("a registry-issued target must enter its own authority");
    }
    match builder.build() {
        Ok(snapshot) => snapshot,
        Err(error) => panic!("smoke authority: {error:?}"),
    }
}

fn credential(secret: String, placement: CredentialPlacement) -> OpenerCredential {
    let (account, version) = match (
        AccountId::new(ACCOUNT),
        CredentialVersion::new(CREDENTIAL_VERSION),
    ) {
        (Some(account), Some(version)) => (account, version),
        _ => panic!("smoke credential coordinates"),
    };
    match OpenerCredential::inject(account, version, placement, Secret::new(secret)) {
        Ok(credential) => credential,
        Err(error) => panic!("smoke credential refused: {error}"),
    }
}

fn deadlines() -> CodexDeadlines {
    match CodexDeadlines::new(Duration::from_secs(10), Duration::from_secs(60)) {
        Ok(deadlines) => deadlines,
        Err(error) => panic!("smoke deadlines: {error}"),
    }
}

fn limits() -> CodexLimits {
    match NonZeroUsize::new(1) {
        Some(one) => CodexLimits::new(one),
        None => panic!("one is non-zero"),
    }
}

fn hello() -> Vec<InputMessage> {
    vec![InputMessage {
        role: MessageRole::User,
        content: "Reply with exactly the word: ok".to_owned(),
    }]
}

fn bound(value: u32) -> MaxOutputTokens {
    match MaxOutputTokens::new(value) {
        Some(bound) => bound,
        None => panic!("a non-zero bound"),
    }
}

/// Drive one attempt and report what came back.
///
/// The assertions are deliberately WEAK on content and STRONG on shape. What a
/// real model says is not this crate's business and will differ run to run;
/// what must hold is that the stream opened, produced normalized fragments, and
/// reached exactly one terminal. A smoke command asserting exact text would
/// fail for reasons that are not defects, and would then be disabled.
fn run(opener: AttemptOpener, request: ProviderRequest, label: &str) {
    let attempt = match PhysicalAttemptId::new("smoke-attempt-1") {
        Some(attempt) => attempt,
        None => panic!("smoke attempt id"),
    };
    let family = request.family();
    let authority = match family {
        ProtocolFamily::GoogleGemini => authority("google", model_of(&request)),
        ProtocolFamily::VertexGemini => authority("google-vertex", model_of(&request)),
        ProtocolFamily::WatsonxChat => authority("watsonx", model_of(&request)),
        ProtocolFamily::AnthropicMessages => authority("anthropic", model_of(&request)),
        ProtocolFamily::CohereChat => authority("cohere", model_of(&request)),
        ProtocolFamily::VertexAnthropic => authority("google-vertex-anthropic", model_of(&request)),
        ProtocolFamily::OpenAiResponses | ProtocolFamily::ChatGptCodex => {
            authority("openai", model_of(&request))
        }
        ProtocolFamily::OpenAiCompatible => authority("xai", model_of(&request)),
        ProtocolFamily::BedrockConverse => authority("amazon-bedrock", model_of(&request)),
    };

    let opened = match opener.open(attempt, authority, request) {
        Ok(opened) => opened,
        Err(refusal) => panic!("{label}: the attempt was refused before any byte: {refusal:?}"),
    };
    let (_custody, mut reader) = opened.into_parts();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => panic!("smoke runtime: {error}"),
    };
    let fragments = runtime.block_on(async {
        let mut fragments = Vec::new();
        while let Some(fragment) = reader.next_fragment().await {
            fragments.push(fragment);
        }
        fragments
    });

    let mut text = String::new();
    let mut reasoning = String::new();
    let mut terminals = 0_usize;
    let mut failure = None;
    for fragment in &fragments {
        match fragment {
            AttemptFragment::TextDelta { text: delta, .. } => text.push_str(delta),
            AttemptFragment::ReasoningDelta { text: delta, .. } => reasoning.push_str(delta),
            AttemptFragment::ProviderTerminal { finish, usage, .. } => {
                terminals += 1;
                println!("{label}: terminal finish={finish:?} usage={usage:?}");
            }
            AttemptFragment::ProviderFailure { class, message, .. } => {
                failure = Some((class.clone(), message.clone()));
            }
            _ => {}
        }
    }

    println!("{label}: {} fragments", fragments.len());
    println!("{label}: text = {text:?}");
    if !reasoning.is_empty() {
        println!("{label}: reasoning = {} chars", reasoning.len());
    }

    if let Some((class, message)) = failure {
        panic!("{label}: the provider failed: {class:?} — {message}");
    }
    assert_eq!(
        terminals, 1,
        "{label}: exactly one terminal must be produced, got {terminals}"
    );
    assert!(
        !text.is_empty(),
        "{label}: a successful attempt must produce some visible text"
    );
}

fn model_of(request: &ProviderRequest) -> &str {
    request.model().model().as_str()
}

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

#[test]
#[ignore = "makes a real, billable request; set GRAPT_SMOKE_ANTHROPIC_KEY and run with --ignored"]
fn anthropic_messages_real_credential() {
    let Some(secret) = gate("GRAPT_SMOKE_ANTHROPIC_KEY") else {
        return;
    };
    let model = std::env::var("GRAPT_SMOKE_ANTHROPIC_MODEL")
        .unwrap_or_else(|_| "claude-sonnet-4-5-20250929".to_owned());

    let opener = match AttemptOpener::route(
        ProtocolFamily::AnthropicMessages,
        deadlines(),
        limits(),
        // No override: this exercises the FAMILY DEFAULT, which is the value a
        // deployment gets when it configures nothing, and therefore the one
        // most worth checking against the real service.
        EndpointConfig::new(),
        route(),
        "grapt-smoke/0.1",
        credential(secret, CredentialPlacement::AnthropicApiKey),
    ) {
        Ok(opener) => opener,
        Err(refusal) => panic!("anthropic opener: {refusal:?}"),
    };

    let request = ProviderRequest::AnthropicMessages(AnthropicMessagesRequest {
        web_search: None,
        uncached_system_suffix: None,
        effort: grapt_provider::EffortPolicy::default(),
        model: model_ref("anthropic", &model),
        messages: hello().into_iter().map(Into::into).collect(),
        max_tokens: bound(64),
        instruction_mode: grapt_provider::AnthropicInstructionMode::CurrentPrefix,
        thinking: None,
        prompt_cache: grapt_provider::AnthropicPromptCache::Off,
        tools: Vec::new(),
        tool_choice: None,
    });
    run(opener, request, "anthropic");
}

#[test]
#[ignore = "makes a real, billable request; set GRAPT_SMOKE_OPENAI_KEY and run with --ignored"]
fn openai_responses_real_credential() {
    let Some(secret) = gate("GRAPT_SMOKE_OPENAI_KEY") else {
        return;
    };
    let model =
        std::env::var("GRAPT_SMOKE_OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_owned());

    let opener = match AttemptOpener::route(
        ProtocolFamily::OpenAiResponses,
        deadlines(),
        limits(),
        EndpointConfig::new(),
        route(),
        "grapt-smoke/0.1",
        credential(secret, CredentialPlacement::BearerAuthorization),
    ) {
        Ok(opener) => opener,
        Err(refusal) => panic!("openai opener: {refusal:?}"),
    };

    let request = ProviderRequest::OpenAiResponses(OpenAiResponsesRequest {
        model: model_ref("openai", &model),
        input: hello().into_iter().map(Into::into).collect(),
        max_output_tokens: Some(bound(64)),
        prompt_cache_key: None,
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
    });
    run(opener, request, "openai-responses");
}

#[test]
#[ignore = "makes a real, billable request; set GRAPT_SMOKE_GROK_KEY and run with --ignored"]
fn grok_compatible_real_credential() {
    let Some(secret) = gate("GRAPT_SMOKE_GROK_KEY") else {
        return;
    };
    // The compatible family has NO default endpoint by design, so this command
    // must be told where to go. That is the same refusal a deployment gets, and
    // exercising it here is deliberate.
    let Some(base) = gate("GRAPT_SMOKE_GROK_BASE") else {
        return;
    };
    let model = std::env::var("GRAPT_SMOKE_GROK_MODEL").unwrap_or_else(|_| "grok-2".to_owned());

    let opener = match AttemptOpener::route(
        ProtocolFamily::OpenAiCompatible,
        deadlines(),
        limits(),
        EndpointConfig::new().with_configured_profile(base),
        route(),
        "grapt-smoke/0.1",
        credential(secret, CredentialPlacement::BearerAuthorization),
    ) {
        Ok(opener) => opener,
        Err(refusal) => panic!("grok opener: {refusal:?}"),
    };

    let request = ProviderRequest::OpenAiCompatible(OpenAiCompatibleRequest {
        reasoning_replay: Default::default(),
        prompt_cache: grapt_provider::CompatiblePromptCache::Off,
        model: model_ref("xai", &model),
        messages: hello().into_iter().map(Into::into).collect(),
        max_tokens: Some(bound(64)),
        tools: Vec::new(),
        tool_choice: None,
        routing_session: None,
    });
    run(opener, request, "grok");
}

/// A one-row catalog reference for the smoke target.
fn model_ref(vendor: &str, name: &str) -> grapt_types::ModelRef {
    let (provider, model, base, family, version) = match (
        ProviderId::new(vendor),
        ModelId::new(name),
        BaseModelId::new(name),
        ModelFamilyId::new(vendor),
        ModelCatalogVersion::new("smoke-catalog"),
    ) {
        (Some(p), Some(m), Some(b), Some(f), Some(v)) => (p, m, b, f, v),
        _ => panic!("smoke model coordinates"),
    };
    let record = ModelCatalogRecord::new(provider.clone(), model.clone(), base, family, None);
    let catalog = match ModelCatalog::load(version, vec![record]) {
        Some(catalog) => catalog,
        None => panic!("smoke catalog"),
    };
    match catalog.entry(&provider, &model, None) {
        Some(entry) => grapt_types::ModelRef::from_catalog(entry),
        None => panic!("smoke catalog entry"),
    }
}

/// The gate itself is witnessed, and this one is NOT ignored.
///
/// It runs in the ordinary suite and proves the smoke commands cannot fire by
/// accident: with no credential variable set, `gate` returns `None`, which is
/// what makes every command above return before it builds an opener.
#[test]
fn a_smoke_command_without_its_credential_does_not_run() {
    // Deliberately a variable no operator would set.
    assert!(
        gate("GRAPT_SMOKE_DEFINITELY_UNSET_a9f3").is_none(),
        "an unset gate must refuse, or the ignore attribute is the only guard"
    );
}
