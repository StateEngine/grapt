//! The content references a durable database carries.
//!
//! A cutover carry is the database and the sibling artifact state it names:
//! a database that references durable content is a database whose truth
//! depends on bytes that live OUTSIDE it. This module enumerates those
//! references for explicit whole-carry inspection or selected-session admission. It reads the event rows and nothing else: the artifact
//! bytes those rows name are never read here.
//!
//! Enumeration is bounded and strict. The caller names four ceilings, and
//! the first breach is a typed refusal naming the bound:
//!
//! - the most event rows the scan reads, duplicate or not - the scan stops
//!   at the bound instead of running to the end of the table;
//! - the most bytes one payload row may hold, checked against the row's
//!   DECLARED length before a single byte of the row is materialized, so an
//!   over-bound row is refused from its declared length, never from an
//!   allocation that already happened;
//! - the most encoded payload bytes the enumeration decodes across all
//!   rows, so a carry of many small rows cannot outrun the bound row by
//!   row;
//! - the most distinct references admitted, checked only when a NEW
//!   reference would be inserted - a later duplicate of an already-admitted
//!   reference never re-triggers the bound.
//!
//! Every payload is decoded by the single closed versioned decoder
//! (grapt_event::session_message::decode_appended) - there is no second,
//! looser decoder of the same contract in this store.
//!
//! Whole-carry inspection is an explicit offline operation. Product startup
//! does not call it: session admission uses an indexed Session-scoped seek.
//! Bounds apply to the selected scope; exceeding one is a typed refusal.
//!
//! Absent and unreadable are distinct facts. An absent database names no
//! content and so is an empty list; an unreadable or malformed one is a
//! typed error, never a silent empty list, because admission must not guess
//! what a damaged database carries.

use std::collections::HashSet;
use std::path::Path;

use grapt_types::{ArtifactRef, SessionId};

use crate::log::{EventLog, LogError};

/// The explicit bounds a carry enumeration runs under.
///
/// Admission is a bounded read, and an unbounded per-launch scan of a
/// growing history is the defect the bounds exist to name. The caller
/// states four ceilings and the first breach is a typed refusal naming the
/// bound:
///
/// - [`CarryEnumerationBounds::max_rows`] bounds the scan itself: the most
///   event rows read, duplicate or not.
/// - [`CarryEnumerationBounds::max_payload_bytes`] bounds one row, checked
///   against the row's declared length before the row is materialized.
/// - [`CarryEnumerationBounds::max_cumulative_payload_bytes`] bounds the
///   total encoded payload bytes decoded across all rows.
/// - [`CarryEnumerationBounds::max_items`] bounds the distinct references
///   admitted, checked only when a new reference is inserted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CarryEnumerationBounds {
    max_rows: u64,
    max_payload_bytes: u64,
    max_cumulative_payload_bytes: u64,
    max_items: usize,
}

impl CarryEnumerationBounds {
    /// Four positive bounds. A zero bound is a misconfiguration, not a
    /// stricter bound, and is refused at construction.
    pub fn new(
        max_rows: u64,
        max_payload_bytes: u64,
        max_cumulative_payload_bytes: u64,
        max_items: usize,
    ) -> Option<Self> {
        (max_rows > 0 && max_payload_bytes > 0 && max_cumulative_payload_bytes > 0 && max_items > 0)
            .then_some(Self {
                max_rows,
                max_payload_bytes,
                max_cumulative_payload_bytes,
                max_items,
            })
    }

    /// The most event rows the scan reads, duplicate or not.
    pub fn max_rows(&self) -> u64 {
        self.max_rows
    }

    /// The most bytes one carried payload row may hold.
    pub fn max_payload_bytes(&self) -> u64 {
        self.max_payload_bytes
    }

    /// The most encoded payload bytes the enumeration decodes across all rows.
    pub fn max_cumulative_payload_bytes(&self) -> u64 {
        self.max_cumulative_payload_bytes
    }

    /// The most distinct artifact references the enumeration will admit.
    pub fn max_items(&self) -> usize {
        self.max_items
    }
}

