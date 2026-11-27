//! The bounded capture page behind an interest-bound cursor.
//!
//! Every durable
//! `session` event the read posture can see, served in `(aggregate_id, seq)`
//! order behind the `CaptureCursor` the reader carries forward. The cursor's
//! exclusion law: a served row is never re-served — the page query excludes
//! every stored row whose sequence is at or before the sequence the cursor
//! already holds for that aggregate.
//!
//! Absent is a fourth state. A missing projection table yields an empty page
//! without continuation; an unreadable store refuses with a named
//! `UnreadableStore`; a row the read posture cannot decode refuses the whole
//! page as a named `CorruptRow`. The page is bounded in rows and in raw
//! stored bytes, and the byte bound never refuses a page's first row.

use grapt_event::{decode_for_read, KindError, LegacyDataLimit, LegacyEventRecord, ReadOutcome};
use grapt_types::{
    CaptureCoordinate, CaptureCursor, CaptureCursorError, CaptureInterest, EventFencing, EventSeq,
    SessionId, WriterEpoch,
};
use rusqlite::{params, Connection, Row};
use thiserror::Error;

use crate::decode::DecodedEvent;
use crate::EventStore;

/// The hard row cap for one capture page.
pub const MAX_CAPTURE_PAGE_ROWS: usize = 64;

/// The hard bound on the raw stored bytes served in one capture page,
/// measured on the stored `data` text as it was committed.
pub const MAX_CAPTURE_PAGE_BYTES: usize = 262_144;

/// EVR1 — the cursor exclusion law. This literal occurs exactly once in this
/// file, and is spliced into both page queries at fetch time: a row whose
/// stored sequence is at or before the coordinate's sequence is excluded, so
/// a served row is never re-served.
const CURSOR_EXCLUSION: &str = " AND NOT EXISTS (SELECT 1 FROM capture_cursor_exclusion c WHERE c.aggregate_id = e.aggregate_id AND e.seq <= c.seq)";

const ALL_SQL_HEAD: &str = "SELECT e.id, e.aggregate_id, e.seq, e.created, e.type, e.data, e.writer_epoch FROM event AS e WHERE e.aggregate_family = 'session'";
const ALL_SQL_TAIL: &str = " ORDER BY e.aggregate_id, e.seq LIMIT ?1";
const LOCATION_SQL_HEAD: &str = "SELECT e.id, e.aggregate_id, e.seq, e.created, e.type, e.data, e.writer_epoch FROM event AS e JOIN session_list_projection AS s ON s.session_id = e.aggregate_id WHERE e.aggregate_family = 'session' AND s.directory = ?1 AND (?2 IS NULL OR s.workspace_id = ?3)";
const LOCATION_SQL_TAIL: &str = " ORDER BY e.aggregate_id, e.seq LIMIT ?4";
const ALL_COUNT_SQL: &str =
    "SELECT COUNT(DISTINCT e.aggregate_id) FROM event AS e WHERE e.aggregate_family = 'session'";
const LOCATION_COUNT_SQL: &str = "SELECT COUNT(DISTINCT e.aggregate_id) FROM event AS e JOIN session_list_projection AS s ON s.session_id = e.aggregate_id WHERE e.aggregate_family = 'session' AND s.directory = ?1 AND (?2 IS NULL OR s.workspace_id = ?3)";

