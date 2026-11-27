//! Real child recovery, reservation cancellation and replacement fencing.
use super::super::*;
use super::fixtures::*;
use grapt_types::{ClockAuthority, Timestamp};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug, Default)]
struct Clock(AtomicI64);
impl ClockAuthority for Clock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0.load(Ordering::SeqCst))
    }
}

struct Fixture {
    root: PathBuf,
    clock: Arc<Clock>,
    registry: McpRegistry,
    declared: BTreeMap<String, Declaration>,
}
impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "grapt-seat04-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&root)?;
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/stateengine-native/fixture_server.py");
        assert!(script.is_file(), "native MCP fixture is absent");
        let command = vec![
            "python3".into(),
            script.to_string_lossy().into_owned(),
            "--fail-first-file".into(),
            root.join("starts").to_string_lossy().into_owned(),
        ];
        let declaration = Declaration::Local {
            command,
            cwd: ".".into(),
            environment: BTreeMap::from([(
                "SEAT04_FIXTURE_SECRET".into(),
                "fixture-private-value".into(),
            )]),
            timeouts: McpTimeouts {
                startup: Duration::from_secs(5),
                catalog: Duration::from_secs(5),
                execution: Duration::from_secs(5),
            },
        };
        let clock = Arc::new(Clock::default());
        let registry =
            McpRegistry::new(bounds(), session_bounds_for_tests(), "0.1.0", clock.clone());
        Ok(Self {
            root,
            clock,
            registry,
            declared: declared("stateengine", declaration),
        })
    }
    async fn status(&self) -> String {
        self.registry
            .with_status("fixture", &self.declared, |entries| {
                entries["stateengine"].status_label().to_owned()
            })
            .await
    }
    fn starts(&self) -> Result<usize, Box<dyn std::error::Error>> {
        Ok(std::fs::read_to_string(self.root.join("starts"))?
            .trim()
            .parse()?)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn pending<F: std::future::Future>(future: &mut std::pin::Pin<Box<F>>) {
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(
        future.as_mut().poll(&mut cx).is_pending(),
        "retry must be outside the registry lock in its backoff wait"
    );
}

#[tokio::test]
async fn seat04_failed_child_reports_redacted_stderr_then_next_refresh_recovers() -> TestResult {
    let fixture = Fixture::new()?;
    assert_ne!(fixture.status().await, "connected");
    let detail = fixture
        .registry
        .with_status("fixture", &fixture.declared, |entries| {
            entries["stateengine"].detail()
        });
    // A concurrent refresh sees the previous failure while a retry is delayed.
    let mut retry = Box::pin(detail);
    pending(&mut retry);
    let unavailable = fixture
        .registry
        .server_unavailable("fixture", "stateengine")
        .await
        .ok_or("unavailable reason")?;
    assert_eq!(
        unavailable.0,
        grapt_session::execution::StateEngineServerFailure::Disconnected
    );
    assert!(unavailable.1.contains("first startup exited"));
    assert!(unavailable.1.contains("[private]"));
    assert!(!unavailable.1.contains("fixture-private-value"));
    drop(retry);
    fixture.clock.0.store(1_000, Ordering::SeqCst);
    assert_eq!(fixture.status().await, "connected");
    assert_eq!(fixture.starts()?, 2);
    assert!(fixture
        .registry
        .server_unavailable("fixture", "stateengine")
        .await
        .is_none());
    assert_eq!(fixture.status().await, "connected");
    assert_eq!(
        fixture.starts()?,
        2,
        "healthy status reads respawned the door"
    );
    Ok(())
}

#[tokio::test]
async fn seat04_cancellation_releases_the_retry_and_concurrent_readers_coalesce() -> TestResult {
    let fixture = Fixture::new()?;
    assert_ne!(fixture.status().await, "connected");
    let mut waiting = Box::pin(fixture.status());
    pending(&mut waiting);
    assert_ne!(fixture.status().await, "connected");
    assert_eq!(
        fixture.registry.connect_attempts(),
        1,
        "reader duplicated a reserved retry"
    );
    drop(waiting);
    fixture.clock.0.store(1_000, Ordering::SeqCst);
    assert_eq!(
        fixture.status().await,
        "connected",
        "canceled retry stranded its reservation"
    );
    assert_eq!(fixture.registry.connect_attempts(), 2);
    Ok(())
}

#[tokio::test]
async fn seat04_replacement_during_backoff_never_revives_the_old_declaration() -> TestResult {
    let fixture = Fixture::new()?;
    assert_ne!(fixture.status().await, "connected");
    let mut waiting = Box::pin(fixture.status());
    pending(&mut waiting);
    let disabled = declared("stateengine", Declaration::Disabled);
    fixture
        .registry
        .with_status("fixture", &disabled, |_| ())
        .await;
    tokio::time::timeout(Duration::from_secs(5), waiting).await?;
    fixture
        .registry
        .with_status("fixture", &disabled, |entries| {
            assert_eq!(entries["stateengine"].status_label(), "disabled")
        })
        .await;
    assert_eq!(fixture.starts()?, 1);
    Ok(())
}

#[tokio::test]
async fn sc_mcp03_dead_connection_recovery_keeps_the_same_declaration_grant() -> TestResult {
    let fixture = Fixture::new()?;
    assert_ne!(fixture.status().await, "connected");
    fixture.clock.0.store(1_000, Ordering::SeqCst);
    assert_eq!(fixture.status().await, "connected");
    let approved = fixture
        .registry
        .with_status("fixture", &fixture.declared, |entries| {
            let ServerEntry::Live(live) = &entries["stateengine"] else {
                panic!("fixture connected")
            };
            live.io.observe(Unusable::Disconnected);
            live.launch().clone()
        })
        .await;
    let mut replacement = Box::pin(fixture.status());
    pending(&mut replacement);
    let state = fixture.registry.entries.lock().await;
    assert_eq!(
        state["fixture"].backoff["stateengine"].remaining(1_000),
        Duration::from_secs(1),
        "success did not reset the prior failure backoff"
    );
    drop(state);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), replacement).await?,
        "connected"
    );
    let tool = McpToolName::new("stateengine_resume").ok_or("fixture tool")?;
    let result = fixture
        .registry
        .call_with(
            "fixture",
            "stateengine",
            &tool,
            &approved,
            "call_recovered_launch",
            serde_json::json!({}),
            Duration::from_secs(5),
            None,
            None,
            Some(serde_json::json!({
                "stateengine.session-capability-v1": "NATIVE_FIXTURE_CAPABILITY"
            })),
        )
        .await;
    assert!(
        result.is_ok(),
        "SCMCP03 recovered door must accept the existing grant: {result:?}"
    );
    assert_eq!(fixture.starts()?, 3);
    Ok(())
}