/// Why the carried-reference enumeration failed.
#[derive(Debug, thiserror::Error)]
pub enum ContentRefsError {
    #[error("the event log could not be opened read-only: {0}")]
    Log(#[from] LogError),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A durable row is not a member of the versioned payload contract. The
    /// row id is named so the damaged row can be found, and the decoder's own
    /// verdict is carried verbatim.
    #[error("carried content row {event} does not decode: {reason}")]
    MalformedPayload { event: String, reason: String },
    /// The carry exceeded an admitted enumeration bound. The bound and its
    /// value are named in the refusal; `which` is one of `row`,
    /// `payload-byte`, `cumulative-payload-byte` or `item`.
    #[error("carried content enumeration exceeded its {which} bound of {limit}")]
    BoundsExceeded { which: &'static str, limit: u64 },
}

/// The exact versioned kind whose payload carries a content reference.
/// Only this kind's payload has the `{id, length, digest}` shape this
/// enumeration validates.
///
/// The shipped-composition witness in grapt-product's carry_admission test
/// binds this constant, so the witness cannot drift from the kind it names.
pub const CARRY_EVENT_TYPE: &str = "session.content.appended.1";

/// The exact query the enumeration runs: the carry kind's rows in stable id
/// order, bounded at the SQL seam by one row past the admitted row ceiling.
/// The carry kind is rare and the table is not, so the access path this
/// query relies on is the type-leading `event_type_id_idx`: every other
/// governed event index leads with the aggregate coordinate, which this
/// predicate never constrains, and without the type-leading index the
/// per-launch admission would scan the whole growing table before the Rust
/// row ceiling could even see a match.
///
/// The shipped-composition witness in grapt-product's carry_admission test
/// binds this exact text, so the witness cannot drift from the query it
/// describes.
pub const CARRY_ENUMERATION_QUERY: &str =
    "SELECT id, length(CAST(data AS BLOB)), data FROM event WHERE type = ?1 ORDER BY id LIMIT ?2";

/// Enumerates the distinct artifact references the durable content events
/// of `path` name, in a stable order, under the caller's `bounds`.
///
/// An absent `path` names no content and returns an empty list. Any other
/// open failure is a typed [`ContentRefsError`], as is any row whose payload
/// is not a member of the versioned payload contract, and any carry that
/// exceeds any admitted bound.
pub fn carried_content_refs(
    path: &Path,
    bounds: &CarryEnumerationBounds,
) -> Result<Vec<ArtifactRef>, ContentRefsError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let log = EventLog::open_read_only(path)?;
    let connection = log.read_only_connection()?;
    let mut statement = connection.prepare(CARRY_ENUMERATION_QUERY)?;
    let mut rows = statement.query(rusqlite::params![CARRY_EVENT_TYPE, row_limit(bounds)?])?;
    collect_references(&mut rows, bounds)
}

/// Enumerate only the selected Session's carried content. The required
/// aggregate/type/sequence index bounds the seek independently of other history.
/// The caller owns the read connection; this function never opens or writes a DB.
pub fn session_content_refs(
    connection: &rusqlite::Connection,
    session: &SessionId,
    bounds: &CarryEnumerationBounds,
) -> Result<Vec<ArtifactRef>, ContentRefsError> {
    let mut statement = connection.prepare(
        "SELECT id, length(CAST(data AS BLOB)), data FROM event
         INDEXED BY event_aggregate_type_seq_idx
         WHERE aggregate_family = 'session' AND aggregate_id = ?1 AND type = ?2
         ORDER BY seq LIMIT ?3",
    )?;
    let mut rows = statement.query(rusqlite::params![
        session.as_str(),
        CARRY_EVENT_TYPE,
        row_limit(bounds)?
    ])?;
    collect_references(&mut rows, bounds)
}

fn row_limit(bounds: &CarryEnumerationBounds) -> Result<i64, ContentRefsError> {
    // Read one extra row to refuse oversized histories rather than truncate them.
    i64::try_from(bounds.max_rows().saturating_add(1)).map_err(|error| {
        ContentRefsError::Sqlite(rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Integer,
            Box::new(error),
        ))
    })
}

