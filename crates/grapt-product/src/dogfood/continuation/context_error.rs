//! Why a request context could not be assembled (SC-REL-07a).
//!
//! Only a capacity refusal is something compaction can answer, so the plan
//! decides compaction from this type and never from an error's text. Each
//! producer names the bound it hit, in that bound's own unit, with what it
//! observed. Any other failure (unreadable history, corruption, a broken
//! exchange) arrives as a plain `String` and is always [`ContextError::Failed`]:
//! nothing infers capacity from text.
//!
//! `Display` keeps every message byte-identical to the strings these
//! producers returned before, so the refusal a user reads is unchanged.

use std::fmt;

/// The bound a request context exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::dogfood) enum Bound {
    /// The assembled conversation itself, in bytes or items.
    Conversation,
    /// Rows plus response-group boundaries, before efforts are counted.
    ResponseGroups,
    /// The retained summary and facts added to the history after the cut.
    SummaryAndRetained,
    /// Captured agent mail, by rows or by its shared source-byte budget.
    CapturedMail,
}

/// The unit a [`Capacity`] counts in. Bytes and items are never mixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::dogfood) enum Unit {
    Bytes,
    Items,
}

/// A refusal compaction can answer: `observed` exceeded `limit`, both in `unit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::dogfood) struct Capacity {
    pub bound: Bound,
    pub unit: Unit,
    pub observed: u64,
    pub limit: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::dogfood) enum ContextError {
    Capacity(Capacity),
    Failed(String),
}

const CONVERSATION: &str = "conversation exceeds bounded request context; compaction required";
const RESPONSE_GROUPS: &str = "response groups exceed the request item bound";
const SUMMARY_AND_RETAINED: &str = "summary and retained history exceed the request bound";

impl ContextError {
    pub(in crate::dogfood) fn capacity(
        bound: Bound,
        unit: Unit,
        observed: usize,
        limit: usize,
    ) -> Self {
        let wide = |value: usize| u64::try_from(value).unwrap_or(u64::MAX);
        Self::Capacity(Capacity {
            bound,
            unit,
            observed: wide(observed),
            limit: wide(limit),
        })
    }

    /// `observed` bytes past the request's byte bound.
    pub(in crate::dogfood) fn over_bytes(bound: Bound, observed: usize) -> Self {
        Self::capacity(bound, Unit::Bytes, observed, super::MAX_CONTEXT_BYTES)
    }

    /// `observed` items past the request's item bound.
    pub(in crate::dogfood) fn over_items(bound: Bound, observed: usize) -> Self {
        Self::capacity(bound, Unit::Items, observed, super::MAX_CONTEXT_ITEMS)
    }

    /// The compaction this refusal asks for; `None` for every other failure.
    pub(in crate::dogfood) fn compaction_trigger(&self) -> Option<grapt_types::CompactionTrigger> {
        match self {
            Self::Capacity(_) => Some(grapt_types::CompactionTrigger::StructuralBound),
            Self::Failed(_) => None,
        }
    }

    pub(in crate::dogfood) fn capacity_bound(&self) -> Option<Capacity> {
        match self {
            Self::Capacity(capacity) => Some(*capacity),
            Self::Failed(_) => None,
        }
    }
}

/// The retained summary, its facts and the history after the cut, within both
/// bounds: a summary-plus-retained overflow in the unit it exceeds.
pub(super) fn retained_fit(
    bytes: usize,
    items: usize,
    prefix: &[super::CodexInputItem],
    summary: Option<&str>,
    provider: &[super::CodexInputItem],
    original_users: bool,
) -> Result<(), ContextError> {
    let summary_bytes = summary.map_or_else(
        || {
            provider
                .iter()
                .map(super::context_item_bytes)
                .sum::<usize>()
        },
        |summary| super::retained_users::labelled_summary(summary, original_users).len(),
    );
    let retained_bytes = bytes
        .saturating_add(summary_bytes)
        .saturating_add(prefix.iter().map(super::context_item_bytes).sum::<usize>());
    if retained_bytes > super::MAX_CONTEXT_BYTES {
        return Err(ContextError::over_bytes(
            Bound::SummaryAndRetained,
            retained_bytes,
        ));
    }
    if super::exceeds_item_bound(items, prefix.len(), summary, provider) {
        let retained = retained_items(items, prefix.len(), summary, provider);
        return Err(ContextError::over_items(
            Bound::SummaryAndRetained,
            retained,
        ));
    }
    Ok(())
}

/// Retained rows plus the facts item and the summary's own items (one for a
/// text summary, each committed item for a provider compaction).
pub(super) fn retained_items(
    rows: usize,
    prefix_items: usize,
    summary: Option<&str>,
    provider: &[super::CodexInputItem],
) -> usize {
    let summary_items = if summary.is_some() { 1 } else { provider.len() };
    rows.saturating_add(prefix_items)
        .saturating_add(summary_items)
}

impl fmt::Display for ContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Capacity(Capacity { bound, .. }) => match bound {
                Bound::Conversation | Bound::CapturedMail => CONVERSATION,
                Bound::ResponseGroups => RESPONSE_GROUPS,
                Bound::SummaryAndRetained => SUMMARY_AND_RETAINED,
            },
            Self::Failed(detail) => detail,
        })
    }
}

impl std::error::Error for ContextError {}

impl From<String> for ContextError {
    fn from(detail: String) -> Self {
        Self::Failed(detail)
    }
}

impl From<&str> for ContextError {
    fn from(detail: &str) -> Self {
        Self::Failed(detail.to_owned())
    }
}

impl From<grapt_inbox::operation_mail::PresentedRowsError> for ContextError {
    fn from(error: grapt_inbox::operation_mail::PresentedRowsError) -> Self {
        use grapt_inbox::operation_mail::PresentedRowsError;
        match error {
            PresentedRowsError::Rows { limit } => Self::capacity(
                Bound::CapturedMail,
                Unit::Items,
                limit.saturating_add(1),
                limit,
            ),
            // The next source body needs `charge` and the shared budget has
            // only `remaining` left.
            PresentedRowsError::SourceBytes { charge, remaining } => {
                Self::capacity(Bound::CapturedMail, Unit::Bytes, charge, remaining)
            }
            PresentedRowsError::Failed(detail) => Self::Failed(detail),
        }
    }
}

impl From<ContextError> for String {
    fn from(error: ContextError) -> Self {
        match error {
            ContextError::Failed(detail) => detail,
            capacity => capacity.to_string(),
        }
    }
}

#[cfg(test)]
#[path = "context_error_tests.rs"]
mod tests;
