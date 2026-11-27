//! Local tools the agent runs on this machine: terminal and pseudo-console
//! sessions, patching, file read, glob and grep, checkpoints, the shell
//! environment and sandbox glue.
//!
//! This crate depends only on lower crates. The server application composes it
//! and owns everything that spans sessions or users.

/// Maximum bytes in one local file effect.
pub const FILE_EFFECT_MAX_BYTES: u64 = 1_024 * 1_024;

pub mod filesystem;
pub mod patch;
pub mod read_lines;
pub mod read_window;
pub mod search;
pub mod terminal;
