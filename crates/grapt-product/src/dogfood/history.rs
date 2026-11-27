use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use grapt_api::u3a::{
    MessageListQueryDto, SessionMessagesApplicationError, SessionMessagesResponseDto, WireId,
};
use grapt_types::{ExecutionId, IdAuthority, MessageId, SessionId};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::DogfoodApplication;
mod index;
pub(super) use index::recovery_point;
mod omitted;
mod page_bytes;
pub(super) use omitted::OmittedItems;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Order {
    Asc,
    Desc,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Direction {
    Previous,
    Next,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    id: WireId<MessageId>,
    order: Order,
    direction: Direction,
}

#[derive(Debug)]
pub(super) struct MessagePage {
    limit: usize,
    order: Order,
    cursor: Option<Cursor>,
}

/// Resume older history directly from a trusted compaction recovery anchor.
pub(super) fn older_cursor(message: &MessageId) -> Result<String, String> {
    let id = WireId::parse(message.as_str().to_owned()).ok_or("invalid recovery identity")?;
    serde_json::to_vec(&Cursor {
        id,
        order: Order::Desc,
        direction: Direction::Next,
    })
    .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
    .map_err(|error| error.to_string())
}

fn invalid_cursor() -> SessionMessagesApplicationError {
    cursor_error("Invalid cursor")
}

fn cursor_error(message: &str) -> SessionMessagesApplicationError {
    match super::decode(json!({"_tag":"InvalidCursorError", "message": message})) {
        Ok(error) => SessionMessagesApplicationError::InvalidCursor(error),
        Err(error) => SessionMessagesApplicationError::Unknown(super::unknown(error)),
    }
}

/// Why a history page could not be produced.
#[derive(Debug)]
pub(super) enum PageRefusal {
    /// The cursor is well formed but names no message in this Session's
    /// history: it belongs to another Session or was never issued here. An
    /// empty page would read as "no further history", so it refuses instead.
    StaleCursor,
    RowBytes {
        id: String,
        bound: usize,
    },
    Failed(String),
}

impl From<String> for PageRefusal {
    fn from(error: String) -> Self {
        Self::Failed(error)
    }
}

impl PageRefusal {
    pub(super) fn into_application(self) -> SessionMessagesApplicationError {
        match self {
            Self::StaleCursor => cursor_error(
                "Cursor does not identify a message in this Session; restart from the first page",
            ),
            Self::RowBytes { id, bound } => {
                SessionMessagesApplicationError::Unknown(super::unknown(format!(
                    "History row {id} exceeds the {bound}-byte encoded page bound"
                )))
            }
            Self::Failed(error) => SessionMessagesApplicationError::Unknown(super::unknown(error)),
        }
    }
}

impl MessagePage {
    pub(super) fn parse(
        query: MessageListQueryDto,
    ) -> Result<Self, SessionMessagesApplicationError> {
        let input = serde_json::to_value(query).map_err(|error| {
            SessionMessagesApplicationError::InvalidRequest(super::invalid(error))
        })?;
        let limit = match input.get("limit").filter(|value| !value.is_null()) {
            Some(value) => value
                .as_str()
                .and_then(|raw| raw.parse::<usize>().ok())
                .filter(|limit| (1..=200).contains(limit))
                .ok_or_else(|| {
                    SessionMessagesApplicationError::InvalidRequest(super::invalid(
                        "message limit must be an integer from 1 through 200",
                    ))
                })?,
            None => 50,
        };
        let cursor = match input.get("cursor").filter(|value| !value.is_null()) {
            Some(value) => {
                if input.get("order").is_some_and(|value| !value.is_null()) {
                    return Err(invalid_cursor());
                }
                let raw = value
                    .as_str()
                    .filter(|raw| raw.len() <= 1024)
                    .ok_or_else(invalid_cursor)?;
                let bytes = URL_SAFE_NO_PAD
                    .decode(raw)
                    .or_else(|_| URL_SAFE.decode(raw))
                    .map_err(|_| invalid_cursor())?;
                Some(serde_json::from_slice::<Cursor>(&bytes).map_err(|_| invalid_cursor())?)
            }
            None => None,
        };
        let order = match &cursor {
            Some(cursor) => cursor.order,
            None => {
                if input.get("order").and_then(Value::as_str) == Some("asc") {
                    Order::Asc
                } else {
                    Order::Desc
                }
            }
        };
        Ok(Self {
            limit,
            order,
            cursor,
        })
    }

    /// The page of `rows` (ascending by sequence) this query selects. Only
    /// the selected rows are rendered, together.
    #[cfg(test)]
    fn project(
        &self,
        mut rows: Vec<IndexedRow>,
        render: impl FnMut(&[IndexedRow]) -> Result<Vec<String>, String>,
    ) -> Result<String, PageRefusal> {
        let previous = self
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.direction == Direction::Previous);
        let ascending = (self.order == Order::Asc) != previous;
        if !ascending {
            rows.reverse();
        }
        if let Some(cursor) = &self.cursor {
            let index = rows
                .iter()
                .position(|row| row.id == cursor.id.as_inner().as_str())
                .ok_or(PageRefusal::StaleCursor)?;
            rows.drain(..=index);
        }
        rows.truncate(self.limit);
        if previous {
            rows.reverse();
        }
        self.render(&rows, render)
    }

    fn previous(&self) -> bool {
        self.cursor
            .as_ref()
            .is_some_and(|cursor| cursor.direction == Direction::Previous)
    }

    fn ascending(&self) -> bool {
        (self.order == Order::Asc) != self.previous()
    }
}

