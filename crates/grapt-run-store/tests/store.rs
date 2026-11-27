use grapt_run_store::{
    Command, Durability, MigrationSpec, OpenError, ReadError, RequestKey, RunStore, Scope,
    Statement, Value, WriteError,
};
use std::error::Error;
use std::sync::mpsc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::Instant;

type ResultTest = Result<(), Box<dyn Error + Send + Sync>>;

const SCHEMA: &str = "
CREATE TABLE event (session TEXT, seq INTEGER, data BLOB, PRIMARY KEY (session, seq));
CREATE TABLE session_list (id TEXT PRIMARY KEY, updated INTEGER);
";
const LEDGER: &[MigrationSpec] = &[MigrationSpec::new(1, "initial", SCHEMA)];

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

fn put(session: &str, seq: i64, bytes: usize) -> Statement {
    Statement::new(
        "INSERT INTO event VALUES (?1, ?2, ?3)",
        vec![
            Value::Text(session.into()),
            Value::Integer(seq),
            Value::Blob(vec![42; bytes]),
        ],
    )
}

fn command(
    key: &str,
    durability: Durability,
    statements: Vec<Statement>,
) -> Result<Command, WriteError> {
    Command::new(RequestKey::new(key)?, durability, Scope::Global, statements)
}

#[tokio::test]
async fn savepoint_refusal_drops_the_effect_and_commit_notice() -> ResultTest {
    let dir = TempDir::new()?;
    let store = RunStore::open(dir.path().join("run.db"), LEDGER).await?;
    let writer = store.writer();
    let mut live = writer.subscribe();
    let poison = command(
        "poison",
        Durability::Full,
        vec![put("child", 1, 0), put("child", 1, 0)],
    )?;
    let clean = command("clean", Durability::Full, vec![put("parent", 1, 0)])?;
    let (poisoned, committed) = tokio::join!(
        writer.write(poison, deadline()),
        writer.write(clean, deadline())
    );
    assert!(matches!(poisoned, Err(WriteError::RolledBack(_))));
    assert_eq!(committed?.affected_rows, vec![1]);
    let notice = live.recv().await?;
    assert_eq!(notice.key.as_str(), "clean");
    assert!(live.try_recv().is_err());
    let read = store.readers();
    assert!(read
        .resolve(RequestKey::new("poison")?, deadline())
        .await?
        .is_none());
    let count = read
        .snapshot(deadline(), move |read| {
            Ok(read
                .prepare("SELECT count(*) FROM event WHERE session='child'")?
                .query_row([], |row| row.get::<_, i64>(0))?)
        })
        .await?;
    assert_eq!(count, 0);
    store.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn restart_recovers_receipt_and_rejects_key_collision() -> ResultTest {
    let dir = TempDir::new()?;
    let path = dir.path().join("run.db");
    let store = RunStore::open(&path, LEDGER).await?;
    assert!(matches!(
        RunStore::open(&path, LEDGER).await,
        Err(OpenError::WriterExists)
    ));
    let writer = store.writer();
    let original = || {
        command(
            "delivery",
            Durability::Full,
            vec![put("child", 1, 0), put("parent", 1, 0)],
        )
    };
    let receipt = writer.write(original()?, deadline()).await?;
    store.shutdown().await?;
    assert!(matches!(
        writer.write(original()?, deadline()).await,
        Err(WriteError::Closed)
    ));
    let reopened = RunStore::open(&path, LEDGER).await?;
    let writer = reopened.writer();
    let mut live = writer.subscribe();
    let replay = writer.write(original()?, deadline()).await?;
    assert!(!receipt.replayed && replay.replayed);
    assert_eq!(replay.affected_rows, receipt.affected_rows);
    let conflict = command("delivery", Durability::Full, vec![put("child", 2, 0)])?;
    assert!(matches!(
        writer.write(conflict, deadline()).await,
        Err(WriteError::KeyConflict)
    ));
    assert!(live.try_recv().is_err());
    assert_eq!(
        reopened
            .readers()
            .resolve(RequestKey::new("delivery")?, deadline())
            .await?,
        Some(replay)
    );
    reopened.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn snapshot_holds_a_stable_keyset_while_new_commits_appear() -> ResultTest {
    let dir = TempDir::new()?;
    let store = RunStore::open(dir.path().join("run.db"), LEDGER).await?;
    let writer = store.writer();
    let step = |key: &str, id: &str, updated| {
        command(
            key,
            Durability::Full,
            vec![Statement::new(
                "INSERT INTO session_list VALUES (?1, ?2)",
                vec![Value::Text(id.into()), Value::Integer(updated)],
            )],
        )
    };
    writer.write(step("one", "a", 1)?, deadline()).await?;
    writer.write(step("two", "b", 1)?, deadline()).await?;
    let read = store.readers();
    let (pinned, reached) = tokio::sync::oneshot::channel();
    let (release, proceed) = mpsc::sync_channel(1);
    let pages = tokio::spawn(async move {
        read.snapshot(deadline(), move |read| {
            let first: String = read.prepare("SELECT id FROM session_list ORDER BY updated DESC, id DESC LIMIT 1")?
                .query_row([], |row| row.get(0))?;
            pinned.send(()).map_err(|_| ReadError::Worker)?;
            proceed.recv_timeout(Duration::from_secs(3)).map_err(|_| ReadError::Worker)?;
            let rest: String = read.prepare("SELECT id FROM session_list WHERE (updated,id) < (1,?1) ORDER BY updated DESC,id DESC LIMIT 1")?
                .query_row([&first], |row| row.get(0))?;
            assert!(read.prepare("COMMIT").is_err());
            Ok((first, rest))
        }).await
    });
    reached.await?;
    writer.write(step("three", "c", 0)?, deadline()).await?;
    release.send(())?;
    assert_eq!(pages.await??, ("b".into(), "a".into()));
    let count = store
        .readers()
        .snapshot(deadline(), move |read| {
            Ok(read
                .prepare("SELECT count(*) FROM session_list")?
                .query_row([], |row| row.get::<_, i64>(0))?)
        })
        .await?;
    assert_eq!(count, 3);
    store.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn pinned_snapshot_refuses_growth_then_writer_resets_wal() -> ResultTest {
    let dir = TempDir::new()?;
    let store = RunStore::open(dir.path().join("run.db"), LEDGER).await?;
    let writer = store.writer();
    let read = store.readers();
    let (pinned, reached) = tokio::sync::oneshot::channel();
    let (release, proceed) = mpsc::sync_channel(1);
    let pin = tokio::spawn(async move {
        read.snapshot(deadline(), move |_| {
            pinned.send(()).map_err(|_| ReadError::Worker)?;
            proceed
                .recv_timeout(Duration::from_secs(4))
                .map_err(|_| ReadError::Worker)
        })
        .await
    });
    reached.await?;
    let mut pressure = None;
    for seq in 0..170 {
        let key = format!("wal-{seq}");
        let write = command(
            &key,
            Durability::Normal,
            vec![put("session", seq, 512 * 1024)],
        )?;
        match writer.write(write, deadline()).await {
            Ok(_) => {}
            Err(WriteError::WalPressure) => {
                pressure = Some((key, seq));
                break;
            }
            Err(error) => return Err(error.into()),
        }
    }
    let (key, seq) = pressure.ok_or("capacity refusal was not reached")?;
    let stats = writer.stats();
    assert!(stats.wal_pressure);
    println!("pinned_wal_peak_bytes={}", stats.wal_peak_bytes);
    assert!(stats.wal_peak_bytes <= 64 * 1024 * 1024, "{stats:?}");
    release.send(())?;
    pin.await??;
    tokio::time::timeout(Duration::from_secs(2), async {
        while writer.stats().wal_pressure {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    assert_eq!(writer.stats().wal_bytes, 0);
    writer
        .write(
            command(
                &key,
                Durability::Normal,
                vec![put("session", seq, 512 * 1024)],
            )?,
            deadline(),
        )
        .await?;
    store.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn write_sql_cannot_take_transaction_control_or_mint_receipts() -> ResultTest {
    let dir = TempDir::new()?;
    let store = RunStore::open(dir.path().join("run.db"), LEDGER).await?;
    let writer = store.writer();
    for (key, sql) in [
        ("control", "COMMIT"),
        ("receipts", "DELETE FROM run_store_receipt"),
        ("temp", "CREATE TEMP TABLE event(data TEXT)"),
        (
            "multi",
            "INSERT INTO event VALUES ('invalid', 1, X''); COMMIT",
        ),
    ] {
        let result = writer
            .write(
                command(key, Durability::Full, vec![Statement::new(sql, vec![])])?,
                deadline(),
            )
            .await;
        assert!(
            matches!(result, Err(WriteError::RolledBack(_))),
            "{key}: {result:?}"
        );
    }
    let count = store
        .readers()
        .snapshot(deadline(), move |read| {
            Ok(read
                .prepare("SELECT count(*) FROM event")?
                .query_row([], |row| row.get::<_, i64>(0))?)
        })
        .await?;
    assert_eq!(count, 0);
    store.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn short_sql_cannot_commit_a_wal_larger_than_the_cap() -> ResultTest {
    let dir = TempDir::new()?;
    let path = dir.path().join("run.db");
    const SEEDED: &[MigrationSpec] = &[MigrationSpec::new(
        1,
        "seeded",
        "CREATE TABLE event(seq INTEGER PRIMARY KEY, data BLOB NOT NULL);
        WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<80)
            INSERT INTO event SELECT i, X'' FROM n;",
    )];
    let store = RunStore::open(&path, SEEDED).await?;
    let writer = store.writer();
    let mut notices = writer.subscribe();
    let read = store.readers();
    let (pinned, reached) = tokio::sync::oneshot::channel();
    let (release, proceed) = mpsc::sync_channel(1);
    let pin = tokio::spawn(async move {
        read.snapshot(deadline(), move |_| {
            pinned.send(()).map_err(|_| ReadError::Worker)?;
            proceed
                .recv_timeout(Duration::from_secs(4))
                .map_err(|_| ReadError::Worker)
        })
        .await
    });
    reached.await?;
    let oversized = command(
        "over-cap",
        Durability::Full,
        vec![Statement::new(
            "UPDATE event SET data=zeroblob(983040)",
            vec![],
        )],
    )?;
    let outcome = writer.write(oversized, deadline()).await;
    assert!(
        matches!(outcome, Err(WriteError::WalPressure)),
        "{outcome:?}"
    );
    assert!(notices.try_recv().is_err());
    assert!(std::fs::metadata(dir.path().join("run.db-wal"))?.len() <= 64 * 1024 * 1024);
    assert!(store
        .readers()
        .resolve(RequestKey::new("over-cap")?, deadline())
        .await?
        .is_none());
    let count = store
        .readers()
        .snapshot(deadline(), |view| {
            Ok(view
                .prepare("SELECT count(*) FROM event WHERE length(data)=0")?
                .query_row([], |row| row.get::<_, i64>(0))?)
        })
        .await?;
    assert_eq!(count, 80);
    release.send(())?;
    pin.await??;
    store.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn automatic_transaction_rollback_keeps_the_original_constraint_error() -> ResultTest {
    let dir = TempDir::new()?;
    let store = RunStore::open(dir.path().join("run.db"), LEDGER).await?;
    let writer = store.writer();
    let poison = command(
        "rollback",
        Durability::Full,
        vec![
            put("session", 1, 0),
            Statement::new(
                "INSERT OR ROLLBACK INTO event VALUES ('session', 1, X'')",
                vec![],
            ),
        ],
    )?;
    let error = writer
        .write(poison, deadline())
        .await
        .err()
        .ok_or("unexpected commit")?;
    assert_eq!(
        error
            .sqlite_cause()
            .and_then(rusqlite::Error::sqlite_error_code),
        Some(rusqlite::ErrorCode::ConstraintViolation),
    );
    let next = command("retry", Durability::Full, vec![put("session", 1, 0)])?;
    assert_eq!(writer.write(next, deadline()).await?.affected_rows, vec![1]);
    store.shutdown().await?;
    Ok(())
}