fn collect_references(
    rows: &mut rusqlite::Rows<'_>,
    bounds: &CarryEnumerationBounds,
) -> Result<Vec<ArtifactRef>, ContentRefsError> {
    let mut seen = HashSet::new();
    let mut references = Vec::new();
    let mut rows_seen = 0u64;
    let mut cumulative_bytes = 0u64;
    loop {
        let row = match rows.next() {
            Ok(None) => break,
            Ok(Some(row)) => row,
            Err(error) => return Err(ContentRefsError::Sqlite(error)),
        };
        let event: String = row.get(0).map_err(ContentRefsError::Sqlite)?;
        let declared_bytes: i64 = row.get(1).map_err(ContentRefsError::Sqlite)?;
        let declared_bytes = u64::try_from(declared_bytes).map_err(|error| {
            ContentRefsError::Sqlite(rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Integer,
                Box::new(error),
            ))
        })?;
        rows_seen += 1;
        if rows_seen > bounds.max_rows() {
            return Err(ContentRefsError::BoundsExceeded {
                which: "row",
                limit: bounds.max_rows(),
            });
        }
        if declared_bytes > bounds.max_payload_bytes() {
            return Err(ContentRefsError::BoundsExceeded {
                which: "payload-byte",
                limit: bounds.max_payload_bytes(),
            });
        }
        if cumulative_bytes.saturating_add(declared_bytes) > bounds.max_cumulative_payload_bytes() {
            return Err(ContentRefsError::BoundsExceeded {
                which: "cumulative-payload-byte",
                limit: bounds.max_cumulative_payload_bytes(),
            });
        }
        cumulative_bytes += declared_bytes;
        let data: String = row.get(2).map_err(ContentRefsError::Sqlite)?;
        let reference = decode_reference(&event, &data)?;
        if !seen.contains(&reference) && seen.len() >= bounds.max_items() {
            return Err(ContentRefsError::BoundsExceeded {
                which: "item",
                limit: bounds.max_items() as u64,
            });
        }
        if seen.insert(reference.clone()) {
            references.push(reference);
        }
    }
    references.sort_by(|a, b| {
        (a.id().as_str(), a.length(), a.digest().as_str()).cmp(&(
            b.id().as_str(),
            b.length(),
            b.digest().as_str(),
        ))
    });
    Ok(references)
}

