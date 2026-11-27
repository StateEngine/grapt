//! Shared in-crate test fixtures.
//!
//! `#[cfg(test)]` only, and never part of the public API.
//!
//! # Why this module exists
//!
//! [`crate::limits::ProviderLimitsParts`] has 24 mandatory fields with no
//! `Default`, which is the point — a profile cannot be completed by omission. The
//! cost is that every test needing a profile must write all 24 down, and before
//! this module four separate files carried their own copy of that literal. Four
//! copies of a 24-field fixture is a maintenance hazard of exactly the kind the
//! completeness rule is meant to expose: adding a 25th field should force ONE
//! honest edit, not four mechanical ones.
//!
//! The fixture takes the four ceiling-settled values as parameters, because those
//! are the ones tests actually vary; the 20 residual fields are fixed here and no
//! test asserts on them except the one proving they survive settling.

use crate::binding::{
    BindingGeneration, MailBinding, MailBindingReceipt, MailBindingReceiptParts, TakeoverReceipt,
};
use crate::identity::{
    AdapterVersion, ProjectPair, RecipientMappingVersion, SourceBindingId, SourcePrincipalId,
    SourceProjectId, TargetProjectId,
};
use crate::limits::{ProviderLimits, ProviderLimitsParts, SettledTransportLimits};
use core::num::{NonZeroU32, NonZeroUsize};
use grapt_types::mailbox::SeatRecipientId;
use grapt_types::protocol::ProtocolVersion;

fn nz32(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap_or(NonZeroU32::MIN)
}

fn nzs(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap_or(NonZeroUsize::MIN)
}

/// A complete profile whose four ceiling-settled fields the caller chooses.
pub(crate) fn provider_with(
    body: usize,
    messages_per_page: u32,
    aggregate: usize,
    diagnostic: usize,
) -> ProviderLimits {
    ProviderLimits::new(ProviderLimitsParts {
        max_refs_per_message: nz32(64),
        max_ref_utf8_bytes: nzs(512),
        max_aggregate_ref_bytes_per_message: nzs(32_768),
        max_decoded_message_bytes_including_metadata: nzs(131_072),
        max_decoded_page_bytes_including_metadata: nzs(524_288),
        max_cursor_bytes: nzs(1_024),
        max_snapshot_token_bytes: nzs(1_024),
        max_pages_per_runtime_slice: nz32(8),
        max_messages_per_runtime_slice: nz32(256),
        max_decoded_bytes_per_runtime_slice: nzs(1_048_576),
        max_ack_debt_items_per_recovery_page: nz32(64),
        max_connect_attempts_per_activation: nz32(4),
        max_reconnect_attempts_per_runtime_slice: nz32(2),
        max_connect_duration_ticks: nz32(300),
        max_single_reconnect_delay_ticks: nz32(600),
        max_total_reconnect_delay_ticks_per_slice: nz32(1_200),
        max_fetch_duration_ticks: nz32(300),
        max_ack_duration_ticks: nz32(300),
        max_shutdown_duration_ticks: nz32(300),
        max_joined_transport_tasks_at_shutdown: nz32(8),
        max_diagnostic_bytes: nzs(diagnostic),
        max_body_utf8_bytes: nzs(body),
        max_messages_per_page: nz32(messages_per_page),
        max_aggregate_body_bytes_per_page: nzs(aggregate),
    })
}

/// A profile sitting exactly at every Grapt ceiling.
pub(crate) fn provider_at_ceilings() -> ProviderLimits {
    provider_with(
        crate::limits::GRAPT_MAX_BODY_BYTES,
        crate::limits::GRAPT_MAX_MESSAGES_PER_PAGE,
        crate::limits::GRAPT_MAX_AGGREGATE_BODY_BYTES_PER_PAGE,
        crate::limits::GRAPT_MAX_DIAGNOSTIC_BYTES,
    )
}

/// The settled form of [`provider_at_ceilings`].
pub(crate) fn settled_at_ceilings() -> SettledTransportLimits {
    provider_at_ceilings().settle()
}

/// An open binding over a named project pair.
///
/// Returns `Option` rather than panicking because the workspace denies
/// `unwrap`/`expect` in test code too; a caller uses `?` or `ok_or`.
///
/// This lives here rather than in each test module because
/// [`MailBinding::new`] is now `pub(crate)` — which is the point of the seal, and
/// also means every in-crate test needing a binding needs one crate-owned route
/// to build it.
pub(crate) fn binding_between(source: &str, target: &str) -> Option<MailBinding> {
    Some(MailBinding::new(MailBindingReceipt::new(
        MailBindingReceiptParts {
            binding_id: SourceBindingId::new("bind-1")?,
            protocol: ProtocolVersion::new(crate::REMOTE_SEAT_MAIL_TRANSPORT_V1)?,
            adapter_version: AdapterVersion::new("deterministic-fake-1")?,
            projects: ProjectPair::new(
                SourceProjectId::new(source)?,
                TargetProjectId::new(target)?,
            ),
            verified_principal: SourcePrincipalId::new("agt-xztg")?,
            local_recipient: SeatRecipientId::new("seat-delta")?,
            mapping_version: RecipientMappingVersion::new("map-1")?,
            takeover: TakeoverReceipt::first_open(BindingGeneration::FIRST),
            limits: settled_at_ceilings(),
        },
    )))
}

/// The default cross-project binding: `stateengine` → `secode`.
pub(crate) fn binding() -> Option<MailBinding> {
    binding_between("stateengine", "secode") // grapt-rename: keep
}
