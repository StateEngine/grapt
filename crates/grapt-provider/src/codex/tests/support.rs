use super::*;
pub(super) use crate::normalize::StreamValidator;
pub(super) use crate::request::{InputMessage, MessageRole, ToolChoice, ToolDefinition};
pub(super) use grapt_policy::{PhysicalAttemptAuthoritySnapshot, ToolName, ToolVisibility};
pub(super) use grapt_types::{
    BaseModelId, ModelCatalog, ModelCatalogRecord, ModelCatalogVersion, ModelFamilyId, ModelId,
    ModelRef, ModelRegistry, PolicyVersion, ProviderId, ResolvedModelTarget, RoutingRequest,
};
pub(super) use grapt_types::{
    CapabilityId, EffectClass, EgressAuthority, FilesystemAuthority, RealizedEnforcement,
    ResolvedCapability,
};
pub(super) use tokio::io::{AsyncReadExt, AsyncWriteExt};
pub(super) use tokio::net::TcpListener;

pub(super) fn deadlines() -> CodexDeadlines {
    // Explicit small injected values. These are TEST values; no production
    // default exists and none is implied by their presence here.
    match CodexDeadlines::new(Duration::from_millis(500), Duration::from_secs(5)) {
        Ok(deadlines) => deadlines,
        Err(_) => panic!("both test deadlines are non-zero"),
    }
}

pub(super) fn limits() -> CodexLimits {
    // An explicit FIXTURE capacity. It is not production policy: no
    // ratified concurrent-attempt count exists, so the production number
    // is a composition residual and this value says nothing about it.
    match std::num::NonZeroUsize::new(2) {
        Some(capacity) => CodexLimits::new(capacity),
        None => panic!("2 is non-zero"),
    }
}

pub(super) fn model() -> ModelRef {
    match (
        ProviderId::new("openai"),
        ModelId::new("gpt-5.4"),
        BaseModelId::new("gpt-5.4"),
        ModelFamilyId::new("openai"),
        ModelCatalogVersion::new("test-catalog"),
    ) {
        (Some(provider), Some(model), Some(base), Some(family), Some(version)) => {
            let record =
                ModelCatalogRecord::new(provider.clone(), model.clone(), base, family, None);
            let catalog = match ModelCatalog::load(version, vec![record]) {
                Some(catalog) => catalog,
                None => panic!("test catalog"),
            };
            match catalog.entry(&provider, &model, None) {
                Some(entry) => ModelRef::from_catalog(entry),
                None => panic!("catalog entry"),
            }
        }
        _ => panic!("test model coordinates must be non-empty"),
    }
}

pub(super) fn request() -> ChatGptCodexRequest {
    ChatGptCodexRequest {
        model: model(),
        input: vec![InputMessage {
            role: MessageRole::User,
            content: "Hello".into(),
        }
        .into()],
        prompt_cache_key: None,
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
        reasoning: Default::default(),
        turn: Default::default(),
    }
}

pub(super) fn lease() -> CredentialLease {
    match (AccountId::new("acct_test"), CredentialVersion::new("7")) {
        (Some(account), Some(version)) => {
            match CredentialLease::issue(
                account,
                version,
                crate::credential::CredentialPlacement::BearerAuthorization,
                Secret::new("token-value-never-printed".to_owned()),
            ) {
                Ok(lease) => lease,
                Err(_) => panic!("a non-empty test secret is a valid lease"),
            }
        }
        _ => panic!("test credential coordinates must be non-empty"),
    }
}

/// The route every test endpoint and target agrees on.
pub(super) fn route() -> ModelRouteId {
    match ModelRouteId::new("route_test") {
        Some(route) => route,
        None => panic!("test route id must be non-empty"),
    }
}

/// A registry-issued target, through the REAL path.
///
/// This is what the nine `prepared_request` witnesses were blocked on: until
/// `ModelRegistry` landed there was no route by which any component could
/// hold a `ResolvedModelTarget`, so these fixtures could only carry inert
/// `ModelRef` metadata where authority was required. Nothing here widens a
/// seal — `ResolvedModelTarget::resolve` is still crate-private to
/// `grapt-types`, and issuance is the narrower public door it always
/// intended.
pub(super) fn issued_target() -> ResolvedModelTarget {
    let (Some(provider), Some(model), Some(base), Some(family), Some(version)) = (
        ProviderId::new("openai"),
        ModelId::new("gpt-5.4"),
        BaseModelId::new("gpt-5.4"),
        ModelFamilyId::new("openai"),
        ModelCatalogVersion::new("test-catalog"),
    ) else {
        panic!("test model coordinates must be non-empty")
    };
    let record = ModelCatalogRecord::new(provider.clone(), model.clone(), base, family, None);
    let Some(catalog) = ModelCatalog::load(version, vec![record]) else {
        panic!("test catalog")
    };
    let registry = ModelRegistry::install(catalog);
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
        Err(refusal) => panic!("the test catalog must issue its own row: {refusal:?}"),
    }
}

