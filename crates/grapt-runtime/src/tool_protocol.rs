//! Governed, bounded tool-effect protocol below the sealed authority boundary.
//!
//! This crate owns the bounded execution and artifact protocol but no authority
//! issuer. Its [`ToolRunner`] composition therefore fails closed with typed
//! `AuthorityUnavailable`. Product composition may enter
//! [`GovernedToolRunner::run_effect`] only from the permission owner's one-shot
//! consume-and-run closure.

use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use grapt_artifact::{ArtifactKind, ArtifactStore, CaptureBound, Sensitivity};
use grapt_types::{
    CapabilityId, InterruptionSource, ToolCallId, ToolFailureReason, ToolTerminalArtifact,
    ToolTerminalOutcome,
};
use tokio::sync::{mpsc, watch};

use crate::budgets::{
    TOOL_ARTIFACT_BUFFER_BYTES, TOOL_ARTIFACT_MAX_BYTES, TOOL_INPUT_MAX_BYTES,
    TOOL_PROGRESS_CAPACITY, TOOL_PROGRESS_MAX_BYTES,
};
use crate::supervisor::{BoxFuture, ToolRunner};

/// Checked tool input. The executor never receives an unbounded input string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedToolInput(Box<[u8]>);

impl BoundedToolInput {
    pub fn new(bytes: Vec<u8>) -> Result<Self, ToolInputTooLarge> {
        if bytes.len() > TOOL_INPUT_MAX_BYTES {
            return Err(ToolInputTooLarge { bytes: bytes.len() });
        }
        Ok(Self(bytes.into_boxed_slice()))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("tool input is {bytes} bytes; the limit is {TOOL_INPUT_MAX_BYTES}")]
pub struct ToolInputTooLarge {
    pub bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolProgress {
    call: ToolCallId,
    payload: Box<[u8]>,
}

#[derive(Debug, Default)]
struct ProgressState {
    coalesced: AtomicUsize,
    dropped: AtomicUsize,
    high_water: AtomicUsize,
    pending: Mutex<Option<ToolProgress>>,
}

/// Exact transient progress accounting. It is diagnostic, never durable truth.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProgressDiagnostics {
    pub coalesced: usize,
    pub dropped: usize,
    pub high_water: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressOffer {
    Enqueued,
    Coalesced,
    Dropped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("tool progress is {bytes} bytes; the limit is {TOOL_PROGRESS_MAX_BYTES}")]
pub struct ToolProgressTooLarge {
    pub bytes: usize,
}

/// A bounded, non-blocking progress carrier independent of terminal capacity.
#[derive(Debug, Clone)]
pub struct BoundedProgressSink {
    tx: mpsc::Sender<ToolProgress>,
    state: Arc<ProgressState>,
}

impl BoundedProgressSink {
    pub fn channel() -> (Self, ToolProgressDrain) {
        let (tx, rx) = mpsc::channel(TOOL_PROGRESS_CAPACITY);
        let state = Arc::new(ProgressState::default());
        (
            Self {
                tx,
                state: Arc::clone(&state),
            },
            ToolProgressDrain { rx, state },
        )
    }

    pub fn try_emit(
        &self,
        call: ToolCallId,
        payload: Vec<u8>,
    ) -> Result<ProgressOffer, ToolProgressTooLarge> {
        if payload.len() > TOOL_PROGRESS_MAX_BYTES {
            return Err(ToolProgressTooLarge {
                bytes: payload.len(),
            });
        }
        let progress = ToolProgress {
            call,
            payload: payload.into_boxed_slice(),
        };
        match self.tx.try_send(progress) {
            Ok(()) => {
                let depth = self.tx.max_capacity().saturating_sub(self.tx.capacity());
                self.state.high_water.fetch_max(depth, Ordering::Relaxed);
                Ok(ProgressOffer::Enqueued)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.state.dropped.fetch_add(1, Ordering::Relaxed);
                Ok(ProgressOffer::Dropped)
            }
            Err(mpsc::error::TrySendError::Full(progress)) => {
                let mut pending = lock(&self.state.pending);
                if pending.is_none()
                    || pending
                        .as_ref()
                        .is_some_and(|current| current.call == progress.call)
                {
                    *pending = Some(progress);
                    self.state.coalesced.fetch_add(1, Ordering::Relaxed);
                    Ok(ProgressOffer::Coalesced)
                } else {
                    self.state.dropped.fetch_add(1, Ordering::Relaxed);
                    Ok(ProgressOffer::Dropped)
                }
            }
        }
    }

