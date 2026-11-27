//! Permanent send-permit evidence, independent of reclaimable recovery receipts.
use crate::attempt_receipt::{
    decode_record, encode_record, AttemptReceiptRecord, AttemptRecordCeiling,
};
use crate::governed::GovernedTable;
use crate::read_snapshot::ReadSnapshot;
use crate::{GovernedTransaction, WriteError};
use grapt_types::SessionId;

#[cfg(test)]
mod tests;

pub(crate) fn ensure_schema(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    let installed: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='provider_attempt_coverage')",
        [], |row| row.get(0),
    )?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS provider_attempt_accounting (
            attempt_id TEXT PRIMARY KEY NOT NULL, session_id TEXT NOT NULL, record_bytes BLOB NOT NULL
         );
         CREATE TABLE IF NOT EXISTS provider_attempt_coverage (
            session_id TEXT PRIMARY KEY NOT NULL, complete INTEGER NOT NULL CHECK(complete IN (0,1))
         );",
    )?;
    // An upgrade cannot recover already reclaimed identities. Record that gap
    // once, atomically with installation; reopening must never clear it.
    if !installed {
        tx.execute_batch(
            "INSERT INTO provider_attempt_coverage(session_id, complete)
             SELECT aggregate_id, 0 FROM event WHERE aggregate_family='session'
             UNION SELECT session_id, 0 FROM attempt_receipt
             UNION SELECT session_id, 0 FROM attempt_receipt_quarantine;",
        )?;
    }
    Ok(())
}

pub(crate) fn ensure_index(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch("CREATE INDEX IF NOT EXISTS provider_attempt_accounting_session_idx ON provider_attempt_accounting(session_id, attempt_id);")
}

pub(crate) fn retain(
    tx: &GovernedTransaction<'_>,
    record: &AttemptReceiptRecord,
) -> Result<(), WriteError> {
    let receipt = record.receipt();
    let bytes = encode_record(record);
    let existing: Option<Option<Vec<u8>>> = tx.query_row(
        "SELECT CASE WHEN length(record_bytes)<=?2 THEN record_bytes END FROM provider_attempt_accounting WHERE attempt_id=?1",
        rusqlite::params![receipt.attempt().as_str(), bytes.len() as i64], |row| row.get(0),
    ).optional()?;
    if let Some(existing) = existing {
        if existing.as_deref() != Some(bytes.as_slice()) {
            return Err(WriteError::ReceiptConflict {
                attempt: receipt.attempt().as_str().into(),
            });
        }
        return Ok(());
    }
    tx.gate().execute(tx, GovernedTable::ProviderAttemptAccounting,
        "INSERT INTO provider_attempt_accounting(attempt_id, session_id, record_bytes) VALUES (?1,?2,?3)",
        rusqlite::params![receipt.attempt().as_str(), receipt.session().as_str(), bytes])?;
    tx.gate().execute(tx, GovernedTable::ProviderAttemptCoverage,
        "INSERT INTO provider_attempt_coverage(session_id, complete) VALUES (?1,1) ON CONFLICT(session_id) DO NOTHING",
        [receipt.session().as_str()])?;
    Ok(())
}

use rusqlite::OptionalExtension;

/// Bounded evidence from the same read transaction as the Session facts.
#[derive(Debug)]
pub struct ProviderAttemptAccounting {
    pub coverage_complete: bool,
    pub bytes_read: usize,
    pub attempts: Vec<AttemptReceiptRecord>,
}

