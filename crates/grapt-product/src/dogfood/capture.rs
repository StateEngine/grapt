//! The capture application port for the dogfood composition (charter
//! EVENT-STREAM-REPLAY-CURSOR-01, revision 8, obligation B4).
//!
//! The API route (`grapt-api/src/u3a/capture.rs`) owns the wire protocol and
//! the store owns the durable rows; this module is the product-side
//! projection between them, and its law is:
//!
//! - A durable row whose wire payload is fully reconstructable from the
//!   stored row projects to [`CapturePageRow::Event`], rebuilt exactly the
//!   way the live sink rebuilds it (`input_events`, `step_events`,
//!   `execution/observations`, `continuation/compaction`), so replay and
//!   live delivery cannot diverge on the same committed fact.
//! - A durable row whose wire payload depends on non-durable state - the v2
//!   compaction descriptor that retains history by reference, an assistant
//!   opening with no executor-captured selection, a permission ask committed
//!   without replay coordinates - projects to [`CapturePageRow::Skipped`]
//!   carrying the stored kind. The named skip keeps the cursor coordinate
//!   advancing, so replay stays exactly-once and resumable. This extends the
//!   store's `SkippedUnknownKind` law from kinds the reader does not know to
//!   kinds it knows but cannot project; it is not a generic feed framework,
//!   which the charter rejects.
//! - A row whose stored `sessionID` field, when present, disagrees with the
//!   coordinate's aggregate is corruption: the whole page refuses
//!   ([`CapturePageError::CorruptRow`]) and the stream ends in a
//!   `corrupt-row` terminal. A projection that lies about identity is worse
//!   than a refused page. Absence is not disagreement: most durable payload
//!   shapes carry no `sessionID`, and the DTO decode enforces its presence
//!   wherever a payload requires one.
//!
//! Durable kinds this reader does not project, by design: `session.created`
//! and the content, usage, step-terminal and tool rows (their wire payloads
//! are assembled by the history serializer and the executor from non-durable
//! state), and `session.compaction.ended.2` (references-only descriptor).
use grapt_api::u3a::{
    AggregateIdDto, CapturePage, CapturePageError, CapturePageRow, CompactionEndedData,
    CompactionReason, DogfoodEvent, DtoString, DurableEvent, DurableEventRef, ExecutionFailedData,
    ExecutionInterruptedData, ExecutionSucceededData, FiniteF64, InterruptReason, StepStartedData,
    Utf16Text, WireId,
};
use grapt_api::{ApplicationFuture, CaptureApplicationPort, SealedPrincipal};
use grapt_store::{
    CaptureReadError, CaptureRow as StoredCaptureRow, DecodedEvent, MAX_CAPTURE_PAGE_ROWS,
};
use grapt_types::{
    CaptureCoordinate, CaptureCursor, CaptureInterest, EventId, ExecutionId, PermissionReply,
};
use serde_json::Value;

use super::support::{decode, ready};
use super::ServerApplication;

#[cfg(test)]
#[path = "capture_agent_mail_tests.rs"]
mod agent_mail_tests;
#[cfg(test)]
#[path = "capture_permission_tests.rs"]
mod permission_tests;

impl CaptureApplicationPort for ServerApplication {
    fn capture_page(
        &self,
        _principal: SealedPrincipal,
        interest: CaptureInterest,
        cursor: CaptureCursor,
    ) -> ApplicationFuture<CapturePage, CapturePageError> {
        ready((|| {
            let held = self.lock_inbox();
            let page = held
                .store()
                .read_capture_page(&interest, &cursor, MAX_CAPTURE_PAGE_ROWS)
                .map_err(map_read_error)?;
            let rows = page
                .rows
                .into_iter()
                .map(map_row)
                .collect::<Result<Vec<CapturePageRow>, _>>()?;
            Ok(CapturePage {
                rows,
                continuation: page.continuation,
            })
        })())
    }

    fn count_capture_aggregates(
        &self,
        _principal: SealedPrincipal,
        interest: CaptureInterest,
    ) -> ApplicationFuture<u64, CapturePageError> {
        ready((|| {
            let held = self.lock_inbox();
            let width = held
                .store()
                .count_capture_aggregates(&interest)
                .map_err(map_read_error)?;
            Ok(width)
        })())
    }
}

fn map_read_error(error: CaptureReadError) -> CapturePageError {
    match error {
        CaptureReadError::Cursor(cursor) => CapturePageError::Cursor(cursor),
        CaptureReadError::CorruptRow { id, detail } => CapturePageError::CorruptRow {
            id: bounded(&id),
            detail: bounded_detail(&detail),
        },
        CaptureReadError::UnreadableStore { detail } => CapturePageError::UnreadableStore {
            detail: bounded_detail(&detail),
        },
        CaptureReadError::PageLimitZero => CapturePageError::UnreadableStore {
            detail: bounded("the store refused the bounded capture page limit"),
        },
    }
}

