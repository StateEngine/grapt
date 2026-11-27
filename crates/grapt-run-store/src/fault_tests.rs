//! Re-execute the test harness and terminate the writer at the commit boundary.
use crate::{Command, Durability, RequestKey, RunStore, Scope, Statement};
use std::error::Error;
use std::io::{self, BufRead, Write};
use std::process::{Command as Process, Stdio};
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::Instant;

const CRASH_CUT: &str = "GRAPT_TEST_RUN_STORE_CRASH_CUT";
const DATABASE: &str = "GRAPT_TEST_RUN_STORE_DATABASE";
const SCHEMA: &str = "CREATE TABLE terminal(id INTEGER);
CREATE TABLE delivery(id INTEGER);
CREATE TABLE follow_up(id INTEGER);
CREATE TABLE session_list(id TEXT PRIMARY KEY, updated INTEGER);
INSERT INTO session_list VALUES ('existing', 1);";
type ResultTest = Result<(), Box<dyn Error + Send + Sync>>;

pub(crate) fn cut(point: &str) {
    if std::env::var(CRASH_CUT).as_deref() == Ok(point) {
        let mut stdout = io::stdout().lock();
        if writeln!(stdout, "reached {point}")
            .and_then(|()| stdout.flush())
            .is_ok()
        {
            std::thread::park_timeout(Duration::from_secs(8));
        }
    }
}

fn handoff() -> Result<Command, crate::WriteError> {
    Command::new(
        RequestKey::new("handoff")?,
        Durability::Full,
        Scope::Global,
        [
            "INSERT INTO terminal VALUES (1)",
            "INSERT INTO delivery VALUES (1)",
            "INSERT INTO follow_up VALUES (1)",
        ]
        .map(|sql| Statement::new(sql, vec![]))
        .into(),
    )
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(4)
}

#[tokio::test]
async fn crash_child() -> ResultTest {
    let Some(path) = std::env::var_os(DATABASE) else {
        return Ok(());
    };
    let store = RunStore::open(path, SCHEMA).await?;
    store.writer().write(handoff()?, deadline()).await?;
    store.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn process_loss_preserves_the_atomic_delivery_and_original_key() -> ResultTest {
    for (point, expected) in [("before-commit", 0), ("after-commit", 1)] {
        let folder = TempDir::new()?;
        let path = folder.path().join("run.db");
        let mut child = Process::new(std::env::current_exe()?)
            .args(["--exact", "fault_tests::crash_child", "--nocapture"])
            .env(CRASH_CUT, point)
            .env(DATABASE, &path)
            .stdout(Stdio::piped())
            .spawn()?;
        let output = child.stdout.take().ok_or("child stdout missing")?;
        let expected_line = format!("reached {point}");
        let reached = io::BufReader::new(output)
            .lines()
            .any(|line| line.is_ok_and(|line| line.ends_with(&expected_line)));
        let killed = child.kill();
        let status = child.wait()?;
        assert!(reached, "child did not reach {point}: {status}");
        killed?;
        assert!(!status.success());
        let store = RunStore::open(path, SCHEMA).await?;
        let read = store.readers();
        let counts = read
            .snapshot(deadline(), |view| {
                Ok(view
                    .prepare(
                        "SELECT (SELECT count(*) FROM terminal),
                (SELECT count(*) FROM delivery), (SELECT count(*) FROM follow_up)",
                    )?
                    .query_row([], |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    })?)
            })
            .await?;
        assert_eq!(counts, (expected, expected, expected));
        let recorded = read
            .resolve(RequestKey::new("handoff")?, deadline())
            .await?;
        assert_eq!(recorded.is_some(), expected == 1);
        store.writer().write(handoff()?, deadline()).await?;
        assert!(read
            .resolve(RequestKey::new("handoff")?, deadline())
            .await?
            .is_some());
        store.shutdown().await?;
    }
    Ok(())
}

type Stall = (
    tokio::sync::oneshot::Sender<()>,
    std::sync::mpsc::Receiver<()>,
);
static STALL: std::sync::Mutex<Option<Stall>> = std::sync::Mutex::new(None);

