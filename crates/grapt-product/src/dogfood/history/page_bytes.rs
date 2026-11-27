// Copyright 2025 OpenAI. Licensed under the Apache License, Version 2.0.
// Cursor anchors follow returned rows.
// Modified by Grapt: exact encoded-byte accounting and bidirectional byte cuts.
use super::*;
use grapt_api::u3a::{
    encode_page, DtoCodecError, DtoString, SessionMessageInfoDto, SessionMessagesResponseDtoCursor,
    U3aLimits, U3A_PRODUCTION_LIMITS,
};

/// One kept row: the typed DTO its byte accounting already decoded, and the
/// exact wire text the legacy page serves for it.
type KeptRow = (SessionMessageInfoDto, String);

impl MessagePage {
    /// The encoded previous/next anchors of a page's rows, computed once and
    /// shared by the legacy cursor text and the typed snapshot cursor.
    fn anchors(
        &self,
        rows: &[IndexedRow],
    ) -> Result<(Option<String>, Option<String>), PageRefusal> {
        let encode = |row: &IndexedRow, direction| -> Result<String, String> {
            let id = WireId::parse(row.id.clone()).ok_or("projected message identity invalid")?;
            let bytes = serde_json::to_vec(&Cursor {
                id,
                order: self.order,
                direction,
            })
            .map_err(|error| error.to_string())?;
            Ok(URL_SAFE_NO_PAD.encode(bytes))
        };
        let previous = rows.first().map(|first| encode(first, Direction::Previous));
        let next = rows.last().map(|last| encode(last, Direction::Next));
        Ok((previous.transpose()?, next.transpose()?))
    }

    fn cursors(&self, rows: &[IndexedRow]) -> Result<String, PageRefusal> {
        let (previous, next) = self.anchors(rows)?;
        let mut cursors = serde_json::Map::new();
        if let Some(previous) = previous {
            cursors.insert("previous".into(), Value::String(previous));
        }
        if let Some(next) = next {
            cursors.insert("next".into(), Value::String(next));
        }
        Ok(Value::Object(cursors).to_string())
    }

