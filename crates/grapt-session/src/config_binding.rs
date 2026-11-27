//! Audited per-Session configuration selection, without storing credentials.
use grapt_store::{AggregateId, AppendInput, Appended, EventStore};
use grapt_types::remote_mail::{SourcePrincipalId, SourceProjectId};
use grapt_types::{DeterministicAuthorities, EventId, EventVersion, SessionId};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawBinding", into = "RawBinding")]
pub struct ConfigBinding {
    pub path: String,
    pub project: SourceProjectId,
    pub agent: SourcePrincipalId,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBinding {
    path: String,
    project: String,
    agent: String,
}
impl TryFrom<RawBinding> for ConfigBinding {
    type Error = String;
    fn try_from(raw: RawBinding) -> Result<Self, Self::Error> {
        let binding = Self {
            path: raw.path,
            project: SourceProjectId::try_new(raw.project).map_err(|error| error.to_string())?,
            agent: SourcePrincipalId::try_new(raw.agent).map_err(|error| error.to_string())?,
        };
        binding.validate()?;
        Ok(binding)
    }
}
impl From<ConfigBinding> for RawBinding {
    fn from(binding: ConfigBinding) -> Self {
        Self {
            path: binding.path,
            project: binding.project.as_str().into(),
            agent: binding.agent.as_str().into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub binding: ConfigBinding,
    pub sequence: u64,
}
impl ConfigBinding {
    fn validate(&self) -> Result<(), String> {
        if self.path.is_empty()
            || self.path.len() > 4096
            || self.path.chars().any(char::is_control)
            || ![grapt_types::Platform::Posix, grapt_types::Platform::Windows]
                .into_iter()
                .any(|platform| grapt_types::LocationRef::on(platform, &self.path, None).is_ok())
            || [self.project.as_str(), self.agent.as_str()]
                .iter()
                .any(|value| {
                    value.is_empty() || value.len() > 256 || value.chars().any(char::is_control)
                })
        {
            return Err("Session config binding exceeds its identity or path bound".into());
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {
    #[serde(rename = "sessionID")]
    session: String,
    binding: ConfigBinding,
}

pub fn read(connection: &Connection, session: &SessionId) -> Result<Option<Selection>, String> {
    let row: Option<(String, Option<String>, i64)> = connection
        .query_row(
            "SELECT type, CASE WHEN length(CAST(data AS BLOB)) <= 32768 THEN data END, seq
         FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
         AND type GLOB 'session.config.bound.*' ORDER BY +seq DESC LIMIT 1",
            [session.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|_| "Session config binding cannot be read")?;
    let Some((kind, raw, sequence)) = row else {
        return Ok(None);
    };
    if kind != "session.config.bound.1" {
        return Err("Session config binding version is unsupported".into());
    }
    let data: Data =
        serde_json::from_str(&raw.ok_or("Session config binding exceeds its byte bound")?)
            .map_err(|_| "Session config binding is corrupt")?;
    if data.session != session.as_str() {
        return Err("Session config binding identity differs".into());
    }
    data.binding.validate()?;
    Ok(Some(Selection {
        binding: data.binding,
        sequence: u64::try_from(sequence).map_err(|_| "Session config sequence is negative")?,
    }))
}

/// Each accepted replacement is a fenced event, retaining the prior audit row.
pub fn bind(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    binding: ConfigBinding,
) -> Result<(), String> {
    binding.validate()?;
    let aggregate = AggregateId::Session(session.clone());
    let writer = store
        .fencing_token(&aggregate)
        .map_err(|error| error.to_string())?;
    let tx = store
        .governed_transaction()
        .map_err(|error| error.to_string())?;
    crate::SessionStore::new(&tx)
        .record(session)
        .map_err(|error| error.to_string())?;
    let event: EventId = format!("evt_{}", authorities.mint().as_str())
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    let data = serde_json::to_value(Data {
        session: session.as_str().into(),
        binding,
    })
    .map_err(|_| "Session config binding cannot be encoded")?;
    let appended = EventStore::append_within(
        &tx,
        AppendInput {
            id: &event,
            aggregate,
            writer,
            kind_name: "session.config.bound",
            version: EventVersion::new(1).ok_or("Session config version refused")?,
            created_ms: authorities.now().as_millis(),
            data: &data,
        },
        &mut [],
    )
    .map_err(|error| error.to_string())?;
    if !matches!(appended, Appended::Committed { .. }) {
        return Err("Session config event identity repeated".into());
    }
    tx.commit().map_err(|error| error.to_string())?.discard();
    Ok(())
}
