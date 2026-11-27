use super::*;
use crate::operation::{PartialDismiss, SourceState};

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn decode(v: &Value, write: bool) -> Result<OperationFailure, OperationFailure> {
    let fields = v.as_object().ok_or_else(|| malformed(write))?;
    if fields.keys().any(|k| {
        !matches!(
            k.as_str(),
            "family"
                | "tag"
                | "field"
                | "reason"
                | "source_state"
                | "effect"
                | "recovery"
                | "by_principal"
                | "by_client"
                | "tasks_remaining"
                | "dismissed"
                | "already_dismissed"
                | "completed"
        )
    }) {
        return Err(malformed(write));
    }
    for key in ["field", "reason"] {
        if let Some(value) = v.get(key) {
            if !value
                .as_str()
                .is_some_and(|s| !s.is_empty() && s.len() <= 512)
            {
                return Err(malformed(write));
            }
        }
    }
    if v.get("completed")
        .is_some_and(|n| !n.as_u64().is_some_and(|n| n <= 64))
    {
        return Err(malformed(write));
    }
    let tag = decode::text(v, "tag")?;
    let field = || known_field(decode::text(v, "field")?).ok_or_else(|| malformed(write));
    let cause = match decode::text(v, "family")? {
        "OpenFailure" => OperationCause::Open(match tag {
            // Only this named reason is transient; every other one is permanent.
            "Unauthenticated"
                if v.get("reason").and_then(Value::as_str)
                    == Some("capability-floor-changed-retry") =>
            {
                OpenFailure::UnauthenticatedRetry
            }
            "Unauthenticated" => OpenFailure::Unauthenticated,
            "ForeignProjectMismatch" => OpenFailure::ForeignProjectMismatch,
            "ExactPrincipalMismatch" => OpenFailure::ExactPrincipalMismatch,
            "RecipientMappingMissing" => OpenFailure::RecipientMappingMissing,
            "RecipientMismatch" => OpenFailure::RecipientMismatch,
            "MappingVersionMismatch" => OpenFailure::MappingVersionMismatch,
            "ProtocolVersionUnsupported" => OpenFailure::ProtocolVersionUnsupported,
            "RequiredCapabilityMissing" => {
                OpenFailure::RequiredCapabilityMissing { field: field()? }
            }
            "LimitProfileIncomplete" => OpenFailure::LimitProfileIncomplete { field: field()? },
            _ => return Err(malformed(write)),
        }),
        "TransportFailure" => OperationCause::Transport(match tag {
            "TransportUnavailable" => TransportFailure::TransportUnavailable,
            "Disconnected" => TransportFailure::Disconnected,
            "SubscriptionRefused" => TransportFailure::SubscriptionRefused,
            "SnapshotExpired" => TransportFailure::SnapshotExpired,
            "CursorRegressed" => TransportFailure::CursorRegressed,
            "PageCountExceeded" => TransportFailure::PageCountExceeded,
            "PageBodyBytesExceeded" => TransportFailure::PageBodyBytesExceeded,
            "PageDecodedBytesExceeded" => TransportFailure::PageDecodedBytesExceeded,
            "MessageBodyExceeded" => TransportFailure::MessageBodyExceeded,
            "ReferenceCountExceeded" => TransportFailure::ReferenceCountExceeded,
            "ReferenceBytesExceeded" => TransportFailure::ReferenceBytesExceeded,
            "CursorBytesExceeded" => TransportFailure::CursorBytesExceeded,
            "MalformedEnvelope" => TransportFailure::MalformedEnvelope,
            "UnknownRequiredField" => TransportFailure::UnknownRequiredField { field: field()? },
            "UnknownKind" => TransportFailure::UnknownKind,
            "UnknownArchiveReason" => TransportFailure::UnknownArchiveReason,
            "SupersessionConflict" => TransportFailure::SupersessionConflict,
            "ProjectProvenanceMissing" => TransportFailure::ProjectProvenanceMissing,
            "CursorInvalid" => {
                let reason = CursorInvalidReason::parse(decode::text(v, "reason")?)
                    .ok_or_else(|| malformed(write))?;
                let recovery = &v["recovery"];
                let recovery = match decode::text(recovery, "tag")? {
                    "ReplayFrom" => {
                        decode::exact(recovery, &["tag", "origin"])?;
                        CursorRecovery::ReplayFrom(
                            SafeReplayOrigin::new(decode::text(recovery, "origin")?)
                                .ok_or_else(|| malformed(write))?,
                        )
                    }
                    "RecoveryUnavailable" => {
                        decode::exact(recovery, &["tag", "detail"])?;
                        if decode::text(recovery, "detail")? != "source-file-replaced" {
                            return Err(malformed(write));
                        }
                        CursorRecovery::RecoveryUnavailable {
                            detail: "source-file-replaced",
                        }
                    }
                    _ => return Err(malformed(write)),
                };
                TransportFailure::CursorInvalid(CursorInvalidation::new(reason, recovery))
            }
            _ => return Err(malformed(write)),
        }),
        "DeliveryAckFailure" => OperationCause::Delivery(match tag {
            "MessageNeverEnumerated" => DeliveryAckFailure::MessageNeverEnumerated,
            "AcknowledgementCoordinateMismatch" => {
                DeliveryAckFailure::AcknowledgementCoordinateMismatch
            }
            "AcknowledgementDigestMismatch" => DeliveryAckFailure::AcknowledgementDigestMismatch,
            "AcknowledgementConflict" => DeliveryAckFailure::AcknowledgementConflict,
            "DeliveryAckUnsupported" => DeliveryAckFailure::DeliveryAckUnsupported,
            "ReadAckSubstitutionRefused" => DeliveryAckFailure::ReadAckSubstitutionRefused,
            _ => return Err(malformed(write)),
        }),
        "SubscriptionClosed" => OperationCause::Subscription(match tag {
            "Disconnected" => SubscriptionClosed::Disconnected,
            "LocalShutdown" => SubscriptionClosed::LocalShutdown,
            "DisplacedByTakeover" => SubscriptionClosed::DisplacedByTakeover {
                by_principal: SourcePrincipalId::new(decode::text(v, "by_principal")?)
                    .ok_or_else(|| malformed(write))?,
                by_client: MailTransportClientInstanceId::new(decode::text(v, "by_client")?)
                    .ok_or_else(|| malformed(write))?,
            },
            _ => return Err(malformed(write)),
        }),
        "LifecycleOutcome" if tag == "ShutdownBudgetExhausted" => {
            OperationCause::Lifecycle(LifecycleOutcome::ShutdownBudgetExhausted {
                tasks_remaining: u32::try_from(decode::integer(v, "tasks_remaining")?)
                    .map_err(|_| malformed(write))?,
            })
        }
        _ => return Err(malformed(write)),
    };
    let effect = match v.get("effect").and_then(Value::as_str) {
        Some("none") => WriteEffect::None,
        Some("unknown-retry-exact") => WriteEffect::UnknownRetryExact,
        None if !v.as_object().is_some_and(|o| o.contains_key("effect")) => {
            if write && matches!(cause, OperationCause::Transport(_)) {
                WriteEffect::UnknownRetryExact
            } else {
                WriteEffect::None
            }
        }
        _ => return Err(malformed(write)),
    };
    let mut failure = OperationFailure::new(cause, effect);
    failure.source_state = match v.get("source_state") {
        None => None,
        Some(Value::String(s)) => Some(match s.as_str() {
            "absent" => SourceState::Absent,
            "incomplete" => SourceState::Incomplete,
            "unreadable" => SourceState::Unreadable,
            _ => return Err(malformed(write)),
        }),
        _ => return Err(malformed(write)),
    };
    if v.get("dismissed").is_some() || v.get("already_dismissed").is_some() {
        failure.partial_dismiss = Some(PartialDismiss {
            dismissed: ids(&v["dismissed"])?,
            already_dismissed: ids(&v["already_dismissed"])?,
        });
    }
    Ok(failure)
}
pub(super) fn ids(v: &Value) -> Result<Vec<RemoteMailboxMessageId>, OperationFailure> {
    let array = v
        .as_array()
        .filter(|a| a.len() <= 64)
        .ok_or_else(|| malformed(true))?;
    array
        .iter()
        .map(|v| {
            RemoteMailboxMessageId::new(v.as_str().ok_or_else(|| malformed(true))?)
                .ok_or_else(|| malformed(true))
        })
        .collect()
}

