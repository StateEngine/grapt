//! U2 production-source witnesses: the composed vertical from plan through
//! sealed open, durable reservation, the one supervisor, and receipt
//! settlement inside the Step-terminal commit.
//!
//! Fixture bindings, named per the release: the clean-terminal witness carries
//! the `provider-transcript-reasoning-text-usage-stop` shape; the
//! rate-limit-after-visible-output witness carries `visible-output-no-retry`
//! (one physical call, partial transcript preserved, Step failed typed).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use grapt_inbox::Inbox;
use grapt_provider::deterministic::ScriptBuilder;
use grapt_provider::request::ProviderRequest;
use grapt_provider::{
    AttemptOpener, CodexDeadlines, FinishReason, OpenerCredential,
    PhysicalAttemptAuthoritySnapshot, ProviderFailureClass, Script, TokenUsage,
};
use grapt_store::{AggregateId, AttemptRecordCeiling, EventStore};
use grapt_types::usage::UsageField;
use grapt_types::{
    AccountId, AttemptTerminalReason, BaseModelId, CredentialVersion, EventId, IdAuthority,
    IdToken, InterruptionSource, LostEffect, ModelCatalog, ModelCatalogRecord, ModelCatalogVersion,
    ModelFamilyId, ModelId, ModelRegistry, ModelRouteId, PhysicalAttemptId, PolicyVersion,
    ProviderFailureClass as TerminalClass, ProviderId, ReceiptDisposition, ResolvedModelTarget,
    RoutingRequest, Secret, SessionId, StepNumber, StepTerminalReason, TokenCount,
};

#[path = "test_completion.rs"]
mod completion;
use completion::terminal_rows;
#[path = "test_request_cache.rs"]
mod request_cache_witness;
use request_cache_witness::request_rows;

use super::*;
use crate::application::{Clock, EventIds};
use crate::owner::{DurableFact, RecordingOwner};
use crate::settlement::{StepSettlementContext, StepSettlementOwner};
use crate::step::{step_interrupt, NoCompaction, StepDriver, StepPolicy, StepResult};
use crate::supervisor::{BoxFuture, ToolRunner};
use crate::{StepOutcome, StepReport, ToolOutcome};

const PROVIDER: &str = "openai";
const MODEL: &str = "gpt-5-codex";

fn ids() -> (ProviderId, ModelId) {
    let (Some(provider), Some(model)) = (ProviderId::new(PROVIDER), ModelId::new(MODEL)) else {
        panic!("test ids must be non-empty")
    };
    (provider, model)
}

