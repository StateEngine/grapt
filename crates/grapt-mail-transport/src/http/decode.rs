use super::*;
use crate::binding::{MailBindingReceipt, MailBindingReceiptParts};
use crate::limits::ProviderLimitsParts;
use grapt_types::{MailboxBody, MailboxError, SeatRecipientId};

pub(super) fn exact(v: &Value, fields: &[&str]) -> Result<(), OperationFailure> {
    let object = v.as_object().ok_or_else(|| malformed(false))?;
    if object.len() != fields.len() || fields.iter().any(|key| !object.contains_key(*key)) {
        return Err(malformed(false));
    }
    Ok(())
}
pub(super) fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, OperationFailure> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| malformed(false))
}
pub(super) fn integer(v: &Value, key: &str) -> Result<u64, OperationFailure> {
    v.get(key)
        .and_then(Value::as_u64)
        .filter(|n| *n > 0 && *n <= 9_007_199_254_740_991)
        .ok_or_else(|| malformed(false))
}
pub(super) fn pair(v: &Value) -> Result<ProjectPair, OperationFailure> {
    exact(v, &["source", "target"])?;
    Ok(ProjectPair::new(
        SourceProjectId::new(text(v, "source")?).ok_or_else(|| malformed(false))?,
        TargetProjectId::new(text(v, "target")?).ok_or_else(|| malformed(false))?,
    ))
}
pub(super) fn pair_json(p: &ProjectPair) -> Value {
    json!({"source":p.source().as_str(),"target":p.target().as_str()})
}
pub(super) fn profile(v: &Value) -> Result<ProviderLimits, OperationFailure> {
    macro_rules! number {
        ($key:ident,$ty:ty) => {{
            let n = v
                .get(stringify!($key))
                .and_then(Value::as_u64)
                .and_then(|n| <$ty>::try_from(n).ok())
                .and_then(|n| core::num::NonZero::new(n));
            n.ok_or_else(|| {
                OperationFailure::from(OpenFailure::LimitProfileIncomplete {
                    field: stringify!($key),
                })
            })?
        }};
    }
    let p = ProviderLimitsParts {
        max_refs_per_message: number!(max_refs_per_message, u32),
        max_ref_utf8_bytes: number!(max_ref_utf8_bytes, usize),
        max_aggregate_ref_bytes_per_message: number!(max_aggregate_ref_bytes_per_message, usize),
        max_decoded_message_bytes_including_metadata: number!(
            max_decoded_message_bytes_including_metadata,
            usize
        ),
        max_decoded_page_bytes_including_metadata: number!(
            max_decoded_page_bytes_including_metadata,
            usize
        ),
        max_cursor_bytes: number!(max_cursor_bytes, usize),
        max_snapshot_token_bytes: number!(max_snapshot_token_bytes, usize),
        max_pages_per_runtime_slice: number!(max_pages_per_runtime_slice, u32),
        max_messages_per_runtime_slice: number!(max_messages_per_runtime_slice, u32),
        max_decoded_bytes_per_runtime_slice: number!(max_decoded_bytes_per_runtime_slice, usize),
        max_ack_debt_items_per_recovery_page: number!(max_ack_debt_items_per_recovery_page, u32),
        max_connect_attempts_per_activation: number!(max_connect_attempts_per_activation, u32),
        max_reconnect_attempts_per_runtime_slice: number!(
            max_reconnect_attempts_per_runtime_slice,
            u32
        ),
        max_connect_duration_ticks: number!(max_connect_duration_ticks, u32),
        max_single_reconnect_delay_ticks: number!(max_single_reconnect_delay_ticks, u32),
        max_total_reconnect_delay_ticks_per_slice: number!(
            max_total_reconnect_delay_ticks_per_slice,
            u32
        ),
        max_fetch_duration_ticks: number!(max_fetch_duration_ticks, u32),
        max_ack_duration_ticks: number!(max_ack_duration_ticks, u32),
        max_shutdown_duration_ticks: number!(max_shutdown_duration_ticks, u32),
        max_joined_transport_tasks_at_shutdown: number!(
            max_joined_transport_tasks_at_shutdown,
            u32
        ),
        max_diagnostic_bytes: number!(max_diagnostic_bytes, usize),
        max_body_utf8_bytes: number!(max_body_utf8_bytes, usize),
        max_messages_per_page: number!(max_messages_per_page, u32),
        max_aggregate_body_bytes_per_page: number!(max_aggregate_body_bytes_per_page, usize),
    };
    if v.as_object().map(|o| o.len()) != Some(24) {
        return Err(malformed(false));
    }
    Ok(ProviderLimits::new(p))
}
pub(super) fn profile_json(p: ProviderLimits) -> Value {
    json!({
        "max_refs_per_message":p.max_refs_per_message().get(),
        "max_ref_utf8_bytes":p.max_ref_utf8_bytes().get(),
        "max_aggregate_ref_bytes_per_message":p.max_aggregate_ref_bytes_per_message().get(),
        "max_decoded_message_bytes_including_metadata":p.max_decoded_message_bytes_including_metadata().get(),
        "max_decoded_page_bytes_including_metadata":p.max_decoded_page_bytes_including_metadata().get(),
        "max_cursor_bytes":p.max_cursor_bytes().get(),
        "max_snapshot_token_bytes":p.max_snapshot_token_bytes().get(),
        "max_pages_per_runtime_slice":p.max_pages_per_runtime_slice().get(),
        "max_messages_per_runtime_slice":p.max_messages_per_runtime_slice().get(),
        "max_decoded_bytes_per_runtime_slice":p.max_decoded_bytes_per_runtime_slice().get(),
        "max_ack_debt_items_per_recovery_page":p.max_ack_debt_items_per_recovery_page().get(),
        "max_connect_attempts_per_activation":p.max_connect_attempts_per_activation().get(),
        "max_reconnect_attempts_per_runtime_slice":p.max_reconnect_attempts_per_runtime_slice().get(),
        "max_connect_duration_ticks":p.max_connect_duration_ticks().get(),
        "max_single_reconnect_delay_ticks":p.max_single_reconnect_delay_ticks().get(),
        "max_total_reconnect_delay_ticks_per_slice":p.max_total_reconnect_delay_ticks_per_slice().get(),
        "max_fetch_duration_ticks":p.max_fetch_duration_ticks().get(),
        "max_ack_duration_ticks":p.max_ack_duration_ticks().get(),
        "max_shutdown_duration_ticks":p.max_shutdown_duration_ticks().get(),
        "max_joined_transport_tasks_at_shutdown":p.max_joined_transport_tasks_at_shutdown().get(),
        "max_diagnostic_bytes":p.max_diagnostic_bytes().get(),
        "max_body_utf8_bytes":p.max_body_utf8_bytes().get(),
        "max_messages_per_page":p.max_messages_per_page().get(),
        "max_aggregate_body_bytes_per_page":p.max_aggregate_body_bytes_per_page().get(),
    })
}
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn binding(
    v: &Value,
    request: &OpenRequest,
    offered: ProviderLimits,
) -> Result<MailBinding, OperationFailure> {
    exact(
        v,
        &[
            "binding_id",
            "protocol",
            "adapter_version",
            "projects",
            "verified_principal",
            "local_recipient",
            "mapping_version",
            "takeover",
            "limits",
            "capabilities",
        ],
    )?;
    let protocol = MailTransportVersion::parse(request.requested_protocol().as_str())
        .ok_or(OpenFailure::ProtocolVersionUnsupported)?;
    if text(v, "protocol")? != protocol.as_str() {
        return Err(OpenFailure::ProtocolVersionUnsupported.into());
    }
    if text(v, "verified_principal")? != request.expected_recipient().as_str() {
        return Err(OpenFailure::ExactPrincipalMismatch.into());
    } // NMAIL:exact-principal
    if text(v, "local_recipient")? != request.expected_recipient().as_str() {
        return Err(OpenFailure::RecipientMismatch.into());
    }
    let projects = pair(&v["projects"])?;
    if projects.target() != request.expected_local_project() {
        return Err(OpenFailure::ForeignProjectMismatch.into());
    }
    if text(v, "adapter_version")? != protocol.adapter() {
        return Err(OpenFailure::RequiredCapabilityMissing {
            field: "adapter_version",
        }
        .into());
    }
    if text(v, "mapping_version")? != "exact-seat-id/v1" {
        return Err(OpenFailure::MappingVersionMismatch.into());
    }
    let capabilities = &v["capabilities"];
    exact(
        capabilities,
        &[
            "atomic_subscribe_and_snapshot",
            "durable_delivery_ack",
            "bulk_dismiss",
            "explicit_project_pair",
            "opaque_authenticated_cursors",
            "advisory_payload_bytes",
            "coalesced_pending_signals",
        ],
    )?;
    for field in [
        "atomic_subscribe_and_snapshot",
        "durable_delivery_ack",
        "bulk_dismiss",
        "explicit_project_pair",
        "opaque_authenticated_cursors",
    ] {
        if capabilities.get(field) != Some(&Value::Bool(true)) {
            return Err(OpenFailure::RequiredCapabilityMissing { field }.into());
        }
    }
    if capabilities["advisory_payload_bytes"] != json!(0)
        || capabilities["coalesced_pending_signals"] != json!(1)
    {
        return Err(OpenFailure::RequiredCapabilityMissing {
            field: "advisory-bounds",
        }
        .into());
    }
    let settled = profile(&v["limits"])?;
    // The offer sent to open is exactly the provider profile, settled at our four ceilings.
    if profile_json(settled) != settled_profile_json(offered) {
        return Err(OpenFailure::LimitProfileIncomplete {
            field: "profile-mismatch",
        }
        .into());
    }
    let takeover = &v["takeover"];
    exact(takeover, &["generation", "displaced"])?;
    let generation =
        BindingGeneration::new(integer(takeover, "generation")?).ok_or_else(|| malformed(false))?;
    let receipt = if takeover["displaced"].is_null() {
        TakeoverReceipt::first_open(generation)
    } else {
        let d = &takeover["displaced"];
        exact(d, &["principal", "client", "generation"])?;
        let old =
            BindingGeneration::new(integer(d, "generation")?).ok_or_else(|| malformed(false))?;
        if old >= generation {
            return Err(malformed(false));
        }
        TakeoverReceipt::took_over(
            generation,
            DisplacedOwner::new(
                SourcePrincipalId::new(text(d, "principal")?).ok_or_else(|| malformed(false))?,
                MailTransportClientInstanceId::new(text(d, "client")?)
                    .ok_or_else(|| malformed(false))?,
                old,
            ),
        )
    };
    Ok(MailBinding::new(MailBindingReceipt::new(
        MailBindingReceiptParts {
            binding_id: SourceBindingId::new(text(v, "binding_id")?)
                .ok_or_else(|| malformed(false))?,
            protocol: ProtocolVersion::new(text(v, "protocol")?).ok_or_else(|| malformed(false))?,
            adapter_version: AdapterVersion::new(text(v, "adapter_version")?)
                .ok_or_else(|| malformed(false))?,
            projects,
            verified_principal: SourcePrincipalId::new(text(v, "verified_principal")?)
                .ok_or_else(|| malformed(false))?,
            local_recipient: SeatRecipientId::new(text(v, "local_recipient")?)
                .ok_or_else(|| malformed(false))?,
            mapping_version: RecipientMappingVersion::new(text(v, "mapping_version")?)
                .ok_or_else(|| malformed(false))?,
            takeover: receipt,
            limits: settled.settle(),
        },
    )))
}
fn settled_profile_json(p: ProviderLimits) -> Value {
    let limits = p.settle();
    let mut value = profile_json(p);
    value["max_body_utf8_bytes"] = json!(limits.body_utf8_bytes());
    value["max_messages_per_page"] = json!(limits.messages_per_page());
    value["max_aggregate_body_bytes_per_page"] = json!(limits.aggregate_body_bytes_per_page());
    value["max_diagnostic_bytes"] = json!(limits.diagnostic_bytes());
    value
}
pub(super) fn snapshot(
    v: &Value,
    limits: &SettledTransportLimits,
) -> Result<MailSnapshot, OperationFailure> {
    exact(
        v,
        &["snapshot_id", "barrier", "start_after", "retained_origin"],
    )?;
    for key in ["barrier", "start_after"] {
        if v[key]
            .as_str()
            .is_some_and(|s| s.len() > limits.provider().max_cursor_bytes().get())
        {
            return Err(TransportFailure::CursorBytesExceeded.into());
        }
    }
    if text(v, "snapshot_id")?.len() > limits.provider().max_snapshot_token_bytes().get() {
        return Err(malformed(false));
    }
    Ok(MailSnapshot::new(
        MailSnapshotId::new(text(v, "snapshot_id")?).ok_or_else(|| malformed(false))?,
        SnapshotBarrier::new(text(v, "barrier")?).ok_or_else(|| malformed(false))?,
        nullable_cursor(&v["start_after"])?,
        SafeReplayOrigin::new(text(v, "retained_origin")?).ok_or_else(|| malformed(false))?,
    ))
}
pub(super) fn snapshot_json(s: &MailSnapshot) -> Value {
    json!({"snapshot_id":s.snapshot_id().as_str(),"barrier":s.barrier().as_str(),"start_after":s.start_after().map(ExclusiveSourceCursor::as_str),"retained_origin":s.retained_origin().as_str()})
}
pub(super) fn nullable_cursor(
    v: &Value,
) -> Result<Option<ExclusiveSourceCursor>, OperationFailure> {
    if v.is_null() {
        Ok(None)
    } else {
        Ok(Some(
            ExclusiveSourceCursor::new(v.as_str().ok_or_else(|| malformed(false))?)
                .ok_or_else(|| malformed(false))?,
        ))
    }
}
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn message(
    v: &Value,
    b: &MailBinding,
) -> Result<BoundRemoteSeatMailMessageV1, OperationFailure> {
    exact(
        v,
        &[
            "protocol",
            "source_binding",
            "projects",
            "source_message",
            "source_cursor",
            "sender",
            "recipient",
            "kind",
            "body",
            "refs",
            "supersession",
            "archive",
            "source_created",
            "attention",
        ],
    )?;
    let l = b.receipt().limits();
    if serde_json::to_vec(v).map_err(|_| malformed(false))?.len()
        > l.provider()
            .max_decoded_message_bytes_including_metadata()
            .get()
    {
        return Err(TransportFailure::PageDecodedBytesExceeded.into());
    }
    if text(v, "source_cursor")?.len() > l.provider().max_cursor_bytes().get() {
        return Err(TransportFailure::CursorBytesExceeded.into());
    }
    if text(v, "protocol")? != REMOTE_SEAT_MAIL_TRANSPORT_V1
        || text(v, "source_binding")? != b.receipt().binding_id().as_str()
        || text(v, "recipient")? != b.receipt().local_recipient().as_str()
    {
        return Err(OpenFailure::ExactPrincipalMismatch.into());
    }
    let projects = pair(&v["projects"])?;
    if projects.target() != b.receipt().projects().target() {
        return Err(OpenFailure::ForeignProjectMismatch.into());
    }
    let refs = v["refs"]
        .as_array()
        .ok_or_else(|| malformed(false))?
        .iter()
        .map(|r| {
            BoundedMailRef::try_new(
                r.as_str().ok_or_else(|| malformed(false))?,
                l.provider().max_ref_utf8_bytes().get(),
            )
            .map_err(|_| OperationFailure::from(TransportFailure::ReferenceBytesExceeded))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let refs = BoundedOrderedMailRefs::try_new(
        refs,
        l.provider().max_refs_per_message().get(),
        l.provider().max_aggregate_ref_bytes_per_message().get(),
    )
    .map_err(|_| OperationFailure::from(TransportFailure::ReferenceCountExceeded))?;
    let id =
        RemoteMailboxMessageId::new(text(v, "source_message")?).ok_or_else(|| malformed(false))?;
    let s = &v["supersession"];
    exact(s, &["supersedes", "superseded_by"])?;
    let nullable_id = |v: &Value| -> Result<Option<RemoteMailboxMessageId>, OperationFailure> {
        if v.is_null() {
            Ok(None)
        } else {
            Ok(Some(
                RemoteMailboxMessageId::new(v.as_str().ok_or_else(|| malformed(false))?)
                    .ok_or_else(|| malformed(false))?,
            ))
        }
    };
    let supersession = SourceSupersession::try_new(
        &id,
        nullable_id(&s["supersedes"])?,
        nullable_id(&s["superseded_by"])?,
    )
    .map_err(|_| OperationFailure::from(TransportFailure::SupersessionConflict))?;
    let a = &v["archive"];
    exact(a, &["disposition", "reason"])?;
    let archive = match text(a, "disposition")? {
        "active" if a["reason"].is_null() => SourceArchiveDisposition::Active,
        "archived" => SourceArchiveDisposition::Archived(
            SourceArchiveReason::parse(text(a, "reason")?)
                .ok_or_else(|| OperationFailure::from(TransportFailure::UnknownArchiveReason))?,
        ),
        _ => return Err(malformed(false)),
    };
    let attention = MailAttention::parse(text(v, "attention")?).ok_or_else(|| malformed(false))?; // NMAIL:attention-required
                                                                                                  // Decode every coordinate and body bound before a row-local failure.
                                                                                                  // Only an inadmissible kind label is row-local; malformed/foreign rows still
                                                                                                  // refuse the page and never supply a usable progress cursor.
    let protocol = ProtocolVersion::new(text(v, "protocol")?).ok_or_else(|| malformed(false))?;
    let source_binding =
        SourceBindingId::new(text(v, "source_binding")?).ok_or_else(|| malformed(false))?;
    let source_cursor =
        ExclusiveSourceCursor::new(text(v, "source_cursor")?).ok_or_else(|| malformed(false))?;
    let sender = SourcePrincipalId::new(text(v, "sender")?).ok_or_else(|| malformed(false))?;
    let recipient = SeatRecipientId::new(text(v, "recipient")?).ok_or_else(|| malformed(false))?;
    // An empty body from the floor is still mail: admitted so it can be shown
    // and acknowledged instead of skipped behind a count.
    let body = MailboxBody::received(text(v, "body")?);
    if matches!(body, Err(MailboxError::BodyTooLarge { .. }))
        || body
            .as_ref()
            .is_ok_and(|body| body.len_bytes() > l.body_utf8_bytes())
    {
        return Err(TransportFailure::MessageBodyExceeded.into());
    }
    let source_created =
        SourceTimestamp::new(text(v, "source_created")?).ok_or_else(|| malformed(false))?;
    let kind = RemoteSeatMailKind::parse(text(v, "kind")?)
        .ok_or_else(|| OperationFailure::from(TransportFailure::UnknownKind))?;
    let body = body.map_err(|_| OperationFailure::from(TransportFailure::MessageBodyExceeded))?;
    BoundRemoteSeatMailMessageV1::try_new(
        BoundRemoteSeatMailMessageParts {
            protocol,
            source_binding,
            projects,
            source_message: id,
            source_cursor,
            sender,
            recipient,
            kind,
            body,
            refs,
            supersession,
            archive,
            source_created,
            attention,
        },
        l,
    )
    .map_err(|_| OperationFailure::from(TransportFailure::MessageBodyExceeded))
}

/// Reject duplicate decoded object keys, including equivalent escape spellings.
/// serde_json validates structure/depth; this bounded lexical pass preserves
/// information Value would discard before strict field checks.
pub(super) fn strict_value(data: &[u8]) -> Result<Value, OperationFailure> {
    let value: Value = serde_json::from_slice(data).map_err(|_| malformed(false))?;
    let mut stack: Vec<Option<(bool, std::collections::BTreeSet<String>)>> = Vec::new();
    let mut index = 0;
    while index < data.len() {
        match data[index] {
            b'{' => stack.push(Some((true, std::collections::BTreeSet::new()))),
            b'[' => stack.push(None),
            b'}' | b']' => {
                stack.pop();
            }
            b':' => {
                if let Some(Some((key, _))) = stack.last_mut() {
                    *key = false;
                }
            }
            b',' => {
                if let Some(Some((key, _))) = stack.last_mut() {
                    *key = true;
                }
            }
            b'"' => {
                let begin = index;
                index += 1;
                while index < data.len() {
                    if data[index] == b'\\' {
                        index += 2;
                        continue;
                    }
                    if data[index] == b'"' {
                        break;
                    }
                    index += 1;
                }
                if let Some(Some((true, keys))) = stack.last_mut() {
                    let key: String = serde_json::from_slice(&data[begin..=index])
                        .map_err(|_| malformed(false))?;
                    if !keys.insert(key) {
                        return Err(malformed(false));
                    }
                }
            }
            _ => {}
        }
        index += 1;
    }
    Ok(value)
}
