//! The store's named production clock adapter.
//!
//! Row-11 law confines ambient wall-clock reads to NAMED production adapters;
//! every durable owner consumes an injected [`grapt_types::ClockAuthority`]
//! instead. The
//! sibling adapter in [`crate::ids`] does the same job for identity.
//!
//! # Why the migration journal needed one
//!
//! `rust_migration.time_completed` is a DURABLE row, and until now it was
//! written by `strftime('%s','now') * 1000` inside the INSERT. That is an
//! ambient clock read that `clippy::disallowed_methods` cannot see, because the
//! gate matches Rust call paths and this one lives in SQL text. The value is
//! not load-bearing for any decision, but it is durable attribution in a
//! journal an R6 rollback rehearsal reads, and a determinism law that stops at
//! the language boundary is not a determinism law.
//!
//! # What is deliberately NOT moved
//!
//! * `epoch.rs`'s `strftime('%s','now')` in the `engine_epoch` attribution
//!   row. Measured and deferred by ruling; this lane must not change it.
//! * The backup FILE NAME's millisecond component in [`crate::migration`].
//!   It is not a durable fact — it only disambiguates a path alongside the
//!   process id — and routing it through an injected authority would make a
//!   frozen test clock produce the same name twice, which is exactly what the
//!   accepted "a previous rollback point is never overwritten" witness proves
//!   cannot happen. Its one-line allow and its own justification stand.

use grapt_types::{ClockAuthority, Timestamp};

/// Reads the operating system's wall clock.
///
/// The single permitted ambient time read behind the store's migration seam.
/// A skewed or wrong reading lands only in journal attribution: it cannot
/// select a migration, change a plan, or enter an event's canonical bytes.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl ClockAuthority for SystemClock {
    /// A pre-epoch clock reading yields millisecond zero rather than a panic
    /// or a negative timestamp: a machine whose clock is set before 1970 must
    /// still be able to record that a migration completed.
    #[allow(clippy::disallowed_methods)]
    fn now(&self) -> Timestamp {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        Timestamp::from_millis(i64::try_from(millis).unwrap_or(i64::MAX))
    }
}
