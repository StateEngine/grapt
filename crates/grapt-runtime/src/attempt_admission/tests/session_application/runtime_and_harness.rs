use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use grapt_inbox::Inbox;
use grapt_provider::{
    AttemptFragment, AttemptStream, DeterministicAdapter, FinishReason, Script, ScriptBuilder,
};
use grapt_store::{AggregateId, AppendInput, EventStore};
use grapt_types::{
    LocationId, PhysicalAttemptId, PolicyVersion, SessionId, StepNumber, ToolCallId,
};
use serde_json::json;

use super::super::admitted;
use super::{
    locked, transcript, version_one, CloseGate, CountingClock, Gate, ReaderCensus, TestIds,
    CREATED_MS,
};
use crate::attempt_admission::UnopenedAttempt;
use crate::{
    AttemptOwner, AttemptPreparation, AttemptSource, BoxFuture, CompactionError, DurableFact,
    EventIds, HistoryCursor, HistoryGeneration, HistorySource, LocationError, LocationRuntime,
    LocationServiceMap, OwnerError, Promotion, ProviderReader, RuntimeApplication,
    SessionCoordinator, StepAllowance, StepChain, StepPolicy, ToolOutcome, ToolRunner,
};

// ---------------------------------------------------------------------------
// The Location runtime under test
// ---------------------------------------------------------------------------

/// Projected history whose generation IS the Session's durable event depth.
///
/// Reloading therefore advances exactly when new facts have committed, which
/// is the property the continuation law depends on: a reload that does not
/// advance means the previous Step's terminals are not visible yet.
#[derive(Debug)]
struct StoreHistory {
    inbox: Arc<Mutex<Inbox>>,
    session: SessionId,
    generation: HistoryGeneration,
    reloads: Arc<AtomicUsize>,
}

impl StoreHistory {
    fn depth(inbox: &Arc<Mutex<Inbox>>, session: &SessionId) -> Result<u64, CompactionError> {
        let guard = locked(inbox);
        let depth: i64 = guard
            .store()
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM event WHERE aggregate_id = ?1",
                [session.as_str()],
                |row| row.get(0),
            )
            .map_err(|error| CompactionError {
                reason: format!("history depth unreadable: {error}"),
            })?;
        u64::try_from(depth).map_err(|_| CompactionError {
            reason: "history depth is negative".to_owned(),
        })
    }
}

impl HistorySource for StoreHistory {
    async fn compact(&mut self) -> Result<(), CompactionError> {
        Err(CompactionError {
            reason: "this witness Location has no compactor".to_owned(),
        })
    }

    fn reload(&mut self) -> Result<HistoryGeneration, CompactionError> {
        self.reloads.fetch_add(1, Ordering::SeqCst);
        let depth = Self::depth(&self.inbox, &self.session)?;
        self.generation = HistoryGeneration::new(depth);
        Ok(self.generation)
    }

    fn generation(&self) -> HistoryGeneration {
        self.generation
    }
}

/// Commits every `DurableFact` as a real event on the Session aggregate.
#[derive(Debug)]
struct StoreOwner {
    inbox: Arc<Mutex<Inbox>>,
    session: SessionId,
    ids: Arc<TestIds>,
    /// Refuse commits once this many have succeeded. Models the store
    /// declining a fact — the seam's own `OwnerError`, not a provider failure.
    refuse_after: Option<usize>,
    committed: usize,
    record_only: bool,
    notifications: Arc<Mutex<Vec<DurableFact>>>,
}

