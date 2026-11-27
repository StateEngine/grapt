use grapt_run_store::{
    Command, Durability, MigrationSpec, ReadError, RequestKey, RunStore, Scope, Statement, Value,
    WriteError,
};
use std::error::Error;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;
use tokio::time::Instant;

type ResultTest = Result<(), Box<dyn Error + Send + Sync>>;
const DATABASE: &str = "GRAPT_TEST_WAL_LIMIT_DATABASE";
const SCHEMA: &str = "CREATE TABLE records(id INTEGER PRIMARY KEY, payload BLOB NOT NULL);
CREATE TABLE ordered_records(id INTEGER PRIMARY KEY, payload BLOB NOT NULL);";
const LEDGER: &[MigrationSpec] = &[MigrationSpec::new(1, "initial", SCHEMA)];
const ROWS: i64 = 80;
const PAYLOAD_LEN: usize = 960 * 1024;
const WAL_CAP: u64 = 64 * 1024 * 1024;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(15)
}

#[tokio::test]
async fn pinned_same_size_rewrite_stays_inside_the_wal_and_memory_budgets() -> ResultTest {
    if let Some(path) = std::env::var_os(DATABASE) {
        return rewrite(Path::new(&path)).await;
    }
    with_seed("pinned_same_size_rewrite_stays_inside_the_wal_and_memory_budgets").await
}

#[tokio::test]
async fn ordered_insert_refuses_before_materializing_large_payloads() -> ResultTest {
    if let Some(path) = std::env::var_os(DATABASE) {
        return ordered_insert(Path::new(&path)).await;
    }
    with_seed("ordered_insert_refuses_before_materializing_large_payloads").await
}

#[tokio::test]
async fn scalar_arguments_cannot_exhaust_the_writer_heap() -> ResultTest {
    if let Some(path) = std::env::var_os(DATABASE) {
        return scalar_insert(Path::new(&path)).await;
    }
    let folder = tempfile::TempDir::new()?;
    isolated(
        "scalar_arguments_cannot_exhaust_the_writer_heap",
        &folder.path().join("run.db"),
    )
}

async fn with_seed(test: &str) -> ResultTest {
    let folder = tempfile::TempDir::new()?;
    let path = folder.path().join("run.db");
    seed(&path).await?;
    isolated(test, &path)
}

