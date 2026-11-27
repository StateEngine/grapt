//! Exact v1/v2 page framing shares the same strict healthy-message decoder.
use super::*;

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn decode(
    value: &Value,
    binding: &MailBinding,
    after: Option<&ExclusiveSourceCursor>,
    page: PageLimits,
    protocol: MailTransportVersion,
) -> Result<RemoteMailPage, OperationFailure> {
    decode::exact(
        value,
        match protocol {
            MailTransportVersion::V1 => &["messages", "next_after"],
            MailTransportVersion::V2 => &["messages", "refused_rows", "next_after", "last_cursor"],
        },
    )?;
    if serde_json::to_vec(value)
        .map_err(|_| malformed(false))?
        .len()
        > binding
            .receipt()
            .limits()
            .provider()
            .max_decoded_page_bytes_including_metadata()
            .get()
    {
        return Err(TransportFailure::PageDecodedBytesExceeded.into());
    }
    let rows = value["messages"]
        .as_array()
        .filter(|rows| rows.len() <= page.max_messages() as usize)
        .ok_or(TransportFailure::PageCountExceeded)?;
    let mut messages = Vec::with_capacity(rows.len());
    let mut failures = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    let mut cursors = std::collections::BTreeSet::new();
    let mut body_bytes = 0usize;
    for row in rows {
        match decode::message(row, binding) {
            Ok(message) => messages.push(message),
            Err(failure)
                if matches!(
                    failure.cause,
                    crate::operation::OperationCause::Transport(
                        TransportFailure::UnknownKind | TransportFailure::EmptyBody
                    )
                ) =>
            {
                let cursor = decode::nullable_cursor(&row["source_cursor"])?
                    .ok_or_else(|| malformed(false))?;
                // The record keeps only the cursor, so both causes share it.
                failures.push(
                    crate::MailDecodeFailure::unknown_kind(cursor)
                        .ok_or_else(|| malformed(false))?,
                );
            }
            Err(failure) => return Err(failure),
        }
        // Row-local failures passed the same binding/shape/body-bound validation.
        // Charge and deduplicate every row, including those never admitted.
        body_bytes = body_bytes
            .checked_add(decode::text(row, "body")?.len())
            .ok_or(TransportFailure::PageBodyBytesExceeded)?;
        let cursor =
            decode::nullable_cursor(&row["source_cursor"])?.ok_or_else(|| malformed(false))?;
        if Some(&cursor) == after
            || !ids.insert((
                decode::pair(&row["projects"])?.source().clone(),
                RemoteMailboxMessageId::new(decode::text(row, "source_message")?)
                    .ok_or_else(|| malformed(false))?,
            ))
            || !cursors.insert(cursor)
        {
            return Err(TransportFailure::CursorRegressed.into());
        }
    }
    if body_bytes > page.max_aggregate_body_bytes() {
        return Err(TransportFailure::PageBodyBytesExceeded.into());
    }
    let next = decode::nullable_cursor(&value["next_after"])?;
    if protocol == MailTransportVersion::V1 {
        if next.as_ref().is_some_and(|cursor| {
            rows.last().and_then(|r| r["source_cursor"].as_str()) != Some(cursor.as_str())
        }) {
            return Err(malformed(false));
        }
        return RemoteMailPage::v1_page_with_decode_failures(messages, failures, next)
            .ok_or_else(|| malformed(false));
    }
    let refused = value["refused_rows"]
        .as_array()
        .filter(|rows| rows.len() <= 64)
        .ok_or(TransportFailure::PageCountExceeded)?;
    if rows.len().saturating_add(refused.len()) > page.max_messages() as usize {
        return Err(TransportFailure::PageCountExceeded.into());
    }
    let mut observations = Vec::with_capacity(refused.len());
    for row in refused {
        decode::exact(row, &["msg_id", "start", "end", "reason", "source_cursor"])?;
        let indexed = if row["msg_id"].is_null() {
            None
        } else {
            Some(
                RefusedIndexId::new(decode::text(row, "msg_id")?.to_owned())
                    .map_err(|_| malformed(false))?,
            )
        };
        let start = row["start"]
            .as_u64()
            .filter(|n| *n <= 9_007_199_254_740_991)
            .ok_or_else(|| malformed(false))?;
        let end = decode::integer(row, "end")?;
        let reason = RefusedRowReason::parse(decode::text(row, "reason")?)
            .ok_or_else(|| malformed(false))?;
        // Older providers could emit the same ambiguous family in both arrays.
        // Reject that page; an untrusted index label never admits or ACKs mail.
        if matches!(
            reason,
            RefusedRowReason::DuplicateMessageId | RefusedRowReason::AmbiguousDuplicateAck
        ) && indexed.as_ref().is_some_and(|label| {
            ids.iter()
                .any(|(_, message)| message.as_str() == label.as_str())
        }) {
            return Err(malformed(false));
        }
        let cursor =
            decode::nullable_cursor(&row["source_cursor"])?.ok_or_else(|| malformed(false))?;
        if Some(&cursor) == after || !cursors.insert(cursor.clone()) {
            return Err(TransportFailure::CursorRegressed.into());
        }
        observations.push(
            RefusedMailRow::new(indexed, start, end, reason, cursor)
                .ok_or_else(|| malformed(false))?,
        );
    }
    let last = decode::nullable_cursor(&value["last_cursor"])?;
    RemoteMailPage::v2_page_with_decode_failures(messages, observations, failures, next, last)
        .ok_or_else(|| malformed(false))
}
