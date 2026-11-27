//! Public opener witnesses.
//!
//! Every witness here drives the PUBLIC [`AttemptOpener::open`] — the sealed
//! issue/bind/from_carrier sequence is exercised through the one production
//! door, never re-plumbed. Fixture bindings: the deterministic clean-terminal
//! witness carries the `provider-transcript-reasoning-text-usage-stop` shape
//! (reasoning, text, usage, stop); the zero-byte refusals carry the
//! obligation "stale/cross-catalog refusal with zero provider bytes".

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::TryRecvError;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;
use crate::deterministic::ScriptBuilder;
use crate::fragment::{FinishReason, TokenUsage};
use crate::normalize::StreamValidator;
use crate::request::ChatGptCodexRequest;
use grapt_policy::PhysicalAttemptAuthoritySnapshot;
use grapt_types::usage::UsageField;
use grapt_types::{
    AccountId, BaseModelId, CredentialVersion, ModelCatalog, ModelCatalogRecord,
    ModelCatalogVersion, ModelFamilyId, ModelId, ModelRegistry, ModelRouteId, PhysicalAttemptId,
    PolicyVersion, ProviderId, ResolvedModelTarget, RoutingRequest, Secret, TokenCount,
};

const PROVIDER: &str = "openai";
const MODEL: &str = "gpt-5-codex";
const SECRET_MATERIAL: &str = "opener-token-never-printed";

#[path = "tests/account_cooldown.rs"]
mod account_cooldown;

fn ids() -> (ProviderId, ModelId) {
    let (Some(provider), Some(model)) = (ProviderId::new(PROVIDER), ModelId::new(MODEL)) else {
        panic!("test ids must be non-empty")
    };
    (provider, model)
}

fn route() -> ModelRouteId {
    match ModelRouteId::new("route_u2") {
        Some(route) => route,
        None => panic!("test route must be non-empty"),
    }
}

fn records() -> Vec<ModelCatalogRecord> {
    let (provider, model) = ids();
    let (Some(base), Some(family)) = (BaseModelId::new(MODEL), ModelFamilyId::new(PROVIDER)) else {
        panic!("test catalog coordinates must be non-empty")
    };
    vec![ModelCatalogRecord::new(provider, model, base, family, None)]
}

fn catalog(version: &str) -> ModelCatalog {
    let Some(version) = ModelCatalogVersion::new(version) else {
        panic!("test catalog version must be non-empty")
    };
    match ModelCatalog::load(version, records()) {
        Some(catalog) => catalog,
        None => panic!("test catalog must load"),
    }
}

fn routing(account: &str) -> RoutingRequest {
    let (Some(account), Some(credential_version), Some(policy)) = (
        AccountId::new(account),
        CredentialVersion::new("7"),
        PolicyVersion::new("policy_u2"),
    ) else {
        panic!("test routing coordinates must be non-empty")
    };
    RoutingRequest {
        route: route(),
        account,
        credential_version,
        policy,
    }
}

fn issue(registry: &ModelRegistry, account: &str) -> ResolvedModelTarget {
    let (provider, model) = ids();
    match registry.issue(&provider, &model, None, routing(account)) {
        Ok(target) => target,
        Err(refusal) => panic!("the test catalog must issue its own row: {refusal:?}"),
    }
}

fn authority_of(target: ResolvedModelTarget) -> PhysicalAttemptAuthoritySnapshot {
    let mut builder = PhysicalAttemptAuthoritySnapshot::builder();
    if builder.model(target).is_err() {
        panic!("a registry-issued target must enter its own authority");
    }
    match builder.build() {
        Ok(snapshot) => snapshot,
        Err(error) => panic!("test authority must build: {error:?}"),
    }
}

fn attempt(id: &str) -> PhysicalAttemptId {
    match PhysicalAttemptId::new(id) {
        Some(attempt) => attempt,
        None => panic!("test attempt id must be non-empty"),
    }
}