fn map_row(row: StoredCaptureRow) -> Result<CapturePageRow, CapturePageError> {
    match row {
        StoredCaptureRow::SkippedUnknownKind { coordinate, kind } => Ok(CapturePageRow::Skipped {
            coordinate,
            kind: bounded_detail(&kind.to_string()),
        }),
        StoredCaptureRow::Event { coordinate, event } => map_event(coordinate, event),
    }
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn map_event(
    coordinate: CaptureCoordinate,
    event: DecodedEvent,
) -> Result<CapturePageRow, CapturePageError> {
    let kind = event.kind.as_str();
    if let Some(stored_session) = event.data.get("sessionID").and_then(Value::as_str) {
        if stored_session != event.aggregate_id.as_str() {
            return Err(corrupt_row(
                &event.id,
                "stored sessionID disagrees with the row coordinate",
            ));
        }
    }
    if event.created_ms < 0 {
        return Err(corrupt_row(
            &event.id,
            "stored created timestamp is negative",
        ));
    }
    // Agent mail has typed canonical input classes, not a user-input wire
    // DTO. Preserve its cursor coordinate without inventing user speech.
    if kind == "session.input.admitted.1"
        && matches!(
            event.data.pointer("/input/type").and_then(Value::as_str),
            Some("operation-delivery" | "agent-message")
        )
    {
        if !matches!(
            grapt_inbox::InboxEvent::decode("session.input.admitted", 1, &event.data)
                .map_err(|error| corrupt_row(&event.id, error.to_string()))?,
            Some(
                grapt_inbox::InboxEvent::OperationDeliveryAdmitted { .. }
                    | grapt_inbox::InboxEvent::AgentMessageAdmitted(_)
            )
        ) {
            return Err(corrupt_row(&event.id, "invalid typed agent mail"));
        }
        return Ok(skip_row(coordinate, kind));
    }
    let id = WireId::parse(event.id.as_str().to_owned())
        .ok_or_else(|| corrupt_row(&event.id, "stored event identity is not a wire identity"))?;
    let session = WireId::parse(event.aggregate_id.as_str().to_owned()).ok_or_else(|| {
        corrupt_row(
            &event.id,
            "stored aggregate identity is not a wire identity",
        )
    })?;
    let created = FiniteF64::try_new(event.created_ms as f64)
        .map_err(|detail| corrupt_row(&event.id, detail))?;
    let aggregate = AggregateIdDto::try_new(event.aggregate_id.as_str())
        .map_err(|detail| corrupt_row(&event.id, detail))?;
    let reference = match kind {
        "permission.asked.2" | "permission.replied.2" => {
            DurableEventRef::version_two(aggregate, event.seq.value().into())
        }
        _ => DurableEventRef::new(aggregate, event.seq.value().into()),
    };
    let dogfood = match kind {
        "session.input.admitted.1" => DogfoodEvent::InputAdmitted(DurableEvent::new(
            id,
            created,
            reference,
            super::input_events::admitted_data(event.data)
                .map_err(|detail| corrupt_row(&event.id, detail))?,
        )),
        "session.input.promoted.1" => DogfoodEvent::InputPromoted(DurableEvent::new(
            id,
            created,
            reference,
            decode(event.data).map_err(|detail| corrupt_row(&event.id, detail))?,
        )),
        "session.agent.selected.1" => DogfoodEvent::AgentSelected(DurableEvent::new(
            id,
            created,
            reference,
            decode(event.data).map_err(|detail| corrupt_row(&event.id, detail))?,
        )),
        "session.model.selected.1" => DogfoodEvent::ModelSelected(DurableEvent::new(
            id,
            created,
            reference,
            decode(event.data).map_err(|detail| corrupt_row(&event.id, detail))?,
        )),
        "session.subagent-model.selected.1" => {
            DogfoodEvent::SubagentModelSelected(DurableEvent::new(
                id,
                created,
                reference,
                decode(event.data).map_err(|detail| corrupt_row(&event.id, detail))?,
            ))
        }
        "session.subagent-model.cleared.1" => {
            DogfoodEvent::SubagentModelCleared(DurableEvent::new(
                id,
                created,
                reference,
                decode(event.data).map_err(|detail| corrupt_row(&event.id, detail))?,
            ))
        }
        "permission.asked.2" => {
            let Some(grapt_permission::PermissionEvent::Asked {
                id: permission,
                session: owner,
                action,
                coordinates: Some(coordinates),
            }) = grapt_permission::PermissionEvent::decode("permission.asked", 2, &event.data)
                .map_err(|error| corrupt_row(&event.id, error.to_string()))?
            else {
                return Ok(skip_row(coordinate, kind));
            };
            let mut data = serde_json::json!({
                "id": permission.as_str(),
                "sessionID": owner.as_str(),
                "action": action,
                "resources": coordinates.resources,
            });
            if let Some(save) = coordinates.save {
                data["save"] = serde_json::json!(save);
            }
            if let Some(metadata) = coordinates.metadata {
                data["metadata"] = metadata;
            }
            if let Some(source) = coordinates.source {
                data["source"] = source;
            }
            super::permission::include_preview(&mut data);
            DogfoodEvent::PermissionAsked(DurableEvent::new(
                id,
                created,
                reference,
                decode(data).map_err(|detail| corrupt_row(&event.id, detail))?,
            ))
        }
        "permission.replied.2" => {
            let Some(grapt_permission::PermissionEvent::Replied {
                request,
                session: owner,
                reply,
                ..
            }) = grapt_permission::PermissionEvent::decode("permission.replied", 2, &event.data)
                .map_err(|error| corrupt_row(&event.id, error.to_string()))?
            else {
                return Ok(skip_row(coordinate, kind));
            };
            let reply = match reply {
                PermissionReply::Once => "once",
                PermissionReply::Always => "always",
                PermissionReply::Session => "session",
                PermissionReply::Reject => "reject",
            };
            DogfoodEvent::PermissionReplied(DurableEvent::new(
                id,
                created,
                reference,
                decode(serde_json::json!({
                    "sessionID": owner.as_str(),
                    "requestID": request.as_str(),
                    "reply": reply,
                }))
                .map_err(|detail| corrupt_row(&event.id, detail))?,
            ))
        }
        "permission.cancelled.1" => {
            if !matches!(
                grapt_permission::PermissionEvent::decode("permission.cancelled", 1, &event.data)
                    .map_err(|error| corrupt_row(&event.id, error.to_string()))?,
                Some(grapt_permission::PermissionEvent::Cancelled { .. })
            ) {
                return Err(corrupt_row(
                    &event.id,
                    "permission cancellation could not be decoded",
                ));
            }
            DogfoodEvent::PermissionCancelled(DurableEvent::new(
                id,
                created,
                reference,
                decode(event.data).map_err(|detail| corrupt_row(&event.id, detail))?,
            ))
        }
        "session.execution.started.1" => DogfoodEvent::ExecutionStarted(DurableEvent::new(
            id,
            created,
            reference,
            ExecutionSucceededData::new(session, stored_execution(&event, &event.id)?),
        )),
        "session.execution.succeeded.1" => DogfoodEvent::ExecutionSucceeded(DurableEvent::new(
            id,
            created,
            reference,
            ExecutionSucceededData::new(session, stored_execution(&event, &event.id)?),
        )),
        "session.execution.failed.1" => DogfoodEvent::ExecutionFailed(DurableEvent::new(
            id,
            created,
            reference,
            ExecutionFailedData::new(
                session,
                stored_execution(&event, &event.id)?,
                decode(event.data.get("error").cloned().ok_or_else(|| {
                    corrupt_row(&event.id, "stored failure carries no structured error")
                })?)
                .map_err(|detail| corrupt_row(&event.id, detail))?,
            ),
        )),
        "session.execution.interrupted.1" => DogfoodEvent::ExecutionInterrupted(DurableEvent::new(
            id,
            created,
            reference,
            ExecutionInterruptedData::new(
                session,
                stored_execution(&event, &event.id)?,
                stored_interrupt_reason(&event, &event.id)?,
            ),
        )),
        "session.compaction.ended.1" => DogfoodEvent::CompactionEnded(DurableEvent::new(
            id,
            created,
            reference,
            CompactionEndedData::new(
                session,
                stored_compaction_reason(&event, &event.id)?,
                event
                    .data
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| corrupt_row(&event.id, "stored compaction carries no text"))?
                    .to_owned(),
                Utf16Text::try_new(
                    event
                        .data
                        .get("recent")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            corrupt_row(&event.id, "stored compaction carries no recent value")
                        })?
                        .encode_utf16()
                        .collect(),
                )
                .map_err(|detail| corrupt_row(&event.id, detail))?,
            ),
        )),
        "session.assistant-message.opened.1" | "session.assistant-message.opened.2" => {
            let fact = if kind == "session.assistant-message.opened.2" {
                grapt_event::session_message::decode_opened_v2(&event.data)
            } else {
                grapt_event::session_message::decode_opened(&event.data)
            }
            .map_err(|error| corrupt_row(&event.id, error.to_string()))?;
            let Some(selection) = fact.selection() else {
                return Ok(skip_row(coordinate, kind));
            };
            let assistant_message =
                WireId::parse(fact.message().as_str().to_owned()).ok_or_else(|| {
                    corrupt_row(
                        &event.id,
                        "stored assistant message identity is not a wire identity",
                    )
                })?;
            let agent = WireId::parse(selection.agent().as_str().to_owned()).ok_or_else(|| {
                corrupt_row(&event.id, "stored agent identity is not a wire identity")
            })?;
            let mut model_ref = serde_json::json!({
                "id": selection.model().as_str(),
                "providerID": selection.provider().as_str(),
            });
            if let Some(variant) = selection
                .variant()
                .filter(|variant| variant.as_str() != "default")
            {
                model_ref["variant"] = serde_json::json!(variant.as_str());
            }
            DogfoodEvent::StepStarted(DurableEvent::new(
                id,
                created,
                reference,
                StepStartedData::new(
                    session,
                    assistant_message,
                    agent,
                    decode(model_ref).map_err(|detail| corrupt_row(&event.id, detail))?,
                ),
            ))
        }
        _ => return Ok(skip_row(coordinate, kind)),
    };
    Ok(CapturePageRow::Event {
        coordinate,
        event: Box::new(dogfood),
    })
}

