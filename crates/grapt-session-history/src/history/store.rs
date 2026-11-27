//! Bounded reads of the durable Session history.
use super::*;
use grapt_store::history_visibility::{HistoryVisibility, VisibilityError};
mod call_pages;
mod message;
mod usage;
type ToolCallPages = Vec<Result<SessionToolCallPage, HistoryReadError>>;
// The accounting reader owns these payloads. Assistant history still counts
// their rows, but never materializes their fingerprint arrays as conversation.
const REQUEST_CACHE_KIND: &str = "session.request.cache.1";

impl<'a> SessionHistoryStore<'a> {
    pub fn over(store: &'a grapt_store::EventStore) -> Self {
        Self {
            conn: store.connection(),
        }
    }

    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self { conn }
    }

    /// Read only the typed tool-call rows this continuation owns.
    ///
    /// This is deliberately not implemented by folding the Session's first
    /// 5,000 rows and counting tool items afterwards. A long conversation can
    /// have few tool calls, and the continuation owner must pay only for its
    /// own bounded page. Malformed or unclassifiable content rows are included
    /// in the probe and refused rather than disappearing behind the SQL
    /// predicate.
    pub fn tool_call_page(
        &self,
        session: &SessionId,
    ) -> Result<SessionToolCallPage, HistoryReadError> {
        self.tool_call_page_at(session, i64::MIN, None, None, MAX_TOOL_CALL_PAGE_ROWS, None)
    }

    /// Inclusive retained-history cut; the caller validates its committed descriptor.
    pub fn tool_call_page_from(
        &self,
        session: &SessionId,
        from: grapt_types::EventSeq,
    ) -> Result<SessionToolCallPage, HistoryReadError> {
        let from = i64::try_from(from.value()).map_err(|_| HistoryReadError::Corrupt {
            session: session.as_str().to_owned(),
            seq: i64::MAX,
            kind: "history boundary".to_owned(),
            reason: "sequence exceeds SQLite range".to_owned(),
        })?;
        self.tool_call_page_at(session, from, None, None, MAX_TOOL_CALL_PAGE_ROWS, None)
    }

    /// Read tool calls in the half-open retained window `[from, until)`.
    pub fn tool_call_page_window_from(
        &self,
        session: &SessionId,
        from: grapt_types::EventSeq,
        until: grapt_types::EventSeq,
    ) -> Result<SessionToolCallPage, HistoryReadError> {
        let convert = |value: grapt_types::EventSeq, kind: &str| {
            i64::try_from(value.value()).map_err(|_| HistoryReadError::Corrupt {
                session: session.as_str().to_owned(),
                seq: i64::MAX,
                kind: kind.to_owned(),
                reason: "sequence exceeds SQLite range".to_owned(),
            })
        };
        self.tool_call_page_at(
            session,
            convert(from, "history boundary")?,
            Some(convert(until, "history window")?),
            None,
            MAX_TOOL_CALL_PAGE_ROWS,
            None,
        )
    }

    /// Read tool calls in the half-open retained window [from, until) under
    /// explicit bounds. The compaction window is exactly the range the
    /// 128-row page refuses; compaction is what makes it fit.
    pub fn tool_call_page_window_from_bounded(
        &self,
        session: &SessionId,
        from: grapt_types::EventSeq,
        until: grapt_types::EventSeq,
        max_rows: usize,
        max_data_bytes: usize,
    ) -> Result<SessionToolCallPage, HistoryReadError> {
        let convert = |value: grapt_types::EventSeq, kind: &str| {
            i64::try_from(value.value()).map_err(|_| HistoryReadError::Corrupt {
                session: session.as_str().to_owned(),
                seq: i64::MAX,
                kind: kind.to_owned(),
                reason: "sequence exceeds SQLite range".to_owned(),
            })
        };
        self.tool_call_page_at(
            session,
            convert(from, "history boundary")?,
            Some(convert(until, "history window")?),
            None,
            max_rows,
            Some(max_data_bytes),
        )
    }

    /// Read tool calls before `until` when no retained cut exists.
    pub fn tool_call_page_window(
        &self,
        session: &SessionId,
        until: grapt_types::EventSeq,
    ) -> Result<SessionToolCallPage, HistoryReadError> {
        let until = i64::try_from(until.value()).map_err(|_| HistoryReadError::Corrupt {
            session: session.as_str().to_owned(),
            seq: i64::MAX,
            kind: "history window".to_owned(),
            reason: "sequence exceeds SQLite range".to_owned(),
        })?;
        self.tool_call_page_at(
            session,
            i64::MIN,
            Some(until),
            None,
            MAX_TOOL_CALL_PAGE_ROWS,
            None,
        )
    }

    /// Read one tool's history independently of the retained transcript window.
    pub fn tool_call_page_for(
        &self,
        session: &SessionId,
        call: &ToolCallId,
    ) -> Result<SessionToolCallPage, HistoryReadError> {
        self.tool_call_page_at(
            session,
            i64::MIN,
            None,
            Some(call),
            MAX_TOOL_CALL_PAGE_ROWS,
            None,
        )
    }

    /// Maximum number of exact tools in one ephemeral history batch.
    pub const CALL_BATCH_SIZE: usize = grapt_store::session_event_queries::batch::TARGETS;

    /// Read selected tools together, preserving each exact reader's page and
    /// refusal. `None` requests individual reads when the shared union exceeds
    /// one page or its byte allowance; it never means no matching calls.
    pub fn tool_call_pages_for(
        &self,
        session: &SessionId,
        calls: &[ToolCallId],
    ) -> Result<Option<ToolCallPages>, HistoryReadError> {
        call_pages::read(self.conn, session, calls)
    }

    fn tool_call_page_at(
        &self,
        session: &SessionId,
        from: i64,
        until: Option<i64>,
        call: Option<&ToolCallId>,
        max_rows: usize,
        max_data_bytes: Option<usize>,
    ) -> Result<SessionToolCallPage, HistoryReadError> {
        let store = |source| HistoryReadError::Store {
            session: session.as_str().to_owned(),
            source,
        };
        let select_data = match max_data_bytes {
            Some(limit) => {
                format!("CASE WHEN length(CAST(data AS BLOB)) <= {limit} THEN data ELSE NULL END")
            }
            None => "data".to_owned(),
        };
        let mut statement = self
            .conn
            .prepare(&call_pages::sql(
                &select_data,
                "= ?4",
                "",
                &visibility(self.conn, session)?.sql("seq"),
            ))
            .map_err(store)?;
        let probe = i64::try_from(max_rows.saturating_add(1)).unwrap_or(i64::MAX);
        let mapped = statement
            .query_map(
                rusqlite::params![
                    session.as_str(),
                    probe,
                    from,
                    call.map(ToolCallId::as_str),
                    until
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .map_err(store)?;
        let mut rows = Vec::new();
        for row in mapped {
            rows.push(row.map_err(store)?);
        }

        call_pages::assemble(session, rows, max_rows, max_data_bytes)
    }

    /// Folds this Session's durable rows into its message history.
    ///
    /// A log longer than [`MAX_HISTORY_ROWS`] is a typed refusal, never a
    /// silent truncation: a caller shown the first five thousand rows of a
    /// longer history would believe it had the whole transcript. Paging is a
    /// later contract; refusing is the truthful answer until it exists. The
    /// query reads one row MORE than the bound so "there are more" is
    /// OBSERVED rather than inferred from a full page.
    pub fn fold(&self, session: &SessionId) -> Result<MessageHistory, HistoryReadError> {
        self.fold_at(session, i64::MIN, None)
    }

    /// Inclusive retained-history cut. It must precede retained message openings;
    /// malformed selected history still follows the ordinary fold's refusals.
    pub fn fold_from(
        &self,
        session: &SessionId,
        from: grapt_types::EventSeq,
    ) -> Result<MessageHistory, HistoryReadError> {
        let from = i64::try_from(from.value()).map_err(|_| HistoryReadError::Corrupt {
            session: session.as_str().to_owned(),
            seq: i64::MAX,
            kind: "history boundary".to_owned(),
            reason: "sequence exceeds SQLite range".to_owned(),
        })?;
        self.fold_at(session, from, None)
    }

    /// Fold a half-open retained window `[from, until)`.
    pub fn fold_window_from(
        &self,
        session: &SessionId,
        from: grapt_types::EventSeq,
        until: grapt_types::EventSeq,
    ) -> Result<MessageHistory, HistoryReadError> {
        let convert = |value: grapt_types::EventSeq, kind: &str| {
            i64::try_from(value.value()).map_err(|_| HistoryReadError::Corrupt {
                session: session.as_str().to_owned(),
                seq: i64::MAX,
                kind: kind.to_owned(),
                reason: "sequence exceeds SQLite range".to_owned(),
            })
        };
        self.fold_at(
            session,
            convert(from, "history boundary")?,
            Some(convert(until, "history window")?),
        )
    }

    /// Fold a half-open retained window [from, until) under explicit bounds.
    ///
    /// The compaction window is exactly the range the page fold's
    /// [MAX_HISTORY_ROWS] bound refuses; compaction is what makes it fit, so
    /// the windowed read takes its bounds as data instead of inheriting the
    /// page's.
    pub fn fold_window_from_bounded(
        &self,
        session: &SessionId,
        from: grapt_types::EventSeq,
        until: grapt_types::EventSeq,
        max_rows: usize,
        max_data_bytes: usize,
    ) -> Result<MessageHistory, HistoryReadError> {
        let convert = |value: grapt_types::EventSeq, kind: &str| {
            i64::try_from(value.value()).map_err(|_| HistoryReadError::Corrupt {
                session: session.as_str().to_owned(),
                seq: i64::MAX,
                kind: kind.to_owned(),
                reason: "sequence exceeds SQLite range".to_owned(),
            })
        };
        let from = convert(from, "history boundary")?;
        let until = convert(until, "history window")?;
        self.fold_bounded_at(session, from, Some(until), max_rows, max_data_bytes)
    }

    /// Fold the whole visible history under explicit bounds.
    ///
    /// For readers that need a lifetime answer but not a transcript page, so
    /// the page fold's [MAX_HISTORY_ROWS] bound is the wrong one. A session
    /// past `max_rows` or `max_data_bytes` is still a typed refusal, never a
    /// truncated answer.
    pub fn fold_bounded(
        &self,
        session: &SessionId,
        max_rows: usize,
        max_data_bytes: usize,
    ) -> Result<MessageHistory, HistoryReadError> {
        self.fold_bounded_at(session, i64::MIN, None, max_rows, max_data_bytes)
    }

    /// The owned rows in `(after, through]` this store cannot read, one per
    /// row, so a paged reader can mark the gap its page spans without folding
    /// a lifetime transcript.
    ///
    /// The same classification the fold uses decides what is an omission, so
    /// the marker and the fold cannot disagree.
    ///
    /// Bounded on three axes across the WHOLE scan, not per page: payload bytes,
    /// inspected rows (which bounds the gap records, since each comes from one
    /// row) and the page size of the statement. A cell's byte charge is read
    /// from SQLite's own length BEFORE the cell is copied into Rust, so a page
    /// of 64 MiB cells cannot be materialized before the first refusal; a
    /// withheld cell charges its real stored length instead of zero. A span
    /// past a bound refuses with its bound named — a shorter gap list reported
    /// as the whole truth is exactly what this scan exists to prevent.
    ///
    /// The caller holds the inbox lock for this whole call. What keeps one poll
    /// small is these bounds and the statement's own cursor, which holds one
    /// page of rows at a time; dropping the statement does not release a Rust
    /// guard the caller still owns.
    ///
    /// A stored coordinate below zero refuses: it cannot locate a row, and
    /// reporting it at zero would place a corrupt fact at the head of the
    /// session. Its payload is never inspected.
    pub fn omissions_between(
        &self,
        session: &SessionId,
        after: Option<u64>,
        through: u64,
    ) -> Result<Vec<HistoryOmission>, HistoryReadError> {
        let store = |source| HistoryReadError::Store {
            session: session.as_str().to_owned(),
            source,
        };
        let visible = visibility(self.conn, session)?.sql("seq");
        let kinds = OWNED_KINDS
            .iter()
            .map(|kind| format!("(type='{kind}' OR type GLOB '{kind}.*')"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let mut cursor = after
            .map(|after| i64::try_from(after).unwrap_or(i64::MAX))
            .unwrap_or(i64::MIN);
        let through = i64::try_from(through).map_err(|_| HistoryReadError::Corrupt {
            session: session.as_str().to_owned(),
            seq: i64::MAX,
            kind: "history window".to_owned(),
            reason: "sequence exceeds SQLite range".to_owned(),
        })?;
        let mut omissions = Vec::new();
        let mut scanned_bytes = 0usize;
        let mut scanned_rows = 0usize;
        loop {
            // `seq > ?2 AND seq <= ?3` with a strictly rising cursor is one
            // page of the range; the loop ends when a page comes back short.
            // The stored length rides along so it can be charged before the
            // payload is copied, and an oversized cell is never selected at all.
            let mut statement = self
                .conn
                .prepare(&format!(
                    "SELECT seq, type,
                            length(CAST(data AS BLOB)),
                            CASE WHEN length(CAST(data AS BLOB)) <= {MAX_HISTORY_DATA_BYTES}
                                 THEN data END
                     FROM event
                     WHERE aggregate_family = 'session' AND aggregate_id = ?1 AND {visible}
                       AND seq > ?2 AND seq <= ?3 AND ({kinds})
                     ORDER BY seq LIMIT ?4"
                ))
                .map_err(store)?;
            let probe = i64::try_from(OMISSION_SCAN_BATCH.saturating_add(1)).unwrap_or(i64::MAX);
            let mut rows = statement
                .query(rusqlite::params![session.as_str(), cursor, through, probe])
                .map_err(store)?;
            let mut page_rows = 0usize;
            let mut last = cursor;
            while let Some(row) = rows.next().map_err(store)? {
                let seq = row.get::<_, i64>(0).map_err(store)?;
                let versioned = row.get::<_, String>(1).map_err(store)?;
                let stored_bytes = row.get::<_, Option<i64>>(2).map_err(store)?;
                page_rows = page_rows.saturating_add(1);
                last = seq;
                // Charge the row and its stored bytes BEFORE copying the
                // payload: the length column costs no copy, the cell below
                // does.
                scanned_rows = scanned_rows.saturating_add(1);
                if scanned_rows > MAX_HISTORY_ROWS {
                    return Err(HistoryReadError::WindowTooLong {
                        session: session.as_str().to_owned(),
                        limit: MAX_HISTORY_ROWS,
                    });
                }
                scanned_bytes = scanned_bytes.saturating_add(
                    usize::try_from(stored_bytes.unwrap_or(0)).unwrap_or(usize::MAX),
                );
                if scanned_bytes > MAX_OMISSION_SCAN_BYTES {
                    return Err(HistoryReadError::WindowDataTooLong {
                        session: session.as_str().to_owned(),
                        limit: MAX_OMISSION_SCAN_BYTES,
                    });
                }
                let located = u64::try_from(seq).map_err(|_| HistoryReadError::Corrupt {
                    session: session.as_str().to_owned(),
                    seq,
                    kind: versioned.clone(),
                    reason: format!("the stored sequence is {seq}"),
                })?;
                // The CASE above already withheld a cell over the per-cell
                // bound, so this copy is the one the charge above paid for.
                let data = row.get::<_, Option<String>>(3).map_err(store)?;
                if let Some(gap) = omission_of_row(located, &versioned, data.as_deref()) {
                    omissions.push(gap);
                }
            }
            drop(rows);
            drop(statement);
            if page_rows < OMISSION_SCAN_BATCH {
                break;
            }
            if last <= cursor {
                return Err(HistoryReadError::Corrupt {
                    session: session.as_str().to_owned(),
                    seq: last,
                    kind: "history window".to_owned(),
                    reason: "the gap scan did not advance".to_owned(),
                });
            }
            cursor = last;
        }
        omissions.sort_by_key(|omission| omission.seq);
        Ok(omissions)
    }

    /// Commit time of this Session's newest visible durable event.
    ///
    /// One indexed read, never a fold: the `updated` a Session row can still
    /// state truthfully when its lifetime metrics exceed their bound.
    pub fn newest_commit_ms(&self, session: &SessionId) -> Result<Option<i64>, HistoryReadError> {
        let store = |source| HistoryReadError::Store {
            session: session.as_str().to_owned(),
            source,
        };
        let visible = visibility(self.conn, session)?.sql("seq");
        let mut statement = self
            .conn
            .prepare(&format!(
                "SELECT created FROM event
                 WHERE aggregate_family = 'session' AND aggregate_id = ?1 AND {visible}
                 ORDER BY seq DESC LIMIT 1",
            ))
            .map_err(store)?;
        let mut rows = statement.query([session.as_str()]).map_err(store)?;
        match rows.next().map_err(store)? {
            Some(row) => row.get(0).map(Some).map_err(store),
            None => Ok(None),
        }
    }

    fn fold_bounded_at(
        &self,
        session: &SessionId,
        from: i64,
        until: Option<i64>,
        max_rows: usize,
        max_data_bytes: usize,
    ) -> Result<MessageHistory, HistoryReadError> {
        let store = |source| HistoryReadError::Store {
            session: session.as_str().to_owned(),
            source,
        };
        let visible = visibility(self.conn, session)?.sql("seq");
        let mut statement = self
            .conn
            .prepare(&format!(
                "SELECT id, seq, created, type,
                        CASE WHEN type = '{REQUEST_CACHE_KIND}' THEN NULL
                             WHEN length(CAST(data AS BLOB)) <= {max_data_bytes} THEN data ELSE NULL END,
                        CASE WHEN type = '{REQUEST_CACHE_KIND}' THEN 0
                             ELSE length(CAST(data AS BLOB)) END FROM event
                 WHERE aggregate_family = 'session' AND aggregate_id = ?1 AND {visible}
                   AND (seq >= ?3 OR seq < 0) AND (?4 IS NULL OR seq < ?4)
                 ORDER BY seq LIMIT ?2",
            ))
            .map_err(store)?;
        let probe = i64::try_from(max_rows.saturating_add(1)).unwrap_or(i64::MAX);
        let mapped = statement
            .query_map(
                rusqlite::params![session.as_str(), probe, from, until],
                |row| {
                    Ok((
                        (
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, Option<String>>(4)?,
                        ),
                        row.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    ))
                },
            )
            .map_err(store)?;
        // Charge materialized payloads as rows arrive, before holding more
        // than the byte bound. Operational diagnostics retain their row charge.
        let mut rows = Vec::new();
        let mut data_bytes = 0usize;
        for row in mapped {
            let (row, stored_bytes) = row.map_err(store)?;
            data_bytes =
                data_bytes.saturating_add(usize::try_from(stored_bytes).unwrap_or(usize::MAX));
            if data_bytes > max_data_bytes {
                return Err(HistoryReadError::WindowDataTooLong {
                    session: session.as_str().to_owned(),
                    limit: max_data_bytes,
                });
            }
            rows.push(row);
        }
        if rows.len() > max_rows {
            return Err(HistoryReadError::WindowTooLong {
                session: session.as_str().to_owned(),
                limit: max_rows,
            });
        }
        self.assemble_history(session, &rows, Some(max_data_bytes))
    }

    /// Fold rows before `until` when no retained cut exists.
    pub fn fold_window(
        &self,
        session: &SessionId,
        until: grapt_types::EventSeq,
    ) -> Result<MessageHistory, HistoryReadError> {
        let until = i64::try_from(until.value()).map_err(|_| HistoryReadError::Corrupt {
            session: session.as_str().to_owned(),
            seq: i64::MAX,
            kind: "history window".to_owned(),
            reason: "sequence exceeds SQLite range".to_owned(),
        })?;
        self.fold_at(session, i64::MIN, Some(until))
    }

    fn fold_at(
        &self,
        session: &SessionId,
        from: i64,
        until: Option<i64>,
    ) -> Result<MessageHistory, HistoryReadError> {
        let store = |source| HistoryReadError::Store {
            session: session.as_str().to_owned(),
            source,
        };
        let visible = visibility(self.conn, session)?.sql("seq");
        let mut statement = self
            .conn
            .prepare(&format!(
                "SELECT id, seq, created, type,
                        CASE WHEN type = '{REQUEST_CACHE_KIND}' THEN ''
                             WHEN length(CAST(data AS BLOB)) <= {MAX_HISTORY_DATA_BYTES} THEN data ELSE NULL END,
                        CASE WHEN type = '{REQUEST_CACHE_KIND}' THEN 0
                             ELSE length(CAST(data AS BLOB)) END FROM event
                 WHERE aggregate_family = 'session' AND aggregate_id = ?1 AND {visible} AND (seq >= ?3 OR seq < 0)
                 AND (?4 IS NULL OR seq < ?4)
                 ORDER BY seq LIMIT ?2",
            ))
            .map_err(store)?;
        let probe = i64::try_from(MAX_HISTORY_ROWS.saturating_add(1)).unwrap_or(i64::MAX);
        let mapped = statement
            .query_map(
                rusqlite::params![session.as_str(), probe, from, until],
                |row| {
                    let stored_bytes = row.get::<_, Option<i64>>(5)?;
                    // A cell past the bound by itself is never fetched. A NULL
                    // cell has no length and is read as text, so it still
                    // refuses as the conversion error it always was.
                    let data = match stored_bytes {
                        Some(bytes)
                            if usize::try_from(bytes)
                                .map_or(true, |bytes| bytes > MAX_HISTORY_DATA_BYTES) =>
                        {
                            None
                        }
                        _ => Some(row.get::<_, String>(4)?),
                    };
                    Ok((
                        (
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, String>(3)?,
                            data,
                        ),
                        stored_bytes.unwrap_or(0),
                    ))
                },
            )
            .map_err(store)?;
        // Charged at each row's stored length as it arrives, as the bounded
        // fold does, so the page fold never holds more than its byte bound.
        let mut rows = Vec::new();
        let mut data_bytes = 0usize;
        for row in mapped {
            let (row, stored_bytes) = row.map_err(store)?;
            data_bytes =
                data_bytes.saturating_add(usize::try_from(stored_bytes).unwrap_or(usize::MAX));
            if data_bytes > MAX_HISTORY_DATA_BYTES {
                return Err(HistoryReadError::WindowDataTooLong {
                    session: session.as_str().to_owned(),
                    limit: MAX_HISTORY_DATA_BYTES,
                });
            }
            rows.push(row);
        }
        if rows.len() > MAX_HISTORY_ROWS {
            return Err(HistoryReadError::TooLong {
                session: session.as_str().to_owned(),
            });
        }
        self.assemble_history(session, &rows, None)
    }

    /// Folds the rows one bounded read already selected into the message
    /// history. A row this fold OWNS that cannot be classified or decoded is
    /// recorded as an omission and skipped: one bad row costs that item, never
    /// the whole transcript. The whole-window bounds still refuse — dropping
    /// unbounded rows with no marker would be the truncation this read exists
    /// to prevent — and a structural contradiction still refuses in the fold.
    fn assemble_history(
        &self,
        session: &SessionId,
        rows: &[(String, i64, i64, String, Option<String>)],
        window_data_bound: Option<usize>,
    ) -> Result<MessageHistory, HistoryReadError> {
        let mut history = MessageHistory::default();
        let mut window_bytes = 0usize;
        for (event, seq, created_ms, versioned, data) in rows {
            if versioned == REQUEST_CACHE_KIND {
                continue;
            }
            let corrupt = |kind: &str, reason: String| HistoryReadError::Corrupt {
                session: session.as_str().to_owned(),
                seq: *seq,
                kind: kind.to_owned(),
                reason,
            };
            // Only an OWNED row may become a named gap or a refusal: evidence
            // at a coordinate it cannot locate leaves the binding absent, as an
            // undecodable terminal already does, and another cluster's row is
            // not this fold's business.
            let owned = owned_kind(versioned);
            let omitted_at = match u64::try_from(*seq) {
                Ok(seq) => seq,
                Err(_) if !owned => continue,
                Err(_) => return Err(corrupt(versioned, format!("the stored sequence is {seq}"))),
            };
            let data = match data {
                Some(data) => data,
                // Withheld evidence leaves the binding absent rather than
                // naming a gap whose kind this fold does not own.
                None if !owned => continue,
                None => {
                    // The cell was withheld by the byte bound. One row over the
                    // bound is a readable history with one named gap; the
                    // readers already distinguish capacity from failure, and a
                    // single row is exactly the case that used to brick the
                    // whole read.
                    let detail = match window_data_bound {
                        Some(limit) => {
                            format!("the stored data exceeds the {limit}-byte window bound")
                        }
                        // A whole-session fold withholds the same cells at its
                        // own fixed bound; naming `usize::MAX` here would
                        // invent a bound no reader applies.
                        None => format!(
                            "the stored data exceeds the {MAX_HISTORY_DATA_BYTES}-byte read bound"
                        ),
                    };
                    history.omit(HistoryOmission {
                        seq: omitted_at,
                        kind: versioned.clone(),
                        class: HistoryOmissionClass::Oversized,
                        detail,
                        message: None,
                    });
                    continue;
                }
            };
            if let Some(limit) = window_data_bound {
                window_bytes = window_bytes.saturating_add(data.len());
                if window_bytes > limit {
                    return Err(HistoryReadError::WindowDataTooLong {
                        session: session.as_str().to_owned(),
                        limit,
                    });
                }
            }
            let row = match classify_row(versioned, *seq, data) {
                Ok(Some(row)) => row,
                Ok(None) => continue,
                // Every other unreadable owned row is a named gap; only the
                // coordinate, validated above, still refuses.
                Err((kind, reason)) => {
                    history.omit(HistoryOmission {
                        seq: omitted_at,
                        kind,
                        class: HistoryOmissionClass::Unclassified,
                        detail: reason,
                        message: None,
                    });
                    continue;
                }
            };
            let (name, version, seq, value) = (row.name, row.version, row.seq, row.value);
            let event = match event.parse::<EventId>() {
                Ok(event) => event,
                // A committed position belongs to the rows this fold renders:
                // an unowned row is not this transcript's gap.
                Err(_) if !owned => continue,
                Err(_) => {
                    history.omit(HistoryOmission {
                        seq: omitted_at,
                        kind: name.to_owned(),
                        class: HistoryOmissionClass::Unclassified,
                        detail: format!("the stored event identity {event:?} is invalid"),
                        message: None,
                    });
                    continue;
                }
            };
            let applied = history.apply_committed(
                HistoryRow {
                    seq,
                    kind_name: name,
                    kind_version: version,
                    data: &value,
                },
                Some(CommittedPosition {
                    event,
                    seq,
                    created_ms: *created_ms,
                }),
            );
            if let Err(error) = applied {
                // An undecodable payload is recorded by the fold itself and
                // already skipped. Anything else is a structural contradiction
                // the transcript cannot be read through, so it keeps refusing.
                if !matches!(error, HistoryError::Payload { .. }) {
                    return Err(corrupt(name, error.to_string()));
                }
            }
        }
        Ok(history)
    }
}

/// Owned rows one gap-scan page reads, so a wide span costs memory in
/// proportion to the gaps it reports, never to the rows it visits.
pub(crate) const OMISSION_SCAN_BATCH: usize = 256;

/// Payload bytes one whole gap scan may read, summed across its pages.
///
/// The per-row cell bound alone is not enough: a span can contain arbitrarily
/// many owned rows, so the scan needs an aggregate bound as well or it would
/// still walk a lifetime transcript on one poll.
pub(crate) const MAX_OMISSION_SCAN_BYTES: usize = 16 * 1024 * 1024;

/// One row's gap, if it has one: the whole-scan decision for a located row.
///
/// A withheld cell is named without decoding it, an unparsable kind or version
/// is named unclassified, and a payload that does not decode is named only
/// after the fold's own dispatch says so. A row that is not owned, or whose
/// payload a plain fold would accept, is not a gap.
pub(crate) fn omission_of_row(
    seq: u64,
    versioned: &str,
    data: Option<&str>,
) -> Option<HistoryOmission> {
    let Some(data) = data else {
        return Some(HistoryOmission {
            seq,
            kind: versioned.to_owned(),
            class: HistoryOmissionClass::Oversized,
            detail: format!("the stored data exceeds the {MAX_HISTORY_DATA_BYTES}-byte read bound"),
            message: None,
        });
    };
    let row = match classify_row(versioned, i64::try_from(seq).unwrap_or(i64::MAX), data) {
        Ok(Some(row)) => row,
        Ok(None) => return None,
        Err((kind, detail)) => {
            return Some(HistoryOmission {
                seq,
                kind,
                class: HistoryOmissionClass::Unclassified,
                detail,
                message: None,
            })
        }
    };
    if MessageHistory::payload_is_readable(row.name, row.version, &row.value) {
        return None;
    }
    Some(HistoryOmission {
        seq,
        kind: row.name.to_owned(),
        class: HistoryOmissionClass::Undecodable,
        detail: format!(
            "the stored {} payload does not decode at version {}",
            row.name, row.version
        ),
        message: None,
    })
}

fn visibility(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<HistoryVisibility, HistoryReadError> {
    HistoryVisibility::read(connection, session).map_err(|error| match error {
        VisibilityError::Store(source) => HistoryReadError::Store {
            session: session.as_str().to_owned(),
            source,
        },
        other => HistoryReadError::Visibility(other),
    })
}

#[derive(Debug, thiserror::Error)]
pub enum HistoryReadError {
    #[error(transparent)]
    Visibility(VisibilityError),
    #[error("history read failed for {session}: {source}")]
    Store {
        session: String,
        #[source]
        source: rusqlite::Error,
    },
    #[error(
        "session {session} has more than {} durable rows; a bounded history read \
         cannot answer truthfully",
        MAX_HISTORY_ROWS
    )]
    TooLong { session: String },
    #[error(
        "session {session} has more than {limit} durable tool-call rows; the continuation page cannot answer truthfully"
    )]
    ToolCallPageTooLong { session: String, limit: usize },
    #[error(
        "session {session} window has more than {limit} durable rows; the bounded windowed history read cannot answer truthfully"
    )]
    WindowTooLong { session: String, limit: usize },
    #[error(
        "session {session} window data exceeds the {limit}-byte bound; the bounded windowed history read cannot answer truthfully"
    )]
    WindowDataTooLong { session: String, limit: usize },
    #[error("session {session} attempt {attempt} has too many durable usage observations")]
    AttemptUsageTooLong { session: String, attempt: String },
    /// A stored row belonging to a kind this fold OWNS could not be read at
    /// all — its type, its sequence or its data is not what the store may hold.
    ///
    /// Evidence-bearing on purpose: the sequence locates the row, and the kind
    /// says which contract was violated. Before this variant existed the same
    /// row was silently skipped, so durable corruption in the middle of a
    /// transcript surfaced as a shorter transcript and a success.
    #[error("session {session} has a corrupt durable row at seq {seq} ({kind}): {reason}")]
    Corrupt {
        session: String,
        seq: i64,
        kind: String,
        reason: String,
    },
    #[error(transparent)]
    Fold(#[from] HistoryError),
}

