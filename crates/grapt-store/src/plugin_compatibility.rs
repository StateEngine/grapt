//! Store-owned durable state used by the external-plugin compatibility layer.
//!
//! The legacy `credential` table remains the single durable credential truth.
//! A plugin-created credential also records its owning plugin in
//! `plugin_credential_owner`, written and removed in the same transaction, so a
//! plugin can read back only credentials it created; credentials with no plugin
//! owner are never plugin-readable. The web-search selection uses the frozen
//! oracle's legacy `kv` key `websearch:provider`. This module owns every
//! statement against these tables; callers receive typed records only.

use grapt_types::Secret;
use rusqlite::{params, OptionalExtension};

use crate::{write::WriteError, EventStore};

pub const MAX_PLUGIN_COMPATIBILITY_ID_BYTES: usize = 256;
pub const MAX_INTEGRATION_CREDENTIAL_BYTES: usize = 16 * 1024;
pub const WEBSEARCH_PROVIDER_KEY: &str = "websearch:provider";
const MAX_LABEL_BYTES: usize = 512;
const MAX_WEBSEARCH_SELECTION_BYTES: usize = 4096;

/// Missing selection uses the registry default; explicit disable never does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebsearchProviderSelection {
    Disabled,
    Provider(String),
}

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS credential (
    id TEXT PRIMARY KEY,
    integration_id TEXT,
    label TEXT NOT NULL,
    value TEXT NOT NULL,
    connector_id TEXT,
    method_id TEXT,
    active INTEGER,
    time_created INTEGER NOT NULL,
    time_updated INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS plugin_credential_owner (
    credential_id TEXT PRIMARY KEY,
    plugin_id TEXT NOT NULL,
    time_created INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS kv (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    time_created INTEGER NOT NULL,
    time_updated INTEGER NOT NULL
);";

#[derive(Debug)]
pub struct IntegrationCredentialWrite {
    pub id: String,
    pub integration_id: String,
    pub label: String,
    pub value: Secret<String>,
    pub method_id: Option<String>,
    /// The transport-authenticated plugin that owns this credential.
    pub owner_plugin: String,
    /// Wall-clock millis from the caller's injected clock authority.
    pub now_ms: i64,
}

#[derive(Debug)]
pub struct IntegrationCredentialRecord {
    pub id: String,
    pub integration_id: String,
    pub label: String,
    pub value: Secret<String>,
    pub method_id: Option<String>,
    pub owner_plugin: String,
    pub time_updated: i64,
}

pub(crate) fn ensure_schema(tx: &rusqlite::Transaction<'_>) -> Result<(), rusqlite::Error> {
    tx.execute_batch(SCHEMA)
}

fn bounded(name: &'static str, value: &str, bound: usize) -> Result<(), WriteError> {
    if value.is_empty() || value.len() > bound {
        return Err(WriteError::PluginCompatibilityInput {
            detail: format!("{name} must contain 1..={bound} UTF-8 bytes"),
        });
    }
    Ok(())
}

fn record(row: &rusqlite::Row<'_>) -> rusqlite::Result<IntegrationCredentialRecord> {
    Ok(IntegrationCredentialRecord {
        id: row.get(0)?,
        integration_id: row.get(1)?,
        label: row.get(2)?,
        value: Secret::new(row.get(3)?),
        method_id: row.get(4)?,
        owner_plugin: row.get(5)?,
        time_updated: row.get(6)?,
    })
}

const OWNED_CREDENTIAL: &str = "SELECT c.id, c.integration_id, c.label, c.value, c.method_id,
        o.plugin_id, c.time_updated
   FROM credential c
   JOIN plugin_credential_owner o ON o.credential_id = c.id";

impl EventStore {
    /// Persist one already-authorized plugin credential with its owner.
    /// Exact retry is idempotent by content; reuse of an id with different
    /// content or owner refuses and preserves the stored truth.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn put_integration_credential(
        &mut self,
        input: IntegrationCredentialWrite,
    ) -> Result<IntegrationCredentialRecord, WriteError> {
        bounded(
            "credential id",
            &input.id,
            MAX_PLUGIN_COMPATIBILITY_ID_BYTES,
        )?;
        bounded(
            "integration id",
            &input.integration_id,
            MAX_PLUGIN_COMPATIBILITY_ID_BYTES,
        )?;
        bounded("credential label", &input.label, MAX_LABEL_BYTES)?;
        bounded(
            "credential value",
            input.value.expose(),
            MAX_INTEGRATION_CREDENTIAL_BYTES,
        )?;
        bounded(
            "credential owner plugin",
            &input.owner_plugin,
            MAX_PLUGIN_COMPATIBILITY_ID_BYTES,
        )?;
        if let Some(method) = &input.method_id {
            bounded(
                "integration method id",
                method,
                MAX_PLUGIN_COMPATIBILITY_ID_BYTES,
            )?;
        }
        if input.now_ms < 0 {
            return Err(WriteError::PluginCompatibilityInput {
                detail: "credential timestamp must not precede the epoch".to_owned(),
            });
        }
        let tx = self.governed_transaction()?;
        let existing = tx
            .query_row(
                "SELECT c.integration_id, c.label, c.value, c.method_id, o.plugin_id, c.time_updated
                   FROM credential c
                   LEFT JOIN plugin_credential_owner o ON o.credential_id = c.id
                  WHERE c.id = ?1",
                [&input.id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .optional()?;
        if let Some((integration, label, value, method, owner, time_updated)) = existing {
            if integration.as_deref() != Some(input.integration_id.as_str())
                || label != input.label
                || value != *input.value.expose()
                || method != input.method_id
                || owner.as_deref() != Some(input.owner_plugin.as_str())
            {
                return Err(WriteError::PluginCompatibilityConflict { identity: input.id });
            }
            tx.commit()?.discard();
            return Ok(IntegrationCredentialRecord {
                id: input.id,
                integration_id: input.integration_id,
                label,
                value: Secret::new(value),
                method_id: method,
                owner_plugin: input.owner_plugin,
                time_updated,
            });
        }
        // Only this plugin's own active credential for the integration yields;
        // user and other-owner credentials are never changed by a plugin write.
        tx.execute(
            "UPDATE credential SET active = 0, time_updated = ?3
              WHERE integration_id = ?1 AND active = 1
                AND id IN (SELECT credential_id FROM plugin_credential_owner WHERE plugin_id = ?2)",
            params![&input.integration_id, &input.owner_plugin, input.now_ms],
        )?;
        tx.execute(
            "INSERT INTO credential
               (id, integration_id, label, value, connector_id, method_id,
                active, time_created, time_updated)
             VALUES (?1, ?2, ?3, ?4, NULL, ?5, 1, ?6, ?6)",
            params![
                &input.id,
                &input.integration_id,
                &input.label,
                input.value.expose(),
                &input.method_id,
                input.now_ms,
            ],
        )?;
        tx.execute(
            "INSERT INTO plugin_credential_owner (credential_id, plugin_id, time_created)
             VALUES (?1, ?2, ?3)",
            params![&input.id, &input.owner_plugin, input.now_ms],
        )?;
        tx.commit()?.discard();
        Ok(IntegrationCredentialRecord {
            id: input.id,
            integration_id: input.integration_id,
            label: input.label,
            value: input.value,
            method_id: input.method_id,
            owner_plugin: input.owner_plugin,
            time_updated: input.now_ms,
        })
    }

    pub fn active_integration_credential_for_plugin(
        &self,
        integration_id: &str,
        plugin_id: &str,
    ) -> Result<Option<IntegrationCredentialRecord>, WriteError> {
        bounded(
            "integration id",
            integration_id,
            MAX_PLUGIN_COMPATIBILITY_ID_BYTES,
        )?;
        bounded("plugin id", plugin_id, MAX_PLUGIN_COMPATIBILITY_ID_BYTES)?;
        Ok(self
            .connection()
            .query_row(
                &format!(
                    "{OWNED_CREDENTIAL}
                      WHERE c.integration_id = ?1 AND c.active = 1 AND o.plugin_id = ?2
                      ORDER BY c.time_updated DESC, c.id DESC LIMIT 1"
                ),
                params![integration_id, plugin_id],
                record,
            )
            .optional()?)
    }

    pub fn integration_credential_for_plugin(
        &self,
        id: &str,
        plugin_id: &str,
    ) -> Result<Option<IntegrationCredentialRecord>, WriteError> {
        bounded("credential id", id, MAX_PLUGIN_COMPATIBILITY_ID_BYTES)?;
        bounded("plugin id", plugin_id, MAX_PLUGIN_COMPATIBILITY_ID_BYTES)?;
        Ok(self
            .connection()
            .query_row(
                &format!(
                    "{OWNED_CREDENTIAL} WHERE c.id = ?1 AND c.active = 1 AND o.plugin_id = ?2"
                ),
                params![id, plugin_id],
                record,
            )
            .optional()?)
    }

    /// Delete one plugin-owned credential and its owner row atomically.
    pub fn delete_integration_credential_for_plugin(
        &mut self,
        id: &str,
        plugin_id: &str,
    ) -> Result<bool, WriteError> {
        bounded("credential id", id, MAX_PLUGIN_COMPATIBILITY_ID_BYTES)?;
        bounded("plugin id", plugin_id, MAX_PLUGIN_COMPATIBILITY_ID_BYTES)?;
        let tx = self.governed_transaction()?;
        let owned = tx
            .query_row(
                "SELECT 1 FROM plugin_credential_owner WHERE credential_id = ?1 AND plugin_id = ?2",
                params![id, plugin_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !owned {
            tx.commit()?.discard();
            return Ok(false);
        }
        tx.execute("DELETE FROM credential WHERE id = ?1", [id])?;
        tx.execute(
            "DELETE FROM plugin_credential_owner WHERE credential_id = ?1",
            [id],
        )?;
        tx.commit()?.discard();
        Ok(true)
    }

    /// Legacy JSON strings select a provider; JSON null persists explicit disable.
    pub fn websearch_provider_selection(
        &self,
    ) -> Result<Option<WebsearchProviderSelection>, WriteError> {
        let read_limit = i64::try_from(MAX_WEBSEARCH_SELECTION_BYTES + 1)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let held = self
            .connection()
            .query_row(
                "SELECT substr(CAST(value AS BLOB), 1, ?2) FROM kv WHERE key = ?1",
                params![WEBSEARCH_PROVIDER_KEY, read_limit],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?;
        held.map(|raw| {
            let corrupt = |detail: &str| WriteError::PluginCompatibilityCorrupt {
                owner: "websearch selection",
                detail: detail.to_owned(),
            };
            if raw.len() > MAX_WEBSEARCH_SELECTION_BYTES {
                return Err(corrupt("stored selection exceeds its byte limit"));
            }
            match serde_json::from_slice::<Option<String>>(&raw)
                .map_err(|_| corrupt("kv value is not a JSON string or null"))?
            {
                None => Ok(WebsearchProviderSelection::Disabled),
                Some(provider) => {
                    bounded(
                        "websearch provider id",
                        &provider,
                        MAX_PLUGIN_COMPATIBILITY_ID_BYTES,
                    )
                    .map_err(|_| corrupt("stored provider id is out of bounds"))?;
                    Ok(WebsearchProviderSelection::Provider(provider))
                }
            }
        })
        .transpose()
    }

    /// Persist a disable separately from resetting to the registry default.
    pub fn disable_websearch_provider_selection(&mut self, now_ms: i64) -> Result<(), WriteError> {
        self.write_websearch_provider_selection("null", now_ms)
    }

    pub fn put_websearch_provider_selection(
        &mut self,
        provider_id: &str,
        now_ms: i64,
    ) -> Result<(), WriteError> {
        bounded(
            "websearch provider id",
            provider_id,
            MAX_PLUGIN_COMPATIBILITY_ID_BYTES,
        )?;
        let value = serde_json::to_string(provider_id).map_err(|_| {
            WriteError::PluginCompatibilityInput {
                detail: "websearch provider id could not be encoded".to_owned(),
            }
        })?;
        self.write_websearch_provider_selection(&value, now_ms)
    }

    fn write_websearch_provider_selection(
        &mut self,
        value: &str,
        now_ms: i64,
    ) -> Result<(), WriteError> {
        if now_ms < 0 {
            return Err(WriteError::PluginCompatibilityInput {
                detail: "websearch selection timestamp must not precede the epoch".to_owned(),
            });
        }
        let tx = self.governed_transaction()?;
        let timestamped = kv_has_timestamps(&tx)?;
        if timestamped {
            tx.execute(
                "INSERT INTO kv (key, value, time_created, time_updated) VALUES (?1, ?2, ?3, ?3)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, time_updated = excluded.time_updated",
                params![WEBSEARCH_PROVIDER_KEY, value, now_ms],
            )?;
        } else {
            tx.execute(
                "INSERT INTO kv (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![WEBSEARCH_PROVIDER_KEY, value],
            )?;
        }
        tx.commit()?.discard();
        Ok(())
    }

    /// Reset the selection so the registry default applies again.
    pub fn clear_websearch_provider_selection(&mut self) -> Result<(), WriteError> {
        let tx = self.governed_transaction()?;
        tx.execute("DELETE FROM kv WHERE key = ?1", [WEBSEARCH_PROVIDER_KEY])?;
        tx.commit()?.discard();
        Ok(())
    }
}

fn kv_has_timestamps(tx: &rusqlite::Transaction<'_>) -> Result<bool, WriteError> {
    let mut statement = tx.prepare("SELECT name FROM pragma_table_info('kv')")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(columns.iter().any(|column| column == "time_created")
        && columns.iter().any(|column| column == "time_updated"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Result<EventStore, Box<dyn std::error::Error>> {
        Ok(EventStore::open_in_memory()?)
    }

    fn write(id: &str, owner: &str, value: &str, now_ms: i64) -> IntegrationCredentialWrite {
        IntegrationCredentialWrite {
            id: id.to_owned(),
            integration_id: "fixture".to_owned(),
            label: "Fixture".to_owned(),
            value: Secret::new(value.to_owned()),
            method_id: Some("key".to_owned()),
            owner_plugin: owner.to_owned(),
            now_ms,
        }
    }

    #[test]
    fn websearch_selection_uses_the_oracle_kv_key_and_reset_deletes_it(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut store = store()?;
        assert_eq!(store.websearch_provider_selection()?, None);
        store.put_websearch_provider_selection("fixture", 1_000)?;
        let raw: (String, i64, i64) = store.connection().query_row(
            "SELECT value, time_created, time_updated FROM kv WHERE key = 'websearch:provider'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(raw, ("\"fixture\"".to_owned(), 1_000, 1_000));
        store.put_websearch_provider_selection("second", 2_000)?;
        assert_eq!(
            store.websearch_provider_selection()?,
            Some(WebsearchProviderSelection::Provider("second".to_owned()))
        );
        store.disable_websearch_provider_selection(3_000)?;
        assert_eq!(
            store.websearch_provider_selection()?,
            Some(WebsearchProviderSelection::Disabled)
        );
        let raw: (String, i64, i64) = store.connection().query_row(
            "SELECT value, time_created, time_updated FROM kv WHERE key = 'websearch:provider'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(raw, ("null".to_owned(), 1_000, 3_000));
        store.put_websearch_provider_selection("third", 4_000)?;
        assert_eq!(
            store.websearch_provider_selection()?,
            Some(WebsearchProviderSelection::Provider("third".to_owned()))
        );
        store.clear_websearch_provider_selection()?;
        assert_eq!(store.websearch_provider_selection()?, None);
        store
            .connection()
            .execute("INSERT INTO kv (key, value, time_created, time_updated) VALUES ('websearch:provider', '7', 1, 1)", [])?;
        let error = store
            .websearch_provider_selection()
            .err()
            .ok_or("a corrupt selection was accepted")?;
        assert!(error.to_string().contains("not a JSON string"));
        Ok(())
    }

    #[test]
    fn websearch_selection_rejects_corrupt_or_oversized_values(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut store = store()?;
        store.disable_websearch_provider_selection(1)?;
        for raw in [
            "7".to_owned(),
            "false".to_owned(),
            "{}".to_owned(),
            "\"\"".to_owned(),
            format!("\"{}\"", "x".repeat(257)),
            " ".repeat(MAX_WEBSEARCH_SELECTION_BYTES + 1),
        ] {
            store.connection().execute(
                "UPDATE kv SET value = ?1 WHERE key = ?2",
                params![raw, WEBSEARCH_PROVIDER_KEY],
            )?;
            assert!(store.websearch_provider_selection().is_err());
        }
        store.connection().execute(
            "UPDATE kv SET value = CAST(X'FF' AS TEXT) WHERE key = ?1",
            [WEBSEARCH_PROVIDER_KEY],
        )?;
        assert!(store.websearch_provider_selection().is_err());
        Ok(())
    }

    #[test]
    fn committed_websearch_disable_survives_abrupt_process_exit(
    ) -> Result<(), Box<dyn std::error::Error>> {
        struct Profile(std::path::PathBuf);
        impl Drop for Profile {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let path = std::env::temp_dir().join(format!(
            "grapt-websearch-disable-crash-{}",
            crate::OsRandomIds::seed()?.instance()
        ));
        std::fs::create_dir(&path)?;
        let profile = Profile(path);
        let database = profile.0.join("store.db");
        {
            let mut store = EventStore::open(&database, "websearch-disable-crash")?;
            store.put_websearch_provider_selection("before-crash", 1)?;
        }
        let mut child = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--ignored",
                "--exact",
                "plugin_compatibility::tests::websearch_disable_crash_child",
            ])
            .env("GRAPT_WEBSEARCH_DISABLE_CRASH_DB", &database)
            .spawn()?;
        let mut status = None;
        for _ in 0..500 {
            if let Some(done) = child.try_wait()? {
                status = Some(done);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let Some(status) = status else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("websearch disable crash child exceeded five seconds".into());
        };
        assert_eq!(status.code(), Some(87));
        let store = EventStore::open(&database, "websearch-disable-crash")?;
        assert_eq!(
            store.websearch_provider_selection()?,
            Some(WebsearchProviderSelection::Disabled)
        );
        drop(store);
        Ok(())
    }

    #[test]
    #[ignore = "spawned only by the bounded abrupt-exit witness"]
    fn websearch_disable_crash_child() -> Result<(), Box<dyn std::error::Error>> {
        let database = std::env::var_os("GRAPT_WEBSEARCH_DISABLE_CRASH_DB")
            .ok_or("missing private crash fixture path")?;
        let mut store =
            EventStore::open(std::path::Path::new(&database), "websearch-disable-crash")?;
        store.disable_websearch_provider_selection(2)?;
        std::process::exit(87);
    }

    #[test]
    fn credential_owner_scopes_reads_and_conflicts_are_secret_safe(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut store = store()?;
        store.put_integration_credential(write("crd_a", "plugin.a", "sentinel-a", 5))?;
        store.connection().execute(
            "INSERT INTO credential (id, integration_id, label, value, active, time_created, time_updated)
             VALUES ('crd_user', 'fixture', 'User', 'sentinel-user', 1, 1, 1)",
            [],
        )?;
        assert!(
            store
                .integration_credential_for_plugin("crd_a", "plugin.b")?
                .is_none(),
            "another plugin resolved a foreign credential"
        );
        assert!(
            store
                .integration_credential_for_plugin("crd_user", "plugin.a")?
                .is_none(),
            "a plugin resolved a user credential"
        );
        let held = store
            .active_integration_credential_for_plugin("fixture", "plugin.a")?
            .ok_or("owned credential")?;
        assert_eq!(held.value.expose(), "sentinel-a");
        assert_eq!(
            held.time_updated, 5,
            "credential timestamps do not come from the injected clock"
        );
        let user_active: i64 = store.connection().query_row(
            "SELECT active FROM credential WHERE id = 'crd_user'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            user_active, 1,
            "a plugin write deactivated a user credential"
        );
        let retry =
            store.put_integration_credential(write("crd_a", "plugin.a", "sentinel-a", 99))?;
        assert_eq!(
            retry.time_updated, 5,
            "an exact retry advanced the timestamp"
        );
        let error = store
            .put_integration_credential(write("crd_a", "plugin.a", "sentinel-other", 6))
            .err()
            .ok_or("credential conflict was accepted")?;
        assert!(!error.to_string().contains("sentinel"));
        let stolen = store
            .put_integration_credential(write("crd_a", "plugin.b", "sentinel-a", 6))
            .err()
            .ok_or("an id was re-owned by another plugin")?;
        assert!(matches!(
            stolen,
            WriteError::PluginCompatibilityConflict { .. }
        ));
        Ok(())
    }

    #[test]
    fn credential_deletion_is_owner_scoped_and_removes_owner_rows_atomically(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut store = store()?;
        store.put_integration_credential(write("crd_a", "plugin.a", "secret", 1))?;
        assert!(!store.delete_integration_credential_for_plugin("crd_a", "plugin.b")?);
        assert!(store.delete_integration_credential_for_plugin("crd_a", "plugin.a")?);
        let remaining: i64 = store.connection().query_row(
            "SELECT (SELECT COUNT(*) FROM credential) + (SELECT COUNT(*) FROM plugin_credential_owner)",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(remaining, 0);
        store.connection().execute(
            "INSERT INTO plugin_credential_owner (credential_id, plugin_id, time_created) VALUES ('crd_gone', 'plugin.a', 1)",
            [],
        )?;
        assert!(
            store
                .integration_credential_for_plugin("crd_gone", "plugin.a")?
                .is_none(),
            "a dangling owner row produced a credential"
        );
        Ok(())
    }
}
