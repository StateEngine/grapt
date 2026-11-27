//! Disposable real-port witness. One bounded private stdin packet carries the
//! capability; output contains only source/local mail facts, never credentials.
use grapt_inbox::Inbox;
use grapt_mail_transport::http::HttpMailTransport;
use grapt_mail_transport::limits::ProviderLimitsParts;
use grapt_mail_transport::{OpenRequest, ProviderLimits};
use grapt_runtime::mailbox::remote::RemoteMailboxRuntime;
use grapt_store::{EventStore, RemoteMailboxScope};
use grapt_types::remote_mail::*;
use grapt_types::{MailboxCursor, ProtocolVersion, SeatRecipientId, Secret};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::num::{NonZeroU16, NonZeroU32, NonZeroUsize};
use std::path::Path;
use std::sync::{Arc, Mutex};
type Error = Box<dyn std::error::Error>;
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, Error> {
    v[key]
        .as_str()
        .ok_or_else(|| format!("missing {key}").into())
}
fn n32(v: &Value, key: &str) -> Result<NonZeroU32, Error> {
    NonZeroU32::new(u32::try_from(
        v[key].as_u64().ok_or("invalid profile field")?,
    )?)
    .ok_or_else(|| "zero profile field".into())
}
fn ns(v: &Value, key: &str) -> Result<NonZeroUsize, Error> {
    NonZeroUsize::new(usize::try_from(
        v[key].as_u64().ok_or("invalid profile field")?,
    )?)
    .ok_or_else(|| "zero profile field".into())
}
fn profile(v: &Value) -> Result<ProviderLimits, Error> {
    Ok(ProviderLimits::new(ProviderLimitsParts {
        max_refs_per_message: n32(v, "max_refs_per_message")?,
        max_ref_utf8_bytes: ns(v, "max_ref_utf8_bytes")?,
        max_aggregate_ref_bytes_per_message: ns(v, "max_aggregate_ref_bytes_per_message")?,
        max_decoded_message_bytes_including_metadata: ns(
            v,
            "max_decoded_message_bytes_including_metadata",
        )?,
        max_decoded_page_bytes_including_metadata: ns(
            v,
            "max_decoded_page_bytes_including_metadata",
        )?,
        max_cursor_bytes: ns(v, "max_cursor_bytes")?,
        max_snapshot_token_bytes: ns(v, "max_snapshot_token_bytes")?,
        max_pages_per_runtime_slice: n32(v, "max_pages_per_runtime_slice")?,
        max_messages_per_runtime_slice: n32(v, "max_messages_per_runtime_slice")?,
        max_decoded_bytes_per_runtime_slice: ns(v, "max_decoded_bytes_per_runtime_slice")?,
        max_ack_debt_items_per_recovery_page: n32(v, "max_ack_debt_items_per_recovery_page")?,
        max_connect_attempts_per_activation: n32(v, "max_connect_attempts_per_activation")?,
        max_reconnect_attempts_per_runtime_slice: n32(
            v,
            "max_reconnect_attempts_per_runtime_slice",
        )?,
        max_connect_duration_ticks: n32(v, "max_connect_duration_ticks")?,
        max_single_reconnect_delay_ticks: n32(v, "max_single_reconnect_delay_ticks")?,
        max_total_reconnect_delay_ticks_per_slice: n32(
            v,
            "max_total_reconnect_delay_ticks_per_slice",
        )?,
        max_fetch_duration_ticks: n32(v, "max_fetch_duration_ticks")?,
        max_ack_duration_ticks: n32(v, "max_ack_duration_ticks")?,
        max_shutdown_duration_ticks: n32(v, "max_shutdown_duration_ticks")?,
        max_joined_transport_tasks_at_shutdown: n32(v, "max_joined_transport_tasks_at_shutdown")?,
        max_diagnostic_bytes: ns(v, "max_diagnostic_bytes")?,
        max_body_utf8_bytes: ns(v, "max_body_utf8_bytes")?,
        max_messages_per_page: n32(v, "max_messages_per_page")?,
        max_aggregate_body_bytes_per_page: ns(v, "max_aggregate_body_bytes_per_page")?,
    }))
}
fn read_receipt(
    store: &EventStore,
    expected: &RemoteMailboxScope,
    wanted: Option<&[RemoteMailboxMessageId]>,
) -> Result<Value, Error> {
    let mut after = MailboxCursor::START;
    let mut count = 0usize;
    let mut rows = Vec::new();
    // The existing activation has at most 16 slices of 8 source pages. Each
    // read still materializes at most 64 rows/4 MiB; output keeps at most 64.
    let pages = if wanted.is_some() { 128 } else { 1 };
    for _ in 0..pages {
        let page = store.read_remote_mailbox_for_binding(expected, after, 64, 4_194_304)?;
        count = count
            .checked_add(page.records.len())
            .ok_or("witness count overflow")?;
        for row in &page.records {
            if wanted.is_some_and(|ids| !ids.contains(row.ack().source_message())) {
                continue;
            }
            if rows.len() == 64 {
                return Err("witness row bound exceeded".into());
            }
            rows.push(json!({
                "source_message":row.ack().source_message().as_str(),
                "source_project":row.scope().projects.source().as_str(),
                "local_message":row.message().id().as_str(),
                "local_admission":row.ack().local_admission().value(),
                "original_binding":row.ack().source_binding().as_str(),
                "source_created":row.source_created().as_str(), "sender":row.sender().as_str(),
                "body":row.message().body().as_str(), "actionable":row.actionable(),
                "delivery_pending":row.delivery_pending(), "repair_pending":row.repair_pending()
            }));
        }
        match page.next_after {
            Some(next) => after = next,
            None => {
                if wanted.is_some_and(|ids| {
                    ids.iter().any(|id| {
                        !rows
                            .iter()
                            .any(|row| row["source_message"].as_str() == Some(id.as_str()))
                    })
                }) {
                    return Err("required witness message is absent".into());
                }
                return Ok(json!({"live":true,"rows":rows,"admitted_count":count}));
            }
        }
    }
    Err("witness read page bound exceeded".into())
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    let mut packet = Vec::new();
    std::io::stdin().take(8193).read_to_end(&mut packet)?;
    if packet.len() > 8192 {
        return Err("private witness packet exceeds bound".into());
    }
    let v: Value = serde_json::from_slice(&packet)?;
    let wanted = v
        .get("witness_messages")
        .map(|value| -> Result<Vec<RemoteMailboxMessageId>, Error> {
            let ids = value
                .as_array()
                .filter(|ids| !ids.is_empty() && ids.len() <= 8)
                .ok_or("witness selector bound")?;
            ids.iter()
                .map(|id| {
                    Ok(RemoteMailboxMessageId::try_new(
                        id.as_str().ok_or("witness id")?,
                    )?)
                })
                .collect()
        })
        .transpose()?;
    let port =
        NonZeroU16::new(u16::try_from(v["port"].as_u64().ok_or("port")?)?).ok_or("zero port")?;
    let capability = Arc::new(Secret::new(text(&v, "capability")?.to_owned()));
    let expected = RemoteMailboxScope {
        projects: ProjectPair::new(
            SourceProjectId::try_new(text(&v, "source_project")?)?,
            TargetProjectId::try_new(text(&v, "target_project")?)?,
        ),
        principal: SourcePrincipalId::try_new(text(&v, "recipient")?)?,
        recipient: SeatRecipientId::try_new(text(&v, "recipient")?)?,
    };
    let limits = profile(&v["limits"])?;
    let inbox = Arc::new(Mutex::new(Inbox::new(EventStore::open(
        Path::new(text(&v, "database")?),
        "0.1.0",
    )?)?));
    let transport_version = match v.get("transport_protocol") {
        None => grapt_mail_transport::MailTransportVersion::V1,
        Some(value) => grapt_mail_transport::MailTransportVersion::parse(
            value.as_str().ok_or("transport protocol")?,
        )
        .ok_or("unsupported transport protocol")?,
    };
    let request = OpenRequest::new(
        expected.projects.target().clone(),
        expected.recipient.clone(),
        ProtocolVersion::new(transport_version.as_str()).ok_or("protocol")?,
        MailTransportClientInstanceId::try_new(text(&v, "client")?)?,
    );
    let mut owner = RemoteMailboxRuntime::new(
        HttpMailTransport::with_protocol(port, capability, transport_version)?,
        inbox.clone(),
        expected.clone(),
        request,
        limits,
    )?;
    if transport_version == grapt_mail_transport::MailTransportVersion::V2 {
        owner = owner.with_progress_clock(Arc::new(grapt_store::SystemClock))?;
    }
    let (_cancel, signal) = tokio::sync::watch::channel(None);
    let result: Result<_, Error> = async {
        let mut live = false;
        for _ in 0..16 {
            let slice = owner.advance_slice(&signal).await?;
            if slice.live {
                live = true;
                break;
            }
        }
        if !live {
            return Err("bounded convergence remains pending".into());
        }
        let held = inbox.lock().map_err(|_| "store poisoned")?;
        let mut receipt = read_receipt(held.store(), &expected, wanted.as_deref())?;
        if transport_version == grapt_mail_transport::MailTransportVersion::V2 {
            receipt["integrity_gaps"] = json!(owner.recorded_integrity_gaps());
            receipt["transport_protocol"] = json!(transport_version.as_str());
        }
        Ok(receipt)
    }
    .await;
    // Always attempt bounded close before returning a measured scan refusal.
    let shutdown = owner.close().await?;
    let mut result = result?;
    result["shutdown"] = json!(shutdown.is_some());
    writeln!(std::io::stdout(), "{result}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_store::RemoteMailboxInput;
    use grapt_types::{MailboxBody, SeatMessageKind};

    #[test]
    fn selected_readout_counts_all_origins_without_unbounded_output_or_missing_evidence(
    ) -> Result<(), Error> {
        let expected = RemoteMailboxScope {
            projects: ProjectPair::new(
                SourceProjectId::try_new("receiving-floor")?,
                TargetProjectId::try_new("secode")?,
            ),
            principal: SourcePrincipalId::try_new("coder")?,
            recipient: SeatRecipientId::try_new("coder")?,
        };
        let mut store = EventStore::open_in_memory()?;
        for i in 0..65 {
            let origin = if i % 2 == 0 {
                "origin-one"
            } else {
                "origin-two"
            };
            let row = RemoteMailboxInput {
                scope: expected.for_origin(SourceProjectId::try_new(origin)?),
                protocol: ProtocolVersion::new(grapt_mail_transport::REMOTE_SEAT_MAIL_TRANSPORT_V1)
                    .ok_or("protocol")?,
                source_message: RemoteMailboxMessageId::try_new(format!("row-{i}"))?,
                source_binding: SourceBindingId::try_new("original")?,
                source_cursor: ExclusiveSourceCursor::try_new(format!("cursor-{i}"))?,
                sender: SourcePrincipalId::try_new("pilot")?,
                kind: SeatMessageKind::Handoff,
                body: MailboxBody::new("bounded body")?,
                source_created: SourceTimestamp::try_new("2026-10-13T00:00:00.000Z")?,
                refs: Vec::new(),
                supersedes: None,
                superseded_by: None,
                archive: SourceArchiveDisposition::Active,
                attention: MailAttention::Wake,
            };
            let ack = store.admit_remote_mailbox(&row.scope, &row)?.ack;
            store.settle_remote_mailbox_delivery(
                &row.scope,
                &ack,
                Some(SecondaryProjectionState::Posted),
            )?;
        }
        assert!(read_receipt(&store, &expected, None).is_err());
        let wanted = [RemoteMailboxMessageId::try_new("row-64")?];
        let receipt = read_receipt(&store, &expected, Some(&wanted))?;
        assert_eq!(receipt["admitted_count"], 65);
        let rows = receipt["rows"].as_array().ok_or("receipt rows")?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["source_message"], "row-64");
        assert_eq!(rows[0]["source_project"], "origin-one");
        assert_eq!(rows[0]["delivery_pending"], false);
        assert!(read_receipt(
            &store,
            &expected,
            Some(&[RemoteMailboxMessageId::try_new("absent")?])
        )
        .is_err());
        Ok(())
    }
}
