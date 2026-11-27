//! Reader exhaustion must survive projections and the real attempt source.
use super::sustained_defects::product_plan;
use super::*;
use grapt_runtime::provider_source::{ContextAdmissionRefusal, PlanRefusalCause};
use grapt_runtime::{AttemptPlan, HistoryGeneration, PlanRefusal};
use grapt_store::read_pool::{ReadPool, ReadPoolError, READ_WAIT};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

enum ReaderCommand {
    ReleaseOne,
    Stop,
}

/// Transactions stay on a synchronous worker throughout the async witness.
struct HeldReaders {
    commands: mpsc::SyncSender<ReaderCommand>,
    replies: mpsc::Receiver<Result<usize, String>>,
    worker: Option<JoinHandle<()>>,
}

impl HeldReaders {
    fn new(pool: ReadPool) -> Result<Self, String> {
        let (commands, receive) = mpsc::sync_channel(1);
        let (reply, replies) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let mut held = Vec::new();
            for _ in 0..256 {
                match pool.checkout() {
                    Ok(reader) => held.push(reader),
                    Err(ReadPoolError::Busy) => break,
                    Err(error) => {
                        let _ = reply.send(Err(error.to_string()));
                        return;
                    }
                }
            }
            if held.is_empty() || !matches!(pool.checkout(), Err(ReadPoolError::Busy)) {
                let _ = reply.send(Err("reader saturation was not bounded".into()));
                return;
            }
            if reply.send(Ok(held.len())).is_err() {
                return;
            }
            while let Ok(ReaderCommand::ReleaseOne) =
                receive.recv_timeout(READ_WAIT * 8 + Duration::from_secs(5))
            {
                let result = held
                    .pop()
                    .ok_or_else(|| "no held reader".to_owned())
                    .and_then(|reader| reader.finish().map_err(|error| error.to_string()))
                    .map(|()| held.len());
                if reply.send(result).is_err() {
                    break;
                }
            }
        });
        let held = Self {
            commands,
            replies,
            worker: Some(worker),
        };
        held.reply()?;
        Ok(held)
    }

    fn reply(&self) -> Result<usize, String> {
        self.replies
            .recv_timeout(READ_WAIT * 3 + Duration::from_secs(5))
            .map_err(|error| error.to_string())?
    }

    fn release_one(&self) -> Result<(), String> {
        self.commands
            .send(ReaderCommand::ReleaseOne)
            .map_err(|error| error.to_string())?;
        self.reply().map(|_| ())
    }
}

impl Drop for HeldReaders {
    fn drop(&mut self) {
        let _ = self.commands.send(ReaderCommand::Stop);
        if let Some(worker) = self.worker.take() {
            assert!(worker.join().is_ok(), "reader holder panicked");
        }
    }
}

fn readers(fixture: &Fixture) -> Result<ReadPool, String> {
    lock(fixture.inbox()?)
        .store()
        .read_pool()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "file reader pool absent".into())
}

#[test]
fn read_capacity_generic_projection_preserves_both_typed_carriers() -> Result<(), String> {
    let fixture = Fixture::new("generic-reader-capacity")?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    let held = HeldReaders::new(readers(&fixture)?)?;
    let planned = continuation.read_projection(|_| Ok::<(), PlanRefusal>(()));
    let admitted = continuation.read_projection(|_| Ok::<(), ContextAdmissionRefusal>(()));
    assert_eq!(
        (planned, admitted),
        (
            Err(PlanRefusal::capacity_busy()),
            Err(ContextAdmissionRefusal::CapacityBusy)
        )
    );
    held.release_one()?;
    continuation
        .read_projection(|_| Ok::<(), PlanRefusal>(()))
        .map_err(|error| error.to_string())?;
    continuation
        .read_projection(|_| Ok::<(), ContextAdmissionRefusal>(()))
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[test]
fn read_capacity_terminal_projection_errors_never_become_waits() -> Result<(), String> {
    let fixture = Fixture::new("terminal-reader-errors")?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    for detail in [
        "database disk image is malformed",
        "instruction epoch attribution is invalid",
        "read capacity is busy; retry the request",
    ] {
        let refused = continuation
            .read_projection(|_| Err::<(), _>(PlanRefusal::from(detail.to_owned())))
            .err()
            .ok_or("projection ignored its terminal failure")?;
        assert_eq!(refused.cause, PlanRefusalCause::Terminal);
        assert_eq!(refused.detail, detail);
        assert_eq!(refused.compaction, None);
    }
    Ok(())
}

#[test]
fn a_failed_projection_does_not_recycle_its_reader_into_the_pool() -> Result<(), String> {
    let fixture = Fixture::new("failed-projection-reader-recycle")?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    let pool = readers(&fixture)?;
    // Idle one marked connection, so the failing projection below takes exactly
    // this one and a later checkout can tell whether it got it back. An I/O
    // error ends the projection; the transaction still ends cleanly.
    let marked = pool.checkout().map_err(|error| error.to_string())?;
    marked
        .store()
        .connection()
        .pragma_update(None, "cache_size", -4321)
        .map_err(|error| error.to_string())?;
    marked.finish().map_err(|error| error.to_string())?;
    let refused = continuation
        .read_projection(|_| {
            Err::<(), PlanRefusal>(PlanRefusal::from(
                "database disk image is malformed".to_owned(),
            ))
        })
        .err()
        .ok_or("the failing projection was expected to refuse")?;
    assert_eq!(refused.cause, PlanRefusalCause::Terminal);
    // The failed read must not have idled its connection for the next reader.
    let next = pool.checkout().map_err(|error| error.to_string())?;
    let marker: i64 = next
        .store()
        .connection()
        .pragma_query_value(None, "cache_size", |row| row.get(0))
        .map_err(|error| error.to_string())?;
    next.finish().map_err(|error| error.to_string())?;
    assert_ne!(
        marker, -4321,
        "a reader whose projection failed was recycled into the pool"
    );
    Ok(())
}

#[test]
fn read_capacity_corrupt_history_stays_terminal_in_the_product_plan(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new("corrupt-planning-content")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &fixture.input,
        "inspect",
    )?;
    let connection = rusqlite::Connection::open(&fixture.database)?;
    assert_eq!(
        connection.execute(
            "UPDATE session_message SET metadata_json='{' WHERE session_id=?1 AND id=?2",
            [fixture.session.as_str(), fixture.input.as_str()]
        )?,
        1
    );
    let refusal = product_plan(&fixture, fixture.input.clone(), "inspect")?
        .plan(
            StepNumber::new(1).ok_or("step")?,
            1,
            HistoryGeneration::new(1),
        )
        .err()
        .ok_or("corrupt history produced a request")?;
    assert_eq!(refusal.cause, PlanRefusalCause::Terminal);
    assert_eq!(refusal.compaction, None);
    Ok(())
}