impl AttemptOwner for StoreOwner {
    fn commit(&mut self, fact: DurableFact) -> Result<(), OwnerError> {
        if let Some(limit) = self.refuse_after {
            if self.committed >= limit {
                return Err(OwnerError {
                    reason: "store declined the fact".to_owned(),
                });
            }
        }
        if self.record_only {
            self.committed += 1;
            return Ok(());
        }
        let id = self.ids.mint();
        let data = json!({
            "sessionID": self.session.as_str(),
            "fact": format!("{fact:?}"),
        });
        // Synchronous, and the guard is dropped before this function returns:
        // no store lock can reach an await.
        let mut guard = locked(&self.inbox);
        let aggregate = AggregateId::Session(self.session.clone());
        let writer = guard
            .store()
            .fencing_token(&aggregate)
            .map_err(|error| OwnerError {
                reason: error.to_string(),
            })?;
        guard
            .store_mut()
            .append(
                AppendInput {
                    id: &id,
                    aggregate,
                    writer,
                    // A REAL durable kind from the closed vocabulary. This fixture used to
                    // mint "test.step.fact", a fabricated name the open store
                    // decoder accepted; the closed decoder correctly refuses it.
                    kind_name: "session.step.ended",
                    version: version_one(),
                    created_ms: CREATED_MS,
                    data: &data,
                },
                &mut [],
            )
            .map_err(|error| OwnerError {
                reason: error.to_string(),
            })?
            // This fixture drives the session application over durable state; it
            // has no live subscriber to deliver to.
            .1
            .discard();
        self.committed += 1;
        Ok(())
    }

    fn notify(&mut self, fact: &DurableFact) {
        locked(&self.notifications).push(fact.clone());
    }

    fn observe(&mut self, _observation: crate::Observation) {}
}

/// Reads one scripted stream. Opening happens inside the factory, so nothing
/// reaches the adapter before the attempt-start fact is durable.
#[derive(Debug)]
struct ScriptReader {
    stream: Option<AttemptStream>,
    gate: Option<Gate>,
    arrived: bool,
    /// Counted from construction to drop. The supervisor owns this value, so
    /// its drop is the moment the reader stops existing.
    census: Arc<ReaderCensus>,
    /// When set, destruction pauses here so a witness can inspect the world
    /// mid-cleanup.
    close_gate: Option<Arc<CloseGate>>,
}

impl Drop for ScriptReader {
    fn drop(&mut self) {
        // Held BEFORE the close is recorded, so for the whole hold this reader
        // still counts as live — which is precisely the state a replacement
        // drain must not be allowed to join.
        if let Some(gate) = &self.close_gate {
            gate.hold();
        }
        self.census.closed();
    }
}

impl ProviderReader for ScriptReader {
    async fn next(&mut self) -> Option<AttemptFragment> {
        if !self.arrived {
            self.arrived = true;
            if let Some(gate) = self.gate.clone() {
                gate.arrive().await;
            }
        }
        let stream = self.stream.as_mut()?;
        // The SUPERVISOR opens the attempt: it commits the attempt-start fact
        // and then opens the validator itself. A script is legal standalone
        // and so begins with `AttemptOpened`, but a reader that replayed it
        // would be a second claim on one stream — which the supervisor
        // correctly rejects as `invalid-provider-output`. A reader yields
        // content only.
        stream.find(|fragment| !matches!(fragment, AttemptFragment::AttemptOpened))
    }
}

/// What the adapter was asked to do, so a witness can assert the
/// one-stream-per-attempt law from the outside.
#[derive(Debug, Default)]
struct AttemptLog {
    opened: Vec<(u32, u32, u64)>,
    shutdowns: usize,
    streams: Vec<String>,
    reuse_errors: Vec<String>,
    /// The projected user transcript visible when each attempt was minted —
    /// the model-visible history this request would carry. It is what lets a
    /// witness say "the NEXT provider request contains the steer" rather than
    /// inferring it from a generation number.
    contexts: Vec<Vec<String>>,
}

#[derive(Debug)]
struct ScriptedAttempts {
    adapter: Arc<Mutex<DeterministicAdapter>>,
    log: Arc<Mutex<AttemptLog>>,
    gate: Option<Gate>,
    session: SessionId,
    census: Arc<ReaderCensus>,
    close_gate: Option<Arc<CloseGate>>,
    /// Read at attempt-open to record what the request would contain.
    inbox: Arc<Mutex<Inbox>>,
    /// Distinguishes attempts across chains within one Session, so a reused
    /// identity is a real defect rather than an artefact of the harness.
    chain: u64,
}

impl AttemptSource for ScriptedAttempts {
    type Reader = ScriptReader;

