//! Which tool definitions a request may carry, and why some are left out.
//!
//! One owner for the numbers a model request is bounded by. The product decides
//! what to offer; this module decides what is admissible, so an ineligible tool
//! is left out of a request instead of failing the whole turn. A definition is
//! judged whole: it is either sent unchanged or not sent at all, because a
//! truncated definition would describe a tool that does not exist.

use crate::{ToolDefinition, ToolInputFormat};

/// The largest model-facing description a single tool may carry.
/// Matches `McpBounds::conservative()` so valid MCP tool descriptions are not
/// artificially dropped at request construction.
pub const MAX_TOOL_DESCRIPTION_BYTES: usize = 4 * 1024;
/// The largest text-tool grammar a single tool may carry.
pub const MAX_TEXT_TOOL_GRAMMAR_BYTES: usize = 64 * 1024;
/// The most tool definitions one request may carry, including its native-tool
/// reservations.
pub const MAX_REQUEST_TOOLS: usize = 1024;

/// Why a definition was left out of a request.
///
/// Closed on purpose: each variant names a reason a reader can act on, and a
/// future reason must be added deliberately rather than arriving as free text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExclusionReason {
    DescriptionTooLarge,
    GrammarTooLarge,
    DescriptionEmpty,
    GrammarEmpty,
    SchemaNotJson,
    SchemaNotObject,
}

impl ExclusionReason {
    /// The reader-facing phrase for this reason.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DescriptionTooLarge => "description is too long",
            Self::GrammarTooLarge => "text grammar is too long",
            Self::DescriptionEmpty => "description is empty",
            Self::GrammarEmpty => "text grammar is empty",
            Self::SchemaNotJson => "malformed schema",
            Self::SchemaNotObject => "malformed schema",
        }
    }
}

/// Whether one definition may be sent, or the single reason it may not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eligibility {
    Eligible,
    Excluded(ExclusionReason),
}

/// Judge one definition. Shape is checked before size so a malformed tool is
/// reported as malformed rather than as oversized.
pub fn eligibility(tool: &ToolDefinition) -> Eligibility {
    if let Some(description) = &tool.description {
        if description.is_empty() {
            return Eligibility::Excluded(ExclusionReason::DescriptionEmpty);
        }
        if description.len() > MAX_TOOL_DESCRIPTION_BYTES {
            return Eligibility::Excluded(ExclusionReason::DescriptionTooLarge);
        }
    }
    if let ToolInputFormat::Text { grammar } = &tool.input_format {
        if grammar.trim().is_empty() {
            return Eligibility::Excluded(ExclusionReason::GrammarEmpty);
        }
        if grammar.len() > MAX_TEXT_TOOL_GRAMMAR_BYTES {
            return Eligibility::Excluded(ExclusionReason::GrammarTooLarge);
        }
    }
    let Ok(schema) = serde_json::from_str::<serde_json::Value>(&tool.input_schema) else {
        return Eligibility::Excluded(ExclusionReason::SchemaNotJson);
    };
    if !schema.is_object() {
        return Eligibility::Excluded(ExclusionReason::SchemaNotObject);
    }
    Eligibility::Eligible
}

/// The definitions a request may carry, and the ones it must leave out with each
/// one's single reason. Judged whole: a definition is sent unchanged or not sent
/// at all, because a truncated definition would describe a tool that does not
/// exist.
pub fn admissible(
    tools: Vec<ToolDefinition>,
) -> (Vec<ToolDefinition>, Vec<(String, ExclusionReason)>) {
    let mut kept = Vec::with_capacity(tools.len());
    let mut left_out = Vec::new();
    for tool in tools {
        match eligibility(&tool) {
            Eligibility::Eligible => kept.push(tool),
            Eligibility::Excluded(reason) => left_out.push((tool.name, reason)),
        }
    }
    (kept, left_out)
}
