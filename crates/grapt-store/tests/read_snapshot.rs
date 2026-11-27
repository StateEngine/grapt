use grapt_store::EventLog;
use rusqlite::Connection;
use std::path::PathBuf;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lua-read-{name}-{}.sqlite", std::process::id()))
}

#[test]
fn lua_read_snapshot_denies_writes_and_protection_changes() -> TestResult {
    let path = path("denial");
    if path.exists() {
        return Err("fixture already exists; preserve evidence".into());
    }
    {
        let writer = Connection::open(&path)?;
        writer
            .execute_batch("CREATE TABLE sample(value INTEGER); INSERT INTO sample VALUES(1);")?;
    }
    let before = std::fs::read(&path)?;
    let log = EventLog::open_read_only(&path)?;
    log.with_read_snapshot(|snapshot| {
        for sql in [
            "UPDATE sample SET value=2",
            "DELETE FROM sample",
            "CREATE TEMP TABLE escaped(value)",
            "PRAGMA query_only=OFF",
            "PRAGMA writable_schema=ON",
            "ATTACH ':memory:' AS escaped",
            "COMMIT",
        ] {
            assert!(
                snapshot.prepare(sql).is_err(),
                "read-only enforcement survived: {sql}"
            );
        }
        let value: i64 = snapshot
            .prepare("SELECT value FROM sample")
            .map_err(|e| e.to_string())?
            .query_row([], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        assert_eq!(value, 1);
        Ok(())
    })?;
    drop(log);
    assert_eq!(
        std::fs::read(&path)?,
        before,
        "read snapshot mutated database"
    );
    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn lua_read_snapshot_is_stable_and_failure_releases_it() -> TestResult {
    let path = path("stable");
    if path.exists() {
        return Err("fixture already exists; preserve evidence".into());
    }
    let writer = Connection::open(&path)?;
    writer.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE sample(value INTEGER); INSERT INTO sample VALUES(1);")?;
    let log = EventLog::open_read_only(&path)?;
    let failure: Result<(), String> = log.with_read_snapshot(|snapshot| {
        let count = || -> Result<i64, String> {
            snapshot
                .prepare("SELECT count(*) FROM sample")
                .map_err(|e| e.to_string())?
                .query_row([], |row| row.get(0))
                .map_err(|e| e.to_string())
        };
        assert_eq!(count()?, 1);
        writer
            .execute("INSERT INTO sample VALUES(2)", [])
            .map_err(|e| e.to_string())?;
        assert_eq!(count()?, 1, "concurrent commit crossed read snapshot");
        assert!(
            log.with_read_snapshot(|_| Ok(())).is_err(),
            "nested snapshot admitted"
        );
        Err("injected reader failure".into())
    });
    assert_eq!(failure, Err("injected reader failure".into()));
    log.with_read_snapshot(|snapshot| {
        let count: i64 = snapshot
            .prepare("SELECT count(*) FROM sample")
            .map_err(|e| e.to_string())?
            .query_row([], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        assert_eq!(count, 2, "failed snapshot retained stale transaction");
        Ok(())
    })?;
    drop(log);
    drop(writer);
    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn lua_read_snapshot_missing_corrupt_and_work_limits_fail_honestly() -> TestResult {
    let path = path("refusals");
    if path.exists() {
        return Err("fixture already exists; preserve evidence".into());
    }
    assert!(EventLog::open_read_only(&path).is_err());
    assert!(!path.exists(), "read owner created absent database");
    std::fs::write(&path, b"corrupt sqlite")?;
    let corrupt = EventLog::open_read_only(&path).and_then(|log| {
        log.with_read_snapshot(|snapshot| {
            snapshot
                .prepare("SELECT name FROM sqlite_master")
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .map_err(|_| grapt_store::log::LogError::Sqlite(rusqlite::Error::InvalidQuery))
    });
    assert!(corrupt.is_err());
    assert_eq!(std::fs::read(&path)?, b"corrupt sqlite");
    std::fs::remove_file(&path)?;
    {
        let writer = Connection::open(&path)?;
        writer.execute_batch("CREATE TABLE sample(value INTEGER);")?;
    }
    let log = EventLog::open_read_only(&path)?;
    log.with_read_snapshot(|snapshot| {
        let oversized = format!("SELECT 1{}", " ".repeat(4096));
        let refusal = snapshot
            .prepare(&oversized)
            .err()
            .ok_or("SQL byte budget survived")?;
        assert!(refusal.to_string().contains("SQL or statement budget"));
        for _ in 0..128 {
            snapshot.prepare("SELECT 1").map_err(|e| e.to_string())?;
        }
        assert!(
            snapshot.prepare("SELECT 1").is_err(),
            "statement budget survived"
        );
        Ok(())
    })?;
    let work = log.with_read_snapshot(|snapshot| {
        snapshot.prepare("WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<10000000) SELECT count(*) FROM n")
            .map_err(|e| e.to_string())?.query_row([], |row| row.get::<_, i64>(0)).map_err(|e| e.to_string())
    });
    let refusal = work.err().ok_or("VM work budget survived")?;
    assert!(
        refusal.contains("interrupted"),
        "wrong VM budget refusal: {refusal}"
    );
    log.with_read_snapshot(|snapshot| {
        snapshot
            .prepare("SELECT 1")
            .map_err(|e| e.to_string())?
            .query_row([], |_| Ok(()))
            .map_err(|e| e.to_string())
    })?;
    drop(log);
    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn lua_read_snapshot_unwind_releases_hooks_and_transaction() -> TestResult {
    let path = path("unwind");
    if path.exists() {
        return Err("fixture already exists; preserve evidence".into());
    }
    let writer = Connection::open(&path)?;
    writer.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE sample(value INTEGER); INSERT INTO sample VALUES(1);")?;
    let log = EventLog::open_read_only(&path)?;
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        log.with_read_snapshot::<()>(|snapshot| {
            let count: i64 = snapshot
                .prepare("SELECT count(*) FROM sample")
                .map_err(|e| e.to_string())?
                .query_row([], |row| row.get(0))
                .map_err(|e| e.to_string())?;
            assert_eq!(count, 1);
            writer
                .execute("INSERT INTO sample VALUES(2)", [])
                .map_err(|e| e.to_string())?;
            panic!("injected snapshot unwind");
        })
    }));
    match unwind {
        Err(payload) => assert_eq!(
            payload.downcast_ref::<&str>(),
            Some(&"injected snapshot unwind"),
            "an unrelated panic cannot prove snapshot cleanup"
        ),
        Ok(result) => return Err(format!("snapshot did not unwind: {result:?}").into()),
    }
    log.with_read_snapshot(|snapshot| {
        assert!(snapshot.prepare("UPDATE sample SET value=3").is_err());
        let count: i64 = snapshot
            .prepare("SELECT count(*) FROM sample")
            .map_err(|e| e.to_string())?
            .query_row([], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        assert_eq!(count, 2, "unwound snapshot retained stale transaction");
        Ok(())
    })?;
    drop(log);
    drop(writer);
    std::fs::remove_file(path)?;
    Ok(())
}