#[test]
fn read_capacity_invalid_instruction_epoch_stays_terminal() -> Result<(), String> {
    let fixture = Fixture::new("invalid-planning-epoch")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &fixture.input,
        "inspect",
    )?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    let update = grapt_instruction::InstructionUpdate {
        seq: grapt_types::EventSeq::new(2),
        changes: vec![grapt_instruction::InstructionChange::Removed {
            key: grapt_types::InstructionKey::new("project/rules").ok_or("key")?,
        }],
    };
    let refused = continuation
        .read_projection(|reader| {
            reader
                .request_with_instructions("inspect", false, &[update.clone(), update])
                .map_err(|error| PlanRefusal::from(error.to_string()))
        })
        .err()
        .ok_or("duplicate instruction coordinates produced a request")?;
    assert_eq!(refused.cause, PlanRefusalCause::Terminal);
    assert_eq!(refused.compaction, None);
    Ok(())
}

struct ProjectionPlan {
    continuation: DurableContinuation,
    inner: crate::dogfood::production::ProductPlan,
    waiting: Arc<tokio::sync::Notify>,
    coordinates: Arc<Mutex<Vec<(StepNumber, u32, HistoryGeneration)>>>,
}

impl AttemptPlan for ProjectionPlan {
    fn plan(
        &mut self,
        step: StepNumber,
        ordinal: u32,
        generation: HistoryGeneration,
    ) -> Result<grapt_runtime::PlannedAttempt, PlanRefusal> {
        lock(&self.coordinates).push((step, ordinal, generation));
        self.continuation
            .read_projection(|_| Ok::<(), PlanRefusal>(()))?;
        self.inner.plan(step, ordinal, generation)
    }

    fn retrying(&mut self, notice: Option<grapt_runtime::step::RetryNotice>) {
        if let Some(notice) = notice {
            assert!(notice.capacity_wait.is_some());
            assert_eq!(notice.attempt, 1);
            self.waiting.notify_one();
        }
    }
}

#[derive(Debug)]
struct RuntimeClock;
impl grapt_runtime::Clock for RuntimeClock {
    fn now_ms(&self) -> i64 {
        1_788_220_800_000
    }
}

struct NoTools;
impl grapt_runtime::ToolRunner for NoTools {
    fn run(
        &self,
        _: ToolCallId,
        _: String,
        _: grapt_runtime::BoundedToolInput,
    ) -> grapt_runtime::BoxFuture<'static, grapt_runtime::ToolOutcome> {
        panic!("the text-only provider requested a tool")
    }
}