/// A refused capture page, with the refusal named so a reader can tell its
/// cursor's law from a corrupt row from an unreadable store.
#[derive(Debug, Error)]
pub enum CaptureReadError {
    /// The carried cursor failed its own law.
    #[error("cursor refused: {0}")]
    Cursor(#[from] CaptureCursorError),
    /// A row the read posture cannot decode; the whole page is refused.
    #[error("corrupt row {id}: {detail}")]
    CorruptRow { id: String, detail: String },
    /// The store itself could not be read: distinct from absent and from
    /// corrupt, never collapsed into either.
    #[error("store unreadable: {detail}")]
    UnreadableStore { detail: String },
    /// A zero page limit is a caller defect, not an empty page.
    #[error("capture page limit must not be zero")]
    PageLimitZero,
}

/// One row of a capture page: a decoded event, or a typed skip for a kind
/// the read posture does not know. Both carry the durable coordinate the
/// cursor advances over.
#[derive(Debug, Clone)]
pub enum CaptureRow {
    Event {
        coordinate: CaptureCoordinate,
        event: DecodedEvent,
    },
    SkippedUnknownKind {
        coordinate: CaptureCoordinate,
        kind: KindError,
    },
}

impl CaptureRow {
    /// The durable coordinate this row occupies, skip or not.
    pub fn coordinate(&self) -> &CaptureCoordinate {
        match self {
            CaptureRow::Event { coordinate, .. } => coordinate,
            CaptureRow::SkippedUnknownKind { coordinate, .. } => coordinate,
        }
    }
}

/// One bounded page of durable capture: the rows served, the cursor the
/// reader carries forward when more may remain, and the raw stored bytes
/// served in this page.
#[derive(Debug, Clone)]
pub struct CapturePage {
    pub rows: Vec<CaptureRow>,
    pub continuation: Option<CaptureCursor>,
    pub page_bytes: usize,
}

struct StoredRow {
    id: String,
    aggregate_id: String,
    seq: i64,
    created: i64,
    kind: String,
    data: Option<String>,
    writer_epoch: Option<i64>,
}

fn stored_row(row: &Row<'_>) -> rusqlite::Result<StoredRow> {
    Ok(StoredRow {
        id: row.get(0)?,
        aggregate_id: row.get(1)?,
        seq: row.get(2)?,
        created: row.get(3)?,
        kind: row.get(4)?,
        data: row.get(5)?,
        writer_epoch: row.get(6)?,
    })
}

fn corrupt_row(id: String, detail: String) -> CaptureReadError {
    CaptureReadError::CorruptRow { id, detail }
}

/// Stages the carried coordinates into the per-connection exclusion table.
/// The table is always created and reset: the page SQL references it
/// unconditionally, so even a start cursor must have a table to exclude
/// from, and a leftover row from a previous read can never leak into this
/// one.
fn prepare_exclusion(
    conn: &Connection,
    coordinates: &[CaptureCoordinate],
) -> Result<(), CaptureReadError> {
    let batch = "CREATE TEMP TABLE IF NOT EXISTS capture_cursor_exclusion (
        aggregate_id TEXT NOT NULL,
        seq INTEGER NOT NULL
    );
    DELETE FROM capture_cursor_exclusion;";
    conn.execute_batch(batch)
        .map_err(|error| CaptureReadError::UnreadableStore {
            detail: error.to_string(),
        })?;
    let mut insert = conn
        .prepare("INSERT INTO capture_cursor_exclusion (aggregate_id, seq) VALUES (?1, ?2)")
        .map_err(|error| CaptureReadError::UnreadableStore {
            detail: error.to_string(),
        })?;
    for coordinate in coordinates {
        let seq = i64::try_from(coordinate.seq.value()).unwrap_or(i64::MAX);
        insert
            .execute(params![coordinate.aggregate.as_str(), seq])
            .map_err(|error| CaptureReadError::UnreadableStore {
                detail: error.to_string(),
            })?;
    }
    Ok(())
}