/// One public history row: where it sits, its wire id, and how it renders.
/// A page renders only the rows it selects, so a poll of a long Session
/// costs its page, not its history.
pub(super) struct IndexedRow {
    seq: u64,
    id: String,
    source: RowSource,
}

enum RowSource {
    /// Inputs, failures and notices: small rows, rendered as they are read.
    Rendered(String),
    Assistant(Box<grapt_session::AssistantMessage>),
    Compaction(super::history_compaction::CommittedCompaction),
}

impl IndexedRow {
    pub(super) fn rendered(seq: u64, id: String, raw: String) -> Self {
        Self {
            seq,
            id,
            source: RowSource::Rendered(raw),
        }
    }

    /// This row's durable coordinate, which starts the range its content covers.
    pub(super) fn seq(&self) -> u64 {
        self.seq
    }

    /// `raw`, once it is known to carry this row's id: cursors are cut from
    /// the index, so a row rendered under another id refuses.
    fn checked(&self, raw: String) -> Result<String, String> {
        let value: Value = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
        if value.get("id").and_then(Value::as_str) != Some(self.id.as_str()) {
            return Err(format!("history row {} rendered under another id", self.id));
        }
        Ok(raw)
    }
}

/// Renders the rows one read selected.
struct RowRenderer<'a> {
    application: &'a DogfoodApplication,
    session: &'a SessionId,
    cache_diagnostics: bool,
}