/// The durable kind NAMES this fold owns.
///
/// One list, consulted by [`MessageHistory::apply`] for the version match and
/// by [`SessionHistoryStore::fold`] for the ownership question, because two
/// lists would drift and the drift would be invisible: a name dropped from the
/// fold's list becomes a row that skips rather than a row that fails.
pub const OWNED_KINDS: &[&str] = &[
    "session.assistant-message.opened",
    "session.content-block.opened",
    "session.content.appended",
    "session.usage.observed",
    "session.step.ended",
    "session.step.failed",
];

/// Kinds the fold reads only as evidence for a binding it owns, without owning
/// them: they are not version-strict here. A fork copies them with its tool
/// history (`lifecycle::FORK_TOOL_KINDS`), not as owned history. A tool
/// terminal binds a tool-only retry attempt to its Step message.
pub(crate) const BINDING_EVIDENCE_KINDS: &[&str] = &["session.tool.terminal"];

/// Whether this fold owns the named kind, at ANY version.
pub(super) fn owns_kind(name: &str) -> bool {
    OWNED_KINDS.contains(&name)
}

/// One stored row the fold may read: its split kind, sequence and JSON data.
pub(super) struct ClassifiedRow<'a> {
    pub(super) name: &'a str,
    pub(super) version: u32,
    pub(super) seq: u64,
    pub(super) value: Value,
}

