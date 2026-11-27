//! Single-file SQLite execution for run storage. SQLite is confined to the
//! writer, checkpointer and bounded snapshot jobs.
//!
//! The writer keeps SQLite pager spilling enabled. Row and VM callbacks
//! enforce WAL headroom while the SQL runs, and a per-request write budget
//! bounds the work retained by its in-memory savepoint. Live writes target
//! ordinary rowid tables, and only run programs that stream their work
//! without temporary query collections. Domain owners supply all affected
//! sessions or declare global scope when the footprint is shared.

mod api;
mod checkpoint;
mod command;
mod error;
mod key;
mod read;
mod sqlite;
mod transaction;
mod wal_budget;
mod writer;

pub use api::{CommitNotice, RunStore, StoreStats, Writer};
pub use command::{Command, Durability, Receipt, Scope, Statement};
pub use error::{OpenError, ReadError, WriteError};
pub use key::{InvalidRequestKey, RequestKey};
pub use read::{ReadPool, ReadView};
pub use rusqlite::types::Value;

#[cfg(test)]
mod fault_tests;