pub(crate) fn stall(key: &str) {
    if key != "isolation-block" {
        return;
    }
    let stall = STALL.lock().ok().and_then(|mut slot| slot.take());
    if let Some((reached, release)) = stall {
        let _ = reached.send(());
        let _ = release.recv_timeout(Duration::from_secs(5));
    }
}

#[tokio::test]
async fn sqlite_stall_keeps_the_runtime_and_admission_bounded() -> ResultTest {
    let folder = TempDir::new()?;
    let path = folder.path().join("run.db");
    let store = RunStore::open(&path, SCHEMA).await?;
    let (at_cut, reached) = tokio::sync::oneshot::channel();
    let (release, proceed) = std::sync::mpsc::sync_channel(1);
    *STALL.lock().map_err(|_| "stall fixture poisoned")? = Some((at_cut, proceed));
    let writer = store.writer();
    let busy = writer.clone();
    let first = tokio::spawn(async move {
        busy.write(isolation_command("isolation-block", 0)?, deadline())
            .await
    });
    reached.await?;
    let list = store
        .readers()
        .snapshot(deadline(), |view| {
            Ok(view
                .prepare("SELECT id FROM session_list ORDER BY updated DESC, id DESC LIMIT 1")?
                .query_row([], |row| row.get::<_, String>(0))?)
        })
        .await?;
    assert_eq!(list, "existing");
    assert!(
        !first.is_finished(),
        "the writer left the commit cut before the list read"
    );
    // Admit requests and allow the callers' deadlines to fire while the
    // writer is paused inside the SQLite transaction.
    let bytes = fill(&writer, 80, 992 * 1024).await?;
    assert!(bytes.0 > 0 && bytes.1 > 0, "{bytes:?}");
    let slots = fill(&writer, 270, 0).await?;
    assert!(slots.0 > 0 && slots.1 > 0, "{slots:?}");
    let stats = writer.stats();
    assert_eq!(stats.admitted_requests, 256);
    assert!(stats.admitted_bytes <= 64 * 1024 * 1024);
    let mut heartbeat = tokio::time::interval(Duration::from_millis(5));
    tokio::time::timeout(Duration::from_millis(300), async {
        for _ in 0..20 {
            heartbeat.tick().await;
        }
    })
    .await?;
    release.send(())?;
    first.await??;
    store.shutdown().await?;
    let store = RunStore::open(&path, SCHEMA).await?;
    let original = slots.2.ok_or("no expired admitted key")?;
    let original_key = RequestKey::new(&original)?;
    assert!(store
        .readers()
        .resolve(original_key, deadline())
        .await?
        .is_some());
    assert_eq!(
        store
            .writer()
            .write(isolation_command(&original, 0)?, deadline())
            .await?
            .affected_rows,
        vec![1]
    );
    store.shutdown().await?;
    Ok(())
}

fn isolation_command(key: &str, bytes: usize) -> Result<Command, crate::WriteError> {
    Command::new(
        RequestKey::new(key)?,
        Durability::Normal,
        Scope::Global,
        vec![Statement::new(
            "INSERT INTO terminal VALUES (length(?1))",
            vec![crate::Value::Blob(vec![0; bytes])],
        )],
    )
}

async fn fill(
    writer: &crate::Writer,
    count: usize,
    bytes: usize,
) -> Result<(usize, usize, Option<String>), Box<dyn Error + Send + Sync>> {
    let mut writes = tokio::task::JoinSet::new();
    for index in 0..count {
        let command = isolation_command(&format!("pressure-{bytes}-{index}"), bytes)?;
        let writer = writer.clone();
        writes.spawn(async move {
            writer
                .write(command, Instant::now() + Duration::from_millis(30))
                .await
        });
    }
    let mut unknown = 0;
    let mut refused = 0;
    let mut sample = None;
    while let Some(result) = writes.join_next().await {
        match result? {
            Err(crate::WriteError::OutcomeUnknown { key, .. }) => {
                unknown += 1;
                if sample.is_none() {
                    sample = Some(key.as_str().to_owned());
                }
            }
            Err(crate::WriteError::ByteBudget) if bytes > 0 => refused += 1,
            Err(crate::WriteError::QueueFull) if bytes == 0 => refused += 1,
            outcome => return Err(format!("unexpected admission outcome: {outcome:?}").into()),
        }
    }
    Ok((unknown, refused, sample))
}
