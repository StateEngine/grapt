use super::*;
use crate::{
    governed::{GovernedTransaction, WriteGate},
    notify::DurableNotice,
};
use grapt_event::{AggregateId, EventKind};
use grapt_types::{SessionId, WriterEpoch};
use rusqlite::Connection;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug, Default)]
struct Observer {
    published: Arc<AtomicI64>,
    preparations: AtomicUsize,
}

#[derive(Debug)]
struct Prepared {
    published: Arc<AtomicI64>,
    value: i64,
}

impl PreparedReadProjection for Prepared {
    fn publish(self: Box<Self>) {
        self.published.store(self.value, Ordering::SeqCst);
    }
}

impl CommittedReadProjection for Observer {
    fn affected(&self, kind: EventKind) -> bool {
        kind == EventKind::SessionRenamedV1
    }
    fn prepare(&self, conn: &Connection) -> Box<dyn PreparedReadProjection> {
        self.preparations.fetch_add(1, Ordering::SeqCst);
        let value = conn
            .query_row("SELECT COUNT(*) FROM child", [], |row| row.get(0))
            .unwrap_or_else(|error| panic!("projection witness query failed: {error}"));
        Box::new(Prepared {
            published: Arc::clone(&self.published),
            value,
        })
    }
}

fn connection() -> Result<(Connection, WriteGate), Box<dyn std::error::Error>> {
    let mut conn = Connection::open_in_memory()?;
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE parent (id INTEGER PRIMARY KEY);
         CREATE TABLE child (parent INTEGER REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED);
         INSERT INTO parent VALUES (1);",
    )?;
    let gate = WriteGate::new();
    gate.install(&mut conn)?;
    Ok((conn, gate))
}

fn dirty(tx: &GovernedTransaction<'_>, kind: EventKind) -> TestResult {
    let session: SessionId = "ses_projection".parse()?;
    tx.push_notice(DurableNotice::new(
        "evt_projection".parse()?,
        AggregateId::Session(session),
        1,
        kind,
        WriterEpoch::FIRST,
    ));
    Ok(())
}

#[test]
fn publication_is_after_commit_and_failure_preserves_previous_generation() -> TestResult {
    let (mut conn, gate) = connection()?;
    let observer = Arc::new(Observer::default());
    let projection: Arc<dyn CommittedReadProjection> = observer.clone();
    let tx = GovernedTransaction::begin_with_projection(
        &mut conn,
        gate.clone(),
        Some(projection.clone()),
        None,
    )?;
    tx.execute("INSERT INTO child VALUES (1)", [])?;
    dirty(&tx, EventKind::SessionRenamedV1)?;
    assert_eq!(
        observer.published.load(Ordering::SeqCst),
        0,
        "uncommitted data was published"
    );
    tx.commit()?.discard();
    assert_eq!(observer.published.load(Ordering::SeqCst), 1);

    let tx = GovernedTransaction::begin_with_projection(
        &mut conn,
        gate.clone(),
        Some(projection.clone()),
        None,
    )?;
    tx.execute("INSERT INTO child VALUES (1)", [])?;
    dirty(&tx, EventKind::SessionRenamedV1)?;
    drop(tx);
    assert_eq!(
        observer.published.load(Ordering::SeqCst),
        1,
        "rollback published a generation"
    );
    assert_eq!(observer.preparations.load(Ordering::SeqCst), 1);

    let tx = GovernedTransaction::begin_with_projection(&mut conn, gate, Some(projection), None)?;
    tx.execute("INSERT INTO child VALUES (99)", [])?;
    dirty(&tx, EventKind::SessionRenamedV1)?;
    assert!(
        tx.commit().is_err(),
        "deferred foreign key must force actual COMMIT failure"
    );
    assert_eq!(observer.preparations.load(Ordering::SeqCst), 2);
    assert_eq!(
        observer.published.load(Ordering::SeqCst),
        1,
        "failed COMMIT published a generation"
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM child", [], |row| row.get::<_, i64>(0))?,
        1
    );
    Ok(())
}

#[test]
fn lossy_advisory_notices_cannot_hide_projection_dirtiness() -> TestResult {
    let (mut conn, gate) = connection()?;
    let observer = Arc::new(Observer::default());
    let tx =
        GovernedTransaction::begin_with_projection(&mut conn, gate, Some(observer.clone()), None)?;
    tx.execute("INSERT INTO child VALUES (1)", [])?;
    for _ in 0..crate::notify::MAX_PENDING_NOTICES {
        dirty(&tx, EventKind::SessionCreatedV1)?;
    }
    dirty(&tx, EventKind::SessionRenamedV1)?;
    tx.commit()?.discard();
    assert_eq!(observer.published.load(Ordering::SeqCst), 1);
    assert_eq!(observer.preparations.load(Ordering::SeqCst), 1);
    Ok(())
}

#[test]
fn unrelated_commit_does_not_prepare_a_generation() -> TestResult {
    let (mut conn, gate) = connection()?;
    let observer = Arc::new(Observer::default());
    let tx =
        GovernedTransaction::begin_with_projection(&mut conn, gate, Some(observer.clone()), None)?;
    tx.execute("INSERT INTO child VALUES (1)", [])?;
    dirty(&tx, EventKind::SessionCreatedV1)?;
    tx.commit()?.discard();
    assert_eq!(observer.preparations.load(Ordering::SeqCst), 0);
    Ok(())
}

#[derive(Debug)]
struct Executor(usize);

impl crate::commit_executor::CommitExecutor for Executor {
    fn execute(&self, work: &mut dyn FnMut()) {
        for _ in 0..self.0 {
            work();
        }
    }
}

#[test]
fn executor_cannot_skip_a_commit_ack_or_commit_twice() -> TestResult {
    for calls in [0, 1, 2] {
        let (mut conn, gate) = connection()?;
        let observer = Arc::new(Observer::default());
        let tx = GovernedTransaction::begin_with_projection(
            &mut conn,
            gate,
            Some(observer.clone()),
            Some(Arc::new(Executor(calls))),
        )?;
        tx.execute("INSERT INTO child VALUES (1)", [])?;
        dirty(&tx, EventKind::SessionRenamedV1)?;
        let outcome = tx.commit();
        if calls == 0 {
            assert!(
                outcome.is_err(),
                "executor omission fabricated a commit ACK"
            );
        } else {
            outcome?.discard();
        }
        let committed = i64::from(calls != 0);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM child", [], |row| row.get::<_, i64>(0))?,
            committed
        );
        assert_eq!(observer.published.load(Ordering::SeqCst), committed);
        assert_eq!(
            observer.preparations.load(Ordering::SeqCst),
            usize::from(calls != 0)
        );
    }
    Ok(())
}
