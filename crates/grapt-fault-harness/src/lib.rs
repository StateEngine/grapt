//! Deterministic seeded fault-injection and chaos harness.
//!
//! Every chaos run is driven by a single [`FaultSeed`], produces a reproducible
//! [`FailureTranscript`] on fault or failure, and injects transport, SSE, and
//! storage faults strictly by deterministic counts (request, frame, byte, or
//! operation index) rather than wall-clock sleeps.

pub mod proxy;
pub mod schedule;
pub mod seed;
pub mod storage;

pub use proxy::{ChaosProxy, ChaosProxyHandle, ProxyError};
pub use schedule::{
    FailureTranscript, FaultAction, FaultEvent, FaultPlan, FaultSchedule, FaultTrigger,
};
pub use seed::{FaultSeed, SeededRng};
pub use storage::{
    StorageError, StorageFault, StorageFaultScript, StorageOperation, StorageOutcome,
    TerminalCommitError, TerminalLedger, TerminalRecord,
};
