//! Decoding one session-list row. A row that breaks the projection law is
//! reported as corrupt here; the page decides to skip it rather than fail.
use super::keyset::After;
use super::projection::validate_location;
use super::request::{
    KeyDigest, NonNegativeMs, SessionListBoundary, SessionListError, SessionListItem,
    SessionListKey, SessionListProvenance,
};
use grapt_types::{ProjectId, SessionId, WorkspaceId};
use rusqlite::types::{Type, ValueRef};
use rusqlite::{Connection, OptionalExtension};

/// Column of the row's `rowid`, selected after every decoded column.
pub(super) const ROWID_COLUMN: usize = 13;
/// Column holding whether the row's key is not NULL.
pub(super) const KEYED_COLUMN: usize = 14;
/// Bytes of a stored key hashed at a time, so a key of any size is digested
/// whole in bounded memory.
const KEY_CHUNK_BYTES: usize = 64 * 1024;

/// One decoded row and the text bytes it adds to its page.
pub(super) fn decode(
    conn: &Connection,
    row: &rusqlite::Row<'_>,
) -> Result<(SessionListItem, usize), SessionListError> {
    let oversized: i64 = row
        .get(0)
        .map_err(|error| corrupt_sql("oversized marker", error))?;
    if oversized != 0 {
        return Err(SessionListError::ProjectionCorrupt {
            reason: "a selected row contains text over the configured row bound".to_owned(),
        });
    }
    let session_raw = required_text(row, 1, "session_id")?;
    // Parsed first, so every later reason names a valid identity.
    let session_id = stored_session(&session_raw)?;
    let project_raw = required_text(row, 2, "project_id")?;
    let workspace_raw: Option<String> = row
        .get(3)
        .map_err(|error| corrupt_sql("workspace_id", error))?;
    let parent_raw: Option<String> = row
        .get(4)
        .map_err(|error| corrupt_sql("parent_id", error))?;
    let directory = required_text(row, 5, "directory")?;
    let title = required_text(row, 6, "title")?;
    let updated_raw = integer(row, 7, "updated_ms")?;
    let provenance_kind = required_text(row, 8, "provenance_kind")?;
    let digest = required_text(row, 9, "provenance_digest")?;
    let cutover_seq = integer(row, 10, "cutover_seq")?;
    let watermark: Option<i64> = row
        .get(11)
        .map_err(|error| corrupt_sql("projection watermark", error))?;
    let head: Option<i64> = row
        .get(12)
        .map_err(|error| corrupt_sql("aggregate head", error))?;
    validate_watermark(&session_raw, cutover_seq, watermark, head)?;

    let project_id =
        ProjectId::new(project_raw.clone()).ok_or_else(|| SessionListError::ProjectionCorrupt {
            reason: format!("session {session_raw} has an empty project_id"),
        })?;
    let workspace_id = workspace_raw
        .map(|raw| {
            WorkspaceId::new(raw).ok_or_else(|| SessionListError::ProjectionCorrupt {
                reason: format!("session {session_raw} has an empty workspace_id"),
            })
        })
        .transpose()?;
    validate_location(&directory, workspace_id.as_ref()).map_err(|reason| {
        SessionListError::ProjectionCorrupt {
            reason: format!("session {session_raw} location: {reason}"),
        }
    })?;
    let parent_id = parent_raw
        .map(|raw| {
            raw.parse::<SessionId>()
                .map_err(|error| SessionListError::ProjectionCorrupt {
                    reason: format!("session {session_raw} parent_id: {error}"),
                })
        })
        .transpose()?;
    let updated_ms =
        NonNegativeMs::new(updated_raw).ok_or_else(|| SessionListError::ProjectionCorrupt {
            reason: format!("session {session_raw} has negative updated_ms"),
        })?;
    if digest.trim().is_empty() {
        return Err(SessionListError::ProjectionCorrupt {
            reason: format!("session {session_raw} has blank provenance digest"),
        });
    }
    let provenance = provenance(
        conn,
        &session_raw,
        &provenance_kind,
        digest,
        cutover_seq,
        updated_raw,
    )?;
    let row_bytes = session_raw.len()
        + project_raw.len()
        + workspace_id
            .as_ref()
            .map_or(0, |value| value.as_str().len())
        + parent_id.as_ref().map_or(0, |value| value.as_str().len())
        + directory.len()
        + title.len()
        + provenance_kind.len()
        + match &provenance {
            SessionListProvenance::LegacyBaseline { digest, .. }
            | SessionListProvenance::RustEvent { digest } => digest.len(),
        };
    let item = SessionListItem {
        key: SessionListKey::new(updated_ms, session_id),
        project_id,
        workspace_id,
        parent_id,
        directory,
        title,
        provenance,
    };
    Ok((item, row_bytes))
}

