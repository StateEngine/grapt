// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: store-owned SQLite revision comparison and persistent
// sign-out tombstones; no file/keyring fallback or plugin credential namespace.
// (retire_refresh_token_if_matches), commit dffa0deb69268ed4ac31578794c9ea897b336ae3.

//! The MCP OAuth credential's sole durable owner. Network refresh serialization
//! stays outside this synchronous transaction; a revision comparison prevents
//! late refresh, login, or rejection from overwriting a newer login/sign-out.

use crate::governed::GovernedTable;
use crate::{EventStore, WriteError};
use grapt_types::Secret;
use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};

pub const MAX_MCP_CREDENTIAL_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct McpCredentialKey([u8; 32]);

impl std::fmt::Debug for McpCredentialKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("McpCredentialKey([REDACTED])")
    }
}

impl McpCredentialKey {
    /// `endpoint` is the adapter's canonical full server URL. Aliases and
    /// path/query differences intentionally do not share OAuth credentials.
    pub fn for_server(server: &str, endpoint: &str) -> Result<Self, WriteError> {
        if server.is_empty() || server.len() > 256 || endpoint.is_empty() || endpoint.len() > 8192 {
            return Err(WriteError::McpCredentialInput);
        }
        let mut hash = Sha256::new();
        for field in ["secode-mcp-oauth-v1", server, endpoint] {
            hash.update((field.len() as u64).to_be_bytes());
            hash.update(field.as_bytes());
        }
        Ok(Self(hash.finalize().into()))
    }

    fn sql_key(&self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

/// An absent row and a stored sign-out tombstone are distinct observations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpCredentialRevision {
    Missing,
    Stored(u64),
}

#[derive(Debug)]
pub struct McpCredentialSnapshot {
    pub revision: McpCredentialRevision,
    pub value: Option<Secret<String>>,
}

pub(crate) fn ensure_schema(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS mcp_oauth_credential (
            key TEXT PRIMARY KEY CHECK(length(key) = 64),
            revision INTEGER NOT NULL CHECK(revision > 0),
            value TEXT CHECK(value IS NULL OR length(CAST(value AS BLOB)) BETWEEN 1 AND 65536)
        );",
    )
}

impl EventStore {
    /// A database error or corrupt row is an error, never an empty credential.
    pub fn mcp_credential(
        &self,
        key: &McpCredentialKey,
    ) -> Result<McpCredentialSnapshot, WriteError> {
        let row = self.connection().query_row(
            "SELECT revision, value IS NULL, substr(CAST(value AS BLOB), 1, 65537) FROM mcp_oauth_credential WHERE key = ?1",
            [key.sql_key()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?, row.get::<_, Option<Vec<u8>>>(2)?)),
        ).optional()?;
        match row {
            None => Ok(McpCredentialSnapshot {
                revision: McpCredentialRevision::Missing,
                value: None,
            }),
            Some((revision, is_null, value)) => {
                // SQLite substr maps an empty blob to NULL. Only an original
                // SQL NULL is a tombstone; damaged empty values stay errors.
                if is_null != value.is_none() {
                    return Err(WriteError::McpCredentialCorrupt);
                }
                let revision = u64::try_from(revision)
                    .ok()
                    .filter(|revision| *revision > 0)
                    .ok_or(WriteError::McpCredentialCorrupt)?;
                if value
                    .as_ref()
                    .is_some_and(|value| value.is_empty() || value.len() > MAX_MCP_CREDENTIAL_BYTES)
                {
                    return Err(WriteError::McpCredentialCorrupt);
                }
                let value = value
                    .map(String::from_utf8)
                    .transpose()
                    .map_err(|_| WriteError::McpCredentialCorrupt)?;
                Ok(McpCredentialSnapshot {
                    revision: McpCredentialRevision::Stored(revision),
                    value: value.map(Secret::new),
                })
            }
        }
    }

    /// Commit credentials, or sign out with `None`, only if the exact observed
    /// revision still holds. Sign-out retains a tombstone: deleting the row
    /// would let a pre-login `Missing` snapshot resurrect a credential.
    /// Callers expose new tokens only after this operation succeeds.
    pub fn replace_mcp_credential(
        &mut self,
        key: &McpCredentialKey,
        expected: McpCredentialRevision,
        value: Option<Secret<String>>,
    ) -> Result<McpCredentialSnapshot, WriteError> {
        if value.as_ref().is_some_and(|value| {
            value.expose().is_empty() || value.expose().len() > MAX_MCP_CREDENTIAL_BYTES
        }) {
            return Err(WriteError::McpCredentialInput);
        }
        let (next, prior) = match expected {
            McpCredentialRevision::Missing => (1, None),
            McpCredentialRevision::Stored(revision) => {
                let revision = i64::try_from(revision)
                    .ok()
                    .filter(|revision| *revision > 0)
                    .ok_or(WriteError::McpCredentialExhausted)?;
                (
                    revision
                        .checked_add(1)
                        .ok_or(WriteError::McpCredentialExhausted)?,
                    Some(revision),
                )
            }
        };
        let tx = self.governed_transaction()?;
        let key = key.sql_key();
        let value_ref = value.as_ref().map(|value| value.expose().as_str());
        let changed = match prior {
            None => tx.gate().execute(
                &tx,
                GovernedTable::McpOAuthCredential,
                "INSERT INTO mcp_oauth_credential (key, revision, value) VALUES (?1, 1, ?2)
                 ON CONFLICT(key) DO NOTHING",
                params![key, value_ref],
            )?,
            Some(revision) => tx.gate().execute(
                &tx,
                GovernedTable::McpOAuthCredential,
                "UPDATE mcp_oauth_credential SET revision = ?2, value = ?3
                 WHERE key = ?1 AND revision = ?4",
                params![key, next, value_ref, revision],
            )?,
        };
        if changed != 1 {
            return Err(WriteError::McpCredentialConflict);
        }
        tx.commit()?.discard();
        Ok(McpCredentialSnapshot {
            revision: McpCredentialRevision::Stored(next as u64),
            value,
        })
    }
}

#[cfg(test)]
mod tests;
