//! The R4 CLI/noninteractive/ACP owner.
//!
//! This crate is a client of the versioned `grapt-api` DTO surface. It owns no
//! Session, provider, permission, service-election, or database state. Durable
//! relist is authoritative; live SSE is advisory. Every input, output, retry,
//! process, frame, and retained-cursor dimension has an explicit finite bound.

pub mod acp;
pub mod bounds;
mod branding_compat;
pub mod build_identity;
pub mod client;
pub mod command;
pub mod compatibility;
pub mod convergence;
pub mod discovery;
pub mod events;
pub mod executor;
mod failure;
pub mod installation;
pub mod migration;
mod operator_text;
pub mod output;
mod render;
pub mod run;
pub mod seat;
mod selection;
mod server_log;
#[cfg(test)]
mod testfs;

pub use acp::{
    serve_acp, AcpConnectionError, FrameRead, FrameReader, Inbound, RpcFault, RpcId,
    ACP_PROTOCOL_VERSION,
};
pub use bounds::{CliLimitValues, CliLimits, CliLimitsError};
pub use client::stall::MAX_CONSECUTIVE_TRANSPORT_FAILURES;
pub use client::{ApiClient, ApiClientError, ClientEndpoint, ClientMode};
pub use command::{
    AutoReply, CliCommand, CommandError, ConnectionChoice, OutputMode, PromptSource, ServiceAction,
    SessionSelector, PRESETS,
};
pub use compatibility::Refusal;
pub use convergence::{ConvergenceError, CursorConvergence, CursorPage, CursorRecord, CursorToken};
pub use discovery::{
    ChildLiveness, ClientCredential, DiscoveredService, DiscoveryError, LaunchWindowObservation,
    ReadinessWait, ServiceDiscovery, ServiceStatusReceipt,
};
pub use events::{AdvisoryDurable, AdvisoryError, AdvisoryEvent};
pub use executor::{
    command_error_exit, connect, embedded_client, execute, launch, run_with_cancellation,
};
pub use failure::CommandFailure;
pub use output::{BoundedOutput, OutputError, OutputReceipt};
pub use run::{
    CancellationCause, CancellationOutcome, ExitClass, RetryGate, RunError, RunReceipt, Runner,
    TerminalSettlement, CANCELLATION_HANDOFF, SERVER_LOSS_EXIT_BOUND, TRANSPORT_STALL_EXIT_BOUND,
};
pub use selection::{RequestedModel, StartupSelection};