/// The row's provenance, which must agree with its immutable legacy baseline
/// when it claims one.
fn provenance(
    conn: &Connection,
    session_raw: &str,
    kind: &str,
    digest: String,
    cutover_seq: i64,
    updated_raw: i64,
) -> Result<SessionListProvenance, SessionListError> {
    Ok(match kind {
        "legacy-baseline" if cutover_seq >= -1 => {
            let baseline: Option<(i64, String, i64)> = conn
                .query_row(
                    "SELECT updated_ms, source_digest, cutover_seq
                     FROM session_list_legacy_baseline WHERE session_id = ?1",
                    [session_raw],
                    |baseline| Ok((baseline.get(0)?, baseline.get(1)?, baseline.get(2)?)),
                )
                .optional()
                .map_err(|source| SessionListError::StoreFailure { source })?;
            if baseline != Some((updated_raw, digest.clone(), cutover_seq)) {
                return Err(SessionListError::ProjectionCorrupt {
                    reason: format!("session {session_raw} baseline provenance does not match"),
                });
            }
            SessionListProvenance::LegacyBaseline {
                digest,
                cutover_seq,
            }
        }
        "rust-event" if cutover_seq == -1 => SessionListProvenance::RustEvent { digest },
        other => {
            return Err(SessionListError::ProjectionCorrupt {
                reason: format!(
                    "session {session_raw} has invalid provenance {other:?}/{cutover_seq}"
                ),
            })
        }
    })
}

/// The stored identity as a Session id. One that does not parse is named by
/// a bounded prefix and its length, never its whole text.
fn stored_session(raw: &str) -> Result<SessionId, SessionListError> {
    raw.parse::<SessionId>()
        .map_err(|error| SessionListError::ProjectionCorrupt {
            reason: format!(
                "session_id {:?}… ({} bytes): {error}",
                prefix(raw),
                raw.len()
            ),
        })
}

/// At most the first 64 bytes of `raw`, cut on a character boundary.
fn prefix(raw: &str) -> &str {
    let mut end = raw.len().min(64);
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    &raw[..end]
}

/// The session a corrupt row belongs to, when its identity is still readable.
pub(super) fn session_hint(row: &rusqlite::Row<'_>) -> Option<SessionId> {
    row.get::<_, Option<String>>(1).ok().flatten()?.parse().ok()
}

/// Where the row sits in the keyset, whatever else about it reads.
pub(super) fn after(row: &rusqlite::Row<'_>) -> Option<After> {
    Some(After::Row {
        rowid: row.get(ROWID_COLUMN).ok()?,
        keyed: row.get(KEYED_COLUMN).ok()?,
    })
}

/// What is stored at one `rowid`, for placing a cursor that names it.
pub(super) struct StoredKey {
    /// Whether its key is not NULL.
    pub(super) keyed: bool,
    /// The digest of its time and key.
    pub(super) digest: KeyDigest,
}

/// A position column of the projection, hashed into a [`KeyDigest`].
#[derive(Clone, Copy)]
enum Positioned {
    Time,
    Key,
}

impl Positioned {
    const fn sql(self) -> &'static str {
        match self {
            Self::Time => {
                "SELECT typeof(updated_ms),
                        CASE WHEN typeof(updated_ms) IN ('text', 'blob')
                             THEN octet_length(updated_ms) END,
                        CASE WHEN typeof(updated_ms) IN ('integer', 'real')
                             THEN updated_ms END
                   FROM session_list_projection WHERE rowid = ?1"
            }
            Self::Key => {
                "SELECT typeof(session_id),
                        CASE WHEN typeof(session_id) IN ('text', 'blob')
                             THEN octet_length(session_id) END,
                        CASE WHEN typeof(session_id) IN ('integer', 'real')
                             THEN session_id END
                   FROM session_list_projection WHERE rowid = ?1"
            }
        }
    }

    const fn column(self) -> &'static std::ffi::CStr {
        match self {
            Self::Time => c"updated_ms",
            Self::Key => c"session_id",
        }
    }
}