fn route() -> ModelRouteId {
    match ModelRouteId::new("route_u2_runtime") {
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

fn routing() -> RoutingRequest {
    let (Some(account), Some(credential_version), Some(policy)) = (
        AccountId::new("acct_u2_runtime"),
        CredentialVersion::new("7"),
        PolicyVersion::new("policy_u2_runtime"),
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

fn issue(registry: &ModelRegistry) -> ResolvedModelTarget {
    let (provider, model) = ids();
    match registry.issue(&provider, &model, None, routing()) {
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

fn credential() -> OpenerCredential {
    let (Some(account), Some(version)) = (
        AccountId::new("acct_u2_runtime"),
        CredentialVersion::new("7"),
    ) else {
        panic!("test credential coordinates must be non-empty")
    };
    match OpenerCredential::inject(
        account,
        version,
        grapt_provider::CredentialPlacement::BearerAuthorization,
        Secret::new("runtime-token".to_owned()),
    ) {
        Ok(credential) => credential,
        Err(error) => panic!("test credential: {error}"),
    }
}

#[path = "test_plan.rs"]
mod plan;
use plan::{request_for, RegistryPlan, StalePlan, StructuralRefusalPlan};

#[derive(Debug)]
struct TestClock;
impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        1_724_000_000
    }
}

#[derive(Debug)]
struct RetrogradeClock(AtomicUsize);
impl Clock for RetrogradeClock {
    fn now_ms(&self) -> i64 {
        match self.0.fetch_add(1, Ordering::SeqCst) {
            0 => 1_724_000_000,
            _ => 1_723_999_999,
        }
    }
}

#[derive(Debug)]
struct SettlementClock;
impl Clock for SettlementClock {
    fn now_ms(&self) -> i64 {
        1_724_001_000
    }
}

#[derive(Debug)]
struct TestEventIds;
impl EventIds for TestEventIds {
    fn mint(&self) -> EventId {
        match "evt_u2_step_terminal".parse() {
            Ok(id) => id,
            Err(error) => panic!("test event id must parse: {error}"),
        }
    }
}

#[derive(Debug)]
struct SeqAttemptIds(AtomicUsize);
impl IdAuthority for SeqAttemptIds {
    fn mint(&self) -> IdToken {
        let ordinal = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        match IdToken::new(format!("pat_u2_{ordinal}")) {
            Some(token) => token,
            None => panic!("test token must be non-empty"),
        }
    }
}

struct NoTools;
impl ToolRunner for NoTools {
    fn run(
        &self,
        _call: grapt_types::ToolCallId,
        _tool: String,
        _input: crate::tool_protocol::BoundedToolInput,
    ) -> BoxFuture<'static, ToolOutcome> {
        panic!("no witness here calls a tool")
    }
}

fn ceiling() -> AttemptRecordCeiling {
    match std::num::NonZeroU32::new(512) {
        Some(value) => AttemptRecordCeiling::new(value),
        None => panic!("test ceiling must be non-zero"),
    }
}

fn attempt(id: &str) -> PhysicalAttemptId {
    match PhysicalAttemptId::new(id) {
        Some(attempt) => attempt,
        None => panic!("test attempt id must be non-empty"),
    }
}

#[test]
fn swr_c1_structural_plan_refusal_enters_compaction_before_identity_or_open() {
    let opener = Arc::new(AttemptOpener::deterministic(
        [transcript_script()],
        route(),
        credential(),
    ));
    let mut harness = harness(
        "ses_swr_structural",
        opener,
        Box::new(StructuralRefusalPlan),
    );
    let preparation = harness
        .source
        .prepare(harness.step, 1, crate::HistoryGeneration::new(0));
    assert_eq!(
        preparation.compaction_required(),
        Some(grapt_types::CompactionTrigger::StructuralBound)
    );
    assert_eq!(receipt_row(&harness.inbox, &attempt("pat_u2_1")), None);
    assert_eq!(harness.source.ledger().held(), 0);
}

struct Harness {
    session: SessionId,
    inbox: Arc<Mutex<Inbox>>,
    source: ProviderAttemptSource<Box<dyn AttemptPlan>>,
    owner: Option<StepSettlementOwner<RecordingOwner>>,
    step: StepNumber,
}

impl Harness {
    fn take_owner(&mut self) -> StepSettlementOwner<RecordingOwner> {
        match self.owner.take() {
            Some(owner) => owner,
            None => panic!("the harness owner is taken once per run"),
        }
    }

    fn into_recorder(mut self) -> RecordingOwner {
        self.take_owner().into_inner()
    }
}

fn harness(label: &str, opener: Arc<AttemptOpener>, planner: Box<dyn AttemptPlan>) -> Harness {
    harness_clocked(label, opener, planner, Arc::new(TestClock))
}

fn harness_clocked(
    label: &str,
    opener: Arc<AttemptOpener>,
    planner: Box<dyn AttemptPlan>,
    clock: Arc<dyn Clock>,
) -> Harness {
    harness_owned(label, opener, planner, clock, RecordingOwner::new())
}

fn harness_owned(
    label: &str,
    opener: Arc<AttemptOpener>,
    planner: Box<dyn AttemptPlan>,
    clock: Arc<dyn Clock>,
    recorder: RecordingOwner,
) -> Harness {
    let session: SessionId = match label.parse() {
        Ok(session) => session,
        Err(error) => panic!("session id must parse: {error}"),
    };
    let aggregate = AggregateId::Session(session.clone());
    let mut store = match EventStore::open_in_memory() {
        Ok(store) => store,
        Err(error) => panic!("in-memory store must open: {error}"),
    };
    if let Err(error) = store.elect_writer(&aggregate) {
        panic!("test owner must elect once upstream: {error}");
    }
    let writer = match store.fencing_token(&aggregate) {
        Ok(writer) => writer,
        Err(error) => panic!("the elected writer token must exist: {error}"),
    };
    let inbox = match Inbox::new(store) {
        Ok(inbox) => Arc::new(Mutex::new(inbox)),
        Err(error) => panic!("inbox must initialise: {error}"),
    };
    let step = match StepNumber::new(1) {
        Some(step) => step,
        None => panic!("Step 1 must be positive"),
    };
    let terminal_event: EventId = match "evt_u2_step_terminal".parse() {
        Ok(id) => id,
        Err(error) => panic!("event id must parse: {error}"),
    };
    let created_ms = TestClock.now_ms();
    let context = StepSettlementContext::new(
        session.clone(),
        step,
        terminal_event.clone(),
        created_ms,
        writer,
        Arc::new(TestEventIds),
        Arc::new(SettlementClock),
    );
    let mut source = ProviderAttemptSource::new(
        opener,
        Arc::clone(&inbox),
        Arc::new(SeqAttemptIds(AtomicUsize::new(0))),
        clock,
        session.clone(),
        ceiling(),
        planner,
    );
    source.begin_step(StepTerminalHandoff::new(step, terminal_event, created_ms));
    let owner = StepSettlementOwner::new(recorder, Arc::clone(&inbox), context)
        .with_receipts(source.receipt_settlement());
    Harness {
        session,
        inbox,
        source,
        owner: Some(owner),
        step,
    }
}

fn policy(base_backoff: Duration) -> StepPolicy {
    let Some(version) = PolicyVersion::new("policy_u2_witness") else {
        panic!("test policy version must be non-empty")
    };
    match StepPolicy::new(version, 5, base_backoff, 1, Duration::from_secs(30)) {
        Ok(policy) => policy,
        Err(error) => panic!("test policy must construct: {error}"),
    }
}

fn receipt_row(inbox: &Mutex<Inbox>, attempt_id: &PhysicalAttemptId) -> Option<ReceiptDisposition> {
    let guard = match inbox.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    match guard.store().attempt_receipt(attempt_id, ceiling()) {
        Ok(record) => record.map(|record| record.receipt().disposition()),
        Err(error) => panic!("receipt read must succeed: {error}"),
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

async fn run(harness: Harness, policy: StepPolicy) -> (Harness, StepReport) {
    let (_handle, cancellation) = step_interrupt();
    run_with(harness, policy, cancellation).await
}

async fn run_with(
    mut harness: Harness,
    policy: StepPolicy,
    cancellation: crate::Cancellation,
) -> (Harness, StepReport) {
    let owner = harness.take_owner();
    let mut history = NoCompaction;
    let (owner, report) = StepDriver::run(
        harness.step,
        policy,
        &mut harness.source,
        &mut history,
        Arc::new(NoTools),
        owner,
        cancellation,
    )
    .await;
    harness.owner = Some(owner);
    (harness, report)
}

#[tokio::test]
async fn prepare_without_a_terminal_handoff_fails_closed() {
    let registry = ModelRegistry::install(catalog("u2-rt-nohandoff"));
    let opener = Arc::new(AttemptOpener::deterministic(
        [transcript_script()],
        route(),
        credential(),
    ));
    let mut harness = harness(
        "ses_u2_nohandoff",
        opener,
        Box::new(RegistryPlan { registry }),
    );
    harness.source.begin_step(StepTerminalHandoff::new(
        match StepNumber::new(9) {
            Some(step) => step,
            None => panic!("step must be positive"),
        },
        match "evt_u2_wrong_step".parse() {
            Ok(id) => id,
            Err(error) => panic!("event id must parse: {error}"),
        },
        0,
    ));
    let (harness, report) = run(harness, policy(Duration::from_millis(5))).await;

    assert!(matches!(report.result, StepResult::AdmissionRefused { .. }));
    assert_eq!(receipt_row(&harness.inbox, &attempt("pat_u2_1")), None);
    assert_eq!(harness.source.ledger().held(), 0);
    assert_eq!(
        terminal_rows(&harness.inbox, &harness.session, "session.step.failed.3"),
        1
    );
}

#[test]
fn prepare_reserves_and_the_step_ended_commit_settles_and_releases() {
    let registry = ModelRegistry::install(catalog("u2-rt-reserve"));
    let opener = Arc::new(AttemptOpener::deterministic(
        [transcript_script()],
        route(),
        credential(),
    ));
    let mut harness = harness(
        "ses_u2_reserve",
        opener,
        Box::new(RegistryPlan { registry }),
    );

    let preparation = harness
        .source
        .prepare(harness.step, 1, crate::HistoryGeneration::new(0));
    assert!(format!("{preparation:?}").contains("pat_u2_1"));
    match receipt_row(&harness.inbox, &attempt("pat_u2_1")) {
        Some(ReceiptDisposition::Lost(LostEffect::Ambiguous { .. })) => {}
        other => panic!("a reserved+permitted receipt must read as ambiguous-if-lost: {other:?}"),
    }
    assert_eq!(harness.source.ledger().held(), 1);
    assert_eq!(
        request_rows(&harness.inbox, &harness.session),
        1,
        "request fingerprint must be durable before the unopened reader can send"
    );

    let fact = DurableFact::StepEnded {
        finish: FinishReason::Stop,
    };
    let mut owner = harness.take_owner();
    if let Err(error) = crate::AttemptOwner::commit(&mut owner, fact) {
        panic!("the terminal must commit: {error}");
    }
    assert_eq!(receipt_row(&harness.inbox, &attempt("pat_u2_1")), None);
    assert_eq!(harness.source.ledger().held(), 0);
}

#[tokio::test]
async fn a_clean_transcript_settles_receipt_and_custody_at_the_step_ended_commit() {
    let registry = ModelRegistry::install(catalog("u2-rt-clean"));
    let opener = Arc::new(AttemptOpener::deterministic(
        [transcript_script()],
        route(),
        credential(),
    ));
    let harness = harness("ses_u2_clean", opener, Box::new(RegistryPlan { registry }));
    let (harness, report) = run(harness, policy(Duration::from_millis(5))).await;

    assert!(
        matches!(
            report.result,
            StepResult::Ended {
                finish: FinishReason::Stop
            }
        ),
        "{report:?}"
    );
    assert_eq!(report.receipts.len(), 1, "one physical attempt");
    assert_eq!(
        terminal_rows(&harness.inbox, &harness.session, "session.step.ended.3"),
        1
    );
    assert_eq!(receipt_row(&harness.inbox, &attempt("pat_u2_1")), None);
    assert_eq!(harness.source.ledger().held(), 0);

    let inner = harness.into_recorder();
    let committed = &inner.committed;
    assert!(committed.iter().any(|fact| matches!(
        fact,
        DurableFact::Fragment { fragment }
            if matches!(**fragment, grapt_provider::AttemptFragment::ReasoningDelta { .. })
    )));
    assert!(committed.iter().any(|fact| matches!(
        fact,
        DurableFact::Fragment { fragment }
            if matches!(**fragment, grapt_provider::AttemptFragment::Usage { .. })
    )));
    let text: String = committed
        .iter()
        .filter_map(|fact| match fact {
            DurableFact::Fragment { fragment } => match &**fragment {
                grapt_provider::AttemptFragment::TextDelta { text, .. } => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello");
}

#[tokio::test]
async fn visible_output_then_rate_limit_fails_without_a_second_provider_call() {
    let registry = ModelRegistry::install(catalog("u2-rt-noretry"));
    let visible_then_limit = ScriptBuilder::new().text(&["partial visible answer"]).fail(
        ProviderFailureClass::RateLimit {
            retry_after: Some(Duration::from_secs(1)),
        },
        None,
        "Rate limited",
    );
    let opener = Arc::new(AttemptOpener::deterministic(
        [visible_then_limit, transcript_script()],
        route(),
        credential(),
    ));
    let harness = harness(
        "ses_u2_noretry",
        Arc::clone(&opener),
        Box::new(RegistryPlan { registry }),
    );
    let (harness, report) = run(harness, policy(Duration::from_millis(5))).await;

    match &report.result {
        StepResult::Failed { failure } => {
            assert!(
                matches!(
                    failure.reason(),
                    StepTerminalReason::ProviderFailure {
                        class: TerminalClass::RateLimit
                    }
                ),
                "{:?}",
                failure.reason()
            );
        }
        other => panic!("visible output then rate limit must fail the Step: {other:?}"),
    }
    assert_eq!(report.receipts.len(), 1, "exactly one physical attempt");
    assert_eq!(
        terminal_rows(&harness.inbox, &harness.session, "session.step.failed.3"),
        1
    );
    assert_eq!(receipt_row(&harness.inbox, &attempt("pat_u2_1")), None);
    assert_eq!(harness.source.ledger().held(), 0);

    let inner = harness.into_recorder();
    let text: String = inner
        .committed
        .iter()
        .filter_map(|fact| match fact {
            DurableFact::Fragment { fragment } => match &**fragment {
                grapt_provider::AttemptFragment::TextDelta { text, .. } => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(text, "partial visible answer");

    let fresh_registry = ModelRegistry::install(catalog("u2-rt-noretry-b"));
    let authority = authority_of(issue(&fresh_registry));
    let request = request_for(&authority);
    if let Err(error) = opener.open(
        attempt("pat_u2_probe"),
        authority,
        ProviderRequest::ChatGptCodex(request),
    ) {
        panic!("the second script must still be available: {error:?}");
    }
}

#[tokio::test]
async fn a_pre_output_retry_settles_both_receipts_at_the_one_step_terminal() {
    let registry = ModelRegistry::install(catalog("u2-rt-retry"));
    let failing = ScriptBuilder::new().fail(
        ProviderFailureClass::ProviderInternal { retry_after: None },
        None,
        "provider internal",
    );
    let opener = Arc::new(AttemptOpener::deterministic(
        [failing, transcript_script()],
        route(),
        credential(),
    ));
    let harness = harness("ses_u2_retry", opener, Box::new(RegistryPlan { registry }));
    let (harness, report) = run(harness, policy(Duration::from_millis(2))).await;

    assert!(matches!(
        report.result,
        StepResult::Ended {
            finish: FinishReason::Stop
        }
    ));
    assert_eq!(report.receipts.len(), 2, "one retry, two physical attempts");
    assert_eq!(
        terminal_rows(&harness.inbox, &harness.session, "session.step.ended.3"),
        1,
        "however many attempts, ONE Step terminal"
    );
    assert_eq!(receipt_row(&harness.inbox, &attempt("pat_u2_1")), None);
    assert_eq!(receipt_row(&harness.inbox, &attempt("pat_u2_2")), None);
    assert_eq!(harness.source.ledger().held(), 0);
}

// Paused time fires the interrupt only once the driver idles in its backoff.
#[tokio::test(start_paused = true)]
async fn an_interrupt_during_backoff_settles_the_reserved_receipt_truthfully() {
    let registry = ModelRegistry::install(catalog("u2-rt-interrupt"));
    let failing = ScriptBuilder::new().fail(
        ProviderFailureClass::ProviderInternal { retry_after: None },
        None,
        "provider internal",
    );
    let opener = Arc::new(AttemptOpener::deterministic(
        [failing, transcript_script()],
        route(),
        credential(),
    ));
    let harness = harness(
        "ses_u2_interrupt",
        opener,
        Box::new(RegistryPlan { registry }),
    );
    let (handle, cancellation) = step_interrupt();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        handle.interrupt();
    });
    let (harness, report) = run_with(harness, policy(Duration::from_secs(30)), cancellation).await;

    match &report.result {
        StepResult::Failed { failure } => assert!(
            matches!(
                failure.reason(),
                StepTerminalReason::Interrupted {
                    by: InterruptionSource::User
                }
            ),
            "{:?}",
            failure.reason()
        ),
        other => panic!("an interrupt during backoff must fail as interrupted: {other:?}"),
    }
    assert_eq!(
        report.receipts.len(),
        1,
        "the interrupt buys no second call"
    );
    assert_eq!(
        terminal_rows(&harness.inbox, &harness.session, "session.step.failed.3"),
        1
    );
    assert_eq!(receipt_row(&harness.inbox, &attempt("pat_u2_1")), None);
    assert_eq!(harness.source.ledger().held(), 0);
}

#[tokio::test]
async fn a_stale_issuance_refuses_with_zero_durable_attempt_rows() {
    let mut registry = ModelRegistry::install(catalog("u2-rt-stale"));
    let authority = authority_of(issue(&registry));
    if registry
        .supersede(
            match ModelCatalogVersion::new("u2-rt-stale-2") {
                Some(version) => version,
                None => panic!("test version must be non-empty"),
            },
            records(),
        )
        .is_none()
    {
        panic!("the successor release must admit");
    }
    let opener = Arc::new(AttemptOpener::deterministic(
        [transcript_script()],
        route(),
        credential(),
    ));
    let harness = harness(
        "ses_u2_stale",
        opener,
        Box::new(StalePlan {
            authority: Some(authority),
        }),
    );
    let (harness, report) = run(harness, policy(Duration::from_millis(5))).await;

    match &report.result {
        StepResult::AdmissionRefused { refusal, .. } => {
            assert!(
                matches!(refusal, AttemptAdmissionRefusal::OpenRefused { .. }),
                "{refusal:?}"
            );
        }
        other => panic!("a stale issuance must refuse admission: {other:?}"),
    }
    assert_eq!(
        receipt_row(&harness.inbox, &attempt("pat_u2_1")),
        None,
        "a refused open must leave no durable receipt"
    );
    assert_eq!(harness.source.ledger().held(), 0);
    assert_eq!(
        terminal_rows(&harness.inbox, &harness.session, "session.step.failed.3"),
        1
    );
}

#[tokio::test]
async fn a_permit_refusal_after_reserve_leaves_no_receipt_and_no_provider_call() {
    let registry = ModelRegistry::install(catalog("u2-rt-permitrefused"));
    let opener = Arc::new(AttemptOpener::deterministic(
        [transcript_script()],
        route(),
        credential(),
    ));
    let harness = harness_clocked(
        "ses_u2_permitrefused",
        opener,
        Box::new(RegistryPlan { registry }),
        Arc::new(RetrogradeClock(AtomicUsize::new(0))),
    );
    let (harness, report) = run(harness, policy(Duration::from_millis(5))).await;

    match &report.result {
        StepResult::AdmissionRefused { refusal, .. } => match refusal {
            AttemptAdmissionRefusal::StoreRefused { detail } => assert!(
                detail.contains("send permit refused"),
                "the SECOND phase must be the refusing one: {detail}"
            ),
            other => panic!("a refused permit must refuse admission at the store: {other:?}"),
        },
        other => panic!("a refused permit must refuse admission: {other:?}"),
    }
    assert_eq!(
        receipt_row(&harness.inbox, &attempt("pat_u2_1")),
        None,
        "a refused permit must leave no durable receipt behind"
    );
    assert!(
        report.receipts.is_empty(),
        "a refused permit must make zero provider calls: {:?}",
        report.receipts
    );
    assert_eq!(harness.source.ledger().held(), 0);
    assert_eq!(
        request_rows(&harness.inbox, &harness.session),
        0,
        "permit refusal must roll request diagnostics back with the receipt"
    );
    assert_eq!(
        terminal_rows(&harness.inbox, &harness.session, "session.step.failed.3"),
        1
    );
}

/// U2-ETA-R1-F1: the first inner fragment MUST open the attempt, and a later
/// `AttemptOpened` reaches `StreamValidator` to fail as `AlreadyOpened`.
#[tokio::test]
async fn a_stream_that_does_not_open_first_is_refused_and_a_reopening_is_not_swallowed() {
    // Case 1: content where the opening marker belongs.
    let registry = ModelRegistry::install(catalog("u2-rt-unopened"));
    let opener = Arc::new(AttemptOpener::deterministic(
        [ScriptBuilder::unopened()
            .text(&["orphaned content"])
            .finish(FinishReason::Stop, None)],
        route(),
        credential(),
    ));
    let first = harness(
        "ses_u2_unopened",
        opener,
        Box::new(RegistryPlan { registry }),
    );
    let (first, report) = run(first, policy(Duration::from_millis(5))).await;

    match &report.result {
        StepResult::Failed { failure } => assert!(
            matches!(
                failure.reason(),
                StepTerminalReason::ProviderFailure {
                    class: TerminalClass::InvalidProviderOutput
                }
            ),
            "{:?}",
            failure.reason()
        ),
        other => panic!("a stream that never opened must fail the Step: {other:?}"),
    }
    assert_eq!(report.receipts.len(), 1, "exactly one physical attempt");
    assert_eq!(
        terminal_rows(&first.inbox, &first.session, "session.step.failed.3"),
        1
    );
    assert_eq!(receipt_row(&first.inbox, &attempt("pat_u2_1")), None);
    assert_eq!(first.source.ledger().held(), 0, "custody released");
    let inner = first.into_recorder();
    assert!(
        !inner.committed.iter().any(|fact| matches!(
            fact,
            DurableFact::Fragment { fragment }
                if matches!(**fragment, AttemptFragment::TextDelta { .. })
        )),
        "a stream that never opened must commit no content"
    );

    // Case 2: a valid opening, content, then a SECOND opening.
    let reopened = ModelRegistry::install(catalog("u2-rt-reopened"));
    let opener = Arc::new(AttemptOpener::deterministic(
        [ScriptBuilder::new()
            .text(&["visible before the second open"])
            .raw(AttemptFragment::AttemptOpened)
            .finish(FinishReason::Stop, None)],
        route(),
        credential(),
    ));
    let second = harness(
        "ses_u2_reopened",
        opener,
        Box::new(RegistryPlan { registry: reopened }),
    );
    let (second, report) = run(second, policy(Duration::from_millis(5))).await;

    match &report.result {
        StepResult::Failed { failure } => {
            assert!(
                matches!(
                    failure.reason(),
                    StepTerminalReason::ProviderFailure {
                        class: TerminalClass::InvalidProviderOutput
                    }
                ),
                "{:?}",
                failure.reason()
            );
            assert!(
                failure.detail().as_str().contains("opened twice"),
                "the duplicate must be refused AS AlreadyOpened: {:?}",
                failure.detail().as_str()
            );
        }
        other => panic!("a reopened stream must fail the Step: {other:?}"),
    }
    assert_eq!(report.receipts.len(), 1, "exactly one physical attempt");
    assert_eq!(
        terminal_rows(&second.inbox, &second.session, "session.step.failed.3"),
        1
    );
    assert_eq!(receipt_row(&second.inbox, &attempt("pat_u2_1")), None);
    assert_eq!(second.source.ledger().held(), 0, "custody released");
}

/// U2-ETA-R1-F2: a failure arriving BEFORE any stream fragment keeps its own
/// class, so retry eligibility survives. Driven over the REAL staged-Codex
/// path, because a script never reaches `CodexOpen::Failed` and would mask it.
#[tokio::test]
async fn a_pre_stream_transport_failure_keeps_its_class_and_still_retries() {
    let registry = ModelRegistry::install(catalog("u2-rt-prestream"));
    let deadlines = match CodexDeadlines::new(Duration::from_millis(50), Duration::from_millis(50))
    {
        Ok(deadlines) => deadlines,
        Err(error) => panic!("test deadlines: {error}"),
    };
    let limits = match std::num::NonZeroUsize::new(4) {
        Some(capacity) => grapt_provider::codex::CodexLimits::new(capacity),
        None => panic!("test capacity must be non-zero"),
    };
    // A CONTROLLED blackhole: bound here and held for the whole Step, so the
    // port is ours and no availability is assumed. It accepts nothing, so the
    // send reaches its response-head deadline and the adapter classifies a real
    // Transport failure — the production path, with no script in it.
    let blackhole = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(error) => panic!("blackhole bind: {error}"),
    };
    let address = match blackhole.local_addr() {
        Ok(address) => address,
        Err(error) => panic!("blackhole address: {error}"),
    };
    let opener = match AttemptOpener::codex(
        deadlines,
        limits,
        &format!("https://{address}"),
        route(),
        "grapt-u2-prestream",
        credential(),
    ) {
        Ok(opener) => Arc::new(opener),
        Err(error) => panic!("codex opener: {error:?}"),
    };
    let harness = harness(
        "ses_u2_prestream",
        opener,
        Box::new(RegistryPlan { registry }),
    );
    let (harness, report) = run(harness, policy(Duration::from_millis(1))).await;

    // Every ATTEMPT carries the adapter's own class. The STEP terminal is the
    // policy limit, because a retryable class is retried until the bound —
    // which is itself the proof that eligibility survived: a synthesized
    // InvalidProviderOutput is terminal and would have stopped at one attempt.
    for receipt in &report.receipts {
        match &receipt.outcome {
            StepOutcome::Failed { failure } => assert!(
                matches!(
                    failure.reason(),
                    AttemptTerminalReason::ProviderFailure {
                        class: TerminalClass::Transport
                    }
                ),
                "the adapter's OWN class must survive the opening law: {:?}",
                failure.reason()
            ),
            other => panic!("each pre-stream attempt must fail typed: {other:?}"),
        }
    }
    assert_eq!(
        report.receipts.len(),
        5,
        "a retryable class exhausts the governed attempt bound"
    );
    match &report.result {
        StepResult::Failed { failure } => assert!(
            matches!(
                failure.reason(),
                StepTerminalReason::PolicyLimit { policy }
                    if policy.as_str() == "policy_u2_witness"
            ),
            "{:?}",
            failure.reason()
        ),
        other => panic!("the exhausted Step must fail: {other:?}"),
    }
    assert_eq!(
        terminal_rows(&harness.inbox, &harness.session, "session.step.failed.3"),
        1
    );
    for receipt in &report.receipts {
        assert_eq!(
            receipt_row(&harness.inbox, &receipt.attempt),
            None,
            "every reserved receipt settles at the one terminal"
        );
    }
    assert_eq!(harness.source.ledger().held(), 0, "custody released");
    drop(blackhole);
}

#[path = "compaction_tests.rs"]
mod compaction_tests;

#[path = "context_identity_tests.rs"]
mod context_identity_tests;

#[path = "compaction_budget_tests.rs"]
mod compaction_budget_tests;

#[path = "context_preparation_tests.rs"]
mod context_preparation_tests;

#[path = "retry_visibility_tests.rs"]
mod retry_visibility_tests;

#[path = "retry_feedback_tests.rs"]
mod retry_feedback_tests;

#[path = "capacity_tests.rs"]
mod capacity_tests;

#[path = "capacity_wire.rs"]
mod capacity_wire;