/// One durable payload, decoded by the single closed versioned decoder.
/// Every failure is a named [`ContentRefsError::MalformedPayload`] carrying
/// the row id and the decoder's own verdict: a reference is never repaired
/// by guess, and this store keeps no second decoder of the contract.
fn decode_reference(event: &str, data: &str) -> Result<ArtifactRef, ContentRefsError> {
    let value: serde_json::Value =
        serde_json::from_str(data).map_err(|error| ContentRefsError::MalformedPayload {
            event: event.to_owned(),
            reason: error.to_string(),
        })?;
    let fact = grapt_event::session_message::decode_appended(&value).map_err(|error| {
        ContentRefsError::MalformedPayload {
            event: event.to_owned(),
            reason: error.to_string(),
        }
    })?;
    Ok(fact.item().payload().clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const ENGINE_VERSION: &str = "0.1.0";

    fn scratch(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "grapt-store-content-refs-{label}-{}",
            std::process::id()
        ));
        if root.exists() {
            if let Err(error) = std::fs::remove_dir_all(&root) {
                panic!("could not reset the scratch root: {error}");
            }
        }
        if let Err(error) = std::fs::create_dir_all(&root) {
            panic!("could not create the scratch root: {error}");
        }
        root
    }

    fn ready_database(root: &Path) -> PathBuf {
        let database = root.join("database.sqlite3");
        if let Err(error) = crate::write::EventStore::open(&database, ENGINE_VERSION) {
            panic!("database creation failed: {error}");
        }
        let migrations = match crate::migrations::ProductionMigrations::load() {
            Ok(migrations) => migrations,
            Err(error) => panic!("migration load failed: {error}"),
        };
        match crate::migration::apply(&database, root, ENGINE_VERSION, &migrations.as_slice()) {
            Ok(
                crate::migration::MigrationOutcome::Applied { .. }
                | crate::migration::MigrationOutcome::AlreadyCurrent,
            ) => {}
            other => panic!("migration was refused: {other:?}"),
        }
        database
    }

    fn insert_content_row(database: &Path, event_id: &str, seq: i64, data: &str) {
        let connection = match rusqlite::Connection::open(database) {
            Ok(connection) => connection,
            Err(error) => panic!("test database open failed: {error}"),
        };
        if let Err(error) = connection.execute(
            "INSERT OR IGNORE INTO event_sequence (aggregate_family, aggregate_id, seq, owner_id) VALUES (?1, ?2, ?3, NULL)",
            rusqlite::params!["crf-carry", "crf-carry", seq],
        ) {
            panic!("test sequence insert failed: {error}");
        }
        if let Err(error) = connection.execute(
            "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![event_id, "crf-carry", "crf-carry", seq, 0i64, CARRY_EVENT_TYPE, data, 1i64],
        ) {
            panic!("test event insert failed: {error}");
        }
    }

    fn test_bounds() -> CarryEnumerationBounds {
        match CarryEnumerationBounds::new(10_000, 1024 * 1024, 256 * 1024 * 1024, 1024) {
            Some(bounds) => bounds,
            None => panic!("a nonzero test bound must construct"),
        }
    }

    fn row_with(ref_id: &str, length: u64) -> String {
        format!(
            "{{\"message\":\"msg_crfcarry\",\"item\":{{\"ordinal\":0,\"kind\":\"text\",\"payload\":{{\"id\":\"{ref_id}\",\"length\":{length},\"digest\":\"{digest}\"}},\"disposition\":\"complete\"}}}}",
            digest = "b".repeat(64),
        )
    }

    fn valid_row() -> String {
        row_with(&"a".repeat(64), 7)
    }

    #[test]
    fn an_absent_database_names_no_content() {
        let root = scratch("absent");
        let references = match carried_content_refs(&root.join("database.sqlite3"), &test_bounds())
        {
            Ok(references) => references,
            Err(error) => panic!("an absent database must name zero references: {error}"),
        };
        assert!(
            references.is_empty(),
            "an absent database must name zero references",
        );
    }

    #[test]
    fn a_content_free_database_names_no_content() {
        let root = scratch("empty");
        let database = ready_database(&root);
        let references = match carried_content_refs(&database, &test_bounds()) {
            Ok(references) => references,
            Err(error) => panic!("a content-free database must name zero references: {error}"),
        };
        assert!(references.is_empty(), "no content events were inserted");
    }

    #[test]
    fn a_carried_reference_is_enumerated_exactly_once() {
        let root = scratch("one");
        let database = ready_database(&root);
        let row = valid_row();
        insert_content_row(&database, "evt-crf-01", 0, &row);
        insert_content_row(&database, "evt-crf-02", 1, &row);
        let references = match carried_content_refs(&database, &test_bounds()) {
            Ok(references) => references,
            Err(error) => panic!("a valid row must enumerate: {error}"),
        };
        assert_eq!(
            references.len(),
            1,
            "two events naming one reference enumerate one reference",
        );
        assert_eq!(references[0].length(), 7);
        assert_eq!(references[0].digest().as_str(), "b".repeat(64));
    }

    #[test]
    fn a_payload_without_a_reference_is_malformed_by_field() {
        let root = scratch("nopayload");
        let database = ready_database(&root);
        insert_content_row(
            &database,
            "evt-crf-03",
            0,
            "{\"message\":\"msg_x\",\"item\":{\"ordinal\":0}}",
        );
        let error = match carried_content_refs(&database, &test_bounds()) {
            Ok(_) => panic!("a payload without a reference must refuse"),
            Err(error) => error,
        };
        assert!(
            matches!(&error, ContentRefsError::MalformedPayload { event, .. } if event == "evt-crf-03"),
            "the damaged row must be named: {error}",
        );
    }

    #[test]
    fn a_non_numeric_length_is_malformed_not_zero() {
        let root = scratch("badlength");
        let database = ready_database(&root);
        let row = format!(
            "{{\"message\":\"msg_x\",\"item\":{{\"ordinal\":0,\"kind\":\"text\",\"payload\":{{\"id\":\"{}\",\"length\":\"seven\",\"digest\":\"{}\"}},\"disposition\":\"complete\"}}}}",
            "a".repeat(64),
            "b".repeat(64),
        );
        insert_content_row(&database, "evt-crf-04", 0, &row);
        let error = match carried_content_refs(&database, &test_bounds()) {
            Ok(references) => panic!("a string length must refuse, and got: {references:?}"),
            Err(error) => error,
        };
        assert!(
            matches!(&error, ContentRefsError::MalformedPayload { event, .. } if event == "evt-crf-04"),
            "a non-numeric length must refuse named: {error}",
        );
    }

    #[test]
    fn an_item_bound_exceeded_is_a_named_refusal() {
        let root = scratch("item-bound");
        let database = ready_database(&root);
        insert_content_row(&database, "evt-crf-b1", 0, &row_with(&"a".repeat(64), 7));
        insert_content_row(&database, "evt-crf-b2", 1, &row_with(&"c".repeat(64), 9));
        insert_content_row(&database, "evt-crf-b3", 2, &row_with(&"e".repeat(64), 11));
        let bounds = match CarryEnumerationBounds::new(10_000, 1024 * 1024, 256 * 1024 * 1024, 2) {
            Some(bounds) => bounds,
            None => panic!("a nonzero test bound must construct"),
        };
        let error = match carried_content_refs(&database, &bounds) {
            Ok(references) => panic!(
                "a carry past its item bound must refuse, but the enumeration returned {} references",
                references.len()
            ),
            Err(error) => error,
        };
        assert!(
            matches!(&error, ContentRefsError::BoundsExceeded { which, limit } if *which == "item" && *limit == 2),
            "the refusal must name the item bound: {error}",
        );
    }

    #[test]
    fn a_payload_byte_bound_exceeded_is_a_named_refusal() {
        let root = scratch("byte-bound");
        let database = ready_database(&root);
        insert_content_row(&database, "evt-crf-b4", 0, &valid_row());
        let bounds = match CarryEnumerationBounds::new(10_000, 50, 256 * 1024 * 1024, 1024) {
            Some(bounds) => bounds,
            None => panic!("a nonzero test bound must construct"),
        };
        let error = match carried_content_refs(&database, &bounds) {
            Ok(references) => panic!(
                "a row past its payload byte bound must refuse, but the enumeration returned {} references",
                references.len()
            ),
            Err(error) => error,
        };
        assert!(
            matches!(&error, ContentRefsError::BoundsExceeded { which, limit } if *which == "payload-byte" && *limit == 50),
            "the refusal must name the payload byte bound: {error}",
        );
    }

    #[test]
    fn duplicate_rows_past_the_row_bound_are_a_named_refusal() {
        let root = scratch("row-bound");
        let database = ready_database(&root);
        insert_content_row(&database, "evt-crf-r1", 0, &valid_row());
        insert_content_row(&database, "evt-crf-r2", 1, &valid_row());
        insert_content_row(&database, "evt-crf-r3", 2, &valid_row());
        insert_content_row(&database, "evt-crf-r4", 3, &valid_row());
        insert_content_row(&database, "evt-crf-r5", 4, &valid_row());
        let bounds = match CarryEnumerationBounds::new(4, 1024 * 1024, 256 * 1024 * 1024, 1024) {
            Some(bounds) => bounds,
            None => panic!("a nonzero test bound must construct"),
        };
        let error = match carried_content_refs(&database, &bounds) {
            Ok(references) => panic!(
                "duplicate rows past the row bound must refuse, but the enumeration returned {} references",
                references.len()
            ),
            Err(error) => error,
        };
        assert!(
            matches!(&error, ContentRefsError::BoundsExceeded { which, limit } if *which == "row" && *limit == 4),
            "the refusal must name the row bound: {error}",
        );
    }

    #[test]
    fn cumulative_payload_bytes_past_the_ceiling_are_a_named_refusal() {
        let root = scratch("cumulative-bound");
        let database = ready_database(&root);
        let row = valid_row();
        insert_content_row(&database, "evt-crf-c1", 0, &row);
        insert_content_row(&database, "evt-crf-c2", 1, &row);
        let one_row = row.len() as u64;
        let bounds = match CarryEnumerationBounds::new(10_000, one_row + 1024, one_row, 1024) {
            Some(bounds) => bounds,
            None => panic!("a nonzero test bound must construct"),
        };
        let error = match carried_content_refs(&database, &bounds) {
            Ok(references) => panic!(
                "cumulative payload bytes past the ceiling must refuse, but the enumeration returned {} references",
                references.len()
            ),
            Err(error) => error,
        };
        let (which, limit) = match &error {
            ContentRefsError::BoundsExceeded { which, limit } => (which, limit),
            other => panic!("the refusal must be the cumulative byte bound: {other:?}"),
        };
        assert_eq!(
            *which, "cumulative-payload-byte",
            "the refusal must name the cumulative payload byte bound"
        );
        assert_eq!(*limit, one_row, "the refusal must name the ceiling value");
    }

    #[test]
    fn a_duplicate_at_the_exact_item_cap_is_not_refused() {
        let root = scratch("exact-cap");
        let database = ready_database(&root);
        insert_content_row(&database, "evt-crf-e1", 0, &row_with(&"a".repeat(64), 7));
        insert_content_row(&database, "evt-crf-e2", 1, &row_with(&"c".repeat(64), 9));
        insert_content_row(&database, "evt-crf-e3", 2, &row_with(&"e".repeat(64), 11));
        insert_content_row(&database, "evt-crf-e4", 3, &row_with(&"a".repeat(64), 7));
        let bounds = match CarryEnumerationBounds::new(10_000, 1024 * 1024, 256 * 1024 * 1024, 3) {
            Some(bounds) => bounds,
            None => panic!("a nonzero test bound must construct"),
        };
        let references = match carried_content_refs(&database, &bounds) {
            Ok(references) => references,
            Err(error) => panic!("a duplicate at the exact item cap must admit: {error}"),
        };
        assert_eq!(
            references.len(),
            3,
            "the item cap counts distinct references, and a duplicate at the cap is admitted"
        );
    }

    const UNRELATED_POPULATION: i64 = 25_000;

    /// The rows the plan witnesses run against: one unrelated aggregate with
    /// a long, non-carry history. The volume is what makes the plan
    /// assertion stable - against a small table SQLite may choose to scan,
    /// and a scan that only loses on a small table is still the defect,
    /// because the table grows without bound while the carry kind stays
    /// rare.
    fn populate_unrelated_population(database: &Path, rows: i64) {
        let mut connection = match rusqlite::Connection::open(database) {
            Ok(connection) => connection,
            Err(error) => panic!("test database open failed: {error}"),
        };
        let transaction = match connection.transaction() {
            Ok(transaction) => transaction,
            Err(error) => panic!("population transaction failed: {error}"),
        };
        if let Err(error) = transaction.execute(
            "INSERT OR IGNORE INTO event_sequence (aggregate_family, aggregate_id, seq, owner_id) VALUES ('crf-noise', 'crf-noise', 0, NULL)",
            [],
        ) {
            panic!("population sequence insert failed: {error}");
        }
        let mut statement = match transaction.prepare(
            "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
             VALUES (?1, 'crf-noise', 'crf-noise', ?2, 0, 'session.message.appended.1', '{}', 1)",
        ) {
            Ok(statement) => statement,
            Err(error) => panic!("population insert must prepare: {error}"),
        };
        for seq in 0..rows {
            if let Err(error) =
                statement.execute(rusqlite::params![format!("evt-crf-noise-{seq}"), seq])
            {
                panic!("unrelated population insert failed at {seq}: {error}");
            }
        }
        drop(statement);
        if let Err(error) = transaction.commit() {
            panic!("population commit failed: {error}");
        }
    }

    /// The exact plan the enumeration's query gets on this database: one
    /// detail line per plan row, as SQLite renders it.
    fn query_plan_lines(database: &Path) -> Vec<String> {
        let connection = match rusqlite::Connection::open(database) {
            Ok(connection) => connection,
            Err(error) => panic!("test database open failed: {error}"),
        };
        let mut statement =
            match connection.prepare(&format!("EXPLAIN QUERY PLAN {CARRY_ENUMERATION_QUERY}")) {
                Ok(statement) => statement,
                Err(error) => panic!("the carry query plan must prepare: {error}"),
            };
        let rows = match statement.query_map(rusqlite::params![CARRY_EVENT_TYPE, 10i64], |row| {
            row.get::<_, String>(3)
        }) {
            Ok(rows) => rows,
            Err(error) => panic!("the carry query plan must run: {error}"),
        };
        let mut lines = Vec::new();
        for line in rows {
            match line {
                Ok(detail) => lines.push(detail),
                Err(error) => panic!("the carry query plan must read: {error}"),
            }
        }
        lines
    }

    fn index_columns(database: &Path, index: &str) -> Vec<String> {
        let connection = match rusqlite::Connection::open(database) {
            Ok(connection) => connection,
            Err(error) => panic!("test database open failed: {error}"),
        };
        let mut statement = match connection.prepare(&format!("PRAGMA index_info(\"{index}\")")) {
            Ok(statement) => statement,
            Err(error) => panic!("the index must inspect: {error}"),
        };
        let rows = match statement.query_map([], |row| row.get::<_, Option<String>>(2)) {
            Ok(rows) => rows,
            Err(error) => panic!("the index must inspect: {error}"),
        };
        let mut columns = Vec::new();
        for column in rows {
            match column {
                Ok(Some(name)) => columns.push(name),
                Ok(None) => panic!("the index must carry named columns, not expressions"),
                Err(error) => panic!("the index columns must read: {error}"),
            }
        }
        columns
    }

    #[test]
    fn a_fresh_database_searches_the_carry_kind_by_index() {
        let root = scratch("plan-fresh");
        let database = ready_database(&root);
        populate_unrelated_population(&database, UNRELATED_POPULATION);
        insert_content_row(&database, "evt-crf-p1", 0, &row_with(&"a".repeat(64), 7));
        insert_content_row(&database, "evt-crf-p2", 1, &row_with(&"c".repeat(64), 9));
        insert_content_row(&database, "evt-crf-p3", 2, &row_with(&"e".repeat(64), 11));
        let plan = query_plan_lines(&database);
        let plan_text = plan.join(" | ");
        assert!(
            plan.iter()
                .any(|line| line.contains("SEARCH") && line.contains("event_type_id_idx")),
            "the carry enumeration must SEARCH event_type_id_idx, plan: {plan_text}"
        );
        assert!(
            !plan.iter().any(|line| line.contains("SCAN event")),
            "the carry enumeration must not full-scan the event table, plan: {plan_text}"
        );
        let references = match carried_content_refs(&database, &test_bounds()) {
            Ok(references) => references,
            Err(error) => panic!("a valid carry must enumerate: {error}"),
        };
        assert_eq!(
            references.len(),
            3,
            "the three carried references must enumerate"
        );
    }

    #[test]
    fn the_migration_path_restores_the_carry_enumeration_index() {
        // CRF105 witness: the index and its journal row are both gone - the
        // exact state of a pre-fix existing database - and the production
        // migration path, not the fresh-database schema, must rebuild the
        // bounded access path. The grown unrelated history is installed
        // before the precondition, so the plan the migration restores is
        // asserted under the volume a live database actually carries.
        let migrations = match crate::migrations::ProductionMigrations::load() {
            Ok(migrations) => migrations,
            Err(error) => panic!("migration load failed: {error}"),
        };
        let index_migration = match migrations
            .as_slice()
            .iter()
            .find(|migration| migration.id().as_str() == "20260920_create_event_type_id_index")
        {
            Some(migration) => migration.id(),
            None => panic!("the production migration set must include the carry index migration"),
        };
        let root = scratch("plan-migration");
        let database = ready_database(&root);
        populate_unrelated_population(&database, UNRELATED_POPULATION);
        let connection = match rusqlite::Connection::open(&database) {
            Ok(connection) => connection,
            Err(error) => panic!("test database open failed: {error}"),
        };
        // `IF EXISTS`: the precondition asserts the ABSENT index (the
        // pre-fix state), it does not assume the fresh path left one -
        // under a migration that never creates it, the drop must still
        // stand and the witness must reach the plan assertion.
        if let Err(error) = connection.execute("DROP INDEX IF EXISTS event_type_id_idx", []) {
            panic!("the precondition index drop failed: {error}");
        }
        let removed = match connection.execute(
            "DELETE FROM rust_migration WHERE id = ?1",
            [index_migration.as_str()],
        ) {
            Ok(removed) => removed,
            Err(error) => panic!("the precondition journal delete failed: {error}"),
        };
        assert_eq!(
            removed, 1,
            "the precondition must remove exactly the index migration's journal row"
        );
        let present: i64 = match connection.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'event_type_id_idx'",
            [],
            |row| row.get(0),
        ) {
            Ok(present) => present,
            Err(error) => panic!("the precondition must be inspectable: {error}"),
        };
        assert_eq!(present, 0, "the precondition must leave the index absent");
        drop(connection);
        let backup = root.join("migration-backup");
        match crate::write::EventStore::migrate(&database, &backup, ENGINE_VERSION) {
            Ok(crate::migration::MigrationOutcome::Applied { .. }) => {}
            other => {
                panic!("the production migration path must apply the index migration: {other:?}")
            }
        }
        let plan = query_plan_lines(&database);
        let plan_text = plan.join(" | ");
        assert!(
            plan.iter().any(|line| line.contains("SEARCH") && line.contains("event_type_id_idx")),
            "the production migration must restore the bounded access path before the carry enumeration can search it; plan: {plan_text}"
        );
        assert!(
            !plan.iter().any(|line| line.contains("SCAN event")),
            "the migrated access path must not full-scan the event table, plan: {plan_text}"
        );
        assert_eq!(
            index_columns(&database, "event_type_id_idx"),
            ["type", "id"],
            "the index must carry exactly the query's predicate and ordering columns"
        );
    }

    #[test]
    fn open_refuses_a_wrong_shaped_carry_enumeration_index() {
        // A same-named index that does not carry the declared column list
        // would be honoured silently by `CREATE INDEX IF NOT EXISTS`, so
        // the open verifies the shape and refuses - the established
        // discipline for every required index in this store.
        let root = scratch("plan-shape");
        let database = ready_database(&root);
        let connection = match rusqlite::Connection::open(&database) {
            Ok(connection) => connection,
            Err(error) => panic!("test database open failed: {error}"),
        };
        if let Err(error) = connection.execute_batch(
            "DROP INDEX event_type_id_idx;
             CREATE INDEX event_type_id_idx ON event (type);",
        ) {
            panic!("the wrong-shaped namesake must install: {error}");
        }
        drop(connection);
        match crate::write::EventStore::open(&database, ENGINE_VERSION) {
            Err(crate::write::WriteError::IndexShapeMismatch {
                index,
                found,
                expected,
                ..
            }) => {
                assert_eq!(index, "event_type_id_idx");
                assert_eq!(found, ["type"]);
                assert_eq!(expected, ["type", "id"]);
            }
            other => panic!(
                "a same-named carry enumeration index over the wrong columns must refuse the open, got {other:?}"
            ),
        }
    }
}
