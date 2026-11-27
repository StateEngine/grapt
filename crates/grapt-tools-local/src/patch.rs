//! Bounded text-patch parsing and contextual matching, without filesystem effects.
//!
//! The application resolves paths, authorizes changes and applies the resulting
//! contents through its governed filesystem plan.

mod parser;
mod seek;
mod streaming;
mod text;
mod update;

pub use parser::{parse_patch, Chunk, Hunk, ParseError, ParsedPatch};
pub use update::updated;

/// Text patch input grammar for providers that support freeform tool input.
pub const GRAMMAR: &str = include_str!("patch/grammar.lark");

#[cfg(test)]
mod tests;