    fn shutdown(&mut self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            locked(&self.log).shutdowns += 1;
            Ok(())
        })
    }

    fn prepare(
        &mut self,
        step: StepNumber,
        ordinal: u32,
        generation: HistoryGeneration,
    ) -> AttemptPreparation<Self::Reader> {
        let raw = format!(
            "att_{}_c{}_s{}_o{ordinal}",
            self.session.as_str(),
            self.chain,
            step.value()
        );
        let attempt = match PhysicalAttemptId::new(raw) {
            Some(attempt) => attempt,
            None => panic!("attempt id must be non-empty"),
        };
        let context = transcript(&self.inbox, &self.session);
        {
            let mut log = locked(&self.log);
            log.opened.push((step.value(), ordinal, generation.value()));
            log.contexts.push(context);
        }

        let adapter = Arc::clone(&self.adapter);
        let log = Arc::clone(&self.log);
        let gate = self.gate.clone();
        let census = Arc::clone(&self.census);
        let close_gate = self.close_gate.clone();
        let id = attempt.clone();
        let unopened = UnopenedAttempt::new(attempt, move || {
            let opened = locked(&adapter).open(id.clone());
            let stream = match opened {
                Ok(stream) => {
                    locked(&log).streams.push(id.as_str().to_owned());
                    Some(stream)
                }
                Err(error) => {
                    locked(&log).reuse_errors.push(error.to_string());
                    None
                }
            };
            census.opened();
            ScriptReader {
                stream,
                gate,
                arrived: false,
                census: Arc::clone(&census),
                close_gate: close_gate.clone(),
            }
        });
        admitted(unopened)
    }
}

#[derive(Debug, Default)]
struct RecordingTools {
    outputs: HashMap<String, String>,
    calls: Arc<Mutex<Vec<String>>>,
}

impl ToolRunner for RecordingTools {
    fn run(
        &self,
        call: ToolCallId,
        tool: String,
        _input: crate::tool_protocol::BoundedToolInput,
    ) -> BoxFuture<'static, ToolOutcome> {
        locked(&self.calls).push(tool.clone());
        let output = self
            .outputs
            .get(&tool)
            .cloned()
            .unwrap_or_else(|| format!("{tool} ran"));
        let _ = call;
        let _ = output;
        Box::pin(async move { ToolOutcome::Completed { output: None } })
    }
}

/// One Location's services, backed by the real store and a scripted provider.
#[derive(Debug)]
struct TestLocation {
    step_limit: Option<std::num::NonZeroU32>,
    location: LocationId,
    inbox: Arc<Mutex<Inbox>>,
    ids: Arc<TestIds>,
    adapter: Arc<Mutex<DeterministicAdapter>>,
    log: Arc<Mutex<AttemptLog>>,
    tools: Arc<RecordingTools>,
    reloads: Arc<AtomicUsize>,
    /// Every (session, input) this Location was asked to run. Proves work does
    /// not leak across Locations.
    opened: Arc<Mutex<Vec<(String, String)>>>,
    chains: AtomicU64,
    gate: Option<Gate>,
    /// Every reader this Location ever handed the supervisor.
    readers: Arc<ReaderCensus>,
    close_gate: Option<Arc<CloseGate>>,
    refuse_commits_after: Option<usize>,
    refuse_chain: bool,
    record_only: bool,
    notifications: Arc<Mutex<Vec<DurableFact>>>,
}

impl LocationRuntime for TestLocation {
    type Attempts = ScriptedAttempts;
    type History = StoreHistory;
    type Tools = RecordingTools;
    type Owner = StoreOwner;

    fn input_step_limit(
        &self,
        _session: &SessionId,
    ) -> Result<Option<std::num::NonZeroU32>, LocationError> {
        Ok(self.step_limit)
    }

