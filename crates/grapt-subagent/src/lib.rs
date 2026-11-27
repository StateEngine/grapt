//! Durable subagent operation domain.
//!
//! A subagent operation is one KIND of the shared durable operation LAW: it has
//! a stable identity, an immutable owner route, typed lifecycle states, an
//! exactly-once delivery claim, and durable recovery. This crate owns the
//! subagent payloads and its own application service; it does NOT fold
//! Permission/Form/Shell into a god implementation.
//!
//! Correction (red-team B.2, fixture `waivers.json`): the operation is keyed by
//! [`grapt_types::SubagentOperationId`], distinct from the child
//! [`grapt_types::SessionId`], and records both. Domain terminality is
//! separate from delivery; a running operation from a dead runtime generation
//! settles `Cancelled(ProcessLoss)` and is never replayed. Delivery of a
//! background completion durably admits exactly one typed operation-delivery
//! fact on the parent through the inbox seam. It is never user speech and has
//! no steer/queue mode.

pub mod operation;
pub mod service;

pub use operation::{
    ArtifactBackedOutput, ClaimRoute, DeliveryClaim, DeliveryLimits, DeliverySettlement,
    DeliveryStatus, OperationStatus, OutputVerificationError, SubagentAdmission,
    SubagentCancelReason, SubagentMode, SubagentTerminal,
};
pub use service::{
    AgentMessageReceipt, AgentMessageRequest, ChildOperation, ChildOperationContext,
    ChildOperationPage, ClaimOutcome, FollowupCancelReason, FollowupDecision, FollowupReceipt,
    FollowupRun, FollowupStart, FollowupState, SubagentError, SubagentService,
};