impl RowRenderer<'_> {
    fn page_recent(rows: &[IndexedRow]) -> super::history_compaction::PageRecent {
        super::history_compaction::PageRecent::new(rows.iter().filter_map(
            |row| match &row.source {
                RowSource::Compaction(committed) => Some(committed.seq),
                _ => None,
            },
        ))
    }

    /// A complete batch shares the same read-local lineage state as a byte page.
    fn render_page(&self, rows: &[IndexedRow]) -> Result<Vec<String>, String> {
        self.render_with_recent(
            rows,
            &mut Self::page_recent(rows),
            &OmittedItems::default(),
            &Self::row_ranges(rows),
            0,
        )
    }

    /// The `[from, until)` durable range each row of the page owns, in row
    /// order.
    ///
    /// The pager renders ONE row per callback, so a range computed from the
    /// callback slice would run to `u64::MAX` and every row would claim every
    /// gap after it. These bounds come from the whole page instead: a row owns
    /// its own coordinate up to the next rendered row's.
    fn row_ranges(rows: &[IndexedRow]) -> Vec<(u64, u64)> {
        // Bounds follow DURABLE SEQUENCE, then map back to response order: a
        // descending page returns 100 before 50, and taking the next row in
        // response order would give `[100, 50)` (empty) and `[50, MAX)`, so it
        // would attach a gap at 110 to the older row and report a displayed
        // row's own gap nowhere. Response order is a view of the page, never
        // its coordinates.
        let mut by_sequence = rows.iter().map(IndexedRow::seq).collect::<Vec<_>>();
        by_sequence.sort_unstable();
        let mut ranges = Vec::with_capacity(rows.len());
        for row in rows {
            let next = by_sequence
                .iter()
                .copied()
                .find(|seq| *seq > row.seq())
                .unwrap_or(u64::MAX);
            ranges.push((row.seq(), next));
        }
        ranges
    }

    fn render_with_recent(
        &self,
        rows: &[IndexedRow],
        recent: &mut super::history_compaction::PageRecent,
        omitted: &OmittedItems,
        ranges: &[(u64, u64)],
        offset: usize,
    ) -> Result<Vec<String>, String> {
        let through = rows
            .iter()
            .filter_map(|row| match &row.source {
                RowSource::Compaction(committed) => Some(committed.seq),
                _ => None,
            })
            .max();
        let recent = through
            .map(|through| recent.through(self.application, self.session, through))
            .transpose()?;
        // Small batches amortize lock handoffs without holding a page-sized
        // output collection or blocking the live owner for the whole page.
        let mut rendered = Vec::with_capacity(rows.len());
        for (chunk, batch) in rows
            .chunks(grapt_session::SessionHistoryStore::CALL_BATCH_SIZE)
            .enumerate()
        {
            let targets = batch
                .iter()
                .flat_map(|row| match &row.source {
                    RowSource::Assistant(message) => message
                        .content
                        .as_ref()
                        .map(|content| content.items())
                        .unwrap_or_default(),
                    _ => &[],
                })
                .filter_map(|held| match held.item.kind() {
                    grapt_types::ContentKind::Tool { call } => Some(call.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let projections = super::continuation::ToolProjection::read_batches(
                &self.application.inbox,
                self.application.executor.artifacts(),
                self.session,
                &targets,
            );
            for (index, row) in batch.iter().enumerate() {
                let raw = self
                    .render_with_tools(row, recent, &projections)
                    .map_err(|error| format!("history row {}: {error}", row.id))
                    .and_then(|raw| row.checked(raw))?;
                let range = ranges
                    .get(
                        offset
                            + chunk * grapt_session::SessionHistoryStore::CALL_BATCH_SIZE
                            + index,
                    )
                    .copied()
                    .unwrap_or((row.seq(), u64::MAX));
                rendered.push(omitted.annotate(range.0, range.1, raw)?);
            }
        }
        Ok(rendered)
    }

    fn render_with_tools(
        &self,
        row: &IndexedRow,
        recent: Option<
            &std::collections::HashMap<String, super::history_compaction::ReportedCompaction>,
        >,
        projections: &super::continuation::ToolProjections,
    ) -> Result<String, String> {
        match &row.source {
            RowSource::Rendered(raw) => Ok(raw.clone()),
            RowSource::Assistant(message) => self.application.assistant_json_with_tools(
                self.session,
                message,
                Some(projections),
                self.cache_diagnostics,
            ),
            RowSource::Compaction(committed) => {
                let recent = recent.ok_or("compaction recent lineage was not read")?;
                self.application
                    .compaction_json(self.session, committed, recent)
            }
        }
    }
}

// Display bounds for stored failure text: the message keeps the durable
// bound, the name gets a tag-sized bound of its own.
const MAX_FAILURE_MESSAGE_DISPLAY_BYTES: usize = 512;
const MAX_FAILURE_NAME_DISPLAY_BYTES: usize = 64;

/// Untrusted stored text, made safe for a screen: byte-capped on a char
/// boundary, then every non-printable char replaced by its `\u{hex}`
/// escape. The escape form keeps the output ONE line and free of ESC,
/// newline and control bytes — a tag that carried a terminal escape cannot
/// re-inject it.
fn is_terminal_printable(ch: char) -> bool {
    let code = ch as u32;
    // ASCII control chars (newline, ESC included), DEL and the C1 block are
    // terminal machinery, not text: an unescaped one of them could steer a
    // screen. The two Unicode separator chars break lines in most renderers.
    code >= 0x20
        && code != 0x7f
        && !((0x80..=0x9f).contains(&code))
        && code != 0x2028
        && code != 0x2029
}

pub(super) fn display_safe(raw: &str, raw_cap_bytes: usize) -> String {
    let mut end = raw.len().min(raw_cap_bytes);
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::new();
    for ch in raw[..end].chars() {
        if is_terminal_printable(ch) {
            out.push(ch);
        } else {
            out.push_str(&format!("\\u{{{:x}}}", ch as u32));
        }
    }
    out
}

// Encode through the declared DTO: the public union requires its discriminant
// first. A serde_json::Value sorts id before type and makes old failures unreadable.
//
// The stored name and message are UNTRUSTED text: rows this build wrote carry
// the known vocabulary and render byte-identically to before; a row a later
// build wrote renders its stored name, so a future class reads as one.
fn failure_message(
    id: &str,
    created: i64,
    failure: &grapt_session::execution::PublicFailure,
) -> Result<String, String> {
    let message = display_safe(&failure.message, MAX_FAILURE_MESSAGE_DISPLAY_BYTES);
    let text = if failure.is_known_name() {
        format!("Execution failed: {message}")
    } else {
        format!(
            "Execution failed ({}): {message}",
            display_safe(&failure.name, MAX_FAILURE_NAME_DISPLAY_BYTES)
        )
    };
    let message: grapt_api::u3a::SessionMessageSystemDto = super::decode(json!({
        "type": "system",
        "id": id,
        "metadata": {"executionFailure": true},
        "time": {"created": created},
        "text": text,
    }))?;
    serde_json::to_string(&message).map_err(|error| error.to_string())
}

/// A public row for a selected message whose content could not be read.
///
/// The index selected this identity from its durable coordinate,
/// so the message exists and the page must keep its position: refusing would
/// hide every other message on the page because of one. It is rendered as a
/// system row carrying the durable coordinate, never as the user's or the
/// model's words, and its metadata marks it as an unreadable item.
fn unreadable_message_row(id: &str, seq: u64) -> Result<String, String> {
    let message: grapt_api::u3a::SessionMessageSystemDto = super::decode(json!({
        "type": "system",
        "id": id,
        "metadata": {"unreadableMessage": true, "seq": seq},
        "time": {"created": 0},
        "text": "This message could not be read from the Session store. The rest of the conversation is intact.",
    }))?;
    serde_json::to_string(&message).map_err(|error| error.to_string())
}

pub(super) fn messages_get(
    application: &DogfoodApplication,
    session: &SessionId,
    query: MessageListQueryDto,
    view: grapt_api::u3a::MessageListView,
) -> Result<SessionMessagesResponseDto, SessionMessagesApplicationError> {
    let page = MessagePage::parse(query)?;
    let result = application
        .history_json(session, &page, view)
        .and_then(|raw| {
            serde_json::from_str::<SessionMessagesResponseDto>(&raw)
                .map_err(|error| PageRefusal::Failed(error.to_string()))
        });
    result.map_err(|error| {
        if application.get_record(session).is_err() {
            SessionMessagesApplicationError::SessionNotFound(super::not_found(session))
        } else {
            error.into_application()
        }
    })
}

pub(super) fn message_get(
    application: &DogfoodApplication,
    session: &SessionId,
    message: &MessageId,
) -> Result<grapt_api::u3a::SessionMessageResponseDto, grapt_api::u3a::SessionMessageApplicationError>
{
    use grapt_api::u3a::{SessionLookupApplicationError, SessionMessageApplicationError as Error};
    super::context::require_readable_session(application, session).map_err(
        |error| match error {
            SessionLookupApplicationError::InvalidRequest(error) => Error::InvalidRequest(error),
            SessionLookupApplicationError::SessionNotFound(error) => Error::SessionNotFound(error),
            SessionLookupApplicationError::Unavailable => Error::Unavailable,
        },
    )?;
    match application
        .message_json(session, message)
        .map_err(|error| Error::Unknown(super::unknown(error)))?
    {
        Some(raw) => {
            let value: grapt_api::u3a::SessionMessageResponseDto = serde_json::from_str(&raw)
                .map_err(|error| Error::Unknown(super::unknown(error)))?;
            let limits = grapt_api::u3a::U3aLimits::production()
                .map_err(|error| Error::Unknown(super::unknown(error)))?;
            match grapt_api::u3a::encode_page(&value, limits) {
                Ok(_) => Ok(value),
                Err(grapt_api::u3a::DtoCodecError::PageBytes) => {
                    Err(Error::Unknown(super::unknown(format!(
                        "History row {message} exceeds the {}-byte encoded page bound",
                        limits.receipt().page_bytes
                    ))))
                }
                Err(error) => Err(Error::Unknown(super::unknown(error))),
            }
        }
        None => {
            let error = super::decode(json!({
                "_tag":"MessageNotFoundError", "sessionID":session.as_str(),
                "messageID":message.as_str(), "message":format!("Message not found: {}",message.as_str()),
            })).map_err(|error| Error::Unknown(super::unknown(error)))?;
            Err(Error::MessageNotFound(error))
        }
    }
}

impl DogfoodApplication {
    /// Rows stay in the internal (strict) form through the page
    /// pipeline: strict parsers touch every row after this point.
    /// The frozen string wire form is produced exactly once, by the
    /// final DTO serialization.
    pub(super) fn history_json(
        &self,
        session: &SessionId,
        page: &MessagePage,
        view: grapt_api::u3a::MessageListView,
    ) -> Result<String, PageRefusal> {
        self.history_page(session, page, view, |page, rows, render| {
            page.render(rows, render)
        })
    }

    /// The same page as [`Self::history_json`], typed for the session
    /// snapshot: the rows and cursor its bound already decided, never re-read.
    pub(super) fn history_typed(
        &self,
        session: &SessionId,
        page: &MessagePage,
        view: grapt_api::u3a::MessageListView,
    ) -> Result<
        (
            Vec<grapt_api::u3a::SessionMessageInfoDto>,
            grapt_api::u3a::SessionMessagesResponseDtoCursor,
        ),
        PageRefusal,
    > {
        self.history_page(session, page, view, |page, rows, render| {
            page.render_typed(rows, render)
        })
    }

    /// One page's rows and renderer, handed to the page encoder `finish`.
    fn history_page<T>(
        &self,
        session: &SessionId,
        page: &MessagePage,
        view: grapt_api::u3a::MessageListView,
        finish: impl FnOnce(
            &MessagePage,
            &[IndexedRow],
            &mut dyn FnMut(&[IndexedRow]) -> Result<Vec<String>, String>,
        ) -> Result<T, PageRefusal>,
    ) -> Result<T, PageRefusal> {
        self.verify_session_carry_for_read(session)?;
        let (rows, omissions) = self.public_page(session, page)?;
        let skipped =
            grapt_session::notice::skipped_kinds(self.lock_inbox().store().connection(), session)?;
        super::history_notices::append(
            session,
            grapt_session::notice::NoticeRead {
                skipped,
                ..Default::default()
            },
            &mut Vec::new(),
        )?;
        let mut renderer = self.row_renderer(session);
        renderer.cache_diagnostics = view == grapt_api::u3a::MessageListView::LiveCache;
        let mut recent = RowRenderer::page_recent(&rows);
        // Each unreadable owned item is reported on the row whose durable range
        // contains it, as data in that row's metadata. Nothing rewrites a user's
        // words or an assistant's content, and nothing adds a row: the page's
        // cursor contract stays exactly one rendered row per indexed row.
        super::history_notices::log_omissions(session, &omissions);
        let omitted = super::history::OmittedItems::new(omissions);
        // The pager renders one row per callback, so the ranges come from the
        // whole page and the callback's row is located within it by identity.
        // Both page encoders (legacy JSON and the snapshot's typed rows) take
        // this one callback, so they render the same omission metadata.
        let ranges = RowRenderer::row_ranges(&rows);
        let located = rows
            .iter()
            .enumerate()
            .map(|(index, row)| (row.seq(), index))
            .collect::<std::collections::BTreeMap<_, _>>();
        finish(page, &rows, &mut |rows| {
            let offset = rows
                .first()
                .and_then(|row| located.get(&row.seq()).copied())
                .unwrap_or(0);
            renderer.render_with_recent(rows, &mut recent, &omitted, &ranges, offset)
        })
    }

    pub(super) fn message_json(
        &self,
        session: &SessionId,
        message: &MessageId,
    ) -> Result<Option<String>, String> {
        let Some(row) = self.public_message(session, message)? else {
            return Ok(None);
        };
        let (_, raw) = page_bytes::readable_row(
            &row,
            self.row_renderer(session)
                .render_page(std::slice::from_ref(&row)),
        )?;
        Ok(Some(format!("{{\"data\":{raw}}}")))
    }

    /// Visit public history in bounded pages. Consumers retain their own total
    /// item/byte limits instead of materializing a lifetime transcript first.
    pub(super) fn visit_history_rows(
        &self,
        session: &SessionId,
        mut visit: impl FnMut(String) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut page = MessagePage {
            limit: 128,
            order: Order::Asc,
            cursor: None,
        };
        loop {
            let (rows, _) = self
                .public_page(session, &page)
                .map_err(|error| format!("{error:?}"))?;
            if rows.is_empty() {
                return Ok(());
            }
            for raw in self.row_renderer(session).render_page(&rows)? {
                visit(raw)?;
            }
            let last = rows.last().ok_or("history page has no final row")?;
            page.cursor = Some(Cursor {
                id: WireId::parse(last.id.clone()).ok_or("history identity")?,
                order: Order::Asc,
                direction: Direction::Next,
            });
        }
    }

    fn row_renderer<'a>(&'a self, session: &'a SessionId) -> RowRenderer<'a> {
        RowRenderer {
            application: self,
            session,
            cache_diagnostics: false,
        }
    }

    fn promoted_user_row_at(
        &self,
        session: &SessionId,
        sequence: u64,
        coordinates: grapt_inbox::AdmissionCoordinates,
    ) -> Result<IndexedRow, String> {
        let inbox = self.lock_inbox();
        let sequence = grapt_types::EventSeq::new(sequence);
        let promoted = inbox
            .promoted_message_at(session, sequence)
            .map_err(|error| error.to_string())?
            .ok_or("selected promoted input is absent")?;
        let input = promoted.input;
        let id = serde_json::to_string(input.as_str()).map_err(|error| error.to_string())?;
        let text = serde_json::to_string(&promoted.text).map_err(|error| error.to_string())?;
        let encoded = match promoted.kind {
            grapt_inbox::PromotedInputKind::User => {
                let metadata = promoted
                    .metadata
                    .filter(|value| {
                        value.get("origin").and_then(Value::as_str) == Some("seat-startup")
                    })
                    .map(|_| json!({"origin":"seat-startup"}))
                    .map(|value| serde_json::to_string(&value))
                    .transpose()
                    .map_err(|error| error.to_string())?
                    .map(|value| format!(",\"metadata\":{value}"))
                    .unwrap_or_default();
                format!(
                    "{{\"type\":\"user\",\"id\":{id}{metadata},\"time\":{{\"created\":{}.0}},\"text\":{text}}}",
                    coordinates.created_ms
                )
            }
            grapt_inbox::PromotedInputKind::Synthetic => {
                let metadata = promoted
                    .metadata
                    .map(super::input_metadata::public_metadata)
                    .map(|value| serde_json::to_string(&value))
                    .transpose()
                    .map_err(|error| error.to_string())?
                    .map(|value| format!(",\"metadata\":{value}"))
                    .unwrap_or_default();
                let description = promoted
                    .description
                    .map(|value| serde_json::to_string(&value))
                    .transpose()
                    .map_err(|error| error.to_string())?
                    .map(|value| format!(",\"description\":{value}"))
                    .unwrap_or_default();
                format!(
                    "{{\"type\":\"synthetic\",\"id\":{id}{metadata},\"time\":{{\"created\":{}.0}},\"text\":{text}{description}}}",
                    coordinates.created_ms
                )
            }
        };
        Ok(IndexedRow::rendered(
            promoted.seq,
            input.as_str().to_owned(),
            encoded,
        ))
    }

    pub(super) fn mint_execution(&self) -> Result<ExecutionId, String> {
        ExecutionId::new(format!("exe_{}", self.ids.mint().as_str()))
            .ok_or_else(|| "execution identity was invalid".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row the index already rendered.
    fn ready(row: &IndexedRow) -> Result<String, String> {
        match &row.source {
            RowSource::Rendered(raw) => Ok(raw.clone()),
            _ => Err(format!("row {} is not rendered", row.id)),
        }
    }

    #[test]
    fn failure_message_roundtrips_through_public_message_page() -> Result<(), String> {
        let row = failure_message(
            "msg_failure_wire",
            123,
            &grapt_session::execution::Failure::Generic.public(),
        )?;
        let page = MessagePage {
            limit: 10,
            order: Order::Desc,
            cursor: None,
        };
        let raw = page
            .project(
                vec![IndexedRow::rendered(
                    2,
                    "msg_failure_wire".into(),
                    row.clone(),
                )],
                |rows| rows.iter().map(ready).collect(),
            )
            .map_err(|error| format!("{error:?}"))?;
        let decoded = serde_json::from_str::<grapt_api::u3a::SessionMessagesResponseDto>(&raw)
            .map_err(|error| format!("failure history rejected by public DTO: {error}"))?;
        let output = serde_json::to_value(decoded).map_err(|error| error.to_string())?;
        assert_eq!(output["data"][0]["id"], "msg_failure_wire");
        assert_eq!(output["data"][0]["type"], "system");
        assert_eq!(output["data"][0]["metadata"]["executionFailure"], true);
        assert_eq!(output["data"][0]["time"]["created"], 123.0);
        let reordered: Value = serde_json::from_str(&row).map_err(|error| error.to_string())?;
        assert!(
            serde_json::from_str::<grapt_api::u3a::SessionMessageInfoDto>(&reordered.to_string())
                .is_err(),
            "public tag-first decoder guard was not reached"
        );
        Ok(())
    }

    #[test]
    fn retained_message_order_and_bidirectional_cursors_preserve_the_timeline(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let rows = || {
            (0..5)
                .map(|i| {
                    let id = format!("msg_{i}");
                    let raw = format!(
                        r#"{{"type":"user","id":"{id}","time":{{"created":0.0}},"text":"row"}}"#
                    );
                    IndexedRow::rendered(i, id, raw)
                })
                .collect::<Vec<_>>()
        };
        let page = |input| -> Result<Value, String> {
            let query = super::super::decode(input)?;
            let page = MessagePage::parse(query).map_err(|error| format!("{error:?}"))?;
            let raw = page
                .project(rows(), |rows| rows.iter().map(ready).collect())
                .map_err(|error| format!("{error:?}"))?;
            let mut value: Value = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
            for row in value["data"].as_array_mut().ok_or("data")? {
                let id = row["id"].clone();
                *row = json!({"id":id});
            }
            Ok(value)
        };
        let first = page(json!({"limit":"2"}))?;
        assert_eq!(first["data"], json!([{"id":"msg_4"},{"id":"msg_3"}]));
        let next = page(json!({"limit":"2","cursor":first["cursor"]["next"]}))?;
        assert_eq!(next["data"], json!([{"id":"msg_2"},{"id":"msg_1"}]));
        let prior = page(json!({"limit":"2","cursor":next["cursor"]["previous"]}))?;
        assert_eq!(prior["data"], first["data"]);
        let asc = page(json!({"limit":"2","order":"asc"}))?;
        assert_eq!(asc["data"], json!([{"id":"msg_0"},{"id":"msg_1"}]));
        let next_asc = page(json!({"limit":"2","cursor":asc["cursor"]["next"]}))?;
        assert_eq!(next_asc["data"], json!([{"id":"msg_2"},{"id":"msg_3"}]));
        assert!(page(json!({"order":"asc","cursor":first["cursor"]["next"]})).is_err());
        assert!(page(json!({"cursor":"not-json"})).is_err());
        assert!(page(json!({"limit":"201"})).is_err());
        assert!(page(json!({"limit":"0"})).is_err());
        let foreign =
            URL_SAFE_NO_PAD.encode(br#"{"id":"msg_foreign","order":"desc","direction":"next"}"#);
        let stale = page(json!({"cursor":foreign}));
        assert!(
            stale
                .as_ref()
                .is_err_and(|error| error.contains("StaleCursor")),
            "a cursor naming no message in this Session returned a page: {stale:?}"
        );
        Ok(())
    }

    /// W5: untrusted stored text is made safe for a screen — every terminal
    /// control char is escaped, the result is one printable line, and the
    /// byte cap holds on a char boundary.
    #[test]
    fn display_safe_escapes_terminal_machinery_and_caps_on_char_boundaries() {
        let raw = "boom\u{1b}[31m\nsecond\u{9f}line";
        let out = display_safe(raw, 512);
        assert_eq!(
            out, "boom\\u{1b}[31m\\u{a}second\\u{9f}line",
            "each control char becomes its named escape"
        );
        assert!(
            out.chars().all(is_terminal_printable),
            "no terminal machinery survives: {out:?}"
        );
        assert_eq!(
            display_safe("k".repeat(200).as_str(), 64),
            "k".repeat(64),
            "the cap counts bytes"
        );
        let wide = "\u{4e2d}".repeat(40);
        assert_eq!(
            display_safe(&wide, 64),
            "\u{4e2d}".repeat(21),
            "the cap never splits a char"
        );
    }

    /// W5: a row a later build wrote renders its STORED name, so a future
    /// class reads as one; a row this build wrote keeps its byte-identical
    /// rendering.
    #[test]
    fn failure_message_renders_a_future_name_and_keeps_known_rows_identical() -> Result<(), String>
    {
        let future = grapt_session::execution::PublicFailure {
            name: "ProviderQuotaExhausted".to_owned(),
            message: "the future build's own wording".to_owned(),
        };
        let row = failure_message("msg_future_name", 456, &future)?;
        let value: Value = serde_json::from_str(&row).map_err(|error| error.to_string())?;
        assert_eq!(
            value["text"],
            "Execution failed (ProviderQuotaExhausted): the future build's own wording",
            "the stored name is rendered, so the user sees a future class as one"
        );

        let known = grapt_session::execution::Failure::Generic.public();
        let row = failure_message("msg_known_name", 789, &known)?;
        let value: Value = serde_json::from_str(&row).map_err(|error| error.to_string())?;
        assert_eq!(
            value["text"],
            "Execution failed: The execution stopped before completing. Inspect the Session before retrying.",
            "a known row keeps its byte-identical rendering, name and all"
        );
        Ok(())
    }
}
