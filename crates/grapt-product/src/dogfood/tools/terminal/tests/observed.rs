use super::*;
use crate::commit_fault;
use grapt_event::session_terminal::TerminalState;
use grapt_types::DeterministicAuthorities;

type TestResult = Result<(), Box<dyn std::error::Error>>;
const TEST: &str = "dogfood::tools::terminal::tests::observed::completion_survives_eviction_and_owner_loss_without_replaying_commands";
const CRASH_PATH: &str = "GRAPT_TERMINAL_OWNER_LOSS_DATABASE";
const CRASH_SESSION: &str = "GRAPT_TERMINAL_OWNER_LOSS_SESSION";

fn crash_after_admission() -> TestResult {
    let Some(path) = std::env::var_os(CRASH_PATH) else {
        return Ok(());
    };
    let owner = session(&std::env::var(CRASH_SESSION)?)?;
    let mut store = grapt_store::EventStore::open(Path::new(&path), "terminal-owner-loss")?;
    let ids = OsRandomIds::seed()?;
    let clock = SystemClock;
    let fact = grapt_event::session_terminal::TerminalFact::new(
        owner,
        operation("op_retained_owner_loss")?,
        TerminalState::Starting,
        None,
    )?;
    grapt_session::terminal::record(
        &mut store,
        &DeterministicAuthorities::new(&clock, &ids),
        &fact,
    )?;
    // Leave without running store, registry or runtime destructors. No command
    // may be recreated from this durable admission after its owner disappears.
    std::process::exit(86);
}