#[tokio::test]
async fn read_capacity_driver_waits_without_an_attempt_then_recovers_same_plan(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_provider::{AttemptOpener, CredentialPlacement, OpenerCredential};
    use grapt_runtime::{
        step_interrupt, AttemptSource, NoCompaction, ProviderAttemptSource, RecordingOwner,
        StepDriver, StepPolicy, StepResult, StepTerminalHandoff,
    };
    use grapt_types::{AccountId, CredentialVersion, ModelRouteId, PolicyVersion, Secret};
    let fixture = Fixture::new("driver-reader-capacity")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &fixture.input,
        "inspect",
    )?;
    let waiting = Arc::new(tokio::sync::Notify::new());
    let coordinates = Arc::new(Mutex::new(Vec::new()));
    let planner = ProjectionPlan {
        continuation: fixture.continuation(fixture.input.clone())?,
        inner: product_plan(&fixture, fixture.input.clone(), "inspect")?,
        waiting: Arc::clone(&waiting),
        coordinates: Arc::clone(&coordinates),
    };
    let opener = AttemptOpener::deterministic(
        [grapt_provider::deterministic::ScriptBuilder::new()
            .text(&["ready"])
            .finish(FinishReason::Stop, None)],
        ModelRouteId::new("route_product").ok_or("route")?,
        OpenerCredential::inject(
            AccountId::new("acct_product").ok_or("account")?,
            CredentialVersion::new("v1").ok_or("version")?,
            CredentialPlacement::BearerAuthorization,
            Secret::new("SYNTHETIC_CAPACITY_TEST".into()),
        )?,
    );
    let ids = Arc::new(CountingIds::default());
    let step = StepNumber::new(1).ok_or("step")?;
    let mut source = ProviderAttemptSource::new(
        Arc::new(opener),
        Arc::clone(fixture.inbox()?),
        ids.clone(),
        Arc::new(RuntimeClock),
        fixture.session.clone(),
        grapt_store::AttemptRecordCeiling::new(std::num::NonZeroU32::new(512).ok_or("ceiling")?),
        planner,
    );
    source.begin_step(StepTerminalHandoff::new(
        step,
        "evt_capacity_terminal".parse::<EventId>()?,
        1_788_220_800_000,
    ));
    let held = HeldReaders::new(readers(&fixture)?)?;
    let policy = StepPolicy::new(
        PolicyVersion::new("reader-capacity").ok_or("policy")?,
        1,
        Duration::from_millis(5),
        1,
        Duration::from_secs(5),
    )?;
    let (_interrupt, cancellation) = step_interrupt();
    let mut history = NoCompaction;
    let mut driver = Box::pin(StepDriver::run(
        step,
        policy,
        &mut source,
        &mut history,
        Arc::new(NoTools),
        RecordingOwner::new(),
        cancellation,
    ));
    tokio::select! {
        result = &mut driver => panic!("reader contention terminated the step: {:?}", result.1),
        result = tokio::time::timeout(READ_WAIT + Duration::from_secs(3), waiting.notified()) => { result?; }
    }
    assert_eq!(ids.0.load(Ordering::SeqCst), 0, "waiting minted an attempt");
    held.release_one()?;
    let (owner, report) = tokio::time::timeout(Duration::from_secs(5), driver).await?;
    assert!(
        matches!(report.result, StepResult::Ended { .. }),
        "{report:?}"
    );
    assert_eq!(report.receipts.len(), 1);
    assert_eq!(report.compactions, 0);
    assert_eq!(owner.step_terminal_count(), 1);
    assert!(ids.0.load(Ordering::SeqCst) > 0);
    let calls = lock(&coordinates);
    assert!(calls.len() >= 2);
    assert!(calls
        .iter()
        .all(|(s, ordinal, generation)| *s == step && *ordinal == 1 && *generation == calls[0].2));
    Ok(())
}

#[test]
fn read_capacity_expired_application_projection_releases_the_blocking_worker() -> Result<(), String>
{
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .map_err(|error| error.to_string())?
        .block_on(async {
            let mut fixture = Fixture::new("expired-reader-worker")?;
            fixture.close();
            let store = EventStore::open(&fixture.database, "expired-reader-worker")
                .map_err(|error| error.to_string())?;
            let target = crate::dogfood::production::tests::projection_target()?;
            let (app, _driver) = crate::dogfood::test_provider::open(
                &fixture._scratch.0, store, &target, crate::dogfood::event_hub()?,
                crate::dogfood::plugins::PluginRuntime::empty(),
            ).map_err(|error| error.to_string())?;
            let pool = lock(&app.inbox).store().read_pool()
                .map_err(|error| error.to_string())?.ok_or("file reader pool absent")?;
            let held = HeldReaders::new(pool)?;
            // Start the sole blocking worker before asking it to check out a reader.
            tokio::task::spawn_blocking(|| ()).await.map_err(|error| error.to_string())?;
            let response = tokio::time::timeout(Duration::from_secs(1),
                app.read_projection(|_| Ok::<(), String>(()), |error| error))
                .await.map_err(|_| "application read missed its wait deadline")?;
            assert!(response.is_err(), "a saturated application read succeeded");
            let mut unrelated = tokio::task::spawn_blocking(|| ());
            let released = match tokio::time::timeout(Duration::from_millis(750), &mut unrelated).await {
                Ok(done) => { done.map_err(|error| error.to_string())?; true }
                Err(_) => false,
            };
            // Release the actual leases and reap queued work even when the assertion fails.
            drop(held);
            if !released {
                unrelated.await.map_err(|error| error.to_string())?;
            }
            assert!(released, "an expired application read retained the only blocking worker while waiting for reader capacity");
            Ok(())
        })
}
