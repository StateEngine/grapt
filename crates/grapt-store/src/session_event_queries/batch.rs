//! Ephemeral, bounded selected-call reads; no retained state or new indexes.
use super::*;

pub const TARGETS: usize = 8;
pub const MAX_ROWS: usize = 1_280;
const SHARED_DATA_BYTES: usize = 512 * 1024;
pub type Rows = Vec<(u64, String)>;

fn materialize_data(data: &str) -> String {
    #[cfg(test)]
    COPIED_DATA_BYTES.with(|bytes| bytes.set(bytes.get().saturating_add(data.len())));
    data.to_owned()
}

#[cfg(test)]
std::thread_local! {
    static COPIED_DATA_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn take_materialized_bytes() -> usize {
    COPIED_DATA_BYTES.with(|bytes| bytes.replace(0))
}

/// Terminals go through the stored-terminal bound, as every reader's do;
/// every other kind reads its stored data column.
fn data_column(terminal_kind: bool) -> std::borrow::Cow<'static, str> {
    if terminal_kind {
        crate::stored_terminal::bounded_column("e.data").into()
    } else {
        "e.data".into()
    }
}

/// Only the bounded terminal column is bytes. Every other kind keeps the
/// strict text read its individual query uses, so a blob takes that refusal.
fn row_text(value: rusqlite::types::ValueRef<'_>, terminal_kind: bool) -> Option<&str> {
    if terminal_kind {
        value
            .as_bytes()
            .ok()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
    } else {
        value.as_str().ok()
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Selection {
    Call(&'static str),
    Model,
}
impl Selection {
    pub(super) fn scope(self, target: &ToolCallId) -> Scope<'_> {
        match self {
            Self::Call(path) => Scope::Call(Some(target), path),
            Self::Model => Scope::Model(Some(target)),
        }
    }
}

#[derive(Debug)]
pub struct Pages {
    pub rows: Vec<Rows>,
    #[cfg(test)]
    pub(super) steps: u64,
    #[cfg(test)]
    pub(super) statements: usize,
}
/// One original row ceiling bounds the shared probe. Overflow, negative
/// sequences and invalid stored SQL column types use the exact readers, so
/// one target cannot change another target's bound or corruption precedence.
/// `None` requests the individual path, never an empty result. Real SQLite
/// read failures remain failures; they never retry into success.
pub fn read(
    store: &EventStore,
    session: &SessionId,
    kind: &str,
    targets: &[ToolCallId],
    selection: Selection,
    bound: usize,
) -> Result<Option<Pages>, String> {
    if targets.is_empty() || targets.len() > TARGETS || bound > MAX_ROWS {
        return Err("invalid selected-call batch bound".into());
    }
    let (_, path, _) = scope_selection(selection.scope(&targets[0]));
    let comparison = "IN (SELECT value FROM json_each(?5))";
    let predicate = match selection {
        Selection::Call(_) => call_predicate(comparison),
        Selection::Model => model_predicate(comparison),
    };
    let visible = crate::history_visibility::HistoryVisibility::read(store.connection(), session)
        .map_err(|error| error.to_string())?
        .sql("e.seq");
    let terminal_kind = kind == "session.tool.terminal.1";
    let sql = scoped_sql(
        &data_column(terminal_kind),
        &predicate,
        // The target a row names, as its ordinal in ?5: the identity itself
        // is compared in place and never copied out of SQLite.
        ", CASE WHEN json_valid(e.data) = 0 THEN NULL
         WHEN json_type(e.data, ?6) = 'text' THEN COALESCE((SELECT key FROM json_each(?5)
             WHERE value = json_extract(e.data, ?6) LIMIT 1), -1)
         ELSE NULL END",
        &visible,
    );
    let target_json =
        serde_json::to_string(&targets.iter().map(ToolCallId::as_str).collect::<Vec<_>>())
            .map_err(|error| error.to_string())?;
    let limit = i64::try_from(bound.saturating_add(1)).map_err(|error| error.to_string())?;
    let mut statement = match store.connection().prepare(&sql) {
        Ok(statement) => statement,
        Err(error) => return Err(error.to_string()),
    };
    let rows = {
        let mut cursor = statement
            .query((
                session.as_str(),
                kind,
                limit,
                i64::MIN,
                target_json,
                path,
                Option::<i64>::None,
            ))
            .map_err(|error| format!("continuation query failed: {error}"))?;
        let mut rows = Vec::new();
        let mut bytes = 0usize;
        while let Some(row) = cursor.next().map_err(|error| error.to_string())? {
            if rows.len() == bound {
                return Ok(None);
            }
            let value = row.get_ref(1).map_err(|error| error.to_string())?;
            let Some(data) = row_text(value, terminal_kind) else {
                return Ok(None);
            };
            let next_bytes = bytes.saturating_add(data.len());
            // SQLite still owns the data when the cumulative copy limit fires.
            if next_bytes > SHARED_DATA_BYTES {
                return Ok(None);
            }
            let header = row
                .get::<_, i64>(0)
                .and_then(|seq| row.get::<_, Option<i64>>(2).map(|key| (seq, key)));
            let (seq, key) = match header {
                Ok(header) => header,
                Err(error) if error.sqlite_error().is_none() => return Ok(None),
                Err(error) => return Err(error.to_string()),
            };
            if seq < 0 {
                return Ok(None);
            }
            rows.push((seq, materialize_data(data), key));
            bytes = next_bytes;
        }
        rows
    };
    #[cfg(test)]
    let steps = u64::try_from(statement.get_status(rusqlite::StatementStatus::VmStep))
        .map_err(|error| error.to_string())?;
    Ok(Some(Pages {
        rows: targets
            .iter()
            .map(|target| {
                let names = |ordinal: &i64| {
                    usize::try_from(*ordinal)
                        .ok()
                        .and_then(|ordinal| targets.get(ordinal))
                        .is_some_and(|named| named == target)
                };
                rows.iter()
                    .filter(|(_, _, key)| key.as_ref().is_none_or(names))
                    .map(|(seq, data, _)| (*seq as u64, data.clone()))
                    .collect()
            })
            .collect(),
        #[cfg(test)]
        steps,
        #[cfg(test)]
        statements: 1,
    }))
}
