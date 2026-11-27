//! Store-owned injection of durable rows this store's own API can never
//! produce, for cross-crate counterexample fixtures.
//!
//! The governed tables are refused on every consumer connection (see
//! [`crate::governed`]), and the one governed boundary a consumer may open —
//! [`crate::EventStore::governed_transaction`] paired with
//! [`crate::EventStore::append_within`] — refuses these rows by construction:
//! the fixture states are over-bound windows, negative sequences, writerless
//! terminals and byte-exact walls that no admission would accept. A raw
//! consumer connection could stage them, but a consumer-owned transaction is
//! exactly the escape the write-chokepoint law refuses. The staging is
//! therefore a governed act: this module is the single named, gate-lifted
//! writer for cross-crate fixtures, the cross-crate counterpart of the
//! crate-private `hostile_rows` module, which stays sealed to the store's own
//! tests.
//!
//! The transaction is the store's own: it opens and closes on the store's
//! connection, the fixture never sees a connection, and every governed write
//! takes the one-statement lift, one table at a time, exactly like the
//! store's own append. The write-chokepoint gate pins this API's consumers
//! to the reviewed fixture files, so the injection cannot drift into a
//! production write route.

use crate::governed::{GovernedTable, WriteGate};
use grapt_event::AggregateId;
use rusqlite::Connection;

/// One store-owned injection transaction over the fixture database.
///
/// Rolls back on drop unless [`commit`](Self::commit) ran, so a failed
/// fixture never leaves a half-seeded database behind.
#[derive(Debug)]
pub struct FixtureRows<'store> {
    conn: &'store Connection,
    gate: WriteGate,
    open: bool,
}

/// One durable event row in the session contract's shape, the values a
/// hostile or drifting writer would leave behind, bundled as a single
/// coherent value so the staging API stays under the argument bound.
#[derive(Debug)]
pub struct EventRow<'a> {
    pub aggregate: &'a AggregateId,
    pub id: &'a str,
    pub seq: i64,
    pub created_ms: i64,
    pub versioned_type: &'a str,
    pub data: &'a str,
    pub writer_epoch: i64,
}

impl<'store> FixtureRows<'store> {
    /// Opens the injection transaction on the store's own connection.
    pub(crate) fn begin(conn: &'store Connection, gate: &WriteGate) -> rusqlite::Result<Self> {
        conn.execute_batch("BEGIN IMMEDIATE")?;
        Ok(Self {
            conn,
            gate: gate.clone(),
            open: true,
        })
    }

    /// Stages one verbatim durable event row, exactly as a hostile or
    /// drifting writer would leave it.
    pub fn event_row(&self, row: &EventRow<'_>) -> rusqlite::Result<()> {
        self.gate.execute(
            self.conn,
            GovernedTable::Event,
            "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                row.id,
                crate::election::family_of(row.aggregate),
                crate::election::identity_of(row.aggregate),
                row.seq,
                row.created_ms,
                row.versioned_type,
                row.data,
                row.writer_epoch
            ],
        )?;
        Ok(())
    }

    /// The aggregate's durable sequence cursor, as the append would have left
    /// it at `seq`.
    pub fn sequence_cursor(&self, aggregate: &AggregateId, seq: i64) -> rusqlite::Result<()> {
        self.gate.execute(
            self.conn,
            GovernedTable::EventSequence,
            "INSERT OR IGNORE INTO event_sequence (aggregate_family, aggregate_id, seq, owner_id)
             VALUES (?1, ?2, ?3, NULL)",
            rusqlite::params![
                crate::election::family_of(aggregate),
                crate::election::identity_of(aggregate),
                seq
            ],
        )?;
        Ok(())
    }

    /// One verbatim `session_message` projection row.
    pub fn session_message_row(
        &self,
        id: &str,
        session_id: &str,
        kind: &str,
        text: &str,
        delivery: &str,
        seq: i64,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO session_message (id, session_id, type, text, delivery, seq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![id, session_id, kind, text, delivery, seq],
        )?;
        Ok(())
    }

    /// One verbatim `session_message` row in the user/queue state the recent
    /// fixtures seed.
    pub fn user_session_message(
        &self,
        id: &str,
        session_id: &str,
        text: &str,
        seq: i64,
    ) -> rusqlite::Result<()> {
        self.session_message_row(id, session_id, "user", text, "queue", seq)
    }

    /// Closes the transaction with a commit.
    pub fn commit(mut self) -> rusqlite::Result<()> {
        self.conn.execute_batch("COMMIT")?;
        self.open = false;
        Ok(())
    }
}

impl Drop for FixtureRows<'_> {
    fn drop(&mut self) {
        if self.open {
            // A failed connection may refuse the rollback; the scratch
            // database is discarded with the fixture either way.
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }
}