pub(super) fn known_field(raw: &str) -> Option<&'static str> {
    const FIELDS: &[&str] = &[
        "binding-capacity",
        "binding-generation",
        "operation",
        "unrecognized-field",
        "adapter_version",
        "advisory-bounds",
        "atomic_subscribe_and_snapshot",
        "durable_delivery_ack",
        "bulk_dismiss",
        "explicit_project_pair",
        "opaque_authenticated_cursors",
        "max_refs_per_message",
        "max_ref_utf8_bytes",
        "max_aggregate_ref_bytes_per_message",
        "max_decoded_message_bytes_including_metadata",
        "max_decoded_page_bytes_including_metadata",
        "max_cursor_bytes",
        "max_snapshot_token_bytes",
        "max_pages_per_runtime_slice",
        "max_messages_per_runtime_slice",
        "max_decoded_bytes_per_runtime_slice",
        "max_ack_debt_items_per_recovery_page",
        "max_connect_attempts_per_activation",
        "max_reconnect_attempts_per_runtime_slice",
        "max_connect_duration_ticks",
        "max_single_reconnect_delay_ticks",
        "max_total_reconnect_delay_ticks_per_slice",
        "max_fetch_duration_ticks",
        "max_ack_duration_ticks",
        "max_shutdown_duration_ticks",
        "max_joined_transport_tasks_at_shutdown",
        "max_diagnostic_bytes",
        "max_body_utf8_bytes",
        "max_messages_per_page",
        "max_aggregate_body_bytes_per_page",
    ];
    FIELDS.iter().copied().find(|field| *field == raw)
}