fn credential(account: &str) -> OpenerCredential {
    let (Some(account), Some(version)) = (AccountId::new(account), CredentialVersion::new("7"))
    else {
        panic!("test credential coordinates must be non-empty")
    };
    match OpenerCredential::inject(
        account,
        version,
        crate::credential::CredentialPlacement::BearerAuthorization,
        Secret::new(SECRET_MATERIAL.to_owned()),
    ) {
        Ok(credential) => credential,
        Err(error) => panic!("test credential: {error}"),
    }
}

/// The Codex request every witness here sends, already wrapped in the closed
/// dispatch. Wrapping happens once here rather than at every call site.
fn request_for(authority: &PhysicalAttemptAuthoritySnapshot) -> ProviderRequest {
    ProviderRequest::ChatGptCodex(codex_request_for(authority))
}

fn codex_request_for(authority: &PhysicalAttemptAuthoritySnapshot) -> ChatGptCodexRequest {
    ChatGptCodexRequest {
        model: authority.model().model_ref().clone(),
        input: vec![crate::request::InputMessage {
            role: crate::request::MessageRole::User,
            content: "hello".into(),
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

fn transcript_script() -> Script {
    let reported = |value| match TokenCount::new(value) {
        Ok(count) => UsageField::reported(count),
        Err(error) => panic!("fixture token count is bounded: {error}"),
    };
    ScriptBuilder::new()
        .reasoning(&["thinking"])
        .text(&["Hel", "lo"])
        .usage(TokenUsage::new(
            reported(11),
            reported(11),
            reported(2),
            reported(1),
            UsageField::ABSENT,
            UsageField::ABSENT,
            reported(13),
        ))
        .finish(FinishReason::Stop, None)
}

async fn drain(reader: &mut AttemptReader) -> Vec<AttemptFragment> {
    let mut fragments = Vec::new();
    while let Some(fragment) = reader.next_fragment().await {
        fragments.push(fragment);
    }
    fragments
}

#[test]
fn a_stale_issuance_refuses_and_consumes_no_script() {
    let mut registry = ModelRegistry::install(catalog("u2-first"));
    let authority = authority_of(issue(&registry, "acct_u2"));
    if registry
        .supersede(
            match ModelCatalogVersion::new("u2-second") {
                Some(version) => version,
                None => panic!("test version must be non-empty"),
            },
            records(),
        )
        .is_none()
    {
        panic!("the successor release must admit");
    }

    let opener =
        AttemptOpener::deterministic([transcript_script()], route(), credential("acct_u2"));
    let request = request_for(&authority);
    match opener.open(attempt("pat_u2_stale"), authority, request) {
        Err(AttemptOpenRefusal::Carrier(CarrierRefusal::StaleIssuance { .. })) => {}
        other => panic!("a stale issuance must refuse as stale: {other:?}"),
    }

    let fresh = authority_of(issue(&registry, "acct_u2"));
    let request = request_for(&fresh);
    match opener.open(attempt("pat_u2_fresh"), fresh, request) {
        Ok(_) => {}
        Err(error) => panic!("a current issuance must open: {error:?}"),
    }
}

#[test]
fn a_request_vouched_by_another_catalog_refuses_before_transport() {
    let registry = ModelRegistry::install(catalog("u2-genuine"));
    let genuine = authority_of(issue(&registry, "acct_u2"));

    let rogue_registry = ModelRegistry::install(catalog("u2-rogue"));
    let rogue = issue(&rogue_registry, "acct_u2");
    let request = ProviderRequest::ChatGptCodex(ChatGptCodexRequest {
        model: rogue.model_ref().clone(),
        input: Vec::new(),
        prompt_cache_key: None,
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
        reasoning: Default::default(),
        turn: Default::default(),
    });

    let opener =
        AttemptOpener::deterministic([transcript_script()], route(), credential("acct_u2"));
    match opener.open(attempt("pat_u2_rogue"), genuine, request) {
        Err(AttemptOpenRefusal::Carrier(CarrierRefusal::RequestFromAnotherCatalog)) => {}
        other => panic!("a cross-catalog request must refuse as such: {other:?}"),
    }
}

#[test]
fn an_opener_configured_for_another_route_refuses_the_binding() {
    let registry = ModelRegistry::install(catalog("u2-route"));
    let authority = authority_of(issue(&registry, "acct_u2"));
    let request = request_for(&authority);

    let Some(other_route) = ModelRouteId::new("route_other") else {
        panic!("test route must be non-empty")
    };
    let opener =
        AttemptOpener::deterministic([transcript_script()], other_route, credential("acct_u2"));
    match opener.open(attempt("pat_u2_route"), authority, request) {
        Err(AttemptOpenRefusal::Carrier(CarrierRefusal::EndpointServesAnotherRoute)) => {}
        other => panic!("a foreign route must refuse: {other:?}"),
    }
}

#[test]
fn an_opener_holding_another_account_refuses_the_binding() {
    let registry = ModelRegistry::install(catalog("u2-acct"));
    let authority = authority_of(issue(&registry, "acct_u2"));
    let request = request_for(&authority);

    let opener =
        AttemptOpener::deterministic([transcript_script()], route(), credential("acct_other"));
    match opener.open(attempt("pat_u2_acct"), authority, request) {
        Err(AttemptOpenRefusal::Carrier(CarrierRefusal::LeaseCarriesAnotherAccount)) => {}
        other => panic!("a foreign account must refuse: {other:?}"),
    }
}

#[test]
fn the_wire_arm_contacts_nothing_when_the_binding_refuses() {
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(error) => panic!("loopback bind: {error}"),
    };
    if let Err(error) = listener.set_nonblocking(true) {
        panic!("loopback nonblocking: {error}");
    }
    let address = match listener.local_addr() {
        Ok(address) => address,
        Err(error) => panic!("loopback addr: {error}"),
    };

    let mut registry = ModelRegistry::install(catalog("u2-wire-stale"));
    let authority = authority_of(issue(&registry, "acct_u2"));
    if registry
        .supersede(
            match ModelCatalogVersion::new("u2-wire-second") {
                Some(version) => version,
                None => panic!("test version must be non-empty"),
            },
            records(),
        )
        .is_none()
    {
        panic!("the successor release must admit");
    }

    let opener = match AttemptOpener::codex_loopback(
        deadlines(),
        limits(),
        &format!("http://{address}"),
        route(),
        credential("acct_u2"),
    ) {
        Ok(opener) => opener,
        Err(error) => panic!("loopback opener: {error:?}"),
    };

    let request = request_for(&authority);
    match opener.open(attempt("pat_u2_wire_stale"), authority, request) {
        Err(AttemptOpenRefusal::Carrier(CarrierRefusal::StaleIssuance { .. })) => {}
        other => panic!("a stale issuance must refuse as stale: {other:?}"),
    }

    match listener.accept() {
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
        Ok(_) => panic!("the refused open contacted the endpoint"),
        Err(error) => panic!("loopback accept: {error}"),
    }
}

#[test]
fn a_post_custody_refusal_returns_the_custody() {
    let registry = ModelRegistry::install(catalog("u2-post-custody"));
    let authority = authority_of(issue(&registry, "acct_u2"));
    let request = request_for(&authority);

    let opener = AttemptOpener::deterministic([], route(), credential("acct_u2"));
    match opener.open(attempt("pat_u2_post"), authority, request) {
        Err(AttemptOpenRefusal::Deterministic { custody, .. }) => {
            assert_eq!(custody.attempt(), &attempt("pat_u2_post"));
        }
        other => panic!("a post-custody refusal must return the custody: {other:?}"),
    }
}

#[tokio::test]
async fn the_deterministic_arm_yields_reasoning_text_usage_and_stop() {
    let registry = ModelRegistry::install(catalog("u2-clean"));
    let authority = authority_of(issue(&registry, "acct_u2"));
    let request = request_for(&authority);
    let opener =
        AttemptOpener::deterministic([transcript_script()], route(), credential("acct_u2"));

    let opened = match opener.open(attempt("pat_u2_clean"), authority, request) {
        Ok(opened) => opened,
        Err(error) => panic!("a matching binding must open: {error:?}"),
    };
    let (custody, mut reader) = opened.into_parts();
    assert_eq!(custody.attempt(), &attempt("pat_u2_clean"));
    assert!(custody.issuance_is_current());

    let fragments = drain(&mut reader).await;
    let mut validator = StreamValidator::new();
    for fragment in &fragments {
        if validator.accept(fragment).is_err() {
            panic!("the opener emitted an illegal sequence: {fragments:?}");
        }
    }
    assert!(fragments
        .iter()
        .any(|f| matches!(f, AttemptFragment::ReasoningDelta { .. })));
    assert!(fragments
        .iter()
        .any(|f| matches!(f, AttemptFragment::Usage { .. })));
    let text: String = fragments
        .iter()
        .filter_map(|fragment| match fragment {
            AttemptFragment::TextDelta { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello");
    assert!(matches!(
        fragments.last(),
        Some(AttemptFragment::ProviderTerminal {
            finish: FinishReason::Stop,
            ..
        })
    ));
}

fn deadlines() -> CodexDeadlines {
    match CodexDeadlines::new(
        std::time::Duration::from_secs(5),
        std::time::Duration::from_secs(5),
    ) {
        Ok(deadlines) => deadlines,
        Err(error) => panic!("test deadlines: {error}"),
    }
}

fn limits() -> CodexLimits {
    match std::num::NonZeroUsize::new(4) {
        Some(capacity) => CodexLimits::new(capacity),
        None => panic!("test capacity must be non-zero"),
    }
}

fn capture_capacity(value: usize) -> std::num::NonZeroUsize {
    match std::num::NonZeroUsize::new(value) {
        Some(value) => value,
        None => panic!("capture test capacity must be non-zero"),
    }
}

fn capture_opener(exchange: crate::transport::capture::CaptureExchange) -> AttemptOpener {
    match AttemptOpener::loopback(
        ProtocolFamily::ChatGptCodex,
        deadlines(),
        limits(),
        "http://127.0.0.1:1",
        route(),
        credential("acct_u2"),
    ) {
        Ok(opener) => opener.with_capture(exchange),
        Err(error) => panic!("capture opener: {error:?}"),
    }
}

fn matching_binding(version: &str) -> (PhysicalAttemptAuthoritySnapshot, ProviderRequest) {
    let registry = ModelRegistry::install(catalog(version));
    let authority = authority_of(issue(&registry, "acct_u2"));
    let request = request_for(&authority);
    (authority, request)
}

#[test]
fn an_attempt_identity_at_the_byte_ceiling_is_admitted() {
    let (exchange, attempts) = crate::transport::capture::capture_exchange(
        capture_capacity(1),
        capture_capacity(1),
        capture_capacity(64),
    );
    let opener = capture_opener(exchange);
    let (authority, request) = matching_binding("attempt-id-at-ceiling");
    let identity = "a".repeat(MAX_PHYSICAL_ATTEMPT_ID_BYTES);

    let opened = match opener.open(attempt(&identity), authority, request) {
        Ok(opened) => opened,
        Err(error) => panic!("an identity at the declared ceiling must open: {error:?}"),
    };
    let announced = match attempts.try_recv() {
        Ok(announced) => announced,
        Err(error) => panic!("the bounded identity must be announced: {error}"),
    };
    assert_eq!(
        announced.attempt().as_str().len(),
        MAX_PHYSICAL_ATTEMPT_ID_BYTES
    );
    drop(opened);
}

#[test]
fn an_oversized_attempt_identity_is_refused_before_empty_or_full_capture_announcement() {
    let oversized_bytes = MAX_PHYSICAL_ATTEMPT_ID_BYTES + 1;
    let oversized = "x".repeat(oversized_bytes);

    let (empty_exchange, empty_attempts) = crate::transport::capture::capture_exchange(
        capture_capacity(1),
        capture_capacity(1),
        capture_capacity(64),
    );
    let empty_opener = capture_opener(empty_exchange);
    let (authority, request) = matching_binding("attempt-id-empty-queue");
    match empty_opener.open(attempt(&oversized), authority, request) {
        Err(AttemptOpenRefusal::AttemptIdentityTooLarge {
            observed_bytes,
            max_bytes,
        }) => {
            assert_eq!(observed_bytes, oversized_bytes);
            assert_eq!(max_bytes, MAX_PHYSICAL_ATTEMPT_ID_BYTES);
        }
        Err(other) => panic!("the oversized identity got the wrong refusal: {other:?}"),
        Ok(_) => panic!("an oversized identity must be refused with an empty announcement queue"),
    }
    assert!(
        matches!(empty_attempts.try_recv(), Err(TryRecvError::Empty)),
        "the empty announcement queue must remain empty"
    );

    let (full_exchange, full_attempts) = crate::transport::capture::capture_exchange(
        capture_capacity(1),
        capture_capacity(1),
        capture_capacity(64),
    );
    let held_tap = match full_exchange.open(&attempt("occupied_attempt")) {
        Some(tap) => tap,
        None => panic!("the setup attempt must fill the announcement queue"),
    };
    let full_opener = capture_opener(full_exchange);
    let (authority, request) = matching_binding("attempt-id-full-queue");
    match full_opener.open(attempt(&oversized), authority, request) {
        Err(AttemptOpenRefusal::AttemptIdentityTooLarge {
            observed_bytes,
            max_bytes,
        }) => {
            assert_eq!(observed_bytes, oversized_bytes);
            assert_eq!(max_bytes, MAX_PHYSICAL_ATTEMPT_ID_BYTES);
        }
        Err(other) => panic!("the oversized identity got the wrong refusal: {other:?}"),
        Ok(_) => panic!("an oversized identity must be refused with a full announcement queue"),
    }
    let occupied = match full_attempts.try_recv() {
        Ok(occupied) => occupied,
        Err(error) => panic!("the original queued announcement must remain: {error}"),
    };
    assert_eq!(occupied.attempt().as_str(), "occupied_attempt");
    assert!(
        matches!(full_attempts.try_recv(), Err(TryRecvError::Empty)),
        "the full queue must contain no oversized announcement"
    );
    drop(held_tap);
}

const TEXT_STREAM: &str = concat!(
    "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hel\"}\n\n",
    "data: {\"type\":\"response.output_text.delta\",\"delta\":\"lo\"}\n\n",
    "data: {\"type\":\"response.output_text.done\"}\n\n",
    "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":11,\"output_tokens\":2,\"total_tokens\":13}}}\n\n"
);

async fn serve_counting(body: &'static str, connections: Arc<AtomicUsize>) -> String {
    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(error) => panic!("loopback bind: {error}"),
    };
    let address = match listener.local_addr() {
        Ok(address) => address,
        Err(error) => panic!("loopback addr: {error}"),
    };
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            connections.fetch_add(1, Ordering::SeqCst);
            let mut scratch = [0_u8; 4096];
            let _ = socket.read(&mut scratch).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.flush().await;
        }
    });
    format!("http://{address}")
}

#[tokio::test]
async fn the_wire_arm_sends_no_byte_until_the_reader_is_first_polled() {
    #[derive(Debug)]
    struct DispatchClock(std::sync::atomic::AtomicI64);
    impl grapt_types::ClockAuthority for DispatchClock {
        fn now(&self) -> grapt_types::Timestamp {
            grapt_types::Timestamp::from_millis(self.0.load(Ordering::SeqCst))
        }
    }
    let clock = Arc::new(DispatchClock(std::sync::atomic::AtomicI64::new(100)));
    let (observer, samples) = crate::DispatchObserver::bounded(clock.clone());
    let connections = Arc::new(AtomicUsize::new(0));
    let base = serve_counting(TEXT_STREAM, Arc::clone(&connections)).await;

    let registry = ModelRegistry::install(catalog("u2-wire"));
    let authority = authority_of(issue(&registry, "acct_u2"));
    let request = request_for(&authority);
    let opener = match AttemptOpener::codex(
        deadlines(),
        limits(),
        &base,
        route(),
        "grapt-local-provider",
        credential("acct_u2"),
    ) {
        Ok(opener) => opener,
        Err(error) => panic!("loopback opener: {error:?}"),
    };

    let opener = opener.with_dispatch_observer(observer);
    let opened = match opener.open(attempt("pat_u2_wire"), authority, request) {
        Ok(opened) => opened,
        Err(error) => panic!("a matching binding must open: {error:?}"),
    };

    let session: grapt_types::SessionId = "ses_wire_diagnostic"
        .parse()
        .unwrap_or_else(|error| panic!("diagnostic Session: {error}"));
    let opened = opened.with_diagnostic_session(&session);
    assert_eq!(connections.load(Ordering::SeqCst), 0);
    assert!(matches!(samples.try_next(), Err(TryRecvError::Empty)));
    clock.0.store(200, Ordering::SeqCst);

    let (custody, mut reader) = opened.into_parts();
    assert_eq!(custody.attempt(), &attempt("pat_u2_wire"));

    let fragments = drain(&mut reader).await;
    let sample = match samples.try_next() {
        Ok(sample) => sample,
        Err(error) => panic!("dispatch observation absent: {error}"),
    };
    assert_eq!(sample.attempt, attempt("pat_u2_wire"));
    assert_eq!(sample.at_ms, 200);
    assert_eq!(sample.session.as_ref(), Some(&session));
    assert!(
        sample.request.is_some(),
        "serialized request diagnostic absent"
    );
    assert!(sample.terminal_observed);
    assert!(!sample.usage_overflowed);
    let usage = sample
        .usage
        .iter()
        .find(|(phase, _)| *phase == grapt_types::UsageObservationPhase::Terminal)
        .map(|(_, usage)| usage)
        .unwrap_or_else(|| panic!("final usage was lost before dispatch publication"));
    assert_eq!(usage.input_total().value(), Some(11));
    assert!(
        usage.cache_read().value().is_none(),
        "missing cache read is not zero"
    );
    assert!(matches!(samples.try_next(), Err(TryRecvError::Empty)));
    assert_eq!(connections.load(Ordering::SeqCst), 1);
    let text: String = fragments
        .iter()
        .filter_map(|fragment| match fragment {
            AttemptFragment::TextDelta { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello");
    assert!(matches!(
        fragments.last(),
        Some(AttemptFragment::ProviderTerminal {
            finish: FinishReason::Stop,
            ..
        })
    ));
}

#[test]
fn durable_staged_fingerprint_needs_neither_a_dispatch_observer_nor_a_send() -> Result<(), String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    let registry = ModelRegistry::install(catalog("durable-cache-fingerprint"));
    let authority = authority_of(issue(&registry, "acct_u2"));
    let request = request_for(&authority);
    let opener = AttemptOpener::codex_loopback(
        deadlines(),
        limits(),
        &format!("http://{address}"),
        route(),
        credential("acct_u2"),
    )
    .map_err(|e| e.to_string())?;
    let opened = opener
        .open(attempt("pat_cache_before_send"), authority, request)
        .map_err(|e| e.to_string())?;
    let fingerprint = opened
        .cache_fingerprint()
        .ok_or("staged fingerprint absent")?;
    assert_eq!(fingerprint.input_items(), 1);
    assert_eq!(format!("{fingerprint:?}"), "RequestCacheFingerprint { .. }");
    match listener.accept() {
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
        _ => Err("staging a fingerprint must not send any bytes".into()),
    }
}

#[tokio::test]
async fn three_child_reservations_and_root_capacity_are_independent() -> Result<(), String> {
    let opener = AttemptOpener::codex_loopback(
        deadlines(),
        CodexLimits::new(capture_capacity(1)),
        "http://127.0.0.1:1",
        route(),
        credential("acct_u2"),
    )
    .map_err(|error| error.to_string())?
    .with_child_capacity(capture_capacity(3));
    let root = opener
        .reserve_capacity()
        .await
        .map_err(|error| error.to_string())?;
    let mut children = Vec::new();
    for _ in 0..3 {
        let child = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            opener.reserve_child_capacity_observed(capture_capacity(3), |_, _| {}),
        )
        .await
        .map_err(|_| "root consumed child capacity")?
        .map_err(|error| error.to_string())?;
        children.push(child);
    }
    assert!(tokio::time::timeout(
        std::time::Duration::from_millis(5),
        opener.reserve_child_capacity_observed(capture_capacity(3), |_, _| {})
    )
    .await
    .is_err());
    drop(root);
    let root = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        opener.reserve_capacity(),
    )
    .await
    .map_err(|_| "children consumed root capacity")?
    .map_err(|error| error.to_string())?;
    drop((root, children));
    Ok(())
}