/// The optional execution identity a stored execution row carries, as its
/// wire form. A present value that is not a wire identity is corruption,
/// never a silent None.
fn stored_execution(
    event: &DecodedEvent,
    id: &EventId,
) -> Result<Option<WireId<ExecutionId>>, CapturePageError> {
    event
        .data
        .get("executionID")
        .and_then(Value::as_str)
        .map(|raw| {
            WireId::parse(raw.to_owned())
                .ok_or_else(|| corrupt_row(id, "stored execution identity is not a wire identity"))
        })
        .transpose()
}

/// The stored interruption reason, matched to the wire vocabulary: the
/// enum is serialize-only, so the durable string is the decode surface.
fn stored_interrupt_reason(
    event: &DecodedEvent,
    id: &EventId,
) -> Result<InterruptReason, CapturePageError> {
    match event.data.get("reason").and_then(Value::as_str) {
        Some("user") => Ok(InterruptReason::User),
        Some("shutdown") => Ok(InterruptReason::Shutdown),
        Some("serve-window-expired") => Ok(InterruptReason::ServeWindowExpired),
        Some("superseded") => Ok(InterruptReason::Superseded),
        _ => Err(corrupt_row(
            id,
            "stored interruption reason is not a wire reason",
        )),
    }
}

/// The stored compaction reason, matched to the wire vocabulary: the
/// enum is serialize-only, so the durable string is the decode surface.
fn stored_compaction_reason(
    event: &DecodedEvent,
    id: &EventId,
) -> Result<CompactionReason, CapturePageError> {
    match event.data.get("reason").and_then(Value::as_str) {
        Some("auto") => Ok(CompactionReason::Auto),
        Some("manual") => Ok(CompactionReason::Manual),
        _ => Err(corrupt_row(
            id,
            "stored compaction reason is not a wire reason",
        )),
    }
}

