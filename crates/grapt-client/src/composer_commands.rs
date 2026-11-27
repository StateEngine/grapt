//! Shared command tokens and guidance for native composer surfaces.
//! Copyright 2025 OpenAI, Apache-2.0 (licenses/openai-codex/). Modified by Grapt:
//! share native
//! token boundaries and the existing init task without owning client effects.

/// Match a whole command token, accepting whitespace before its argument.
pub fn argument<'a>(text: &'a str, command: &str) -> Option<&'a str> {
    let rest = text.trim().strip_prefix(command)?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim())
}

/// The ordinary agent task used by `/init`; existing guidance must be preserved.
pub const INIT_PROMPT: &str = include_str!("../assets/prompt_for_init_command.md");

/// Guidance after `/compact` schedules compaction for the next message.
pub const COMPACT_NOTICE: &str = "Compaction will run before your next message.";
