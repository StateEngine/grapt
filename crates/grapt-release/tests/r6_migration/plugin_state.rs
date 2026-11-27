//! Release rehearsal admits only new, empty plugin-state tables: populated
//! oracle tables survive unmasked and a failed creation rolls back.

use super::*;

fn table_names(path: &Path) -> TestResult<Vec<String>> {
    let connection = Connection::open(path)?;
    let mut statement =
        connection.prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(names)
}

#[test]
fn populated_ts_credentials_and_kv_survive_plugin_state_creation_unmasked() -> TestResult {
    let root = scratch("plugin-state-unmasked")?;
    let source = root.join("source.sqlite3");
    legacy_database(&source)?;
    Connection::open(&source)?.execute_batch(
        r#"CREATE TABLE credential (
            id TEXT PRIMARY KEY, integration_id TEXT, label TEXT NOT NULL, value TEXT NOT NULL,
            connector_id TEXT, method_id TEXT, active INTEGER,
            time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL
        );
        INSERT INTO credential VALUES
            ('crd_ts_1', 'openai', 'One', '{"type":"key","key":"a"}', NULL, NULL, 1, 1, 1),
            ('crd_ts_2', 'github', 'Two', '{"type":"key","key":"b"}', NULL, NULL, 1, 2, 2),
            ('crd_ts_3', 'openai', 'Three', '{"type":"key","key":"c"}', NULL, NULL, 0, 3, 3),
            ('crd_ts_4', NULL, 'Four', '{"type":"key","key":"d"}', 'connector', NULL, 1, 4, 4);
        INSERT INTO kv VALUES ('websearch:provider', '"ts-provider"');"#,
    )?;
    let receipt = rehearse(
        &source,
        &root.join("run"),
        migration_id("plugin-state-unmasked")?,
        "plugin-state-build",
        &FixedClock,
    )?;
    let rerun = rehearse(
        &source,
        &root.join("run"),
        migration_id("plugin-state-unmasked")?,
        "plugin-state-build",
        &FixedClock,
    )?;
    assert_eq!(
        rerun.candidate_file_fingerprint(),
        receipt.candidate_file_fingerprint(),
        "rerunning plugin-state creation over populated TS state was not idempotent"
    );
    let candidate = Connection::open(receipt.candidate())?;
    let credentials: Vec<(String, i64)> = candidate
        .prepare("SELECT id, time_updated FROM credential ORDER BY id")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    assert_eq!(
        credentials,
        [
            ("crd_ts_1".to_owned(), 1),
            ("crd_ts_2".to_owned(), 2),
            ("crd_ts_3".to_owned(), 3),
            ("crd_ts_4".to_owned(), 4)
        ]
    );
    let owners: i64 =
        candidate.query_row("SELECT COUNT(*) FROM plugin_credential_owner", [], |row| {
            row.get(0)
        })?;
    assert_eq!(owners, 0, "TS credentials acquired a plugin owner");
    let kv: Vec<(String, String)> = candidate
        .prepare("SELECT key, value FROM kv ORDER BY key")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    assert_eq!(
        kv,
        [
            ("theme".to_owned(), "dark".to_owned()),
            (
                "websearch:provider".to_owned(),
                "\"ts-provider\"".to_owned()
            )
        ]
    );
    drop(candidate);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn a_failed_plugin_state_creation_rolls_back_every_created_table() -> TestResult {
    let root = scratch("plugin-state-rollback")?;
    let source = root.join("source.sqlite3");
    legacy_database(&source)?;
    // An index occupies the `kv` name after the credential tables would be
    // created, so the migration fails part-way through its own DDL.
    Connection::open(&source)?.execute_batch(
        "DROP TABLE kv;
         CREATE INDEX kv ON project (name);",
    )?;
    let source_before = fingerprint_file(&source)?;
    let run_root = root.join("run");
    let outcome = rehearse(
        &source,
        &run_root,
        migration_id("plugin-state-rollback")?,
        "plugin-state-build",
        &FixedClock,
    );
    let error = match outcome {
        Ok(_) => return Err("an index collision migrated successfully".into()),
        Err(error) => format!("{error:?}"),
    };
    assert!(
        error.contains("index named kv"),
        "the rehearsal failed before the plugin-state DDL collision: {error}"
    );
    assert_eq!(fingerprint_file(&source)?, source_before);
    for entry in std::fs::read_dir(&run_root)?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("sqlite3") {
            continue;
        }
        let names = table_names(&path)?;
        assert!(
            !names
                .iter()
                .any(|name| name == "credential" || name == "plugin_credential_owner"),
            "a failed migration left partial plugin state in {}: {names:?}",
            path.display()
        );
    }
    std::fs::remove_dir_all(root)?;
    Ok(())
}
