//! Public fail-closed `RuntimeApplication` attempt-admission witness.

use std::io::Read;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use grapt_artifact::{
    ArtifactKind, ArtifactMetadata, ArtifactStore, CaptureBound, CaptureError, CapturedArtifact,
    FetchError, ReadBudget, ReadPage, Sensitivity, StatError,
};
use grapt_inbox::{AdmitRequest, Inbox};
use grapt_provider::AttemptFragment;
use grapt_runtime::{
    AttemptAdmissionRefusal, AttemptPreparation, AttemptSource, BoundedArtifactWriter,
    BoundedProgressSink, BoundedToolInput, BoxFuture, Clock, CompactionError, CoordinatorError,
    EventIds, GovernedToolExecutor, HistoryCursor, HistoryGeneration, HistorySource, LocationError,
    LocationRuntime, LocationServiceMap, Promotion, ProviderReader, RecordingOwner,
    RuntimeApplication, SessionCoordinator, StepAllowance, StepChain, StepPolicy, ToolCancellation,
    ToolExecutionTerminal, ToolOutcome, ToolRunner,
};
use grapt_store::{AggregateId, AppendInput, EventStore};
use grapt_types::{
    ArtifactId, CapabilityId, DeliveryMode, EventId, EventVersion, LocationId, MessageId,
    PolicyVersion, SessionId, StepNumber, ToolCallId,
};
use serde_json::json;

const CREATED_MS: i64 = 1_786_060_800_000;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[derive(Debug)]
struct FixedClock;

impl Clock for FixedClock {
    fn now_ms(&self) -> i64 {
        CREATED_MS
    }
}

#[derive(Debug)]
struct TestIds(AtomicU64);

impl EventIds for TestIds {
    fn mint(&self) -> EventId {
        let ordinal = self.0.fetch_add(1, Ordering::SeqCst);
        match format!("evt_public_refusal_{ordinal}").parse() {
            Ok(id) => id,
            Err(error) => panic!("minted event id must parse: {error}"),
        }
    }
}

struct NeverReader;

impl ProviderReader for NeverReader {
    async fn next(&mut self) -> Option<AttemptFragment> {
        None
    }
}

struct RefusedAttempts;

impl AttemptSource for RefusedAttempts {
    type Reader = NeverReader;

    fn prepare(
        &mut self,
        _step: StepNumber,
        _ordinal: u32,
        _generation: HistoryGeneration,
    ) -> AttemptPreparation<Self::Reader> {
        AttemptPreparation::refused(AttemptAdmissionRefusal::NoIssuer)
    }
}

struct FixedHistory(HistoryGeneration);

impl HistorySource for FixedHistory {
    async fn compact(&mut self) -> Result<(), CompactionError> {
        Ok(())
    }

    fn reload(&mut self) -> Result<HistoryGeneration, CompactionError> {
        Ok(self.0)
    }

    fn generation(&self) -> HistoryGeneration {
        self.0
    }
}

struct DiscardedTools;

impl ToolRunner for DiscardedTools {
    fn run(
        &self,
        _call: ToolCallId,
        _tool: String,
        _input: grapt_runtime::BoundedToolInput,
    ) -> BoxFuture<'static, ToolOutcome> {
        panic!("governed composition must discard the Location's raw tool runner")
    }
}

struct RefusedLocation;

impl LocationRuntime for RefusedLocation {
    type Attempts = RefusedAttempts;
    type History = FixedHistory;
    type Tools = DiscardedTools;
    type Owner = RecordingOwner;

    fn open_chain(
        &self,
        _session: &SessionId,
        _promotion: &Promotion,
        cursor: HistoryCursor,
    ) -> Result<StepChain<Self>, LocationError> {
        Ok(StepChain {
            initial_step: StepNumber::new(1).ok_or_else(|| LocationError::Refused {
                location: "test".to_owned(),
                session: "test".to_owned(),
                reason: "test Step identity was refused".to_owned(),
            })?,
            attempts: RefusedAttempts,
            history: FixedHistory(HistoryGeneration::new(cursor.value())),
            tools: Arc::new(DiscardedTools),
            owner: RecordingOwner::new(),
        })
    }
}

struct OneLocation(Arc<RefusedLocation>);

impl LocationServiceMap for OneLocation {
    type Runtime = RefusedLocation;

    fn get(&self, _location: &LocationId) -> Result<Arc<Self::Runtime>, LocationError> {
        Ok(Arc::clone(&self.0))
    }
}

#[derive(Debug)]
struct NeverArtifactStore;

impl ArtifactStore for NeverArtifactStore {
    fn capture<R: Read>(
        &self,
        _source: R,
        _bound: CaptureBound,
        _kind: ArtifactKind,
        _sensitivity: Sensitivity,
    ) -> Result<CapturedArtifact, CaptureError> {
        panic!("attempt refusal must not capture an artifact")
    }

    fn stat(&self, _id: &ArtifactId) -> Result<ArtifactMetadata, StatError> {
        panic!("attempt refusal must not stat an artifact")
    }

    fn fetch(
        &self,
        _reference: &grapt_types::ArtifactRef,
        _offset: u64,
        _budget: ReadBudget,
    ) -> Result<ReadPage, FetchError> {
        panic!("attempt refusal must not fetch an artifact")
    }
}

struct NeverExecutor {
    calls: Arc<AtomicUsize>,
}

