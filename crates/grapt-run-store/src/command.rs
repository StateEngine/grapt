use crate::error::WriteError;
use crate::key::RequestKey;
use grapt_types::SessionId;
use rusqlite::types::Value;
use sha2::{Digest, Sha256};

pub(crate) const MAX_COMMAND_BYTES: usize = 1024 * 1024;
const MAX_STATEMENTS: usize = 32;
pub(crate) const MAX_SQL_BYTES: usize = 4096;
pub(crate) const MAX_PARAMS: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Durability {
    Normal,
    Full,
}

/// A scheduling promise by the domain owner. Use Global for shared state and
/// commands whose complete write footprint cannot be named by session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Scope {
    Session(SessionId),
    Pair(SessionId, SessionId),
    Global,
}

impl Scope {
    pub(crate) fn intersects(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Global, _) => true,
            (Self::Session(session), other) => other.includes(session),
            (Self::Pair(first, second), other) => other.includes(first) || other.includes(second),
        }
    }

    fn bytes(&self) -> usize {
        match self {
            Self::Global => 0,
            Self::Session(id) => id.as_str().len(),
            Self::Pair(first, second) => first.as_str().len().saturating_add(second.as_str().len()),
        }
    }

    fn includes(&self, session: &SessionId) -> bool {
        match self {
            Self::Global => true,
            Self::Session(id) => id == session,
            Self::Pair(first, second) => first == session || second == session,
        }
    }
}

/// Parameterized domain mutation. Transaction control and receipt storage
/// belong to the writer. SQL must be authored by the domain, with caller data
/// passed as values.
#[derive(Debug)]
pub struct Statement {
    pub(crate) sql: String,
    pub(crate) params: Vec<Value>,
    pub(crate) expected: Option<u64>,
}

impl Statement {
    pub fn new(sql: impl Into<String>, params: Vec<Value>) -> Self {
        Self {
            sql: sql.into(),
            params,
            expected: None,
        }
    }

    pub fn requiring_rows(mut self, rows: u64) -> Self {
        self.expected = Some(rows);
        self
    }

    fn bytes(&self) -> usize {
        self.params.iter().fold(
            self.sql.len() + std::mem::size_of::<Self>(),
            |bytes, value| {
                let payload = match value {
                    Value::Text(text) => text.len(),
                    Value::Blob(blob) => blob.len(),
                    _ => 0,
                };
                bytes.saturating_add(std::mem::size_of::<Value>() + payload)
            },
        )
    }
}

#[derive(Debug)]
pub struct Command {
    pub(crate) key: RequestKey,
    pub(crate) durability: Durability,
    pub(crate) scope: Scope,
    pub(crate) statements: Vec<Statement>,
    pub(crate) bytes: usize,
}

impl Command {
    pub fn new(
        key: RequestKey,
        durability: Durability,
        scope: Scope,
        statements: Vec<Statement>,
    ) -> Result<Self, WriteError> {
        let bytes = statements.iter().fold(
            std::mem::size_of::<Self>()
                .saturating_add(key.as_str().len())
                .saturating_add(scope.bytes()),
            |bytes, statement| bytes.saturating_add(statement.bytes()),
        );
        if statements.is_empty()
            || bytes > MAX_COMMAND_BYTES
            || statements.len() > MAX_STATEMENTS
            || statements.iter().any(|statement| {
                statement.sql.len() > MAX_SQL_BYTES || statement.params.len() > MAX_PARAMS
            })
        {
            return Err(WriteError::CommandTooLarge);
        }
        // Move only checked lengths into the queue. Callers may have spare
        // capacity in their source buffers that is irrelevant to the command.
        let statements = statements
            .into_iter()
            .map(|mut statement| {
                statement.sql = statement.sql.into_boxed_str().into_string();
                statement.params = statement
                    .params
                    .into_iter()
                    .map(|value| match value {
                        Value::Text(text) => Value::Text(text.into_boxed_str().into_string()),
                        Value::Blob(bytes) => Value::Blob(bytes.into_boxed_slice().into_vec()),
                        value => value,
                    })
                    .collect::<Box<[_]>>()
                    .into_vec();
                statement
            })
            .collect::<Box<[_]>>()
            .into_vec();
        Ok(Self {
            key,
            durability,
            scope,
            statements,
            bytes,
        })
    }

    pub(crate) fn digest(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update([1, u8::from(self.durability == Durability::Full)]);
        match &self.scope {
            Scope::Global => hash.update([0]),
            Scope::Session(session) => {
                hash.update([1]);
                field(&mut hash, session.as_str().as_bytes());
            }
            Scope::Pair(first, second) => {
                hash.update([2]);
                field(&mut hash, first.as_str().as_bytes());
                field(&mut hash, second.as_str().as_bytes());
            }
        }
        for statement in &self.statements {
            field(&mut hash, statement.sql.as_bytes());
            hash.update((statement.params.len() as u64).to_le_bytes());
            for value in &statement.params {
                hash_value(&mut hash, value);
            }
            hash.update([u8::from(statement.expected.is_some())]);
            if let Some(expected) = statement.expected {
                hash.update(expected.to_le_bytes());
            }
        }
        hash.finalize().into()
    }
}

fn field(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value);
}

fn hash_value(hash: &mut Sha256, value: &Value) {
    match value {
        Value::Null => hash.update([0]),
        Value::Integer(number) => {
            hash.update([1]);
            hash.update(number.to_le_bytes());
        }
        Value::Real(number) => {
            hash.update([2]);
            hash.update(number.to_bits().to_le_bytes());
        }
        Value::Text(text) => {
            hash.update([3]);
            field(hash, text.as_bytes());
        }
        Value::Blob(blob) => {
            hash.update([4]);
            field(hash, blob);
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct Receipt {
    pub affected_rows: Vec<u64>,
}

impl Receipt {
    pub(crate) fn encode(&self) -> Vec<u8> {
        self.affected_rows
            .iter()
            .flat_map(|rows| rows.to_le_bytes())
            .collect()
    }

    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.is_empty() || bytes.len() > MAX_STATEMENTS * 8 || !bytes.len().is_multiple_of(8) {
            return None;
        }
        let affected_rows = bytes
            .chunks_exact(8)
            .map(|chunk| {
                let mut rows = [0; 8];
                rows.copy_from_slice(chunk);
                u64::from_le_bytes(rows)
            })
            .collect();
        Some(Self { affected_rows })
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, Durability, RequestKey, Scope, Statement, WriteError};

    #[test]
    fn both_session_ids_count_toward_command_capacity() -> Result<(), Box<dyn std::error::Error>> {
        let first: grapt_types::SessionId = format!("ses_{}", "a".repeat(600 * 1024)).parse()?;
        let second = format!("ses_{}", "b".repeat(600 * 1024)).parse()?;
        let command = |scope| {
            Command::new(
                RequestKey::new("one")?,
                Durability::Full,
                scope,
                vec![Statement::new("DELETE FROM event WHERE seq=1", vec![])],
            )
        };
        let single = command(Scope::Session(first.clone()))?;
        assert!(single.bytes >= 600 * 1024);
        assert!(matches!(
            command(Scope::Pair(first, second)),
            Err(WriteError::CommandTooLarge)
        ));
        Ok(())
    }
}