/// The row at `rowid` and the digest of its time and key: for each, its
/// SQLite type, byte length and every byte, text and blob values read in
/// [`KEY_CHUNK_BYTES`] chunks so a value of any size is hashed in bounded
/// memory. `None` when no row has that `rowid`.
pub(super) fn stored_key(
    conn: &Connection,
    rowid: i64,
) -> Result<Option<StoredKey>, rusqlite::Error> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let Some(_) = hash_column(conn, rowid, Positioned::Time, &mut hasher)? else {
        return Ok(None);
    };
    let Some(kind) = hash_column(conn, rowid, Positioned::Key, &mut hasher)? else {
        return Ok(None);
    };
    let mut digest = [0; 16];
    digest.copy_from_slice(&hasher.finalize()[..16]);
    Ok(Some(StoredKey {
        keyed: kind != "null",
        digest: KeyDigest(digest),
    }))
}

/// Hashes one column of the row at `rowid` and returns its SQLite type, or
/// `None` when no row has that `rowid`.
fn hash_column(
    conn: &Connection,
    rowid: i64,
    column: Positioned,
    hasher: &mut sha2::Sha256,
) -> Result<Option<String>, rusqlite::Error> {
    use sha2::Digest;
    let held = conn
        .query_row(column.sql(), [rowid], |row| {
            let number = match row.get_ref(2)? {
                ValueRef::Integer(value) => Some(value.to_be_bytes()),
                ValueRef::Real(value) => Some(value.to_be_bytes()),
                _ => None,
            };
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                number,
            ))
        })
        .optional()?;
    let Some((kind, length, number)) = held else {
        return Ok(None);
    };
    hasher.update(kind.as_bytes());
    hasher.update([0]);
    hasher.update(length.unwrap_or(-1).to_be_bytes());
    if let Some(bytes) = number {
        hasher.update(bytes);
    } else if length.is_some_and(|length| length > 0) {
        let value = conn.blob_open(
            rusqlite::MAIN_DB,
            c"session_list_projection",
            column.column(),
            rowid,
            true,
        )?;
        let mut chunk = vec![0; KEY_CHUNK_BYTES];
        let mut offset = 0;
        loop {
            let read = value.read_at(&mut chunk, offset)?;
            if read == 0 {
                break;
            }
            hasher.update(&chunk[..read]);
            offset += read;
        }
    }
    Ok(Some(kind))
}

/// The cursor position after the row at `rowid`, pinned to the time and key
/// it holds now. `None` when the row is gone.
pub(super) fn boundary_after(
    conn: &Connection,
    rowid: i64,
) -> Result<Option<SessionListBoundary>, rusqlite::Error> {
    Ok(stored_key(conn, rowid)?.map(|stored| SessionListBoundary::after_row(rowid, stored.digest)))
}

fn corrupt_sql(field: &'static str, error: rusqlite::Error) -> SessionListError {
    SessionListError::ProjectionCorrupt {
        reason: format!("{field} has the wrong SQLite type: {error}"),
    }
}

pub(super) fn required_text(
    row: &rusqlite::Row<'_>,
    index: usize,
    field: &'static str,
) -> Result<String, SessionListError> {
    row.get::<_, Option<String>>(index)
        .map_err(|error| corrupt_sql(field, error))?
        .ok_or_else(|| SessionListError::ProjectionCorrupt {
            reason: format!("{field} is absent or oversized"),
        })
}

pub(super) fn integer(
    row: &rusqlite::Row<'_>,
    index: usize,
    field: &'static str,
) -> Result<i64, SessionListError> {
    let value = row
        .get_ref(index)
        .map_err(|error| corrupt_sql(field, error))?;
    if value.data_type() != Type::Integer {
        return Err(SessionListError::ProjectionCorrupt {
            reason: format!(
                "{field} has SQLite type {:?}, expected INTEGER",
                value.data_type()
            ),
        });
    }
    match value {
        ValueRef::Integer(value) => Ok(value),
        _ => Err(SessionListError::ProjectionCorrupt {
            reason: format!("{field} changed SQLite type while decoding"),
        }),
    }
}

fn validate_watermark(
    session: &str,
    cutover: i64,
    watermark: Option<i64>,
    head: Option<i64>,
) -> Result<(), SessionListError> {
    if cutover < -1 {
        return Err(SessionListError::ProjectionCorrupt {
            reason: format!("session {session} has invalid cutover {cutover}"),
        });
    }
    if cutover >= 0 && watermark.is_none() {
        return Err(SessionListError::ProjectionCorrupt {
            reason: format!("session {session} is missing its projection watermark"),
        });
    }
    if let Some(watermark) = watermark {
        if watermark < cutover || head.is_none_or(|head| watermark > head) {
            return Err(SessionListError::ProjectionCorrupt {
                reason: format!(
                    "session {session} watermark {watermark} is outside cutover/head {cutover}/{head:?}"
                ),
            });
        }
    }
    Ok(())
}