    fn open_chain(
        &self,
        session: &SessionId,
        promotion: &Promotion,
        cursor: HistoryCursor,
    ) -> Result<StepChain<Self>, LocationError> {
        if self.refuse_chain {
            return Err(LocationError::Refused {
                location: self.location.as_str().to_owned(),
                session: session.as_str().to_owned(),
                reason: "this witness Location refuses to open a chain".to_owned(),
            });
        }
        locked(&self.opened).push((
            session.as_str().to_owned(),
            promotion.input.message().as_str().to_owned(),
        ));

        // History is loaded HERE, at or after the promotion's own position.
        let depth =
            StoreHistory::depth(&self.inbox, session).map_err(|error| LocationError::Refused {
                location: self.location.as_str().to_owned(),
                session: session.as_str().to_owned(),
                reason: error.reason,
            })?;
        assert!(
            depth > cursor.value(),
            "the promotion at {} must already be in projected history (depth {depth})",
            cursor.value()
        );

        Ok(StepChain {
            initial_step: StepNumber::new(1).ok_or_else(|| LocationError::Refused {
                location: self.location.as_str().to_owned(),
                session: session.as_str().to_owned(),
                reason: "test Step identity was refused".to_owned(),
            })?,
            attempts: ScriptedAttempts {
                adapter: Arc::clone(&self.adapter),
                log: Arc::clone(&self.log),
                gate: self.gate.clone(),
                session: session.clone(),
                census: Arc::clone(&self.readers),
                close_gate: self.close_gate.clone(),
                inbox: Arc::clone(&self.inbox),
                chain: self.chains.fetch_add(1, Ordering::SeqCst),
            },
            history: StoreHistory {
                inbox: Arc::clone(&self.inbox),
                session: session.clone(),
                generation: HistoryGeneration::new(depth),
                reloads: Arc::clone(&self.reloads),
            },
            tools: Arc::clone(&self.tools),
            owner: StoreOwner {
                inbox: Arc::clone(&self.inbox),
                session: session.clone(),
                ids: Arc::clone(&self.ids),
                refuse_after: self.refuse_commits_after,
                committed: 0,
                record_only: self.record_only,
                notifications: Arc::clone(&self.notifications),
            },
        })
    }
}

#[derive(Debug, Default)]
struct TestMap {
    runtimes: Mutex<HashMap<String, Arc<TestLocation>>>,
    /// Every resolution. An empty drain must leave this at zero: Location
    /// services boot on first execution, never on admission or discovery.
    boots: AtomicUsize,
}

impl TestMap {
    fn register(&self, runtime: Arc<TestLocation>) {
        locked(&self.runtimes).insert(runtime.location.as_str().to_owned(), runtime);
    }
}

impl LocationServiceMap for TestMap {
    type Runtime = TestLocation;

