//! The tool definitions a Session's requests had to leave out, and why.
//!
//! A definition the provider cannot carry is dropped from a request rather than
//! failing the turn, so the model simply does not receive the tool. A silently
//! missing tool is the failure a reader cannot diagnose, so the set is recorded
//! once: an unchanged set is not announced again however many turns follow, and
//! a set that empties is announced once as a repair.
//!
//! Session-scoped rather than execution-scoped: planning decides exclusions
//! before an execution exists, and the reader needs to know which tools are
//! missing for the Session, not which attempt noticed it.
//!
//! Identity is a digest of the COMPLETE original set, never of the displayed
//! rows: two sets can share a prefix, a length, or a row and differ past it.

use super::*;
use sha2::{Digest, Sha256};

pub(super) const TOOL_EXCLUDED: &str = "tool-excluded";

/// One definition left out of a request.
#[derive(Debug, Clone, PartialEq, Eq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExcludedTool {
    pub name: String,
    /// A product-authored reason, never a provider string.
    pub reason: String,
}
impl ExcludedTool {
    fn bounded(self) -> Result<Self, String> {
        let tool = Self {
            name: bounded(&self.name),
            reason: bounded(&self.reason),
        };
        if tool.name.is_empty() || tool.reason.is_empty() {
            return Err("an excluded tool names itself and its reason".into());
        }
        Ok(tool)
    }
}

/// The bounded set of definitions left out of a Session's requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolExcludedNotice {
    pub position: CommittedPosition,
    /// The exclusions named, at most [`MAX_UNAVAILABLE`].
    pub excluded: Vec<ExcludedTool>,
    /// How many definitions the named rows do not show, derived at the write.
    pub omitted: usize,
    /// The identity of the complete original set, as written: `None` for a row
    /// written before identities existed, which is unknown rather than unchanged.
    identity: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Data {
    #[serde(rename = "sessionID")]
    session: String,
    kind: String,
    excluded: Vec<ExcludedTool>,
    /// Absent on rows written before the set identity existed.
    #[serde(default)]
    omitted: usize,
    #[serde(default)]
    identity: Option<String>,
}

/// The identity of one canonical set: each part is length-prefixed, so no two
/// different pairs can encode to the same bytes.
fn identity_of(canonical: &[ExcludedTool]) -> String {
    let mut hasher = Sha256::new();
    for tool in canonical {
        for part in [&tool.name, &tool.reason] {
            hasher.update((part.len() as u64).to_le_bytes());
            hasher.update(part.as_bytes());
        }
    }
    format!("{:x}", hasher.finalize())
}

/// A validated row: its bounded rows, the count they do not show, its identity.
type Validated = (Vec<ExcludedTool>, usize, Option<String>);

fn validate(data: &Data, session: &SessionId) -> Result<Validated, String> {
    if data.session != session.as_str()
        || data.kind != TOOL_EXCLUDED
        || data.excluded.len() > MAX_UNAVAILABLE
        || data.omitted > 1_000_000
    {
        return Err("tool exclusion notice is invalid".into());
    }
    let excluded = data
        .excluded
        .iter()
        .cloned()
        .map(ExcludedTool::bounded)
        .collect::<Result<Vec<_>, _>>()?;
    Ok((excluded, data.omitted, data.identity.clone()))
}

/// Record the definitions this Session's requests left out, unless its last such
/// notice carries the same set identity. `None` when nothing changed; a set that
/// empties is recorded once as a repair. The caller states the complete set and
/// this owner derives everything else from it.
pub fn record_tool_excluded(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    mut excluded: Vec<ExcludedTool>,
) -> Result<Option<ToolExcludedNotice>, String> {
    // Canonical first, so reordering the same set is not a change; the digest
    // covers the complete originals, before any display bound.
    excluded.sort();
    excluded.dedup();
    let identity = identity_of(&excluded);
    // Cardinality is of the ORIGINAL set and never of the display rows: two
    // distinct long names can bound to one row, and counting rows there would
    // silently drop one missing definition from the reader's total.
    let total = excluded.len();
    let mut excluded = excluded
        .into_iter()
        .map(ExcludedTool::bounded)
        .collect::<Result<Vec<_>, _>>()?;
    excluded.sort();
    excluded.dedup();
    excluded.truncate(MAX_UNAVAILABLE);
    // Everything the named rows do not show, including a bounded collision.
    let omitted = total.saturating_sub(excluded.len());
    let last = at(store.connection(), session, None)?.pop();
    // Silence when this exact set is already recorded, or there is nothing to
    // say. An identity-less row is unknown, so it is replaced, never trusted.
    let silent = total == 0;
    let recorded = last.as_ref().is_some_and(|last| {
        last.identity.as_deref() == Some(identity.as_str())
            || (silent && last.excluded.is_empty() && last.omitted == 0)
    });
    if recorded || (last.is_none() && silent) {
        return Ok(None);
    }
    let data = Data {
        session: session.as_str().to_owned(),
        kind: TOOL_EXCLUDED.to_owned(),
        excluded: excluded.clone(),
        omitted,
        identity: Some(identity.clone()),
    };
    // The writer never commits a row its own reader refuses.
    validate(&data, session)?;
    let data = serde_json::to_value(data).map_err(|error| error.to_string())?;
    let aggregate = AggregateId::Session(session.clone());
    let writer = store
        .fencing_token(&aggregate)
        .map_err(|error| error.to_string())?;
    let tx = store
        .governed_transaction()
        .map_err(|error| error.to_string())?;
    SessionStore::new(&tx)
        .record(session)
        .map_err(|error| error.to_string())?;
    let position = append_in(&tx, aggregate, writer, authorities, &data)?;
    tx.commit().map_err(|error| error.to_string())?.discard();
    Ok(Some(ToolExcludedNotice {
        position,
        excluded,
        omitted,
        identity: Some(identity),
    }))
}

pub(super) fn at(
    connection: &rusqlite::Connection,
    session: &SessionId,
    seq: Option<u64>,
) -> Result<Vec<ToolExcludedNotice>, String> {
    kind_rows_at::<Data>(connection, session, TOOL_EXCLUDED, seq)?
        .into_iter()
        .map(|(position, data)| {
            let (excluded, omitted, identity) = validate(&data, session)?;
            Ok(ToolExcludedNotice {
                position,
                excluded,
                omitted,
                identity,
            })
        })
        .collect()
}
