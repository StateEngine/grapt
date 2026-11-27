use grapt_run_store::RunStore;
use grapt_session_actor::{
    load, ActorConfig, Event, SessionActor, SessionRuntimeState as S, SpawnError, SubmitError,
    SubmitKey, SCHEMA,
};
use grapt_types::SessionId;
use std::error::Error;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::time::{timeout, Instant};

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

fn session() -> Result<SessionId, Box<dyn Error + Send + Sync>> {
    Ok("ses_fixture01".parse()?)
}

fn key(text: &str) -> Result<SubmitKey, Box<dyn Error + Send + Sync>> {
    Ok(SubmitKey::new(text)?)
}

async fn start(
    path: &Path,
    config: ActorConfig,
) -> Result<(RunStore, SessionActor), Box<dyn Error + Send + Sync>> {
    let store = RunStore::open(path, SCHEMA).await?;
    let actor = spawn(&store, config).await?;
    Ok((store, actor))
}

async fn spawn(
    store: &RunStore,
    config: ActorConfig,
) -> Result<SessionActor, Box<dyn Error + Send + Sync>> {
    Ok(SessionActor::spawn(store.writer(), store.readers(), session()?, config).await?)
}

fn soon() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

#[tokio::test]
async fn a_transition_is_durable_before_it_is_published_and_survives_restart() -> TestResult {
    let dir = TempDir::new()?;
    let path = dir.path().join("run.db");
    let (store, actor) = start(&path, ActorConfig::default()).await?;
    let handle = actor.handle();
    let mut changes = handle.subscribe();
    handle.submit(key("a")?, Event::StartTurn).await?;
    let paused = handle.submit(key("b")?, Event::Pause).await?;
    assert_eq!((paused.state, paused.state_seq.get()), (S::Paused, 2));
    for expected in [1, 2] {
        let change = changes.recv().await?;
        assert_eq!(change.state_seq.get(), expected);
        // The store already holds at least what was published.
        let stored = load(&store.readers(), session()?, soon()).await?;
        assert!(stored.state_seq >= change.state_seq);
    }
    let refused = handle.submit(key("c")?, Event::StartTurn).await;
    assert!(matches!(refused, Err(SubmitError::InvalidTransition(_))));
    assert_eq!(handle.snapshot().state_seq.get(), 2);
    actor.shutdown().await?;
    store.shutdown().await?;

    let (store, actor) = start(&path, ActorConfig::default()).await?;
    let handle = actor.handle();
    assert_eq!(handle.snapshot().state, S::Paused);
    assert_eq!(
        handle.submit(key("d")?, Event::Resume).await?.state,
        S::Running
    );
    actor.shutdown().await?;
    store.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn a_retried_submit_key_applies_once_across_restarts() -> TestResult {
    let dir = TempDir::new()?;
    let path = dir.path().join("run.db");
    let (store, actor) = start(&path, ActorConfig::default()).await?;
    let handle = actor.handle();
    let first = handle.submit(key("turn")?, Event::StartTurn).await?;
    let again = handle.submit(key("turn")?, Event::StartTurn).await?;
    assert!(!first.duplicate && again.duplicate);
    assert_eq!(again.state_seq, first.state_seq);
    let conflict = handle.submit(key("turn")?, Event::Pause).await;
    assert!(matches!(conflict, Err(SubmitError::KeyConflict)));
    actor.shutdown().await?;
    store.shutdown().await?;

    // Running refuses a second start; the durable record answers the retry.
    let (store, actor) = start(&path, ActorConfig::default()).await?;
    let handle = actor.handle();
    let replayed = handle.submit(key("turn")?, Event::StartTurn).await?;
    assert!(replayed.duplicate && replayed.state_seq == first.state_seq);
    handle.submit(key("done")?, Event::FinishTurn).await?;
    actor.shutdown().await?;
    store.shutdown().await?;

    // Idle accepts a start, so the retry reaches the store's key check.
    let (store, actor) = start(&path, ActorConfig::default()).await?;
    let handle = actor.handle();
    let replayed = handle.submit(key("turn")?, Event::StartTurn).await?;
    assert!(replayed.duplicate && replayed.state_seq == first.state_seq);
    assert_eq!(
        (handle.snapshot().state, handle.snapshot().state_seq.get()),
        (S::Idle, 2)
    );
    actor.shutdown().await?;
    store.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn a_full_mailbox_refuses_at_once_and_shutdown_leaves_no_task() -> TestResult {
    let dir = TempDir::new()?;
    let config = ActorConfig {
        mailbox: 1,
        ..ActorConfig::default()
    };
    let (store, actor) = start(&dir.path().join("run.db"), config).await?;
    let handle = actor.handle();
    let mut watch = handle.watch();
    // On this single-threaded runtime the actor cannot receive between the
    // two admissions, so the second finds the mailbox full.
    let (admitted, refused) = tokio::join!(
        handle.submit(key("one")?, Event::StartTurn),
        handle.submit(key("two")?, Event::Fail)
    );
    assert_eq!(admitted?.state, S::Running);
    assert!(matches!(refused, Err(SubmitError::MailboxFull)));
    actor.shutdown().await?;
    assert!(matches!(
        handle.submit(key("three")?, Event::Fail).await,
        Err(SubmitError::Closed)
    ));
    assert_eq!(watch.borrow_and_update().state, S::Running);
    assert!(
        watch.changed().await.is_err(),
        "the actor task is still alive"
    );
    store.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn a_write_whose_reply_timed_out_settles_before_anything_else() -> TestResult {
    let dir = TempDir::new()?;
    let (store, actor) = start(&dir.path().join("run.db"), ActorConfig::default()).await?;
    let handle = actor.handle();
    // With the clock paused, the idle runtime jumps to the reply deadline
    // while the writer thread is still syncing the admitted commit.
    tokio::time::pause();
    let unknown = handle.submit(key("start")?, Event::StartTurn).await;
    tokio::time::resume();
    assert!(
        matches!(unknown, Err(SubmitError::OutcomeUnknown)),
        "{unknown:?}"
    );
    let paused = handle.submit(key("pause")?, Event::Pause).await?;
    assert_eq!((paused.state, paused.state_seq.get()), (S::Paused, 2));
    tokio::time::pause();
    let unknown = handle.submit(key("resume")?, Event::Resume).await;
    tokio::time::resume();
    assert!(
        matches!(unknown, Err(SubmitError::OutcomeUnknown)),
        "{unknown:?}"
    );
    // Shutdown returns only once the store has answered that write.
    actor.shutdown().await?;
    let stored = load(&store.readers(), session()?, soon()).await?;
    assert_eq!((stored.state, stored.state_seq.get()), (S::Running, 3));
    store.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn a_second_actor_reports_a_transition_another_committed_as_a_duplicate() -> TestResult {
    let dir = TempDir::new()?;
    let (store, first) = start(&dir.path().join("run.db"), ActorConfig::default()).await?;
    let second = spawn(&store, ActorConfig::default()).await?;
    let (one, two) = (first.handle(), second.handle());
    let mut changes = two.subscribe();
    let committed = one.submit(key("turn")?, Event::StartTurn).await?;
    let repeated = two.submit(key("turn")?, Event::StartTurn).await?;
    assert!(!committed.duplicate && repeated.duplicate);
    assert_eq!(repeated.state_seq, committed.state_seq);
    assert!(matches!(changes.try_recv(), Err(TryRecvError::Empty)));
    // The acknowledged transition is already in the snapshot, also when the
    // repeat is answered from memory.
    for _ in 0..2 {
        let seen = two.snapshot();
        assert_eq!(
            (seen.state, seen.state_seq),
            (S::Running, committed.state_seq)
        );
        assert!(two.submit(key("turn")?, Event::StartTurn).await?.duplicate);
    }
    // Its snapshot reloads before the next transition.
    let failed = two.submit(key("other")?, Event::Fail).await?;
    assert_eq!((failed.state, failed.state_seq.get()), (S::Failed, 2));
    first.shutdown().await?;
    second.shutdown().await?;
    store.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn an_actor_behind_an_initialized_session_reloads_instead_of_sticking() -> TestResult {
    let dir = TempDir::new()?;
    let (store, first) = start(&dir.path().join("run.db"), ActorConfig::default()).await?;
    let second = spawn(&store, ActorConfig::default()).await?;
    first.handle().submit(key("a")?, Event::StartTurn).await?;
    let lost = second.handle().submit(key("b")?, Event::StartTurn).await;
    assert!(matches!(lost, Err(SubmitError::Contended)), "{lost:?}");
    let finished = second.handle().submit(key("c")?, Event::FinishTurn).await?;
    assert_eq!((finished.state, finished.state_seq.get()), (S::Idle, 2));
    first.shutdown().await?;
    second.shutdown().await?;
    store.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn subscriptions_close_when_the_actor_stops() -> TestResult {
    let dir = TempDir::new()?;
    let (store, actor) = start(&dir.path().join("run.db"), ActorConfig::default()).await?;
    let handle = actor.handle();
    let mut early = handle.subscribe();
    actor.shutdown().await?;
    let bound = Duration::from_secs(5);
    assert!(matches!(
        timeout(bound, early.recv()).await?,
        Err(RecvError::Closed)
    ));
    let mut late = handle.subscribe();
    assert!(matches!(
        timeout(bound, late.recv()).await?,
        Err(RecvError::Closed)
    ));
    store.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn settling_a_transition_another_actor_published_does_not_publish_it_again() -> TestResult {
    let dir = TempDir::new()?;
    let (store, first) = start(&dir.path().join("run.db"), ActorConfig::default()).await?;
    let second = spawn(&store, ActorConfig::default()).await?;
    let two = second.handle();
    let mut changes = two.subscribe();
    first
        .handle()
        .submit(key("turn")?, Event::StartTurn)
        .await?;
    tokio::time::pause();
    let unknown = two.submit(key("turn")?, Event::StartTurn).await;
    tokio::time::resume();
    assert!(
        matches!(unknown, Err(SubmitError::OutcomeUnknown)),
        "{unknown:?}"
    );
    let paused = two.submit(key("pause")?, Event::Pause).await?;
    assert_eq!((paused.state, paused.state_seq.get()), (S::Paused, 2));
    // Only the transition this actor committed is published by it.
    assert_eq!(changes.recv().await?.state_seq.get(), 2);
    assert!(matches!(changes.try_recv(), Err(TryRecvError::Empty)));
    first.shutdown().await?;
    second.shutdown().await?;
    store.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn a_replay_whose_snapshot_cannot_be_reloaded_fails_closed() -> TestResult {
    let dir = TempDir::new()?;
    let config = ActorConfig {
        read_timeout: Duration::from_millis(200),
        ..ActorConfig::default()
    };
    let (store, first) = start(&dir.path().join("run.db"), config).await?;
    let second = spawn(&store, config).await?;
    first
        .handle()
        .submit(key("turn")?, Event::StartTurn)
        .await?;
    // Hold every read slot so the second actor cannot reload.
    let (mut releases, mut holders) = (Vec::new(), Vec::new());
    let (started, mut ready) = tokio::sync::mpsc::channel(16);
    for _ in 0..16 {
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let started = started.clone();
        releases.push(release);
        let readers = store.readers();
        holders.push(tokio::spawn(async move {
            readers
                .snapshot(soon(), move |_| {
                    let _ = started.blocking_send(());
                    let _ = wait.recv();
                    Ok(())
                })
                .await
        }));
    }
    for _ in 0..16 {
        ready.recv().await.ok_or("a read slot holder stopped")?;
    }
    let two = second.handle();
    let replayed = two.submit(key("turn")?, Event::StartTurn).await;
    assert!(
        matches!(replayed, Err(SubmitError::Load(_))),
        "{replayed:?}"
    );
    assert_eq!(two.snapshot().state, S::Idle);
    assert!(matches!(
        second.shutdown().await,
        Err(SpawnError::Unsettled)
    ));
    drop(releases);
    for holder in holders {
        holder.await??;
    }
    first.shutdown().await?;
    store.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn an_unchecked_retry_without_a_keyed_record_stays_unsettled() -> TestResult {
    let dir = TempDir::new()?;
    let path = dir.path().join("run.db");
    let (store, actor) = start(&path, ActorConfig::default()).await?;
    let handle = actor.handle();
    // Another connection holds the write lock, so every writer batch aborts
    // before it reads any receipt.
    let blocker = rusqlite::Connection::open(&path)?;
    blocker.execute_batch("BEGIN IMMEDIATE")?;
    tokio::time::pause();
    let unknown = handle.submit(key("start")?, Event::StartTurn).await;
    tokio::time::resume();
    assert!(
        matches!(unknown, Err(SubmitError::OutcomeUnknown)),
        "{unknown:?}"
    );
    let blocked = handle.submit(key("fail")?, Event::Fail).await;
    assert!(
        matches!(blocked, Err(SubmitError::PriorUnsettled)),
        "{blocked:?}"
    );
    assert!(matches!(actor.shutdown().await, Err(SpawnError::Unsettled)));
    blocker.execute_batch("ROLLBACK")?;
    store.shutdown().await?;
    Ok(())
}
