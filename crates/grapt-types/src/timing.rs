//! Volatile timing vocabulary. Samples carry identities and clocks, never payloads.
//! Consumers own the bounded observer; this module owns no queue or persistence.
use crate::{PhysicalAttemptId, SessionId, Timestamp};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimingClass {
    Content,
    Usage,
    Step,
    Execution,
    Other,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimingStage {
    LiveText,
    TextClosed,
    ProviderTerminal,
    ReceiptSettle,
    ReceiptRelease,
    AcpTerminal,
    AcpFinalGet,
    AcpEnqueue,
    AcpWrite,
    AcpFlush,
    ArtifactCapture,
    ArtifactObjectSync,
    ArtifactMetadataSync,
    StoreLock,
    TransactionBegin,
    Append,
    SqlCommit,
    ProjectionPrepare,
    ProjectionPublish,
    ReaderJoin,
    ToolDrain,
    ExecutionSettle,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimingScope {
    pub session: Option<SessionId>,
    pub attempt: Option<PhysicalAttemptId>,
    pub class: TimingClass,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimingSample {
    pub scope: TimingScope,
    pub stage: TimingStage,
    pub start: Timestamp,
    pub end: Timestamp,
}
/// A diagnostic adapter must never wait for queue space or affect execution.
pub trait TimingObserver: std::fmt::Debug + Send + Sync {
    fn now(&self) -> Timestamp;
    fn record(&self, sample: TimingSample);
}
#[derive(Debug, Clone)]
pub struct TimingContext {
    pub observer: Arc<dyn TimingObserver>,
    pub scope: TimingScope,
}
impl TimingContext {
    pub fn span(&self, stage: TimingStage) -> TimingSpan {
        TimingSpan {
            context: self.clone(),
            stage,
            start: self.observer.now(),
        }
    }
}
/// Records completed boundaries on return, refusal, or cancellation. A sample
/// measures elapsed time; its presence makes no claim that the operation succeeded.
#[derive(Debug)]
pub struct TimingSpan {
    context: TimingContext,
    stage: TimingStage,
    start: Timestamp,
}
impl Drop for TimingSpan {
    fn drop(&mut self) {
        self.context.observer.record(TimingSample {
            scope: self.context.scope.clone(),
            stage: self.stage,
            start: self.start,
            end: self.context.observer.now(),
        });
    }
}