/// A bound carrier over the test coordinates. Every coordinate is derived
/// from the SAME issuance, so a refusal here is a real defect rather than a
/// fixture that disagreed with itself.
/// An authority graph over a registry-issued target, with the named tools
/// marked visible or not.
pub(super) fn authority(visible: &[(&str, bool)]) -> PhysicalAttemptAuthoritySnapshot {
    let mut builder = PhysicalAttemptAuthoritySnapshot::builder();
    if builder.model(issued_target()).is_err() {
        panic!("a registry-issued target must enter its own authority graph");
    }
    for (name, is_visible) in visible {
        let Some(tool) = ToolName::new((*name).to_owned()) else {
            panic!("test tool name must be non-empty")
        };
        // Derived, never authored: `CapabilityId` has no public
        // constructor, so a tool's required capability is the canonical
        // form of a resolved ToolInvoke effect.
        let required = CapabilityId::derive(&ResolvedCapability::new(
            EffectClass::ToolInvoke,
            None,
            FilesystemAuthority::new(Vec::new(), Vec::new(), Vec::new()),
            EgressAuthority::default(),
            RealizedEnforcement::HarnessOnly,
        ));
        let visibility = ToolVisibility {
            tool,
            required,
            visible: *is_visible,
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

pub(super) fn request_for(authority: &PhysicalAttemptAuthoritySnapshot) -> ChatGptCodexRequest {
    ChatGptCodexRequest {
        model: authority.model().model_ref().clone(),
        input: vec![InputMessage {
            role: MessageRole::User,
            content: "Hello".into(),
        }
        .into()],
        prompt_cache_key: None,
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
        reasoning: Default::default(),
        turn: Default::default(),
    }
}

pub(super) fn carrier(
    id: &str,
    endpoint: ProviderEndpoint,
) -> ProviderAttemptCarrier<ProviderEndpoint, CredentialLease, ProviderRequest> {
    let authority = authority(&[]);
    let request = request_for(&authority);
    match ProviderAttemptCarrier::bind(
        attempt(id),
        authority,
        endpoint,
        lease(),
        ProviderRequest::ChatGptCodex(request),
    ) {
        Ok(carrier) => carrier,
        Err(refusal) => panic!("test coordinates must bind: {refusal:?}"),
    }
}

pub(super) fn attempt(id: &str) -> PhysicalAttemptId {
    match PhysicalAttemptId::new(id) {
        Some(attempt) => attempt,
        None => panic!("test attempt id must be non-empty"),
    }
}

// -----------------------------------------------------------------------
// Z01 carrier — the wire key set.
// -----------------------------------------------------------------------

// -----------------------------------------------------------------------
// Loopback fixtures. These bytes are authored by this module; they are not
// and are never described as a capture of a live ChatGPT response.
// -----------------------------------------------------------------------

pub(super) async fn serve_once(body: &'static str, status_line: &'static str) -> String {
    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(error) => panic!("loopback bind: {error}"),
    };
    let address = match listener.local_addr() {
        Ok(address) => address,
        Err(error) => panic!("loopback addr: {error}"),
    };
    tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut scratch = [0_u8; 4096];
            let _ = socket.read(&mut scratch).await;
            let response = format!(
                    "{status_line}\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.flush().await;
        }
    });
    format!("http://{address}")
}

pub(super) async fn drain(stream: &mut ProviderStream) -> Vec<AttemptFragment> {
    let mut fragments = Vec::new();
    while let Some(fragment) = stream.next_fragment().await {
        fragments.push(fragment);
    }
    fragments
}

pub(super) fn input_for(base: &str, id: &str) -> AttemptInput {
    let base = match crate::route::LoopbackBase::parse(base) {
        Ok(base) => base,
        Err(error) => panic!("loopback base: {error}"),
    };
    let endpoint =
        ProviderEndpoint::loopback(&base, crate::request::ProtocolFamily::ChatGptCodex, route());
    // Through the carrier, because there is no longer any other way in.
    // Custody leaves by its own door and the aggregate would retain it;
    // these transport tests drive only the wire half.
    match input_from_carrier(carrier(id, endpoint), "grapt-test/0.1") {
        Ok((input, _custody)) => input,
        Err(error) => panic!("attempt input: {error}"),
    }
}

pub(super) const TEXT_STREAM: &str = concat!(
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hel\"}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"lo\"}\n\n",
        "data: {\"type\":\"response.output_text.done\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":11,\"output_tokens\":2,\"total_tokens\":13}}}\n\n"
    );

pub(super) const LENGTH_STREAM: &str = concat!(
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n",
        "data: {\"type\":\"response.incomplete\",\"response\":{\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n"
    );

pub(super) const TOOL_STREAM: &str = concat!(
        "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"
    );
