//! Durable input admission (the session inbox).
//!
//! Law: pending rows contain UNPROMOTED work only; a promotion clears the
//! row in place (identity = the input's own MessageId, never re-keyed).
//! Measured on the frozen clone (2026-08-08): admitted events carry
//! `input.delivery`; all 887 real admissions are `steer` — `queue` delivery
//! is durably UNOBSERVED in real data (contract-legal, fixture-covered);
//! real `session.input.promoted.1` events carry NO scope field.

pub mod admission;
pub mod event;
pub mod fold;
pub mod operation_mail;
pub mod promoted_read;

pub use admission::InboxProjector;
pub use admission::{
    AdmissionCoordinates, AdmitOutcome, AdmitRequest, CompactionAdmission, CompactionAdmitRequest,
    CompactionFailRequest, CompactionFailure, Inbox, InboxError, InputAcceptance, NativeMailResume,
    PendingCompaction, PendingInput, PromoteOutcome, PromoteScope, Promoted, PromotedInput,
    PromotedInputKind, PromotedMessage, SyntheticAdmitRequest, UserInputOptions, UserInputOrigin,
    WakeDisposition,
};
pub use event::{InboxEvent, InboxEventError, OperationDeliverySettlement};
pub use fold::{InboxFold, OperationDeliveryRow, PendingRow};

/// Canonical user image references, decoded by the admission owner.
pub use admission::images::references as user_image_references;
