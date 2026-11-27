//! Composition of the existing sealed HTTP port with the accepted v1 profile.
use super::*;
use grapt_mail_transport::{http::HttpMailTransport, limits::ProviderLimitsParts};
use grapt_types::{remote_mail::MailTransportClientInstanceId, ProtocolVersion, Secret};
use std::num::{NonZeroU16, NonZeroU32, NonZeroUsize};

pub type HttpMailbox = RemoteMailboxRuntime<HttpMailTransport>;
pub type HttpMailboxChangeResult = Result<ChangeSignal, OperationFailure>;
/// One owned advisory operation; no borrow of the runtime and no spawned task.
pub type HttpMailboxChange =
    std::pin::Pin<Box<dyn std::future::Future<Output = HttpMailboxChangeResult> + Send>>;

/// This profile is the released StateEngine dcec0682 MAIL_LIMITS, in milliseconds.
/// Negotiation still validates every field; a different deployment refuses.
pub fn stateengine_profile() -> Result<ProviderLimits, RemoteMailboxError> {
    fn count(n: u32) -> Result<NonZeroU32, RemoteMailboxError> {
        NonZeroU32::new(n).ok_or(RemoteMailboxError::SliceTooSmall)
    }
    fn bytes(n: usize) -> Result<NonZeroUsize, RemoteMailboxError> {
        NonZeroUsize::new(n).ok_or(RemoteMailboxError::SliceTooSmall)
    }
    Ok(ProviderLimits::new(ProviderLimitsParts {
        max_refs_per_message: count(64)?,
        max_ref_utf8_bytes: bytes(2048)?,
        max_aggregate_ref_bytes_per_message: bytes(131072)?,
        max_decoded_message_bytes_including_metadata: bytes(196608)?,
        max_decoded_page_bytes_including_metadata: bytes(524288)?,
        max_cursor_bytes: bytes(144)?,
        max_snapshot_token_bytes: bytes(36)?,
        max_pages_per_runtime_slice: count(8)?,
        max_messages_per_runtime_slice: count(512)?,
        max_decoded_bytes_per_runtime_slice: bytes(4194304)?,
        max_ack_debt_items_per_recovery_page: count(64)?,
        max_connect_attempts_per_activation: count(3)?,
        max_reconnect_attempts_per_runtime_slice: count(3)?,
        max_connect_duration_ticks: count(5000)?,
        max_single_reconnect_delay_ticks: count(1000)?,
        max_total_reconnect_delay_ticks_per_slice: count(3000)?,
        max_fetch_duration_ticks: count(10000)?,
        max_ack_duration_ticks: count(10000)?,
        max_shutdown_duration_ticks: count(5000)?,
        max_joined_transport_tasks_at_shutdown: count(2)?,
        max_diagnostic_bytes: bytes(512)?,
        max_body_utf8_bytes: bytes(32768)?,
        max_messages_per_page: count(64)?,
        max_aggregate_body_bytes_per_page: bytes(262144)?,
    }))
}

pub fn http_mailbox(
    port: NonZeroU16,
    capability: Arc<Secret<String>>,
    inbox: Arc<Mutex<Inbox>>,
    expected: RemoteMailboxScope,
    client: MailTransportClientInstanceId,
) -> Result<HttpMailbox, RemoteMailboxError> {
    let request = OpenRequest::new(
        expected.projects.target().clone(),
        expected.recipient.clone(),
        ProtocolVersion::new(grapt_mail_transport::REMOTE_SEAT_MAIL_TRANSPORT_V1)
            .ok_or(RemoteMailboxError::ForeignBinding)?,
        client,
    );
    RemoteMailboxRuntime::new(
        HttpMailTransport::new(port, capability).map_err(RemoteMailboxError::Port)?,
        inbox,
        expected,
        request,
        stateengine_profile()?,
    )
}

/// Explicit v2 uses its own binding and durable page context. No fallback.
pub fn http_mailbox_v2(
    port: NonZeroU16,
    capability: Arc<Secret<String>>,
    inbox: Arc<Mutex<Inbox>>,
    expected: RemoteMailboxScope,
    client: MailTransportClientInstanceId,
    clock: Arc<dyn grapt_types::ClockAuthority + Send + Sync>,
) -> Result<HttpMailbox, RemoteMailboxError> {
    let request = OpenRequest::new(
        expected.projects.target().clone(),
        expected.recipient.clone(),
        ProtocolVersion::new(grapt_mail_transport::REMOTE_SEAT_MAIL_TRANSPORT_V2)
            .ok_or(RemoteMailboxError::ForeignBinding)?,
        client,
    );
    RemoteMailboxRuntime::new(
        HttpMailTransport::with_protocol(
            port,
            capability,
            grapt_mail_transport::MailTransportVersion::V2,
        )
        .map_err(RemoteMailboxError::Port)?,
        inbox,
        expected,
        request,
        stateengine_profile()?,
    )?
    .with_progress_clock(clock)
}
