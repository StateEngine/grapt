//! Standalone qualification workload. Arguments are session count and writes
//! per session. On Linux, eio, enospc and stall modes run the corresponding
//! syscall injection and verify the observed storage behavior.
use grapt_run_store::{
    Command, Durability, MigrationSpec, RequestKey, RunStore, Scope, Statement, Value, WriteError,
};
use grapt_types::SessionId;
use std::error::Error;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::Instant;

#[cfg(target_os = "linux")]
#[path = "qualification/witness.rs"]
mod witness;

const SCHEMA: &str = "CREATE TABLE event(
    session TEXT, seq INTEGER, text BLOB, PRIMARY KEY(session, seq));
    CREATE TABLE session_list(id TEXT PRIMARY KEY, updated INTEGER);
    CREATE INDEX session_order ON session_list(updated DESC, id DESC);";
const LEDGER: &[MigrationSpec] = &[MigrationSpec::new(1, "initial", SCHEMA)];
type BenchResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Default)]
struct Samples {
    normal: Vec<Duration>,
    full: Vec<Duration>,
    pressure: usize,
    read: Vec<Duration>,
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(15)
}

fn operation(session: &SessionId, seq: i64, durability: Durability) -> Result<Command, WriteError> {
    let id = session.as_str();
    let statements = vec![
        Statement::new(
            "INSERT INTO event VALUES (?1, ?2, ?3)",
            vec![
                Value::Text(id.into()),
                Value::Integer(seq),
                Value::Blob(vec![49; if seq % 12 == 0 { 24_600 } else { 256 }]),
            ],
        ),
        Statement::new(
            "INSERT INTO session_list VALUES (?1, ?2)
            ON CONFLICT (id) DO UPDATE SET updated=excluded.updated",
            vec![Value::Text(id.into()), Value::Integer(seq / 12)],
        ),
    ];
    Command::new(
        RequestKey::new(&format!("{id}-{seq}"))?,
        durability,
        Scope::Session(session.clone()),
        statements,
    )
}