fn isolated(test: &str, path: &Path) -> ResultTest {
    // Measure the operation in a fresh process, outside setup and other tests.
    let output = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", test, "--nocapture"])
        .env(DATABASE, path)
        .output()?;
    print!("{}", std::str::from_utf8(&output.stdout)?);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

async fn seed(path: &Path) -> ResultTest {
    let store = RunStore::open(path, LEDGER).await?;
    let writer = store.writer();
    for id in 1..=ROWS {
        let key = RequestKey::new(&format!("seed-{id}"))?;
        let deadline = deadline();
        loop {
            let command = Command::new(
                key.clone(),
                Durability::Normal,
                Scope::Global,
                vec![Statement::new(
                    "INSERT INTO records VALUES (?1,?2)",
                    vec![Value::Integer(id), Value::Blob(vec![b'a'; PAYLOAD_LEN])],
                )],
            )?;
            match writer.write(command, deadline).await {
                Ok(_) => break,
                Err(WriteError::WalPressure) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    store.shutdown().await?;
    Ok(())
}

async fn rewrite(path: &Path) -> ResultTest {
    let store = RunStore::open(path, LEDGER).await?;
    let reader = store.readers();
    let (pinned, reached) = tokio::sync::oneshot::channel();
    let (release, proceed) = mpsc::sync_channel(1);
    let pin = tokio::spawn(async move {
        reader
            .snapshot(deadline(), move |_| {
                pinned.send(()).map_err(|_| ReadError::Worker)?;
                proceed
                    .recv_timeout(Duration::from_secs(8))
                    .map_err(|_| ReadError::Worker)
            })
            .await
    });
    reached.await?;
    #[cfg(target_os = "linux")]
    let baseline = peak_rss_kib()?;
    let writer = store.writer();
    let mut notices = writer.subscribe();
    let key = RequestKey::new("rewrite")?;
    let command = Command::new(
        key.clone(),
        Durability::Full,
        Scope::Global,
        vec![Statement::new(
            "UPDATE records SET payload=CAST('b' || substr(payload,2) AS BLOB)",
            vec![],
        )],
    )?;
    let outcome = writer.write(command, deadline()).await;
    let wal_peak = writer.stats().wal_peak_bytes;
    println!("rewrite_wal_peak_bytes={wal_peak}");
    #[cfg(target_os = "linux")]
    {
        let peak = peak_rss_kib()?;
        let growth = peak.saturating_sub(baseline);
        println!(
            "rewrite_rss_before_kib={baseline} rewrite_rss_peak_kib={peak} growth_kib={growth}"
        );
        assert!(growth <= 32 * 1024, "rewrite grew RSS by {growth} KiB");
    }
    assert!(
        matches!(outcome, Err(WriteError::WalPressure)),
        "{outcome:?}"
    );
    assert!(wal_peak <= WAL_CAP);
    let wal = path.with_file_name("run.db-wal");
    assert!(std::fs::metadata(wal)?.len() <= WAL_CAP);
    assert!(notices.try_recv().is_err());
    let read = store.readers();
    assert!(read.resolve(key, deadline()).await?.is_none());
    let unchanged = read
        .snapshot(deadline(), |view| {
            Ok(view.prepare(
            "SELECT count(*) FROM records WHERE length(payload)=?1 AND substr(payload,1,1)=X'61'",
        )?.query_row([PAYLOAD_LEN as i64], |row| row.get::<_, i64>(0))?)
        })
        .await?;
    assert_eq!(unchanged, ROWS);
    let one_row = Command::new(
        RequestKey::new("rewrite-one")?,
        Durability::Full,
        Scope::Global,
        vec![Statement::new(
            "UPDATE records SET payload=CAST('b' || substr(payload,2) AS BLOB) WHERE id=1",
            vec![],
        )],
    )?;
    assert_eq!(
        writer.write(one_row, deadline()).await?.affected_rows,
        vec![1]
    );
    release.send(())?;
    pin.await??;
    store.shutdown().await?;
    Ok(())
}

async fn ordered_insert(path: &Path) -> ResultTest {
    let store = RunStore::open(path, LEDGER).await?;
    let writer = store.writer();
    let mut notices = writer.subscribe();
    let key = RequestKey::new("ordered-insert")?;
    let command = Command::new(
        key.clone(),
        Durability::Full,
        Scope::Global,
        vec![
            Statement::new("INSERT INTO ordered_records VALUES (1000, X'01')", vec![]),
            Statement::new(
                "INSERT INTO ordered_records
             SELECT id,payload FROM records WHERE id<=72 ORDER BY substr(payload,1,1),id DESC",
                vec![],
            ),
        ],
    )?;
    #[cfg(target_os = "linux")]
    let baseline = peak_rss_kib()?;
    let outcome = writer.write(command, deadline()).await;
    #[cfg(target_os = "linux")]
    {
        let peak = peak_rss_kib()?;
        let growth = peak.saturating_sub(baseline);
        println!("ordered_insert_rss_before_kib={baseline} ordered_insert_rss_peak_kib={peak} growth_kib={growth}");
        assert!(
            growth <= 16 * 1024,
            "ordered insert grew RSS by {growth} KiB"
        );
    }
    assert!(
        matches!(outcome, Err(WriteError::TemporaryStorage)),
        "{outcome:?}"
    );
    assert!(notices.try_recv().is_err());
    let readers = store.readers();
    assert!(readers.resolve(key, deadline()).await?.is_none());
    let count = readers
        .snapshot(deadline(), |view| {
            Ok(view
                .prepare("SELECT count(*) FROM ordered_records")?
                .query_row([], |row| row.get::<_, i64>(0))?)
        })
        .await?;
    assert_eq!(count, 0, "the rejected command leaked its earlier write");
    store.shutdown().await?;
    Ok(())
}

async fn scalar_insert(path: &Path) -> ResultTest {
    let store = RunStore::open(path, LEDGER).await?;
    let writer = store.writer();
    let mut notices = writer.subscribe();
    let key = RequestKey::new("scalar-insert")?;
    // The inner functions each evaluate seven materialized blobs. Use
    // straight-line expressions so the test also covers sparse VM callbacks.
    let args = ["substr(zeroblob(983040),1)"; 7].join(",");
    let group = format!("printf('',{args})");
    let groups = [group.as_str(); 7].join(",");
    let command = Command::new(
        key.clone(),
        Durability::Full,
        Scope::Global,
        vec![
            Statement::new("INSERT INTO records VALUES (1, X'01')", vec![]),
            Statement::new(
                format!("INSERT INTO records VALUES (2, printf('',{groups}))"),
                vec![],
            ),
        ],
    )?;
    #[cfg(target_os = "linux")]
    let baseline = peak_rss_kib()?;
    let outcome = writer.write(command, deadline()).await;
    #[cfg(target_os = "linux")]
    {
        let peak = peak_rss_kib()?;
        let growth = peak.saturating_sub(baseline);
        println!("scalar_insert_rss_before_kib={baseline} scalar_insert_rss_peak_kib={peak} growth_kib={growth}");
        assert!(
            growth <= 32 * 1024,
            "scalar insert grew RSS by {growth} KiB"
        );
    }
    assert!(
        matches!(outcome, Err(WriteError::HeapPressure)),
        "{outcome:?}"
    );
    assert!(matches!(
        notices.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    let readers = store.readers();
    assert!(readers.resolve(key, deadline()).await?.is_none());
    let count = readers
        .snapshot(deadline(), |view| {
            Ok(view
                .prepare("SELECT count(*) FROM records")?
                .query_row([], |row| row.get::<_, i64>(0))?)
        })
        .await?;
    assert_eq!(count, 0, "the rejected command leaked its earlier write");
    store.shutdown().await?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn peak_rss_kib() -> Result<u64, Box<dyn Error + Send + Sync>> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    let rss = status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|value| value.split_whitespace().next())
        .ok_or("missing process RSS high-water mark")?;
    Ok(rss.parse()?)
}