    pub fn diagnostics(&self) -> ProgressDiagnostics {
        ProgressDiagnostics {
            coalesced: self.state.coalesced.load(Ordering::Relaxed),
            dropped: self.state.dropped.load(Ordering::Relaxed),
            high_water: self.state.high_water.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug)]
pub struct ToolProgressDrain {
    rx: mpsc::Receiver<ToolProgress>,
    state: Arc<ProgressState>,
}

impl ToolProgressDrain {
    pub fn try_recv(&mut self) -> Option<(ToolCallId, Box<[u8]>)> {
        match self.rx.try_recv() {
            Ok(progress) => Some((progress.call, progress.payload)),
            Err(_) => lock(&self.state.pending)
                .take()
                .map(|progress| (progress.call, progress.payload)),
        }
    }
}

/// Explicit cancellation carrier delivered to an executor.
#[derive(Debug, Clone)]
pub struct ToolCancellation {
    signal: watch::Receiver<Option<InterruptionSource>>,
}

impl ToolCancellation {
    pub fn channel() -> (watch::Sender<Option<InterruptionSource>>, Self) {
        let (tx, signal) = watch::channel(None);
        (tx, Self { signal })
    }

    pub fn cancelled_by(&self) -> Option<InterruptionSource> {
        *self.signal.borrow()
    }

    pub async fn changed(&mut self) -> Option<InterruptionSource> {
        if self.signal.changed().await.is_err() {
            return None;
        }
        *self.signal.borrow()
    }
}

/// Owned artifact carrier. The bound, kind and sensitivity cannot be omitted.
#[derive(Debug)]
pub struct BoundedArtifactWriter<S> {
    store: Arc<S>,
    bound: CaptureBound,
    kind: ArtifactKind,
    sensitivity: Sensitivity,
}

/// Artifact provenance minted only by [`BoundedArtifactWriter`].
///
/// The field is private so an executor cannot substitute a caller-authored
/// [`grapt_types::ArtifactRef`] for bytes durably captured by this protocol.
#[derive(Debug)]
pub struct CapturedToolArtifact(ToolTerminalArtifact);

impl CapturedToolArtifact {
    pub fn reference(&self) -> &grapt_types::ArtifactRef {
        self.0.reference()
    }
    pub fn disposition(&self) -> grapt_types::CaptureDisposition {
        self.0.disposition()
    }
}

/// Closed executor result below the authority seal.
///
/// An executor can name a truthful terminal class, but artifact-bearing
/// variants accept only writer-minted provenance. The governed runner alone
/// converts this carrier into the public durable terminal vocabulary.
#[derive(Debug)]
pub struct ToolExecutionTerminal(ToolTerminalOutcome);

impl ToolExecutionTerminal {
    pub fn completed(output: Option<CapturedToolArtifact>) -> Self {
        Self(ToolTerminalOutcome::Completed {
            output: output.map(|captured| captured.0),
        })
    }

    pub fn failed(reason: ToolFailureReason, detail: Option<CapturedToolArtifact>) -> Self {
        Self(ToolTerminalOutcome::Failed {
            reason,
            detail: detail.map(|captured| captured.0),
        })
    }

    pub fn cancelled(by: InterruptionSource) -> Self {
        Self(ToolTerminalOutcome::Cancelled { by })
    }

    pub fn result_missing() -> Self {
        Self(ToolTerminalOutcome::ResultMissing)
    }

    fn into_durable(self) -> ToolTerminalOutcome {
        self.0
    }
}

impl<S: ArtifactStore> BoundedArtifactWriter<S> {
    fn new(store: Arc<S>) -> Self {
        let bound = match CaptureBound::new(TOOL_ARTIFACT_MAX_BYTES, TOOL_ARTIFACT_BUFFER_BYTES) {
            Some(bound) => bound,
            None => unreachable!("the ruled artifact bound is non-zero"),
        };
        Self {
            store,
            bound,
            kind: ArtifactKind::CapturedOutput,
            sensitivity: Sensitivity::Ordinary,
        }
    }

