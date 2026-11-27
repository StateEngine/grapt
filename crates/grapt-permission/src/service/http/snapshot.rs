//! Bounded immutable input to the existing owner-scoped HTTP row decoder.

use super::*;
use rusqlite::{types::ValueRef, Connection};

#[derive(Debug)]
struct SnapshotRow {
    subject: String,
    client: String,
    raw: RawHttpRow,
}

/// An accelerator, never permission authority. Over-capacity means unavailable,
/// so callers retain the ordinary durable read path rather than serve a subset.
#[derive(Debug)]
pub struct PermissionHttpSnapshot {
    rows: Vec<SnapshotRow>,
    allocation_bytes: usize,
}

impl PermissionHttpSnapshot {
    /// Checks borrowed SQLite values before allocating their owned copies.
    /// `None` represents capacity exhaustion; database errors stay distinguishable.
    pub fn read_bounded(
        connection: &Connection,
        max_rows: usize,
        max_bytes: usize,
    ) -> rusqlite::Result<Option<Self>> {
        if max_bytes < std::mem::size_of::<Self>() {
            return Ok(None);
        }
        let Some(limit) = max_rows
            .checked_add(1)
            .and_then(|rows| i64::try_from(rows).ok())
        else {
            return Ok(None);
        };
        let mut statement = connection.prepare(
            "SELECT p.id, p.session_id, p.action, p.status, p.suspended_reason,
                    h.agent_id, h.resources_json, h.save_json, h.metadata_json, h.source_json,
                    h.owner_subject, h.owner_client
             FROM permission_request p JOIN permission_http_request h ON h.id = p.id
             WHERE p.status IN ('pending', 'suspended') ORDER BY p.rowid DESC LIMIT ?1",
        )?;
        let mut cursor = statement.query([limit])?;
        let mut rows = Vec::new();
        let mut strings_bytes = 0usize;
        while let Some(row) = cursor.next()? {
            if rows.len() == max_rows {
                return Ok(None);
            }
            let mut borrowed_bytes = 0usize;
            for index in 0..12 {
                if let ValueRef::Text(bytes) = row.get_ref(index)? {
                    borrowed_bytes = borrowed_bytes.saturating_add(bytes.len());
                }
            }
            let required = std::mem::size_of::<Self>()
                .saturating_add((rows.len() + 1).saturating_mul(std::mem::size_of::<SnapshotRow>()))
                .saturating_add(strings_bytes)
                .saturating_add(borrowed_bytes);
            if required > max_bytes {
                return Ok(None);
            }
            rows.reserve_exact(1);
            let snapshot = SnapshotRow {
                subject: row.get(10)?,
                client: row.get(11)?,
                raw: (
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ),
            };
            let raw = &snapshot.raw;
            strings_bytes = strings_bytes
                .saturating_add(
                    [
                        &snapshot.subject,
                        &snapshot.client,
                        &raw.0,
                        &raw.1,
                        &raw.2,
                        &raw.3,
                        &raw.5,
                        &raw.6,
                    ]
                    .into_iter()
                    .map(|value| value.capacity())
                    .sum::<usize>(),
                )
                .saturating_add(
                    [&raw.4, &raw.7, &raw.8, &raw.9]
                        .into_iter()
                        .filter_map(Option::as_ref)
                        .map(String::capacity)
                        .sum::<usize>(),
                );
            rows.push(snapshot);
            let allocation_bytes = std::mem::size_of::<Self>()
                .saturating_add(
                    rows.capacity()
                        .saturating_mul(std::mem::size_of::<SnapshotRow>()),
                )
                .saturating_add(strings_bytes);
            if allocation_bytes > max_bytes {
                return Ok(None);
            }
        }
        let allocation_bytes = std::mem::size_of::<Self>()
            .saturating_add(
                rows.capacity()
                    .saturating_mul(std::mem::size_of::<SnapshotRow>()),
            )
            .saturating_add(strings_bytes);
        Ok(Some(Self {
            rows,
            allocation_bytes,
        }))
    }

    pub fn allocation_bytes(&self) -> usize {
        self.allocation_bytes
    }

    /// Identical owner, Session, ordering, decoder and lookahead to `list`.
    pub fn list(
        &self,
        owner: &PermissionOwner<'_>,
        session: Option<&SessionId>,
    ) -> Result<PermissionHttpPage, PermissionHttpError> {
        let mut requests = self
            .rows
            .iter()
            .filter(|row| {
                row.subject == owner.subject
                    && row.client == owner.client.as_str()
                    && session.is_none_or(|session| row.raw.1 == session.as_str())
            })
            .take(MAX_HTTP_PERMISSION_ROWS + 1)
            .map(|row| decode_row(row.raw.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        let has_more = requests.len() > MAX_HTTP_PERMISSION_ROWS;
        requests.truncate(MAX_HTTP_PERMISSION_ROWS);
        Ok(PermissionHttpPage { requests, has_more })
    }
}