pub fn read(
    source: &ReadSnapshot<'_>,
    session: &SessionId,
    ceiling: AttemptRecordCeiling,
    max_attempts: usize,
) -> Result<ProviderAttemptAccounting, String> {
    let installed: bool = source.prepare(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='provider_attempt_coverage')"
    ).map_err(|e| e.to_string())?.query_row([], |row| row.get(0)).map_err(|e| e.to_string())?;
    let receipt_table: bool = source.prepare(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='attempt_receipt')"
    ).map_err(|e| e.to_string())?.query_row([], |row| row.get(0)).map_err(|e| e.to_string())?;
    if !installed && !receipt_table {
        return Ok(ProviderAttemptAccounting {
            coverage_complete: false,
            bytes_read: 0,
            attempts: Vec::new(),
        });
    }
    let coverage: Option<i64> = if installed {
        source
            .prepare("SELECT complete FROM provider_attempt_coverage WHERE session_id=?1")
            .map_err(|e| e.to_string())?
            .query_row([session.as_str()], |row| row.get(0))
            .optional()
            .map_err(|e| e.to_string())?
    } else {
        None
    };
    if coverage.is_some_and(|value| value != 0 && value != 1) {
        return Err("invalid provider attempt coverage".into());
    }
    // Read each indexed source independently: a UNION sorted with its blobs
    // can materialize the entire Session before the row/byte limits are checked.
    let queries: Vec<&str> = if installed {
        vec![
            "SELECT CASE WHEN length(CAST(attempt_id AS BLOB))<=128 THEN attempt_id END,
                    CASE WHEN length(record_bytes)<=?2 THEN record_bytes END
             FROM provider_attempt_accounting WHERE session_id=?1 ORDER BY attempt_id LIMIT ?3",
            "SELECT CASE WHEN length(CAST(attempt_id AS BLOB))<=128 THEN attempt_id END,
                    CASE WHEN length(record_bytes)<=?2 THEN record_bytes END
             FROM attempt_receipt WHERE session_id=?1 AND request_sent_ms IS NOT NULL
               AND NOT EXISTS(SELECT 1 FROM provider_attempt_accounting a WHERE a.attempt_id=attempt_receipt.attempt_id)
             ORDER BY attempt_id LIMIT ?3",
        ]
    } else {
        vec!["SELECT CASE WHEN length(CAST(attempt_id AS BLOB))<=128 THEN attempt_id END,
                     CASE WHEN length(record_bytes)<=?2 THEN record_bytes END
              FROM attempt_receipt WHERE session_id=?1 AND request_sent_ms IS NOT NULL ORDER BY attempt_id LIMIT ?3"]
    };
    let limit = i64::try_from(max_attempts)
        .map_err(|e| e.to_string())?
        .checked_add(1)
        .ok_or("provider attempt limit overflow")?;
    let mut attempts = Vec::new();
    let mut bytes_read = 0_usize;
    for query in queries {
        let mut statement = source.prepare(query).map_err(|e| e.to_string())?;
        let mut rows = statement
            .query(rusqlite::params![session.as_str(), ceiling.get(), limit])
            .map_err(|e| e.to_string())?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            if attempts.len() == max_attempts {
                return Err("provider attempt accounting exceeds attempt bound".into());
            }
            let id: Option<String> = row.get(0).map_err(|e| e.to_string())?;
            let id = id.ok_or("provider attempt identity exceeds byte bound")?;
            let bytes: Option<Vec<u8>> = row.get(1).map_err(|e| e.to_string())?;
            let bytes = bytes.ok_or("provider attempt record exceeds byte bound")?;
            bytes_read = bytes_read
                .checked_add(bytes.len())
                .ok_or("accounting byte overflow")?;
            if bytes_read > 64 * 1024 * 1024 {
                return Err("provider attempt accounting exceeds 64 MiB".into());
            }
            let record = decode_record(&bytes, ceiling)?;
            if record.receipt().attempt().as_str() != id
                || record.receipt().session().as_str() != session.as_str()
                || record.receipt().request_sent_at().is_none()
                || record
                    .receipt()
                    .request_sent_at()
                    .is_some_and(|sent| sent < record.receipt().reserved_at())
            {
                return Err(
                    "provider attempt record disagrees with its send-permit identity".into(),
                );
            }
            attempts.push(record);
        }
    }
    Ok(ProviderAttemptAccounting {
        coverage_complete: coverage == Some(1),
        bytes_read,
        attempts,
    })
}
