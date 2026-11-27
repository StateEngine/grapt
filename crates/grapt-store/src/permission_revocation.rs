//! Replayable permission withdrawals, keyed by the approvals they retire.
use crate::migration::Migration;
use grapt_types::MigrationId;
use rusqlite::{Connection, Transaction};

#[derive(Debug)]
pub(crate) struct CreatePermissionRuleRevocations {
    pub id: MigrationId,
}

impl Migration for CreatePermissionRuleRevocations {
    fn id(&self) -> MigrationId {
        self.id.clone()
    }

    fn validate(&self, connection: &Connection) -> Result<(), String> {
        for table in [
            "permission_http_rule_revocation",
            "permission_http_rule_origin",
        ] {
            let present: bool = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1)",
                    [table],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            if present {
                validate(connection, table).map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }

    fn up(&self, transaction: &Transaction<'_>) -> Result<(), String> {
        ensure_schema(transaction).map_err(|error| error.to_string())?;
        // Existing rules predate withdrawals. Reconstruct their origins from
        // the already durable HTTP asks and Always replies, once at migration.
        transaction
            .execute(
                "INSERT INTO permission_http_rule_origin(project_id,action,resource,permission_id)
             SELECT DISTINCT h.project_id,p.action,saved.value,p.id
             FROM permission_request p JOIN permission_http_request h ON h.id=p.id
             JOIN json_each(h.save_json) saved JOIN permission_http_rule rule
             ON rule.project_id=h.project_id AND rule.action=p.action AND rule.resource=saved.value
             WHERE p.reply='always' AND saved.type='text'
             AND NOT EXISTS(SELECT 1 FROM permission_http_rule_revocation revoked
                 WHERE revoked.permission_id=p.id AND revoked.project_id=h.project_id
                     AND revoked.action=p.action AND revoked.resource=saved.value)
             ON CONFLICT DO NOTHING",
                [],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

pub(crate) fn ensure_schema(transaction: &Transaction<'_>) -> rusqlite::Result<()> {
    // There is deliberately no request FK: a withdrawal may replay before
    // the independently ordered aggregate that contains its original ask.
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS permission_http_rule_revocation (
            permission_id TEXT NOT NULL,
            project_id TEXT NOT NULL,
            action TEXT NOT NULL,
            resource TEXT NOT NULL,
            PRIMARY KEY(permission_id, project_id, action, resource)
        );
        CREATE TABLE IF NOT EXISTS permission_http_rule_origin (
            project_id TEXT NOT NULL,
            action TEXT NOT NULL,
            resource TEXT NOT NULL,
            permission_id TEXT NOT NULL,
            PRIMARY KEY(project_id, action, resource, permission_id)
        );",
    )?;
    for table in [
        "permission_http_rule_revocation",
        "permission_http_rule_origin",
    ] {
        validate(transaction, table)?;
    }
    Ok(())
}

fn validate(connection: &Connection, table: &str) -> rusqlite::Result<()> {
    let mut query = connection.prepare(
        "SELECT name, type, \"notnull\", pk FROM pragma_table_info(?1)
         ORDER BY cid LIMIT 5",
    )?;
    let observed = query
        .query_map([table], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let expected = if table == "permission_http_rule_origin" {
        ["project_id", "action", "resource", "permission_id"]
    } else {
        ["permission_id", "project_id", "action", "resource"]
    };
    let correct = observed.len() == expected.len()
        && observed.iter().zip(expected).enumerate().all(
            |(index, ((name, kind, required, key), expected))| {
                name == expected
                    && kind.eq_ignore_ascii_case("TEXT")
                    && *required == 1
                    && *key == index as i64 + 1
            },
        );
    let foreign_keys: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_list(?1))",
        [table],
        |row| row.get(0),
    )?;
    // These tables use SQLite's default byte comparison. table_info does not
    // expose column collations; an altered declaration must not merge distinct
    // permission IDs, projects or exact resources during replay/withdrawal.
    let collation_override: bool = connection.query_row(
        "SELECT instr(lower(sql), 'collate') > 0 FROM sqlite_master WHERE name=?1",
        [table],
        |row| row.get(0),
    )?;
    if !correct || foreign_keys || collation_override {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_SCHEMA),
            Some(format!("{table} has an unsupported shape")),
        ));
    }
    Ok(())
}