    fn span<'a>(&self, rows: &'a [IndexedRow], count: usize) -> &'a [IndexedRow] {
        if self.previous() {
            &rows[rows.len() - count..]
        } else {
            &rows[..count]
        }
    }

    pub(crate) fn render(
        &self,
        rows: &[IndexedRow],
        render: impl FnMut(&[IndexedRow]) -> Result<Vec<String>, String>,
    ) -> Result<String, PageRefusal> {
        self.render_bounded(rows, render, U3A_PRODUCTION_LIMITS.page_bytes)
    }

    /// The same page as [`Self::render`], typed: the kept rows' DTOs and a
    /// cursor built from the same anchors, with nothing re-decoded.
    pub(crate) fn render_typed(
        &self,
        rows: &[IndexedRow],
        render: impl FnMut(&[IndexedRow]) -> Result<Vec<String>, String>,
    ) -> Result<(Vec<SessionMessageInfoDto>, SessionMessagesResponseDtoCursor), PageRefusal> {
        let kept = self.kept_rows(rows, render, U3A_PRODUCTION_LIMITS.page_bytes)?;
        let (previous, next) = self.anchors(self.span(rows, kept.len()))?;
        let cursor = SessionMessagesResponseDtoCursor::new(anchor(previous)?, anchor(next)?);
        Ok((kept.into_iter().map(|(dto, _)| dto).collect(), cursor))
    }

    fn render_bounded(
        &self,
        rows: &[IndexedRow],
        render: impl FnMut(&[IndexedRow]) -> Result<Vec<String>, String>,
        bound: usize,
    ) -> Result<String, PageRefusal> {
        let kept = self.kept_rows(rows, render, bound)?;
        let cursors = self.cursors(self.span(rows, kept.len()))?;
        let data: Vec<&str> = kept.iter().map(|(_, raw)| raw.as_str()).collect();
        Ok(format!(
            "{{\"data\":[{}],\"cursor\":{cursors}}}",
            data.join(",")
        ))
    }

    /// The rows one page keeps under `bound`, in reading order: every cut,
    /// metadata fallback and ordering decision the legacy page makes.
    fn kept_rows(
        &self,
        rows: &[IndexedRow],
        mut render: impl FnMut(&[IndexedRow]) -> Result<Vec<String>, String>,
        bound: usize,
    ) -> Result<Vec<KeptRow>, PageRefusal> {
        let limits = U3aLimits::try_new(grapt_api::u3a::U3aLimitValues {
            page_bytes: bound,
            ..U3A_PRODUCTION_LIMITS
        })
        .map_err(|error| error.to_string())?;
        let mut data = Vec::new();
        let mut row_bytes = 0usize;
        // Render only the next candidate: byte overflow must not depend on
        // an unreadable artifact or compaction in a later, unreturned row.
        for offset in 0..rows.len() {
            let index = if self.previous() {
                rows.len() - 1 - offset
            } else {
                offset
            };
            let row = &rows[index];
            let (dto, raw) = readable_row(row, render(std::slice::from_ref(row)))?;
            let encoded = match encode_page(&dto, limits) {
                Ok(encoded) => encoded.len(),
                Err(DtoCodecError::PageBytes) if !data.is_empty() => break,
                Err(DtoCodecError::PageBytes) => {
                    return Err(PageRefusal::RowBytes {
                        id: row.id.clone(),
                        bound,
                    })
                }
                Err(error) => return Err(format!("history row {}: {error}", row.id).into()),
            };
            let next_count = data.len() + 1;
            let cursors = self.cursors(self.span(rows, next_count))?;
            let framing = format!("{{\"data\":[],\"cursor\":{cursors}}}").len();
            let bytes = row_bytes
                .saturating_add(encoded)
                .saturating_add(next_count - 1)
                .saturating_add(framing);
            if bytes > bound {
                if data.is_empty() {
                    return Err(PageRefusal::RowBytes {
                        id: row.id.clone(),
                        bound,
                    });
                }
                break;
            }
            row_bytes += encoded;
            data.push((dto, raw));
        }
        if self.previous() {
            data.reverse();
        }
        Ok(data)
    }
}

/// Keep the selected identity and coordinate when the item's stored content
/// cannot be decoded; both page views and the single-message lookup use this.
/// Only decode failures degrade. Render failures, renderer invariant
/// violations and transient read errors keep their by-name page refusal, as
/// none says the stored item is permanently unreadable.
pub(crate) fn readable_row(
    row: &IndexedRow,
    rendered: Result<Vec<String>, String>,
) -> Result<KeptRow, String> {
    let mut rendered = rendered?;
    if rendered.len() != 1 {
        return Err("history renderer changed its row count".to_owned());
    }
    let raw = row.checked(rendered.pop().ok_or("history renderer omitted its row")?)?;
    decode_item(raw).or_else(|cause| {
        // Error class and position only: a decode error can quote the value.
        crate::diagnostics::report(format!(
            "history row {} seq {}: stored item undecodable ({cause}); served as an unreadable system row",
            row.id,
            row.seq()
        ));
        decode_item(unreadable_message_row(&row.id, row.seq())?)
    })
}

/// Decode only valid public content under an identity already checked.
/// Oversized metadata keeps its marked fallback; the error is a masked cause.
fn decode_item(raw: String) -> Result<KeptRow, String> {
    match serde_json::from_str::<SessionMessageInfoDto>(&raw) {
        Ok(dto) => Ok((dto, raw)),
        Err(error) => decode_without_metadata(&raw)
            .map_err(|_| format!("{:?} at column {}", error.classify(), error.column())),
    }
}

/// One encoded anchor as the typed cursor field, absent when the page is empty.
fn anchor<E: grapt_schema::TriStateEvidence>(
    value: Option<String>,
) -> Result<grapt_schema::OptionalNullable<DtoString, E>, PageRefusal> {
    match value {
        Some(value) => Ok(grapt_schema::OptionalNullable::value(
            DtoString::try_new(value).map_err(|error| error.to_string())?,
        )),
        None => Ok(grapt_schema::OptionalNullable::absent()),
    }
}