    fn get(&self, location: &LocationId) -> Result<Arc<Self::Runtime>, LocationError> {
        self.boots.fetch_add(1, Ordering::SeqCst);
        match locked(&self.runtimes).get(location.as_str()) {
            Some(runtime) => Ok(Arc::clone(runtime)),
            None => Err(LocationError::Unavailable {
                location: location.as_str().to_owned(),
                reason: "no runtime registered".to_owned(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Harness assembly
// ---------------------------------------------------------------------------

fn open_inbox() -> Arc<Mutex<Inbox>> {
    let store = match EventStore::open_in_memory() {
        Ok(store) => store,
        Err(error) => panic!("in-memory store must open: {error}"),
    };
    match Inbox::new(store) {
        Ok(inbox) => Arc::new(Mutex::new(inbox)),
        Err(error) => panic!("inbox must initialise: {error}"),
    }
}

/// A named version for the policies these tests build.
fn test_version() -> PolicyVersion {
    match PolicyVersion::new("session-application-tests/1") {
        Some(version) => version,
        None => panic!("a non-empty literal must build a PolicyVersion"),
    }
}

fn fast_policy(max_attempts: u32) -> StepPolicy {
    match StepPolicy::new(
        test_version(),
        max_attempts,
        Duration::ZERO,
        1,
        Duration::from_secs(30),
    ) {
        Ok(policy) => policy,
        Err(error) => panic!("policy must be valid: {error}"),
    }
}

struct Harness {
    inbox: Arc<Mutex<Inbox>>,
    map: Arc<TestMap>,
    coordinator: Arc<SessionCoordinator<RuntimeApplication<TestMap>>>,
    ids: Arc<TestIds>,
    clock: Arc<CountingClock>,
}

fn open_file_inbox(name: &str) -> (std::path::PathBuf, std::path::PathBuf, Arc<Mutex<Inbox>>) {
    let dir =
        std::env::temp_dir().join(format!("grapt-runtime-r2b3-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(error) = std::fs::create_dir_all(&dir) {
        panic!("file-backed witness directory must be created: {error}");
    }
    let db = dir.join("runtime.sqlite");
    let store = match EventStore::open(&db, "r2b3-runtime-test") {
        Ok(store) => store,
        Err(error) => panic!("file-backed store must open: {error}"),
    };
    let inbox = match Inbox::new(store) {
        Ok(inbox) => Arc::new(Mutex::new(inbox)),
        Err(error) => panic!("file-backed inbox must initialise: {error}"),
    };
    (dir, db, inbox)
}

#[derive(Default)]
struct LocationSpec {
    step_limit: Option<std::num::NonZeroU32>,
    scripts: Vec<Script>,
    gate: Option<Gate>,
    tool_outputs: HashMap<String, String>,
    refuse_commits_after: Option<usize>,
    refuse_chain: bool,
    record_only: bool,
    close_gate: Option<Arc<CloseGate>>,
}

fn build(
    inbox: Arc<Mutex<Inbox>>,
    locations: Vec<(&str, LocationSpec)>,
    policy: StepPolicy,
    allowance: StepAllowance,
) -> (Harness, Vec<Arc<TestLocation>>) {
    let ids = Arc::new(TestIds::default());
    let clock = Arc::new(CountingClock::default());
    let owner_ids = Arc::new(TestIds::named("owner"));
    let map = Arc::new(TestMap::default());
    let mut runtimes = Vec::new();
    for (directory, spec) in locations {
        let reference = match grapt_types::LocationRef::new(directory, None) {
            Ok(reference) => reference,
            Err(error) => panic!("witness directory must be a valid Location: {error}"),
        };
        let runtime = Arc::new(TestLocation {
            step_limit: spec.step_limit,
            location: reference.canonical_id(),
            inbox: Arc::clone(&inbox),
            ids: Arc::clone(&owner_ids),
            adapter: Arc::new(Mutex::new(DeterministicAdapter::new(spec.scripts))),
            log: Arc::new(Mutex::new(AttemptLog::default())),
            tools: Arc::new(RecordingTools {
                outputs: spec.tool_outputs,
                calls: Arc::new(Mutex::new(Vec::new())),
            }),
            reloads: Arc::new(AtomicUsize::new(0)),
            opened: Arc::new(Mutex::new(Vec::new())),
            chains: AtomicU64::new(0),
            gate: spec.gate,
            readers: Arc::new(ReaderCensus::default()),
            close_gate: spec.close_gate.clone(),
            refuse_commits_after: spec.refuse_commits_after,
            refuse_chain: spec.refuse_chain,
            record_only: spec.record_only,
            notifications: Arc::new(Mutex::new(Vec::new())),
        });
        map.register(Arc::clone(&runtime));
        runtimes.push(runtime);
    }

    let application = RuntimeApplication::new_internal(
        Arc::clone(&inbox),
        Arc::clone(&map),
        ids.clone(),
        clock.clone(),
        policy,
        Some(allowance),
    );
    let coordinator = Arc::new(SessionCoordinator::new(Arc::new(application)));
    (
        Harness {
            inbox,
            map,
            coordinator,
            ids,
            clock,
        },
        runtimes,
    )
}

fn stop_script() -> Script {
    ScriptBuilder::new()
        .text(&["done"])
        .finish(FinishReason::Stop, None)
}

/// A stream the provider cut short at its output ceiling.
fn truncated_script() -> Script {
    ScriptBuilder::new()
        .text(&["a partial ans"])
        .finish(FinishReason::Length, None)
}

/// A stream that ends with no stated reason.
fn unstated_script() -> Script {
    ScriptBuilder::new()
        .text(&["done, probably"])
        .finish(FinishReason::Unknown, None)
}

fn tool_script(call: &str, tool: &str) -> Script {
    let call = match ToolCallId::new(call) {
        Some(call) => call,
        None => panic!("call id must be non-empty"),
    };
    ScriptBuilder::new()
        .tool_call(&call, tool, grapt_provider::ToolExecution::Local, &["{}"])
        .finish(FinishReason::ToolCalls, None)
}

fn allowance(steps: u32) -> StepAllowance {
    match StepAllowance::new(steps) {
        Some(allowance) => allowance,
        None => panic!("step allowance must be positive"),
    }
}

fn ran(runtime: &Arc<TestLocation>) -> Vec<String> {
    locked(&runtime.opened)
        .iter()
        .map(|(_, input)| input.clone())
        .collect()
}

mod admission_and_placement;
mod attempts_and_continuation;
mod finish_disposition;
mod races_and_direct_placement;
mod steer_interrupt_and_store_failures;
