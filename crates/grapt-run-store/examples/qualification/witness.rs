use super::{deadline, operation, BenchResult, LEDGER};
use grapt_run_store::{Durability, ReadPool, RequestKey, RunStore, WriteError, Writer};
use grapt_types::SessionId;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::broadcast;
use tokio::time::{timeout, Instant};

pub(super) async fn mode(name: Option<&str>) -> Option<BenchResult<()>> {
    Some(match name? {
        "eio" => trace("fsync", "error=EIO:when=11", "check-eio"),
        "enospc" => trace("pwrite64", "error=ENOSPC:when=55", "check-enospc"),
        "stall" => trace("fsync", "delay_exit=5000000:when=11", "check-stall"),
        "check-eio" => check_fault(rusqlite::ffi::SQLITE_IOERR_FSYNC).await,
        "check-enospc" => check_fault(rusqlite::ffi::SQLITE_FULL).await,
        "check-stall" => check_stall().await,
        _ => return None,
    })
}

fn trace(syscall: &str, injection: &str, child: &str) -> BenchResult<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("syscalls.log");
    let status = Command::new("strace")
        .args(["-f", "-T", "-e"])
        .arg(format!("trace={syscall}"))
        .arg("-e")
        .arg(format!("inject={syscall}:{injection}"))
        .arg("-o")
        .arg(&path)
        .arg(std::env::current_exe()?)
        .arg(child)
        .status()?;
    if !status.success() {
        return Err(format!("qualification child failed: {status}").into());
    }
    let contents = std::fs::read_to_string(path)?;
    let witnessed = match child {
        "check-eio" => contents
            .lines()
            .any(|line| line.contains("= -1 EIO ") && line.contains("(INJECTED)")),
        "check-enospc" => contents
            .lines()
            .any(|line| line.contains("= -1 ENOSPC ") && line.contains("(INJECTED)")),
        _ => contents
            .lines()
            .any(|line| line.contains("fsync(") && line.contains("(DELAYED)")),
    };
    if !witnessed {
        return Err(format!("missing {syscall} injection witness for {child}").into());
    }
    println!("syscall_witness={child}");
    Ok(())
}

async fn check_fault(expected: i32) -> BenchResult<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("run.db");
    let store = RunStore::open(&path, LEDGER).await?;
    let writer = store.writer();
    let mut notices = writer.subscribe();
    let session: SessionId = "ses_001".parse()?;
    for seq in 1..=36 {
        let work = operation(&session, seq, Durability::Full)?;
        let key = RequestKey::new(&format!("{session}-{seq}"))?;
        match writer.write(work, deadline()).await {
            Ok(_) => assert_eq!(notices.try_recv()?.key, key),
            Err(error) if sqlite_code(&error) == Some(expected) => {
                if expected == rusqlite::ffi::SQLITE_IOERR_FSYNC {
                    assert!(
                        matches!(error, WriteError::OutcomeUnknown { .. }),
                        "fsync fault must reach COMMIT: {error:?}"
                    );
                }
                assert!(matches!(
                    notices.try_recv(),
                    Err(broadcast::error::TryRecvError::Empty)
                ));
                verify_retry(&store, &writer, &session, seq, key, error, &mut notices).await?;
                store.shutdown().await?;
                verify_restart(&path, seq)?;
                println!("sqlite_error={expected} provisional_notices=0 durable_effects=1");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(format!("missing SQLite write-time code {expected}").into())
}

fn sqlite_code(error: &WriteError) -> Option<i32> {
    match error.sqlite_cause()? {
        rusqlite::Error::SqliteFailure(code, _) => Some(code.extended_code),
        _ => None,
    }
}

async fn verify_retry(
    store: &RunStore,
    writer: &Writer,
    session: &SessionId,
    seq: i64,
    key: RequestKey,
    error: WriteError,
    notices: &mut broadcast::Receiver<grapt_run_store::CommitNotice>,
) -> BenchResult<()> {
    let readers = store.readers();
    let already_committed = readers.resolve(key.clone(), deadline()).await?.is_some();
    if !matches!(error, WriteError::OutcomeUnknown { .. }) {
        assert!(!already_committed);
    }
    assert_eq!(
        event_count(&readers, seq).await?,
        i64::from(already_committed)
    );
    writer
        .write(operation(session, seq, Durability::Full)?, deadline())
        .await?;
    if !already_committed {
        assert_eq!(notices.try_recv()?.key, key);
    }
    assert!(matches!(
        notices.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    assert!(readers.resolve(key, deadline()).await?.is_some());
    assert_eq!(event_count(&readers, seq).await?, 1);
    Ok(())
}

async fn event_count(readers: &ReadPool, seq: i64) -> BenchResult<i64> {
    Ok(readers
        .snapshot(deadline(), move |read| {
            Ok(read
                .prepare("SELECT count(*) FROM event WHERE seq=?1")?
                .query_row([seq], |row| row.get(0))?)
        })
        .await?)
}

fn verify_restart(path: &Path, seq: i64) -> BenchResult<()> {
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let count: i64 = connection.query_row("SELECT count(*) FROM event", [], |row| row.get(0))?;
    assert_eq!(count, seq);
    let check: String = connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    assert_eq!(check, "ok");
    Ok(())
}

async fn check_stall() -> BenchResult<()> {
    let temp = TempDir::new()?;
    let store = RunStore::open(temp.path().join("run.db"), LEDGER).await?;
    let writer = store.writer();
    let session: SessionId = "ses_001".parse()?;
    assert_eq!(list_snapshot(&store.readers()).await?, (None, 0));
    for seq in 1..=24 {
        let work = operation(&session, seq, Durability::Full)?;
        let writer = writer.clone();
        let started = Instant::now();
        let mut write = tokio::spawn(async move { writer.write(work, deadline()).await });
        if let Ok(result) = timeout(Duration::from_millis(150), &mut write).await {
            result??;
            continue;
        }
        let sample = Instant::now();
        let (list, rows) = list_snapshot(&store.readers()).await?;
        let read_ms = sample.elapsed().as_secs_f64() * 1000.0;
        assert!(
            !write.is_finished(),
            "list returned after the writer resumed"
        );
        assert_eq!(rows, seq - 1);
        assert_eq!(list, (seq > 1).then(|| session.as_str().to_owned()));
        write.await??;
        assert!(started.elapsed() >= Duration::from_millis(4500));
        assert_eq!(list_snapshot(&store.readers()).await?.1, seq);
        store.shutdown().await?;
        println!("list_during_fsync_ms={read_ms:.2}");
        return Ok(());
    }
    Err("no writer stall witnessed during a list snapshot".into())
}

async fn list_snapshot(readers: &ReadPool) -> BenchResult<(Option<String>, i64)> {
    use rusqlite::OptionalExtension;
    Ok(readers
        .snapshot(Instant::now() + Duration::from_millis(500), |view| {
            let list = view
                .prepare("SELECT id FROM session_list ORDER BY updated DESC, id DESC LIMIT 1")?
                .query_row([], |row| row.get(0))
                .optional()?;
            let rows = view
                .prepare("SELECT count(*) FROM event")?
                .query_row([], |row| row.get(0))?;
            Ok((list, rows))
        })
        .await?)
}
