//! Complete, bounded public history for file export, independent of the reading window.
use super::*;
use grapt_api::u3a::{decode_page, SessionMessageInfoDto, SessionMessagesResponseDto};
use std::collections::BTreeSet;

const MAX_EXPORT_BYTES: usize = 64 * 1024 * 1024;
const MAX_EXPORT_ROWS: usize = 100_000;
const MAX_EXPORT_PAGES: usize = 2_000;

/// Public DTO rows in source order. No projection or retention truncation.
#[derive(Debug, serde::Serialize)]
pub struct SessionExport {
    #[serde(rename = "sessionID")]
    pub session: SessionWireId,
    pub data: Vec<SessionMessageInfoDto>,
}

impl<C: Connection> Client<C> {
    /// Read all pages before returning anything exportable. Run on the backend worker.
    /// This does not move the selected session, live projection or reading window.
    pub fn export_history(&mut self) -> Result<SessionExport, ClientError> {
        let session = self.selected.clone().ok_or(ClientError::NoSelection)?;
        let mut export = SessionExport {
            session,
            data: Vec::new(),
        };
        let mut cursor: Option<String> = None;
        let mut cursors = BTreeSet::new();
        let mut ids = BTreeSet::new();
        let mut bytes_read = 0usize;
        for _ in 0..self.bounds.max_message_pages.min(MAX_EXPORT_PAGES) {
            let bytes = self.ingest(
                Operation::MessageList,
                Some(&export.session),
                None,
                None,
                None,
                Some(Page::Window {
                    cursor: cursor.as_deref(),
                    descending: false,
                }),
            )?;
            bytes_read = bytes_read
                .checked_add(bytes.len())
                .filter(|bytes| *bytes <= MAX_EXPORT_BYTES)
                .ok_or_else(|| refused("history exceeds the 64 MiB export read bound"))?;
            let page: SessionMessagesResponseDto = decode_page(&bytes, crate::wire::limits()?)
                .map_err(crate::wire::WireError::from)?;
            let rows = page.data().get().as_slice();
            if rows.len() > 200 || export.data.len().saturating_add(rows.len()) > MAX_EXPORT_ROWS {
                return Err(refused("history exceeds the export row bound"));
            }
            let next = page
                .cursor()
                .get()
                .next()
                .as_value()
                .map(|value| value.as_str().to_owned());
            if next.is_some() && rows.is_empty() {
                return Err(refused("empty history page still offers a continuation"));
            }
            for row in rows {
                if !ids.insert(crate::wire::row_id(row)) {
                    return Err(refused(
                        "history repeats a message; export would be incomplete",
                    ));
                }
            }
            export.data.extend_from_slice(rows);
            let Some(next) = next else {
                return Ok(export);
            };
            if next.is_empty() || !cursors.insert(next.clone()) {
                return Err(refused("history cursor cycle or empty cursor"));
            }
            cursor = Some(next);
        }
        Err(refused(
            "history exceeds the export page bound; no file written",
        ))
    }
}

fn refused(reason: &str) -> ClientError {
    crate::wire::WireError::Invalid {
        what: "session export",
        value: reason.into(),
    }
    .into()
}