async fn settled(entry: &Entry) -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !entry.evictable() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| "terminal owner did not settle".into())
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn completion_survives_eviction_and_owner_loss_without_replaying_commands() -> TestResult {
    crash_after_admission()?;
    let scratch = super::super::super::tests::Scratch::new()?;
    let path = scratch.0.join("terminal.sqlite3");
    let ids = Arc::new(OsRandomIds::seed()?);
    let clock = Arc::new(SystemClock);
    let inbox = Arc::new(Mutex::new(Inbox::new(grapt_store::EventStore::open(
        &path,
        "terminal-retention",
    )?)?));
    let authorities = DeterministicAuthorities::new(&*clock, &*ids);
    let seed = grapt_session::SessionSeed::new(
        grapt_types::ProjectId::new("prj_terminal_retention").ok_or("project")?,
        scratch.0.to_string_lossy(),
        "terminal retention",
    )?;
    let owner =
        grapt_session::create_session(lock(&inbox).store_mut(), &authorities, &seed, &mut [])?
            .session;
    let registry =
        TerminalRegistry::with_history(Arc::clone(&inbox), Arc::clone(&ids), Arc::clone(&clock));
    let fail_after = |allowed: usize| {
        commit_fault::CommitFault::install(
            lock(&inbox).store_mut(),
            rusqlite::hooks::Action::SQLITE_INSERT,
            "event",
            allowed,
        )
    };
    let first = operation("op_retained_exit")?;
    let command = if cfg!(windows) {
        "echo ran>>marker&exit /b 17"
    } else {
        "printf 'ran\n' >> marker; exit 17"
    };
    let mut fault = fail_after(0)?;
    let refused = registry
        .exec(
            &owner,
            operation("op_admission_refused")?,
            spec(&scratch.0, command, false)?,
        )
        .await;
    assert!(
        !scratch.0.join("marker").exists(),
        "a command ran before its admission was saved"
    );
    assert!(refused.is_err(), "failed admission was accepted");
    drop(refused);
    drop(fault);
    fault = fail_after(1)?;
    let no_start = operation("op_no_start_end_failed")?;
    let mut denied = spec(&scratch.0, command, false)?;
    denied.program = scratch.0.clone();
    let error = registry
        .exec(&owner, no_start.clone(), denied)
        .await
        .err()
        .ok_or("directory was accepted as a program")?;
    let receipt = sandbox::receipt(&error.detail)
        .ok_or("terminal persistence failure corrupted the execution receipt")?;
    assert_eq!(receipt["ran"], false, "{}", error.detail);
    let cause = receipt["error"].as_str().ok_or("failure receipt cause")?;
    assert!(cause.contains("terminal end could not be saved"), "{cause}");
    assert!(cause.contains(no_start.as_str()), "{cause}");
    assert!(
        error.detail.contains("terminal end could not be saved"),
        "{}",
        error.detail
    );
    assert!(error.detail.contains(no_start.as_str()), "{}", error.detail);
    drop(fault);
    fault = fail_after(1)?;
    let (bytes, delivery) = registry
        .exec(&owner, first.clone(), spec(&scratch.0, command, false)?)
        .await
        .map_err(|e| e.detail)?;
    delivery.captured();
    drop(bytes);
    let entry = registry.lookup(&owner, &first)?;
    tokio::time::timeout(Duration::from_secs(10), async {
        while lock(&entry.observation).persistence_error.is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    assert!(
        !entry.evictable(),
        "an unsaved process end released its handle"
    );
    assert_eq!(
        grapt_session::terminal::latest(lock(&inbox).store(), &owner, &first)?
            .ok_or("admission")?
            .state(),
        &TerminalState::Starting
    );
    drop(fault);
    let (bytes, delivery) = registry
        .stdin(
            &owner,
            &StdinSpec {
                operation: first.clone(),
                chars: String::new(),
                wait_ms: 5_000,
                page_bytes: 40_000,
                cursor: None,
                line: None,
            },
        )
        .await
        .map_err(|error| error.detail)?;
    delivery.captured();
    drop(bytes);
    settled(&entry).await?;
    assert!(entry.evictable());
    let fact = grapt_session::terminal::latest(lock(&inbox).store(), &owner, &first)?
        .ok_or("completion was not saved by polling after storage recovered")?;
    assert_eq!(fact.state(), &TerminalState::Exited { code: 17 });
    drop(entry);
    for index in 0..SESSION_RESOURCES {
        let id = operation(&format!("op_retained_successor_{index}"))?;
        let (bytes, delivery) = registry
            .exec(&owner, id.clone(), spec(&scratch.0, "exit 0", false)?)
            .await
            .map_err(|e| e.detail)?;
        delivery.captured();
        drop(bytes);
        let entry = registry.lookup(&owner, &id)?;
        settled(&entry).await?;
    }
    assert!(
        matches!(
            grapt_session::terminal::latest(lock(&inbox).store(), &owner, &no_start)?
                .ok_or("failed start admission")?
                .state(),
            TerminalState::NotStarted { .. }
        ),
        "no-start outcome was not saved after storage recovered"
    );
    assert!(!registry.contains(&owner, &first));
    let replay = registry
        .exec(&owner, first.clone(), spec(&scratch.0, command, false)?)
        .await;
    assert!(
        replay.is_err(),
        "a completed command was restarted after eviction"
    );
    drop(replay);
    let error = registry
        .lookup(&owner, &first)
        .err()
        .ok_or("evicted handle was resurrected")?;
    assert!(error.contains("exited with status 17"), "{error}");
    let foreign = session("ses_foreign_retention")?;
    let error = registry
        .lookup(&foreign, &first)
        .err()
        .ok_or("foreign terminal was exposed")?;
    assert!(!error.contains("status 17"), "{error}");
    let lost = operation("op_retained_owner_loss")?;
    drop(registry);
    let weak = Arc::downgrade(&inbox);
    drop(inbox);
    tokio::time::timeout(Duration::from_secs(5), async {
        while weak.strong_count() != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let status = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(std::env::current_exe()?)
            .args(["--exact", TEST, "--nocapture"])
            .env(CRASH_PATH, &path)
            .env(CRASH_SESSION, owner.as_str())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .status(),
    )
    .await??;
    assert_eq!(
        status.code(),
        Some(86),
        "owner did not exit after durable admission"
    );
    let reopened = Arc::new(Mutex::new(Inbox::new(grapt_store::EventStore::open(
        &path,
        "terminal-retention",
    )?)?));
    let registry = TerminalRegistry::with_history(Arc::clone(&reopened), ids, clock);
    let error = registry
        .lookup(&owner, &first)
        .err()
        .ok_or("reopen recreated an old handle")?;
    assert!(error.contains("exited with status 17"), "{error}");
    let error = registry
        .lookup(&owner, &lost)
        .err()
        .ok_or("lost owner recreated a command")?;
    assert!(error.contains("handle was lost"), "{error}");
    assert!(matches!(
        grapt_session::terminal::latest(lock(&reopened).store(), &owner, &lost)?
            .ok_or("lost fact")?
            .state(),
        TerminalState::Lost { .. }
    ));
    assert_eq!(
        std::fs::read_to_string(scratch.0.join("marker"))?
            .lines()
            .collect::<Vec<_>>(),
        ["ran"]
    );
    Ok(())
}