/// Whether a stored `type` belongs to a kind this fold owns, at any version or
/// even with an unparsable one.
///
/// This is the ownership question alone, splitting the same way
/// [`classify_row`] does, so a caller that must decide "can this row become a
/// named gap" cannot drift from the fold's own answer. It is not the question
/// "does this fold read the row": binding evidence is read without being owned.
pub(super) fn owned_kind(versioned: &str) -> bool {
    match versioned.rsplit_once('.') {
        Some((name, version)) => {
            owns_kind(name) || (version.parse::<u32>().is_err() && owns_kind(versioned))
        }
        None => owns_kind(versioned),
    }
}

/// The owner's single decision about one stored row: read it, skip it, or
/// refuse it as `(kind, reason)`.
///
/// Every history reader asks this one function, because a second copy is how
/// the accounting reader once came to refuse rows the fold deliberately skips
/// (an owned name at another era's version) and to demand versions on kinds
/// the fold does not own at all.
pub(super) fn classify_row<'a>(
    versioned: &'a str,
    seq: i64,
    data: &str,
) -> Result<Option<ClassifiedRow<'a>>, (String, String)> {
    // Parse the KIND first, and decide ownership before deciding
    // readability. An earlier revision did the opposite: every failure
    // to split, parse or decode simply `continue`d, so a corrupted
    // `session.content.appended.1` row vanished and the caller received
    // a SHORTER transcript reported as a successful read. A row this
    // fold owns is never allowed to disappear; only a genuinely unowned
    // kind may be skipped.
    let Some((name, version)) = versioned.rsplit_once('.') else {
        if owns_kind(versioned) {
            return Err((
                versioned.to_owned(),
                "the stored type carries no version suffix".to_owned(),
            ));
        }
        return Ok(None);
    };
    let version = match version.parse::<u32>() {
        Ok(version) => version,
        // `owns_kind` is asked about BOTH readings because an owned
        // name stored with no version at all — `session.step.ended` —
        // splits into a name this fold does not own and a "version"
        // that is not a number. Testing only the split reading would
        // let that row skip.
        Err(_) if owns_kind(name) || owns_kind(versioned) => {
            return Err((
                versioned.to_owned(),
                "the stored type has no numeric version".to_owned(),
            ))
        }
        Err(_) => return Ok(None),
    };
    // Mapping a negative sequence to zero — as an earlier revision did — put
    // the row at the head of the fold and made a corrupt sequence look like
    // the first fact the Session ever wrote.
    let Ok(seq_value) = u64::try_from(seq) else {
        if owns_kind(name) {
            return Err((name.to_owned(), format!("the stored sequence is {seq}")));
        }
        return Ok(None);
    };
    let value = match serde_json::from_str::<Value>(data) {
        Ok(value) => value,
        Err(_) if owns_kind(name) => {
            return Err((name.to_owned(), "the stored data is not JSON".to_owned()))
        }
        Err(_) => return Ok(None),
    };
    Ok(Some(ClassifiedRow {
        name,
        version,
        seq: seq_value,
        value,
    }))
}
