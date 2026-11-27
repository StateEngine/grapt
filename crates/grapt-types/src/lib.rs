//! Foundational identity types and closed vocabularies.
//!
//! Law source: `SECODE_RUST_FOUNDATIONAL_TYPES_AND_LIFECYCLES_2026-08-07.md`
//! (type-ownership table). Identity, validation, formatting, and wire codecs
//! live here; downstream crates cannot construct identities from raw parts.
//! Parse happens only at a codec/store edge, via [`std::str::FromStr`].

pub mod artifact;
pub mod attempt;
pub mod attempt_receipt;
pub mod authority;
pub mod blocker;
pub mod capability;
pub mod capture_cursor;
pub mod checkpoint;
pub mod cloudflare;
pub mod completion;
pub mod content;
pub mod context;
pub mod credential;
pub mod event;
pub mod failure_diagnostic;
pub mod form;
pub mod hook;
pub mod id;
pub mod inbox;
pub mod installation;
pub mod instruction;
pub mod lineage;
pub mod location;
pub mod mailbox;
pub mod mcp_auth;
pub mod message;
pub mod migration;
pub mod model;
pub mod money;
pub mod objective;
pub mod operation;
pub mod path;
pub mod permission;
pub mod prefix;
pub mod profile_names;
pub mod protocol;
pub mod provider_limit;
pub mod provider_tool;
pub mod recovery;
pub mod rehydration;
pub mod remote_mail;
pub mod request_cache;
pub mod secret;
pub mod service;
pub mod session;
pub mod subagent;
pub mod terminality;
pub mod timing;
pub mod tool;
pub mod transport;
pub mod usage;
pub mod usage_compat;
pub mod vcs;
pub mod verification;
pub mod watsonx;