fn skip_row(coordinate: CaptureCoordinate, kind: &str) -> CapturePageRow {
    CapturePageRow::Skipped {
        coordinate,
        kind: bounded_detail(kind),
    }
}

/// Fixed literals this module itself emits: they are known to fit the wire
/// bound, so a refusal here is a defect in this file, not in the data.
fn bounded(raw: &str) -> DtoString {
    match DtoString::try_new(raw) {
        Ok(value) => value,
        Err(_) => unreachable!("fixed capture literal exceeded its wire bound"),
    }
}

/// Data-derived diagnostic text (a decode refusal on a stored payload of up
/// to eight MiB) is bounded before it becomes wire DTO text: the wire bound
/// is law, and a refused page must never panic because the refusal was long.
fn bounded_detail(raw: &str) -> DtoString {
    const DETAIL_BOUND: usize = 64_000;
    let cut = raw
        .char_indices()
        .find(|(index, _)| *index >= DETAIL_BOUND)
        .map_or(raw.len(), |(index, _)| index);
    bounded(&raw[..cut])
}

fn corrupt_row(id: &EventId, detail: impl Into<String>) -> CapturePageError {
    CapturePageError::CorruptRow {
        id: bounded(id.as_str()),
        detail: bounded_detail(&detail.into()),
    }
}
