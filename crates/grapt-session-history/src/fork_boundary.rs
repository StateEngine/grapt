//! Bounded read of immutable history-fork ancestry, shared by Session readers.
use grapt_types::{ForkBoundary, MessageId, SessionId};
use rusqlite::Statement;
use serde_json::Value;

pub const SQL: &str = "SELECT CASE WHEN length(CAST(type AS BLOB)) <= 128 THEN type END,
    CASE WHEN length(CAST(data AS BLOB)) <= 16384 THEN data END
    FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
    AND type >= 'session.forked.' AND type < 'session.forked/' ORDER BY +seq LIMIT 2";
pub const BOUNDARY_SQL: &str = "SELECT EXISTS (
    SELECT 1 FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1 AND seq = ?2)";

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("{0}")]
    Store(#[from] rusqlite::Error),
    #[error("{0}")]
    Corrupt(String),
}
impl From<String> for ReadError {
    fn from(reason: String) -> Self {
        Self::Corrupt(reason)
    }
}
impl From<&str> for ReadError {
    fn from(reason: &str) -> Self {
        Self::Corrupt(reason.to_owned())
    }
}

pub fn read(
    statement: &mut Statement<'_>,
    boundary_statement: &mut Statement<'_>,
    session: &SessionId,
) -> Result<Option<ForkBoundary>, ReadError> {
    let mut rows = statement.query([session.as_str()])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let kind: Option<String> = row.get(0)?;
    let kind = kind.ok_or("history fork type exceeds 128 bytes")?;
    let raw: Option<String> = row.get(1)?;
    if kind != "session.forked.2" {
        return Err("unsupported history fork version".into());
    }
    let value: Value = serde_json::from_str(&raw.ok_or("history fork exceeds 16 KiB")?)
        .map_err(|e| format!("history fork JSON: {e}"))?;
    let object = value.as_object().ok_or("history fork is not an object")?;
    if object
        .keys()
        .any(|key| !["sessionID", "parentID", "parentSeq", "from"].contains(&key.as_str()))
    {
        return Err("history fork contains an unknown field".into());
    }
    let child: SessionId = object
        .get("sessionID")
        .and_then(Value::as_str)
        .ok_or("history fork sessionID absent")?
        .parse()
        .map_err(|_| "history fork sessionID invalid")?;
    if &child != session {
        return Err("history fork session differs from aggregate".into());
    }
    let parent: SessionId = object
        .get("parentID")
        .and_then(Value::as_str)
        .ok_or("history fork parentID absent")?
        .parse()
        .map_err(|_| "history fork parentID invalid")?;
    let seq = object
        .get("parentSeq")
        .and_then(Value::as_i64)
        .ok_or("history fork parentSeq is not an integer")?;
    let boundary =
        ForkBoundary::from_raw_parent_seq(child, parent, seq).map_err(|e| e.to_string())?;
    if seq >= 0 {
        let exists: bool = boundary_statement
            .query_row(rusqlite::params![boundary.parent().as_str(), seq], |row| {
                row.get(0)
            })?;
        if !exists {
            return Err("history fork parent boundary is missing".into());
        }
    }
    if let Some(from) = object.get("from") {
        from.as_str()
            .ok_or("history fork from is not a message")?
            .parse::<MessageId>()
            .map_err(|_| "history fork from is not a message")?;
    }
    if rows.next()?.is_some() {
        return Err("repeated history fork".into());
    }
    Ok(Some(boundary))
}
