//! Bounded newest-first reads retain the same verified recipient and original rows.
use super::*;

#[derive(Debug)]
pub struct RemoteMailboxRecentPage {
    pub records: Vec<RemoteMailboxRecord>,
    pub next_before: Option<MailboxAdmissionSeq>,
    pub decoded_bytes: usize,
    /// The newest row this page scanned, in any scope: a pass that starts at
    /// the newest row records it as its high-water mark.
    pub top: Option<MailboxAdmissionSeq>,
}

impl EventStore {
    pub fn read_recent_remote_mailbox_for_binding(
        &self,
        expected: &RemoteMailboxScope,
        before: Option<MailboxAdmissionSeq>,
        limit: usize,
        max_bytes: usize,
    ) -> Result<RemoteMailboxRecentPage, WriteError> {
        self.read_recent_remote_mailbox_for_binding_rows(
            expected,
            before,
            limit,
            max_bytes,
            HistoryRows::All,
        )
    }

    /// The same newest-first page over unconsumed rows only, for a scan that
    /// selects wake candidates. Consumed history is never read or decoded,
    /// and the candidates it returns are not hash-verified here: a claim
    /// re-reads every selected row with full verification before consuming it.
    pub fn read_recent_open_remote_mailbox_for_binding(
        &self,
        expected: &RemoteMailboxScope,
        before: Option<MailboxAdmissionSeq>,
        limit: usize,
        max_bytes: usize,
    ) -> Result<RemoteMailboxRecentPage, WriteError> {
        self.read_recent_remote_mailbox_for_binding_rows(
            expected,
            before,
            limit,
            max_bytes,
            HistoryRows::OpenScan,
        )
    }

    /// The newest-first page over the rows `rows` names. A candidate read
    /// without re-hashing is re-read in full by whatever acts on it.
    pub fn read_recent_remote_mailbox_for_binding_rows(
        &self,
        expected: &RemoteMailboxScope,
        before: Option<MailboxAdmissionSeq>,
        limit: usize,
        max_bytes: usize,
        rows: HistoryRows,
    ) -> Result<RemoteMailboxRecentPage, WriteError> {
        self.read_recent_remote_mailbox_above(
            expected,
            before,
            MailboxCursor::START,
            limit,
            max_bytes,
            rows,
        )
    }

    /// The same newest-first page, reading only rows strictly above `floor`.
    pub fn read_recent_remote_mailbox_above(
        &self,
        expected: &RemoteMailboxScope,
        before: Option<MailboxAdmissionSeq>,
        floor: MailboxCursor,
        limit: usize,
        max_bytes: usize,
        rows: HistoryRows,
    ) -> Result<RemoteMailboxRecentPage, WriteError> {
        if limit == 0 || max_bytes == 0 {
            return Err(refusal("remote page bounds must be positive"));
        }
        let limit = limit.min(MAX_MAILBOX_PAGE_MESSAGES);
        let upper = before
            .map(|seq| {
                i64::try_from(seq.value().saturating_sub(1))
                    .map_err(|_| refusal("remote page cursor exceeds SQL bound"))
            })
            .transpose()?
            .unwrap_or(i64::MAX);
        let (sql, check) = rows.select(true);
        let mut statement = self.connection().prepare_cached(&sql)?;
        let mut found = statement.query(rusqlite::params![
            expected.recipient.as_str(),
            upper,
            i64::try_from(limit + 1).map_err(|_| refusal("remote page count exceeds SQL bound"))?,
            i64::try_from(floor.value())
                .map_err(|_| refusal("remote page floor exceeds SQL bound"))?
        ])?;
        let mut records = Vec::new();
        let mut scanned = 0;
        let mut decoded_bytes = 0usize;
        let mut last = None;
        let mut top = None;
        let mut more = false;
        while let Some(row) = found.next()? {
            let stored = bounded_row(row)?;
            let (_, seq) = decode_row_checked(&stored, check)?;
            if scanned == limit
                || decoded_bytes.saturating_add(stored.record_bytes.len()) > max_bytes
            {
                if scanned == 0 {
                    return Err(refusal(
                        "one recent remote row exceeds the current page byte bound",
                    ));
                }
                more = true;
                break;
            }
            scanned += 1;
            decoded_bytes += stored.record_bytes.len();
            last = Some(seq);
            top = top.or(Some(seq));
            if stored.record_bytes.starts_with(TAG) {
                let record = read_record_budgeted(self.connection(), &stored, None, check)?;
                if expected.same_recipient_binding(record.scope()) {
                    records.push(record);
                }
            }
        }
        Ok(RemoteMailboxRecentPage {
            records,
            next_before: if more { last } else { None },
            decoded_bytes,
            top,
        })
    }
}