impl GovernedToolExecutor<NeverArtifactStore> for NeverExecutor {
    fn execute(
        &self,
        _capability: CapabilityId,
        _call: ToolCallId,
        _input: BoundedToolInput,
        _progress: BoundedProgressSink,
        _cancellation: ToolCancellation,
        _artifacts: BoundedArtifactWriter<NeverArtifactStore>,
    ) -> BoxFuture<'static, ToolExecutionTerminal> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { ToolExecutionTerminal::result_missing() })
    }
}

fn session() -> SessionId {
    match "ses_public_refusal".parse() {
        Ok(session) => session,
        Err(error) => panic!("session id must parse: {error}"),
    }
}

fn seed_and_admit(inbox: &Arc<Mutex<Inbox>>, session: &SessionId) {
    let aggregate = AggregateId::Session(session.clone());
    let mut inbox = lock(inbox);
    let writer = match inbox
        .store_mut()
        .elect_writer(&aggregate)
        .and_then(|_| inbox.store().fencing_token(&aggregate))
    {
        Ok(writer) => writer,
        Err(error) => panic!("test writer must be elected: {error}"),
    };
    let seed_id: EventId = match "evt_public_refusal_seed".parse() {
        Ok(id) => id,
        Err(error) => panic!("seed event id must parse: {error}"),
    };
    let version = match EventVersion::new(1) {
        Some(version) => version,
        None => panic!("event version one must exist"),
    };
    let directory = if cfg!(windows) {
        r"E:\public-refusal"
    } else {
        "/tmp/public-refusal"
    };
    let data = json!({
        "sessionID": session.as_str(),
        "info": { "id": session.as_str(), "directory": directory },
    });
    if let Err(error) = inbox.store_mut().append(
        AppendInput {
            id: &seed_id,
            aggregate,
            writer,
            kind_name: "session.created",
            version,
            created_ms: CREATED_MS,
            data: &data,
        },
        &mut [],
    ) {
        panic!("session seed must append: {error}");
    }

    let admission_id: EventId = match "evt_public_refusal_admit".parse() {
        Ok(id) => id,
        Err(error) => panic!("admission event id must parse: {error}"),
    };
    let input: MessageId = match "msg_public_refusal".parse() {
        Ok(id) => id,
        Err(error) => panic!("message id must parse: {error}"),
    };
    if let Err(error) = inbox.admit(AdmitRequest {
        event_id: &admission_id,
        session,
        input: &input,
        text: "run",
        delivery: DeliveryMode::Queue,
        resume: true,
        created_ms: CREATED_MS,
    }) {
        panic!("input admission must succeed: {error}");
    }
}

#[tokio::test]
async fn public_runtime_application_refusal_opens_zero_provider_streams() {
    let store = match EventStore::open_in_memory() {
        Ok(store) => store,
        Err(error) => panic!("in-memory event store must open: {error}"),
    };
    let inbox = match Inbox::new(store) {
        Ok(inbox) => Arc::new(Mutex::new(inbox)),
        Err(error) => panic!("inbox must initialize: {error}"),
    };
    let session = session();
    seed_and_admit(&inbox, &session);

    let executor_calls = Arc::new(AtomicUsize::new(0));
    let locations = Arc::new(OneLocation(Arc::new(RefusedLocation)));
    let policy_version = match PolicyVersion::new("public-refusal/1") {
        Some(version) => version,
        None => panic!("policy version must be non-empty"),
    };
    let policy = match StepPolicy::new(
        policy_version,
        1,
        Duration::from_secs(1),
        0,
        Duration::from_secs(1),
    ) {
        Ok(policy) => policy,
        Err(error) => panic!("test policy must be valid: {error}"),
    };
    let allowance = match StepAllowance::new(1) {
        Some(allowance) => allowance,
        None => panic!("step allowance must be positive"),
    };
    let application = RuntimeApplication::new_governed(
        Arc::clone(&inbox),
        locations,
        Arc::new(NeverExecutor {
            calls: Arc::clone(&executor_calls),
        }),
        Arc::new(NeverArtifactStore),
        Arc::new(TestIds(AtomicU64::new(0))),
        Arc::new(FixedClock),
        (policy, Some(allowance)),
    );
    let coordinator = SessionCoordinator::new(Arc::new(application));
    let result = tokio::time::timeout(Duration::from_secs(5), coordinator.wake(session)).await;
    let error = match result {
        Ok(Err(error)) => error,
        Ok(Ok(outcome)) => panic!("attempt refusal fabricated a successful wake: {outcome:?}"),
        Err(_) => panic!("attempt refusal must terminate without a hang"),
    };
    match error {
        CoordinatorError::Run { reason, .. } => {
            assert!(reason.contains("attempt admission refused"));
            assert!(reason.contains("no authenticated campaign-attempt admission issuer"));
        }
        other => panic!("attempt refusal must remain a typed run failure: {other}"),
    }
    assert_eq!(executor_calls.load(Ordering::SeqCst), 0);
    let terminal_count: i64 = match lock(&inbox).store().connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type = 'session.tool.terminal.1'",
        [],
        |row| row.get(0),
    ) {
        Ok(count) => count,
        Err(error) => panic!("terminal census must read: {error}"),
    };
    assert_eq!(terminal_count, 0, "refusal cannot fabricate completion");
}