/// Decode a row whose metadata exceeded the reader's own bound, marking each
/// over-budget `metadata` object and keeping every other field.
///
/// A row, its content items, its tool states and a compaction union are
/// tag-first, so the row is parsed to a value, gets only its over-budget
/// metadata replaced, and is re-emitted with the tags first. The emitted text
/// is then decoded into the public DTO and re-serialized from it, so the bytes
/// that are served are exactly the reader's own canonical form.
///
/// Only the two positions that carry this reader's metadata budget are
/// visited: the row's own `metadata` and a tool state's `metadata`. Everything
/// else — a tool's `input`, `content` and `output` — stays exactly as rendered.
/// A tool input is decoded under a larger budget, so a user's own nested key
/// named `metadata` must not be mistaken for this one.
fn decode_without_metadata(raw: &str) -> Result<(SessionMessageInfoDto, String), String> {
    let mut value: Value = serde_json::from_str(raw).map_err(|error| error.to_string())?;
    let dropped = drop_over_budget_metadata(&mut value);
    if dropped == 0 {
        return Err("metadata refusal left no over-budget metadata to drop".to_owned());
    }
    let reordered = tagged_first_json(&value).map_err(|error| error.to_string())?;
    let dto: SessionMessageInfoDto =
        serde_json::from_str(&reordered).map_err(|error| error.to_string())?;
    let text = serde_json::to_string(&dto).map_err(|error| error.to_string())?;
    Ok((dto, text))
}

/// Replace the over-budget metadata of the row and of each tool state, and
/// report how many were dropped. A metadata that is not an object is malformed
/// rather than oversized, so a genuine encoding defect still refuses.
fn drop_over_budget_metadata(value: &mut Value) -> usize {
    let Some(row) = value.as_object_mut() else {
        return 0;
    };
    let mut dropped = drop_metadata_member(row);
    if let Some(Value::Array(items)) = row.get_mut("content") {
        for item in items.iter_mut() {
            if let Some(state) = item.get_mut("state").and_then(Value::as_object_mut) {
                dropped += drop_metadata_member(state);
            }
        }
    }
    dropped
}

/// Replace one object's `metadata` when the reader would refuse it.
fn drop_metadata_member(object: &mut serde_json::Map<String, Value>) -> usize {
    let Some(metadata) = object.get_mut("metadata") else {
        return 0;
    };
    if !metadata.is_object()
        || serde_json::from_value::<grapt_api::u3a::OpenObject>(metadata.clone()).is_ok()
    {
        return 0;
    }
    *metadata = serde_json::json!({
        crate::dogfood::metadata_bound::DROPPED: {
            "reason": "row exceeded the reader's metadata bound",
        },
    });
    1
}

/// Emit `value` with each object's union tag first, which the tag-first unions
/// in a history row require: `type`, then `status` where both are present (a
/// compaction row carries both), else whichever the object has. Every other
/// member keeps its relative order.
fn tagged_first_json(value: &Value) -> Result<String, serde_json::Error> {
    let mut out = String::new();
    write_tagged(value, &mut out)?;
    Ok(out)
}

fn write_tagged(value: &Value, out: &mut String) -> Result<(), serde_json::Error> {
    match value {
        Value::Object(object) => {
            out.push('{');
            let mut first = true;
            let mut member =
                |key: &str, child: &Value, out: &mut String| -> Result<(), serde_json::Error> {
                    if !first {
                        out.push(',');
                    }
                    first = false;
                    out.push_str(&serde_json::to_string(key)?);
                    out.push(':');
                    write_tagged(child, out)
                };
            for tag in ["type", "status"] {
                if let Some(child) = object.get(tag) {
                    member(tag, child, out)?;
                }
            }
            for (key, child) in object {
                if key == "type" || key == "status" {
                    continue;
                }
                member(key, child, out)?;
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, child) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_tagged(child, out)?;
            }
            out.push(']');
        }
        scalar => out.push_str(&serde_json::to_string(scalar)?),
    }
    Ok(())
}

#[cfg(test)]
mod tests;