/// Verifies every carried coordinate against the live store before the page
/// runs. Under a location interest, each coordinate must still name a live
/// projection row in the bound location; under either interest, no
/// coordinate may outrun its aggregate's durable head.
fn verify_coordinates(
    conn: &Connection,
    interest: &CaptureInterest,
    after: &CaptureCursor,
) -> Result<(), CaptureReadError> {
    for coordinate in after.coordinates() {
        if let CaptureInterest::Location {
            directory,
            workspace,
        } = interest
        {
            let located = conn
                .query_row(
                    "SELECT directory, workspace_id FROM session_list_projection WHERE session_id = ?1",
                    params![coordinate.aggregate.as_str()],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .ok();
            let matches = match located {
                Some((row_directory, row_workspace)) => {
                    row_directory == *directory
                        && workspace
                            .as_ref()
                            .is_none_or(|want| row_workspace.as_deref() == Some(want.as_str()))
                }
                None => false,
            };
            if !matches {
                return Err(CaptureReadError::Cursor(CaptureCursorError::Stale {
                    aggregate: coordinate.aggregate.clone(),
                }));
            }
        }
        let head = conn
            .query_row(
                "SELECT seq FROM event_sequence WHERE aggregate_family = 'session' AND aggregate_id = ?1",
                params![coordinate.aggregate.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .ok();
        match head {
            None => {
                return Err(CaptureReadError::Cursor(
                    CaptureCursorError::FutureCoordinate {
                        aggregate: coordinate.aggregate.clone(),
                        seq: coordinate.seq,
                        head: None,
                    },
                ))
            }
            Some(stored) if stored < 0 => {
                return Err(CaptureReadError::UnreadableStore {
                    detail: format!("stored sequence for {} is negative", coordinate.aggregate),
                })
            }
            Some(stored) if coordinate.seq.value() > stored as u64 => {
                return Err(CaptureReadError::Cursor(
                    CaptureCursorError::FutureCoordinate {
                        aggregate: coordinate.aggregate.clone(),
                        seq: coordinate.seq,
                        head: Some(EventSeq::new(stored as u64)),
                    },
                ))
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// Serves one bounded capture page: the durable `session` rows the read
/// posture can see under `interest`, after everything the carried `after`
/// cursor already served, capped at `limit` rows and the page byte bound.
///
/// The page refuses a zero limit, a coordinate the store no longer holds,
/// and any row it cannot decode; a missing projection table under a location
/// interest is absent, and answers with an empty page.
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(crate) fn read_capture_page(
    conn: &Connection,
    interest: &CaptureInterest,
    after: &CaptureCursor,
    limit: usize,
) -> Result<CapturePage, CaptureReadError> {
    if limit == 0 {
        return Err(CaptureReadError::PageLimitZero);
    }
    let (directory, workspace) = match interest {
        CaptureInterest::All => (None, None),
        CaptureInterest::Location {
            directory,
            workspace,
        } => (Some(directory.clone()), workspace.clone()),
    };
    if directory.is_some() && !projection_table_available(conn)? {
        // Absent, not unreadable: the projection table is missing, so this
        // location interest matches nothing at all.
        return Ok(CapturePage {
            rows: Vec::new(),
            continuation: None,
            page_bytes: 0,
        });
    }
    prepare_exclusion(conn, after.coordinates())?;
    verify_coordinates(conn, interest, after)?;
    let fetch = match limit.checked_add(1) {
        Some(count) => i64::try_from(count).unwrap_or(i64::MAX),
        None => i64::MAX,
    };
    let fetched: Vec<StoredRow> = match &directory {
        None => {
            let sql = format!("{ALL_SQL_HEAD}{CURSOR_EXCLUSION}{ALL_SQL_TAIL}");
            let mut stmt =
                conn.prepare(&sql)
                    .map_err(|error| CaptureReadError::UnreadableStore {
                        detail: error.to_string(),
                    })?;
            let rows = stmt
                .query_map(params![fetch], stored_row)
                .map_err(|error| CaptureReadError::UnreadableStore {
                    detail: error.to_string(),
                })?;
            rows.collect::<Result<Vec<StoredRow>, _>>()
                .map_err(|error| CaptureReadError::UnreadableStore {
                    detail: error.to_string(),
                })?
        }
        Some(directory) => {
            let sql = format!("{LOCATION_SQL_HEAD}{CURSOR_EXCLUSION}{LOCATION_SQL_TAIL}");
            let mut stmt =
                conn.prepare(&sql)
                    .map_err(|error| CaptureReadError::UnreadableStore {
                        detail: error.to_string(),
                    })?;
            let rows = stmt
                .query_map(
                    params![directory.as_str(), workspace.clone(), workspace, fetch],
                    stored_row,
                )
                .map_err(|error| CaptureReadError::UnreadableStore {
                    detail: error.to_string(),
                })?;
            rows.collect::<Result<Vec<StoredRow>, _>>()
                .map_err(|error| CaptureReadError::UnreadableStore {
                    detail: error.to_string(),
                })?
        }
    };
    let mut rows: Vec<CaptureRow> = Vec::new();
    let mut served: Vec<CaptureCoordinate> = Vec::new();
    let mut page_bytes = 0usize;
    let mut more = false;
    for stored in fetched {
        let (event, skip) = match decode_for_read(
            &stored.id,
            &stored.aggregate_id,
            stored.seq,
            stored.created,
            &stored.kind,
            stored.data.as_deref().unwrap_or(""),
            LegacyDataLimit::MEASURED_DEFAULT,
        ) {
            Ok(ReadOutcome::Decoded(record)) => (Some(build_decoded(&record)), None),
            Ok(ReadOutcome::SkippedUnknownKind { error, .. }) => (None, Some(error)),
            Err(error) => return Err(corrupt_row(stored.id, error.to_string())),
        };
        if rows.len() == limit {
            more = true;
            break;
        }
        let data_bytes = stored.data.as_ref().map_or(0, String::len);
        if page_bytes.saturating_add(data_bytes) > MAX_CAPTURE_PAGE_BYTES && !rows.is_empty() {
            more = true;
            break;
        }
        let aggregate = match stored.aggregate_id.parse::<SessionId>() {
            Ok(aggregate) => aggregate,
            Err(_) => {
                return Err(corrupt_row(
                    stored.id,
                    "aggregate id is not a session identity".to_owned(),
                ))
            }
        };
        let seq = match u64::try_from(stored.seq) {
            Ok(value) => EventSeq::new(value),
            Err(_) => {
                return Err(corrupt_row(
                    stored.id,
                    "durable sequence is negative".to_owned(),
                ))
            }
        };
        let fencing = match stored.writer_epoch {
            None => EventFencing::LegacyUnfenced,
            Some(raw) => match u32::try_from(raw).ok().and_then(WriterEpoch::new) {
                Some(epoch) => EventFencing::Fenced { epoch },
                None => {
                    return Err(corrupt_row(
                        stored.id,
                        "writer epoch is not a valid elected epoch".to_owned(),
                    ))
                }
            },
        };
        let coordinate = CaptureCoordinate {
            aggregate,
            seq,
            fencing,
        };
        served.push(coordinate.clone());
        page_bytes += data_bytes;
        rows.push(match (event, skip) {
            (Some(event), None) => CaptureRow::Event { coordinate, event },
            (None, Some(kind)) => CaptureRow::SkippedUnknownKind { coordinate, kind },
            (Some(_), Some(_)) => {
                return Err(corrupt_row(
                    stored.id,
                    "decode returned two outcomes".to_owned(),
                ))
            }
            (None, None) => {
                return Err(corrupt_row(
                    stored.id,
                    "decode returned no outcome".to_owned(),
                ))
            }
        });
    }
    let continuation = if more {
        Some(build_continuation(after, &served)?)
    } else {
        None
    };
    Ok(CapturePage {
        rows,
        continuation,
        page_bytes,
    })
}

/// Counts the distinct aggregates the interest can see. The bound check
/// runs upstream of the stream: an interest wider than the cursor's
/// coordinate cap refuses up front instead of wedging mid-replay. A
/// missing projection table is absent, and answers zero.
pub(crate) fn count_capture_aggregates(
    conn: &Connection,
    interest: &CaptureInterest,
) -> Result<u64, CaptureReadError> {
    let (directory, workspace) = match interest {
        CaptureInterest::All => (None, None),
        CaptureInterest::Location {
            directory,
            workspace,
        } => (Some(directory.clone()), workspace.clone()),
    };
    if directory.is_some() && !projection_table_available(conn)? {
        // Absent is zero: no projection table means the location interest
        // matches nothing at all.
        return Ok(0);
    }
    let count: i64 = match &directory {
        None => conn
            .query_row(ALL_COUNT_SQL, [], |row| row.get(0))
            .map_err(|error| CaptureReadError::UnreadableStore {
                detail: error.to_string(),
            })?,
        Some(directory) => conn
            .query_row(
                LOCATION_COUNT_SQL,
                params![directory.as_str(), workspace.clone(), workspace],
                |row| row.get(0),
            )
            .map_err(|error| CaptureReadError::UnreadableStore {
                detail: error.to_string(),
            })?,
    };
    u64::try_from(count).map_err(|_| CaptureReadError::UnreadableStore {
        detail: "aggregate count is negative".to_owned(),
    })
}

/// Advances the carried cursor over the coordinates this page served,
/// keeping the set sorted and holding at most one coordinate per aggregate.
fn build_continuation(
    after: &CaptureCursor,
    served: &[CaptureCoordinate],
) -> Result<CaptureCursor, CaptureCursorError> {
    let mut held: Vec<CaptureCoordinate> = after.coordinates().to_vec();
    for coordinate in served {
        match held
            .iter()
            .position(|held| held.aggregate == coordinate.aggregate)
        {
            Some(index) => held[index] = coordinate.clone(),
            None => held.push(coordinate.clone()),
        }
    }
    held.sort_by(|left, right| left.aggregate.cmp(&right.aggregate));
    CaptureCursor::new(after.interest().clone(), held)
}

/// Lifts a decoded legacy record into the store's read-side view.
fn build_decoded(record: &LegacyEventRecord) -> DecodedEvent {
    DecodedEvent {
        id: record.id().clone(),
        aggregate_id: record.aggregate().clone(),
        seq: record.seq(),
        created_ms: record.created_ms(),
        kind: record.kind(),
        data: record.data().clone(),
        canonical_hash: record.derived_identity_hash().to_owned(),
    }
}

/// Whether the session-list projection table exists at all. A fresh governed
/// open creates it empty, so a missing table means a hand-rolled database —
/// absent, refused as such rather than as unreadable.
fn projection_table_available(conn: &Connection) -> Result<bool, CaptureReadError> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'session_list_projection'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| CaptureReadError::UnreadableStore {
            detail: error.to_string(),
        })?;
    Ok(count > 0)
}

impl EventStore {
    /// Serves one bounded capture page under an interest-bound cursor. This
    /// is the store's single governed read for the campaign's replay lane;
    /// the SQL it runs stays crate-private.
    pub fn read_capture_page(
        &self,
        interest: &CaptureInterest,
        after: &CaptureCursor,
        limit: usize,
    ) -> Result<CapturePage, CaptureReadError> {
        read_capture_page(self.connection(), interest, after, limit)
    }

    /// Counts the distinct aggregates the interest can see, so the API can
    /// refuse an interest wider than the cursor's coordinate bound before
    /// the stream exists. Absent answers zero; unreadable refuses.
    pub fn count_capture_aggregates(
        &self,
        interest: &CaptureInterest,
    ) -> Result<u64, CaptureReadError> {
        count_capture_aggregates(self.connection(), interest)
    }
}