    pub fn capture<R: Read>(&self, source: R) -> Result<CapturedToolArtifact, ToolFailureReason> {
        self.capture_with_sensitivity(source, self.sensitivity)
    }

    /// Nested machine results retain conservative sensitivity even when their
    /// source protocol has no sensitivity annotation.
    pub fn capture_sensitive<R: Read>(
        &self,
        source: R,
    ) -> Result<CapturedToolArtifact, ToolFailureReason> {
        self.capture_with_sensitivity(source, Sensitivity::SecretBearing)
    }

    fn capture_with_sensitivity<R: Read>(
        &self,
        source: R,
        sensitivity: Sensitivity,
    ) -> Result<CapturedToolArtifact, ToolFailureReason> {
        let captured = self
            .store
            .capture(source, self.bound, self.kind, sensitivity)
            .map_err(|_| ToolFailureReason::CaptureFailed)?;
        ToolTerminalArtifact::new(captured.reference().clone(), captured.disposition())
            .map(CapturedToolArtifact)
            .ok_or(ToolFailureReason::CaptureFailed)
    }
}

/// Later executors implement this port; the runtime owns the protocol around it.
pub trait GovernedToolExecutor<S>: Send + Sync + 'static
where
    S: ArtifactStore + Send + Sync + 'static,
{
    fn execute(
        &self,
        capability: CapabilityId,
        call: ToolCallId,
        input: BoundedToolInput,
        progress: BoundedProgressSink,
        cancellation: ToolCancellation,
        artifacts: BoundedArtifactWriter<S>,
    ) -> BoxFuture<'static, ToolExecutionTerminal>;
}

/// Production generic runner. It owns no capability policy or lease issuer.
#[derive(Debug)]
pub struct GovernedToolRunner<E, S> {
    executor: Arc<E>,
    artifacts: Arc<S>,
}

impl<E, S> GovernedToolRunner<E, S> {
    pub fn new(executor: Arc<E>, artifacts: Arc<S>) -> Self {
        Self {
            executor,
            artifacts,
        }
    }
}

impl<E, S> GovernedToolRunner<E, S>
where
    E: GovernedToolExecutor<S>,
    S: ArtifactStore + Send + Sync + 'static,
{
    /// Run the authority-neutral bounded executor/artifact protocol.
    ///
    /// The permission owner must call this from its consume-and-run closure;
    /// this runtime helper neither issues nor consumes authority.
    pub fn run_effect(
        &self,
        capability: CapabilityId,
        call: ToolCallId,
        input: BoundedToolInput,
        progress: BoundedProgressSink,
        cancellation: ToolCancellation,
    ) -> BoxFuture<'static, ToolTerminalOutcome> {
        let execution = self.executor.execute(
            capability,
            call,
            input,
            progress,
            cancellation,
            BoundedArtifactWriter::new(Arc::clone(&self.artifacts)),
        );
        Box::pin(async move { execution.await.into_durable() })
    }
}

/// Until the authenticated issuer lands, normal runtime composition refuses.
impl<E, S> ToolRunner for GovernedToolRunner<E, S>
where
    E: GovernedToolExecutor<S>,
    S: ArtifactStore + Send + Sync + 'static,
{
    fn run(
        &self,
        _call: ToolCallId,
        _tool: String,
        _input: BoundedToolInput,
    ) -> BoxFuture<'static, ToolTerminalOutcome> {
        let artifacts = Arc::clone(&self.artifacts);
        Box::pin(async move {
            let detail = match CaptureBound::new(2048, 2048) {
                Some(bound) => match artifacts.capture(
                    std::io::Cursor::new(
                        b"tool execution refused: governed tool authority is unavailable"
                            .as_slice(),
                    ),
                    bound,
                    ArtifactKind::Diagnostic,
                    Sensitivity::Ordinary,
                ) {
                    Ok(captured) => {
                        let artifact = ToolTerminalArtifact::new(
                            captured.reference().clone(),
                            captured.disposition(),
                        );
                        if artifact.is_none() {
                            grapt_diagnostics::report(
                                "governed tool refusal diagnostic artifact disposition mismatch"
                                    .to_owned(),
                            );
                        }
                        artifact
                    }
                    Err(error) => {
                        grapt_diagnostics::report(format!(
                            "governed tool refusal diagnostic artifact capture failed: {error}"
                        ));
                        None
                    }
                },
                None => {
                    grapt_diagnostics::report(
                        "governed tool refusal diagnostic capture bound invalid".to_owned(),
                    );
                    None
                }
            };
            ToolTerminalOutcome::Failed {
                reason: ToolFailureReason::AuthorityUnavailable,
                detail,
            }
        })
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use grapt_artifact::{
        ArtifactMetadata, CaptureError, CapturedArtifact, FetchError, FilesystemArtifactStore,
        ReadBudget, ReadPage, StatError,
    };
    use grapt_types::{
        ArtifactId, ArtifactRef, ClockAuthority, EffectClass, EgressAuthority, FilesystemAuthority,
        RealizedEnforcement, ResolvedCapability, Timestamp,
    };

    #[derive(Debug)]
    struct FrozenClock;

    impl ClockAuthority for FrozenClock {
        fn now(&self) -> Timestamp {
            Timestamp::from_millis(17)
        }
    }

    static SCRATCH_ORDINAL: AtomicUsize = AtomicUsize::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let ordinal = SCRATCH_ORDINAL.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "grapt-runtime-tool-provenance-{}-{ordinal}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Debug)]
    struct ReopeningArtifactStore {
        root: PathBuf,
    }

    impl ReopeningArtifactStore {
        fn open(&self) -> FilesystemArtifactStore {
            match FilesystemArtifactStore::open(
                &self.root,
                b"runtime-tool-key".to_vec(),
                Box::new(FrozenClock),
            ) {
                Ok(store) => store,
                Err(error) => panic!("scratch artifact store must open: {error}"),
            }
        }
    }

    impl ArtifactStore for ReopeningArtifactStore {
        fn capture<R: Read>(
            &self,
            source: R,
            bound: CaptureBound,
            kind: ArtifactKind,
            sensitivity: Sensitivity,
        ) -> Result<CapturedArtifact, CaptureError> {
            self.open().capture(source, bound, kind, sensitivity)
        }

        fn stat(&self, id: &ArtifactId) -> Result<ArtifactMetadata, StatError> {
            self.open().stat(id)
        }

        fn fetch(
            &self,
            reference: &ArtifactRef,
            offset: u64,
            budget: ReadBudget,
        ) -> Result<ReadPage, FetchError> {
            self.open().fetch(reference, offset, budget)
        }
    }

    #[derive(Debug)]
    struct CapturingExecutor {
        calls: Arc<AtomicUsize>,
    }

    #[derive(Debug)]
    struct FailureCapturingExecutor;

    #[derive(Debug)]
    struct OversizeCapturingExecutor;

    impl GovernedToolExecutor<ReopeningArtifactStore> for CapturingExecutor {
        fn execute(
            &self,
            _capability: CapabilityId,
            _call: ToolCallId,
            input: BoundedToolInput,
            _progress: BoundedProgressSink,
            _cancellation: ToolCancellation,
            artifacts: BoundedArtifactWriter<ReopeningArtifactStore>,
        ) -> BoxFuture<'static, ToolExecutionTerminal> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let captured = artifacts.capture(std::io::Cursor::new(input.as_bytes().to_vec()));
            Box::pin(async move {
                match captured {
                    Ok(captured) => ToolExecutionTerminal::completed(Some(captured)),
                    Err(reason) => ToolExecutionTerminal::failed(reason, None),
                }
            })
        }
    }