#[tokio::test]
async fn seat04_multiple_failed_retries_do_not_hold_healthy_dispatch() -> TestResult {
    let fixture = Fixture::new()?;
    assert_ne!(fixture.status().await, "connected");
    fixture.clock.0.store(1_000, Ordering::SeqCst);
    assert_eq!(fixture.status().await, "connected");
    let mut declared = fixture.declared.clone();
    declared.insert("bad-one".into(), local("grapt-seat04-no-such-one"));
    declared.insert("bad-two".into(), local("grapt-seat04-no-such-two"));
    fixture
        .registry
        .with_status("fixture", &declared, |_| ())
        .await;
    let mut waiting = Box::pin(fixture.registry.with_status("fixture", &declared, |_| ()));
    pending(&mut waiting);
    let approved = fixture
        .registry
        .entries
        .lock()
        .await
        .get("fixture")
        .and_then(|held| held.entries.get("stateengine"))
        .and_then(|entry| match entry {
            ServerEntry::Live(live) => Some(live.launch().clone()),
            _ => None,
        })
        .ok_or("healthy connection")?;
    let outcome = fixture.registry.call_with("fixture", "stateengine",
        &McpToolName::new("stateengine_resume").ok_or("fixture tool")?, &approved,
        "call_healthy_while_retrying", serde_json::json!({"since":null,"byte_budget":16384}),
        Duration::from_secs(5), None, None,
        Some(serde_json::json!({"stateengine.session-capability-v1":"NATIVE_FIXTURE_CAPABILITY"}))).await;
    assert!(
        outcome.is_ok_and(|outcome| !outcome.is_error()),
        "failed startups stalled the healthy server"
    );
    drop(waiting);
    Ok(())
}

#[tokio::test]
async fn seat04_starting_location_keeps_its_slot_until_retry_is_cancelled() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.registry.retained_locations = 1;
    assert_ne!(fixture.status().await, "connected");
    let mut waiting = Box::pin(fixture.registry.with_status(
        "fixture",
        &fixture.declared,
        |entries| entries.len(),
    ));
    pending(&mut waiting);
    fixture
        .registry
        .with_status("other", &declared("off", Declaration::Disabled), |_| ())
        .await;
    assert!(fixture.registry.retains_location("fixture").await);
    assert!(!fixture.registry.retains_location("other").await);
    drop(waiting);
    fixture
        .registry
        .with_status("other", &BTreeMap::new(), |_| ())
        .await;
    assert!(!fixture.registry.retains_location("fixture").await);
    assert_eq!(fixture.registry.retained_location_count().await, 1);
    assert_eq!(fixture.starts()?, 1);
    Ok(())
}

#[tokio::test]
async fn seat04_disabled_servers_do_not_retry() {
    let registry = registry_for_tests();
    let declared = declared("disabled", Declaration::Disabled);
    for _ in 0..4 {
        registry
            .with_status("fixture", &declared, |entries| {
                assert_eq!(entries["disabled"].status_label(), "disabled")
            })
            .await;
    }
    assert_eq!(registry.connect_attempts(), 1);
}