pub use artifact::{ArtifactDigest, ArtifactId, ArtifactRef, CaptureDisposition};
pub use attempt::{
    AttemptGroupId, AttemptOrdinal, AttemptSpend, ExecutionId, PhysicalAttemptId, StepNumber,
};
pub use attempt_receipt::{
    BoundedAttemptId, BoundedIdKind, BoundedSessionId, DurableAttemptReceipt, IdTooLong,
    LostEffect, MarkerOutcome, ReceiptDisposition, ReconciliationCeiling, ReconciliationPage,
    RequestSentMarker, ReservedMarker, SettledMarker,
};
pub use authority::{
    AuthorityPrincipal, AuthoritySource, ClockAuthority, DenialProvenance,
    DeterministicAuthorities, IdAuthority, IdToken, LayerName, LivePermissionDecision,
    PermissionSettlement, PermissionSettlementParts, PermissionSettlementRecord, PolicyVersion,
    Timestamp,
};
pub use blocker::{
    BlockerFamily, BlockerState, CancelledBy, CasOutcome, FormBlocker, FormBlockerState,
    PermissionBlocker, PermissionBlockerState, RuntimeGeneration, SettlementTicket, Waiter,
};
pub use capability::{
    AuthorityLease, CapabilityGrant, CapabilityId, CheckpointRestoreAuthorityLease,
    CheckpointRestoreCapabilityId, EffectClass, EgressAuthority, EgressDestination,
    FilesystemAuthority, GrantScope, ModelTargetCapabilityId, ProgramIdentity, RealizedEnforcement,
    ResolvedCapability, SessionGrant,
};
pub use capture_cursor::{
    CaptureCoordinate, CaptureCursor, CaptureCursorError, CaptureInterest, CaptureInterestError,
    MAX_CAPTURE_CURSOR_COORDINATES, MAX_CAPTURE_CURSOR_TOKEN_BYTES,
};
pub use checkpoint::{
    CaptureRefusalReason, CaptureSkipReason, CheckpointCaptureOutcome, CheckpointFidelity,
    CheckpointHeadId, CheckpointLaneId, CheckpointLineage, CheckpointOperationId,
    CheckpointProviderId, CheckpointProviderVersion, CheckpointRestorePlanId, FidelityLoss,
    PlanPreconditionObservation, RestoreAdmission, RestoreApplicationAdmission,
    RestoreConsequences, RestorePrecondition, RestoreRefusalReason, RestoreRefusalReceipt,
    RestoreRescue, RestoreRescueEvidence, SettledRestorePlan, SettledRestorePlanParts,
    UnusualRestoreCondition, WorkspaceCheckpointId, WorkspaceCheckpointRef,
};
pub use completion::ProviderCompletionDisposition;
pub use content::{ContentKind, ContentOrdinal, NormalizedContentItem, NormalizedContentSequence};
pub use context::{
    AdapterVersion, CompactionDescriptor, CompactionDescriptorParts, CompactionId,
    CompactionSummary, CompactionTrigger, ContextEpoch, ContextWindowId, ContributionId,
    ContributionState, ContributionUnavailable, IncompatibleSummary, NextRequestRef,
    RequestContextIdentity, RetainedHistory, RetentionPolicyVersion, SummaryCompatibility,
    SummaryRedemption, TrustLabel,
};
pub use credential::{AccountId, CredentialRecordId, CredentialStorageMode, CredentialVersion};
pub use event::{EventFencing, EventId, EventSeq, EventVersion, WriterEpoch};
pub use form::{FormAnswer, FormId, FormOwner, FormSuspensionReason, McpElicitationId};
pub use hook::HookId;
pub use id::IdError;
pub use inbox::{AdmissionSeq, DeliveryMode, InboxInputClass, PendingInputId};
pub use instruction::{
    InstructionEpoch, InstructionHash, InstructionHashParseError, InstructionKey,
};
pub use lineage::{
    ForkBoundary, ForkBoundaryError, ForkLineage, ForkLineageBuilder, ForkLineageError, LineageCap,
    LineageSegment, ParentSeq,
};
pub use location::{LocationId, LocationRef, LocationRefError, Platform, ProjectId, WorkspaceId};
pub use mailbox::{
    MailKindLabel, MailboxAdmissionSeq, MailboxBody, MailboxCursor, MailboxError,
    MailboxIdentityError, MailboxMessageId, MailboxSourceCursor, SeatMailboxMessage,
    SeatMessageKind, SeatRecipientId, MAX_MAILBOX_BODY_BYTES, MAX_MAILBOX_MESSAGE_ID_BYTES,
    MAX_MAIL_KIND_LABEL_BYTES, MAX_SEAT_RECIPIENT_ID_BYTES,
};
pub use message::MessageId;
pub use migration::{
    BackupId, DatabaseEvidence, DatabaseFingerprint, EngineEpoch, MigrationEligibility,
    MigrationId, RecoveryDisposition,
};
pub use model::{
    AgentId, BaseModelId, CatalogGeneration, CatalogReissuanceReceipt, IncapableContract,
    IssuanceRefusal, ModelCatalog, ModelCatalogEntry, ModelCatalogRecord, ModelCatalogVersion,
    ModelFamilyId, ModelId, ModelRef, ModelRegistry, ModelRouteId, ModelVariantId, ProviderId,
    ResolvedModelTarget, RoutingRequest,
};
pub use money::{MoneyError, MoneyUsd, NonFinite};
pub use objective::{ObjectiveClaim, ObjectiveId, ObjectiveStatus, WorkBlockerId, WorkItemId};
pub use operation::{
    AmbiguityEvidence, DeliveryClaimId, DeliveryState, OperationId, OperationKind,
    OperationRouteId, OperationState,
};
pub use path::PathIdentity;
pub use permission::{PermissionId, PermissionReply, PermissionSuspensionReason};
pub use prefix::{
    CommittedPrefixId, ContextPrefixReceipt, ContextPrefixReceiptParts, PrefixBoundsError,
    PrefixComponentDiagnostic, PrefixComponentId, PrefixComponentOrdinal, PrefixComponentReceipt,
    PrefixIdentityBounds, PrefixInvalidation, PrefixKeyRealm, PrefixKeyVersion, PrefixLabelError,
    PrefixLabelKind, PrefixReceiptError, ProviderCacheNamespace, COMMITTED_PREFIX_ID_BYTES,
    MAX_PREFIX_LABEL_BYTES,
};
pub use protocol::{ProtocolVersion, CODEX_COMPATIBILITY_VERSION};
pub use provider_tool::{
    AdapterCloseProof, AdapterOpenCloseReceipt, ProviderStreamScope, ProviderToolAccumulation,
    ProviderToolCallValue, ProviderToolHandle, ProviderToolInputKey,
};
pub use recovery::{RecoveryAnchor, RecoveryAnchorId, WorktreeIdentity};
pub use rehydration::{
    RehydrationBounds, RehydrationEntry, RehydrationError, RehydrationManifest, RehydrationSlot,
    RequiredMiss,
};
pub use secret::Secret;
pub use service::{
    CoreField, Endpoint, EnvironmentReceipt, OwnerReceipt, ProcessId, ReplacementOutcome,
    SemanticVersion, ServiceGeneration, ServiceInstanceId, ServiceRegistrationVersion,
    ServiceSlotId, UpgradeDisposition, UpgradeRefusal, VersionOrderError,
};
pub use session::{SessionId, SessionLaunch};
pub use subagent::{
    CarryForwardRefusal, DelegationSelectionProvenance, EffectiveGraphId, SelectorVersion,
    SubagentAddress, SubagentGeneration, SubagentLabel, SubagentModelPreference,
    SubagentOperationId, ToolCallId, UserModelSelection, UserSelectionAuthority,
    UserSelectionSurface,
};
pub use terminality::{
    AttemptTerminalReason, CleanupTerminal, ContextFailure, DeliveryFailure, DeliveryTerminal,
    ExternalEffectState, InterruptionSource, OperationFailure, OperationTerminal,
    ProviderFailureClass, StepTerminalReason,
};
pub use tool::{
    ToolDefinitionId, ToolExecution, ToolFailureReason, ToolSchemaVersion, ToolTerminalArtifact,
    ToolTerminalOutcome,
};
pub use transport::{AuthenticatedClientId, Cursor, DecisionNonce, RequestId, SubscriptionId};
pub use usage::{
    reconcile_recorded_usage, reconcile_usage, AttemptUsage, LegacyAttemptUsage, ReconciledUsage,
    ReconciledUsageField, RecordedAttemptUsage, UsageDerivation, UsageField, UsageFieldKind,
    UsageObservationPhase, UsageProtocolFamily, UsageProvenance, UsageReconciliationError,
};
pub use usage_compat::{
    TokenCacheUsage, TokenCount, TokenCountError, TokenUsageInfo, UsageCompatError, UsageFieldName,
    MAX_EXACT_TOKEN_COUNT,
};
pub use vcs::{BranchName, BranchState, GitCommitId, VcsBaseline, VcsUnavailableReason};
pub use verification::{
    VerificationDisposition, VerificationProvenance, VerificationReceipt, VerificationReceiptId,
    VerifierEnvironmentId, VerifierVersion,
};

pub use mcp_auth::McpLoginId;

pub use watsonx::{WatsonxProjectId, WATSONX_API_VERSION};