    impl GovernedToolExecutor<ReopeningArtifactStore> for FailureCapturingExecutor {
        fn execute(
            &self,
            _capability: CapabilityId,
            _call: ToolCallId,
            _input: BoundedToolInput,
            _progress: BoundedProgressSink,
            _cancellation: ToolCancellation,
            artifacts: BoundedArtifactWriter<ReopeningArtifactStore>,
        ) -> BoxFuture<'static, ToolExecutionTerminal> {
            let captured = artifacts.capture(std::io::Cursor::new(b"detail".to_vec()));
            Box::pin(async move {
                match captured {
                    Ok(captured) => ToolExecutionTerminal::failed(
                        ToolFailureReason::EffectFailed,
                        Some(captured),
                    ),
                    Err(reason) => ToolExecutionTerminal::failed(reason, None),
                }
            })
        }
    }

    impl GovernedToolExecutor<ReopeningArtifactStore> for OversizeCapturingExecutor {
        fn execute(
            &self,
            _capability: CapabilityId,
            _call: ToolCallId,
            _input: BoundedToolInput,
            _progress: BoundedProgressSink,
            _cancellation: ToolCancellation,
            artifacts: BoundedArtifactWriter<ReopeningArtifactStore>,
        ) -> BoxFuture<'static, ToolExecutionTerminal> {
            let captured = artifacts.capture(std::io::Cursor::new(vec![
                b'x';
                TOOL_ARTIFACT_MAX_BYTES
                    as usize
                    + 1
            ]));
            Box::pin(async move {
                match captured {
                    Ok(captured) => ToolExecutionTerminal::completed(Some(captured)),
                    Err(reason) => ToolExecutionTerminal::failed(reason, None),
                }
            })
        }
    }

    fn call() -> ToolCallId {
        match ToolCallId::new("call_progress") {
            Some(call) => call,
            None => panic!("static tool-call id must be valid"),
        }
    }

    #[test]
    fn first_overflow_is_counted_and_latest_progress_is_retained() {
        let (sink, mut drain) = BoundedProgressSink::channel();
        let call = call();
        for value in 0..TOOL_PROGRESS_CAPACITY {
            assert_eq!(
                sink.try_emit(call.clone(), vec![value as u8]),
                Ok(ProgressOffer::Enqueued)
            );
        }

        assert_eq!(
            sink.try_emit(call.clone(), vec![41]),
            Ok(ProgressOffer::Coalesced)
        );
        assert_eq!(
            sink.try_emit(call.clone(), vec![42]),
            Ok(ProgressOffer::Coalesced)
        );
        assert_eq!(
            sink.diagnostics(),
            ProgressDiagnostics {
                coalesced: 2,
                dropped: 0,
                high_water: TOOL_PROGRESS_CAPACITY,
            }
        );

        for _ in 0..TOOL_PROGRESS_CAPACITY {
            assert!(drain.try_recv().is_some());
        }
        assert_eq!(
            drain.try_recv(),
            Some((call, Vec::from([42]).into_boxed_slice()))
        );
        assert_eq!(drain.try_recv(), None);
    }

    #[tokio::test]
    async fn private_continuation_accepts_only_writer_minted_artifact_provenance() {
        let scratch = Scratch::new();
        let store = Arc::new(ReopeningArtifactStore {
            root: scratch.0.clone(),
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let runner = GovernedToolRunner::new(
            Arc::new(CapturingExecutor {
                calls: Arc::clone(&calls),
            }),
            Arc::clone(&store),
        );
        let capability = CapabilityId::derive(&ResolvedCapability::new(
            EffectClass::ToolInvoke,
            None,
            FilesystemAuthority::default(),
            EgressAuthority::default(),
            RealizedEnforcement::HarnessOnly,
        ));
        let input = match BoundedToolInput::new(b"abc".to_vec()) {
            Ok(input) => input,
            Err(error) => panic!("bounded input must fit: {error}"),
        };
        let (progress, _drain) = BoundedProgressSink::channel();
        let (_cancel_tx, cancellation) = ToolCancellation::channel();
        let outcome = runner
            .run_effect(capability, call(), input, progress, cancellation)
            .await;

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let artifact = match outcome {
            ToolTerminalOutcome::Completed {
                output: Some(artifact),
            } => artifact,
            other => panic!("writer-backed execution must complete with an artifact: {other:?}"),
        };
        let metadata = match store.stat(artifact.reference().id()) {
            Ok(metadata) => metadata,
            Err(error) => panic!("writer-minted artifact must stat: {error}"),
        };
        assert_eq!(metadata.kind(), ArtifactKind::CapturedOutput);
        let budget = match ReadBudget::new(8) {
            Some(budget) => budget,
            None => panic!("positive read budget must exist"),
        };
        let page = match store.fetch(artifact.reference(), 0, budget) {
            Ok(page) => page,
            Err(error) => panic!("writer-minted reference must fetch: {error}"),
        };
        assert_eq!(page.bytes(), b"abc");
    }

    #[tokio::test]
    async fn public_runner_refuses_without_authority_and_invokes_zero_executor_calls() {
        let scratch = Scratch::new();
        let store = Arc::new(ReopeningArtifactStore {
            root: scratch.0.clone(),
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let runner = GovernedToolRunner::new(
            Arc::new(CapturingExecutor {
                calls: Arc::clone(&calls),
            }),
            store,
        );

        let input = match BoundedToolInput::new(b"{}".to_vec()) {
            Ok(input) => input,
            Err(error) => panic!("static bounded tool input: {error}"),
        };
        let outcome =
            ToolRunner::run(&runner, call(), "provider-authored-label".to_owned(), input).await;

        match outcome {
            ToolTerminalOutcome::Failed {
                reason: ToolFailureReason::AuthorityUnavailable,
                detail: Some(detail),
            } => {
                assert!(detail.reference().length() > 0);
            }
            other => panic!("expected AuthorityUnavailable with named detail artifact: {other:?}"),
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    fn test_capability() -> CapabilityId {
        CapabilityId::derive(&ResolvedCapability::new(
            EffectClass::ToolInvoke,
            None,
            FilesystemAuthority::default(),
            EgressAuthority::default(),
            RealizedEnforcement::HarnessOnly,
        ))
    }

    fn empty_input() -> BoundedToolInput {
        match BoundedToolInput::new(Vec::new()) {
            Ok(input) => input,
            Err(error) => panic!("empty input must fit: {error}"),
        }
    }

    async fn run_private<E>(
        runner: &GovernedToolRunner<E, ReopeningArtifactStore>,
    ) -> ToolTerminalOutcome
    where
        E: GovernedToolExecutor<ReopeningArtifactStore>,
    {
        let (progress, _drain) = BoundedProgressSink::channel();
        let (_cancel_tx, cancellation) = ToolCancellation::channel();
        runner
            .run_effect(
                test_capability(),
                call(),
                empty_input(),
                progress,
                cancellation,
            )
            .await
    }

    #[tokio::test]
    async fn failed_detail_is_writer_captured_and_never_a_raw_string() {
        let scratch = Scratch::new();
        let store = Arc::new(ReopeningArtifactStore {
            root: scratch.0.clone(),
        });
        let runner =
            GovernedToolRunner::new(Arc::new(FailureCapturingExecutor), Arc::clone(&store));
        let outcome = run_private(&runner).await;
        let detail = match outcome {
            ToolTerminalOutcome::Failed {
                reason: ToolFailureReason::EffectFailed,
                detail: Some(detail),
            } => detail,
            other => panic!("failure detail must be a writer-backed artifact: {other:?}"),
        };
        let budget = match ReadBudget::new(16) {
            Some(budget) => budget,
            None => panic!("positive read budget must exist"),
        };
        let page = match store.fetch(detail.reference(), 0, budget) {
            Ok(page) => page,
            Err(error) => panic!("writer-minted detail must fetch: {error}"),
        };
        assert_eq!(page.bytes(), b"detail");
    }

    #[tokio::test]
    async fn capture_disposition_survives_the_closed_execution_terminal() {
        let scratch = Scratch::new();
        let store = Arc::new(ReopeningArtifactStore {
            root: scratch.0.clone(),
        });
        let runner = GovernedToolRunner::new(Arc::new(OversizeCapturingExecutor), store);
        let outcome = run_private(&runner).await;
        let output = match outcome {
            ToolTerminalOutcome::Completed {
                output: Some(output),
            } => output,
            other => panic!("oversize output must remain an artifact terminal: {other:?}"),
        };
        assert_eq!(
            output.disposition(),
            grapt_types::CaptureDisposition::TruncatedAtBoundary {
                captured: TOOL_ARTIFACT_MAX_BYTES
            }
        );
        assert_eq!(output.reference().length(), TOOL_ARTIFACT_MAX_BYTES);
    }
}