async fn write_session(
    writer: grapt_run_store::Writer,
    session: SessionId,
    rows: i64,
) -> BenchResult<Samples> {
    let mut samples = Samples::default();
    for seq in 1..=rows {
        let durability = if seq % 12 < 6 {
            Durability::Full
        } else {
            Durability::Normal
        };
        let start = Instant::now();
        let mut written = false;
        for _ in 0..16 {
            match writer
                .write(operation(&session, seq, durability)?, deadline())
                .await
            {
                Ok(_) => {
                    written = true;
                    break;
                }
                Err(WriteError::WalPressure | WriteError::QueueFull | WriteError::ByteBudget) => {
                    samples.pressure += 1;
                }
                Err(error) => return Err(error.into()),
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if !written {
            return Err("original-key retry did not recover".into());
        }
        match durability {
            Durability::Full => samples.full.push(start.elapsed()),
            Durability::Normal => samples.normal.push(start.elapsed()),
        }
    }
    Ok(samples)
}

async fn list_sessions(
    read: grapt_run_store::ReadPool,
    stop: tokio::sync::oneshot::Receiver<()>,
) -> BenchResult<Vec<Duration>> {
    tokio::pin!(stop);
    let mut samples = Vec::new();
    let mut interval = tokio::time::interval(Duration::from_millis(50));
    loop {
        tokio::select! {
            _ = &mut stop => return Ok(samples),
            _ = interval.tick() => {
                let start = Instant::now();
                read.snapshot(deadline(), |snapshot| {
                    let mut query = snapshot.prepare(
                        "SELECT id FROM session_list ORDER BY updated DESC, id DESC LIMIT 50")?;
                    let rows: Vec<String> = query.query_map([], |row| row.get(0))?
                        .collect::<Result<_, _>>()?;
                    Ok(rows)
                }).await?;
                samples.push(start.elapsed());
            }
        }
    }
}

fn percentile(samples: &mut [Duration], percent: usize) -> f64 {
    samples.sort_unstable();
    samples
        .get(
            samples
                .len()
                .saturating_mul(percent)
                .div_ceil(100)
                .saturating_sub(1),
        )
        .map_or(f64::NAN, |sample| sample.as_secs_f64() * 1000.0)
}

async fn verify_reads(
    read: grapt_run_store::ReadPool,
    sessions: u32,
    rows: i64,
) -> BenchResult<()> {
    use std::collections::HashSet;
    let count = read
        .snapshot(deadline(), move |view| {
            let mut cursor = (i64::MAX, String::new());
            let mut seen = HashSet::new();
            let mut page = view.prepare(
                "SELECT updated, id FROM session_list WHERE (updated,id)<(?1,?2)
             ORDER BY updated DESC,id DESC LIMIT 17",
            )?;
            loop {
                let batch: Vec<(i64, String)> = page
                    .query_map(rusqlite::params![cursor.0, cursor.1], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })?
                    .collect::<Result<_, _>>()?;
                if batch.is_empty() {
                    break;
                }
                for entry in batch {
                    assert!(entry < cursor);
                    assert!(seen.insert(entry.1.clone()));
                    cursor = entry;
                }
            }
            assert_eq!(seen.len(), sessions as usize);
            Ok(view
                .prepare("SELECT count(*) FROM event WHERE instr(text, X'31') > 0")?
                .query_row([], |row| row.get::<_, i64>(0))?)
        })
        .await?;
    assert_eq!(count, i64::from(sessions) * rows);
    Ok(())
}

fn validate(path: &std::path::Path, sessions: u32, rows: i64) -> BenchResult<()> {
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let check: String = connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    assert_eq!(check, "ok");
    let count: i64 = connection.query_row(
        "SELECT count(*) FROM (SELECT session FROM event GROUP BY session
         HAVING min(seq)=1 AND max(seq)=?1 AND count(*)=?1)",
        [rows],
        |row| row.get(0),
    )?;
    assert_eq!(count, i64::from(sessions));
    let receipts: i64 =
        connection.query_row("SELECT count(*) FROM run_store_receipt", [], |row| {
            row.get(0)
        })?;
    assert_eq!(receipts, i64::from(sessions) * rows);
    Ok(())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> BenchResult<()> {
    let mut args = std::env::args().skip(1);
    let first = args.next();
    #[cfg(target_os = "linux")]
    if let Some(status) = witness::mode(first.as_deref()).await {
        return status;
    }
    let sessions: u32 = first.as_deref().unwrap_or("8").parse()?;
    let rows: i64 = args.next().as_deref().unwrap_or("240").parse()?;
    if sessions == 0 || sessions > 400 || !(12..=800).contains(&rows) {
        return Err("use 1–400 sessions with 12–800 writes each".into());
    }
    let folder = TempDir::new()?;
    let path = folder.path().join("run.db");
    let store = RunStore::open(&path, LEDGER).await?;
    let writer = store.writer();
    let (stop, done) = tokio::sync::oneshot::channel();
    let reads = tokio::spawn(list_sessions(store.readers(), done));
    let start = Instant::now();
    let mut tasks = tokio::task::JoinSet::new();
    for session in 0..sessions {
        let id: SessionId = format!("ses_{session:012x}").parse()?;
        tasks.spawn(write_session(writer.clone(), id, rows));
    }
    let mut all = Samples::default();
    while let Some(result) = tasks.join_next().await {
        let mut worker = result??;
        all.normal.append(&mut worker.normal);
        all.full.append(&mut worker.full);
        all.pressure += worker.pressure;
    }
    let elapsed = start.elapsed();
    stop.send(()).map_err(|_| "list worker stopped")?;
    all.read = reads.await??;
    verify_reads(store.readers(), sessions, rows).await?;
    let stats = writer.stats();
    store.shutdown().await?;
    validate(&path, sessions, rows)?;
    println!("sessions={sessions} writes={} writes_per_s={:.1} full_p99_ms={:.2} normal_p99_ms={:.2} list_p99_ms={:.2} wal_peak_bytes={} pressure={} checkpoint_failures={}",
        i64::from(sessions) * rows,
        f64::from(sessions) * rows as f64 / elapsed.as_secs_f64(),
        percentile(&mut all.full, 99), percentile(&mut all.normal, 99), percentile(&mut all.read, 99),
        stats.wal_peak_bytes, all.pressure, stats.checkpoint_failures,
    );
    Ok(())
}
