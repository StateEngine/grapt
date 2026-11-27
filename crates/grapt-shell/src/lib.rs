//! Shell vocabulary shared by the live terminal and the durable tables.
//!
//! The three fact families are INDEPENDENT: process exit ([`ShellTerminal`]),
//! descendant-pipe drain ([`PipeState`]), and cleanup ([`CleanupState`]). A
//! process exit does not imply the pipe drained or cleanup complete.
//!
//! This crate owns no service and no read path. The live terminal keeps its
//! running sessions in memory; the shell tables exist only so a store written
//! by an earlier build still opens.

pub mod operation;
pub mod schema;

pub use operation::{CleanupState, PipeState, ShellTerminal};
