use grapt_run_store::RunStore;
use grapt_session_actor::{
    load, ActorConfig, Event, SessionActor, SessionRuntimeState as S, SubmitError, SubmitKey,
    SCHEMA,
};
use grapt_types::SessionId;
use std::error::Error;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::Instant;

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
    let actor = SessionActor::spawn(store.writer(), store.readers(), session()?, config).await?;
    Ok((store, actor))
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