#[tokio::test]
async fn seat04_whole_retry_round_bounds_ordinary_publication_and_releases_reservations(
) -> TestResult {
    let fixture = Fixture::new()?;
    assert_ne!(fixture.status().await, "connected");
    fixture.clock.0.store(1_000, Ordering::SeqCst);
    assert_eq!(fixture.status().await, "connected");
    let mut declarations = fixture.declared.clone();
    for name in ["bad-one", "bad-two"] {
        let mut declaration = fixture.declared["stateengine"].clone();
        let Declaration::Local {
            command, timeouts, ..
        } = &mut declaration
        else {
            panic!("local fixture")
        };
        command[3] = fixture.root.join(name).to_string_lossy().into_owned();
        command.extend(["--mode".into(), "startup-silent".into()]);
        timeouts.startup = Duration::from_secs(60);
        declarations.insert(name.into(), declaration);
    }
    fixture
        .registry
        .with_status("fixture", &declarations, |entries| {
            assert_ne!(entries["bad-one"].status_label(), "connected");
            assert_ne!(entries["bad-two"].status_label(), "connected");
        })
        .await;
    // Windows child pipes use blocking tasks, which inhibit paused-time
    // advancement. Keep this real-child test on the live runtime clock.
    // This is the refresh-and-publish path used by ordinary preparation,
    // rather than a direct dispatch bypassing failed-server recovery.
    let mut refresh = Box::pin(
        fixture
            .registry
            .with_status("fixture", &declarations, |entries| {
                tool_definitions(entries, &[])
            }),
    );
    pending(&mut refresh);
    tokio::time::sleep(Duration::from_secs(1)).await;
    pending(&mut refresh);
    tokio::time::sleep(Duration::from_secs(29)).await;
    let published = tokio::time::timeout(Duration::from_secs(1), refresh).await?;
    let route = published
        .routes
        .get("stateengine_stateengine_resume")
        .ok_or("healthy route was lost while failed servers recovered")?;
    assert_eq!(
        fixture
            .registry
            .connections_for_test()
            .load(Ordering::SeqCst),
        1,
        "round deadline retained a failed-start connection slot"
    );
    {
        let locations = fixture.registry.entries.lock().await;
        assert!(
            locations["fixture"]
                .starting
                .values()
                .all(|token| token.strong_count() == 0),
            "round deadline stranded a retry reservation"
        );
    }
    let before = fixture.registry.connect_attempts();
    fixture.clock.0.store(2_000, Ordering::SeqCst);
    let mut again = Box::pin(
        fixture
            .registry
            .with_status("fixture", &declarations, |_| ()),
    );
    pending(&mut again);
    assert_eq!(
        fixture.registry.connect_attempts(),
        before + 1,
        "the next ordinary refresh could not retry after round cancellation"
    );
    drop(again);
    assert_eq!(
        fixture
            .registry
            .connections_for_test()
            .load(Ordering::SeqCst),
        1
    );
    let outcome = fixture.registry.call_with("fixture", "stateengine", &route.tool, &route.launch,
        "call_healthy_after_bounded_refresh", serde_json::json!({"since":null,"byte_budget":16384}),
        Duration::from_secs(5), None, None,
        Some(serde_json::json!({"stateengine.session-capability-v1":"NATIVE_FIXTURE_CAPABILITY"}))).await;
    assert!(
        outcome.is_ok_and(|outcome| !outcome.is_error()),
        "healthy ordinary work did not progress after bounded preparation"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn seat04_initial_declarations_keep_their_startup_budget() -> TestResult {
    let fixture = Fixture::new()?;
    let mut declaration = fixture.declared["stateengine"].clone();
    let Declaration::Local {
        command, timeouts, ..
    } = &mut declaration
    else {
        panic!("local fixture")
    };
    command.truncate(2);
    command.extend(["--mode".into(), "startup-silent".into()]);
    timeouts.startup = Duration::from_secs(45);
    let declarations = declared("new-server", declaration);
    let mut initial = Box::pin(
        fixture
            .registry
            .with_status("fixture", &declarations, |entries| {
                entries["new-server"].status_label().to_owned()
            }),
    );
    pending(&mut initial);
    tokio::time::advance(Duration::from_secs(30)).await;
    pending(&mut initial);
    tokio::time::advance(Duration::from_secs(15)).await;
    assert_ne!(
        tokio::time::timeout(Duration::from_secs(1), initial).await?,
        "connected"
    );
    assert_eq!(fixture.registry.connect_attempts(), 1);
    assert_eq!(
        fixture
            .registry
            .connections_for_test()
            .load(Ordering::SeqCst),
        0
    );
    Ok(())
}
