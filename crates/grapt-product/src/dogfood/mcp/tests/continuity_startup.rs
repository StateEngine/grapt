//! A required first-turn catalog survives one slow StateEngine handshake.
use super::super::*;
use super::fixtures::bounds;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[derive(Debug)]
struct Clock;
impl grapt_types::ClockAuthority for Clock {
    fn now(&self) -> grapt_types::Timestamp {
        grapt_types::Timestamp::from_millis(1)
    }
}
struct Fixture {
    root: PathBuf,
    registry: McpRegistry,
    declared: BTreeMap<String, Declaration>,
}
impl Fixture {
    fn new(name: &str, mode: &str) -> Result<Self, Box<dyn std::error::Error>> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "grapt-required-startup-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root)?;
        let python = spawn::resolve_program(if cfg!(windows) { "python" } else { "python3" })
            .ok_or("Python fixture missing")?;
        let declaration = Declaration::Local {
            command: vec![
                python.to_string_lossy().into_owned(),
                format!(
                    "{}/tests/support/mcp-required-startup-peer.py",
                    env!("CARGO_MANIFEST_DIR")
                ),
                root.join("started").to_string_lossy().into_owned(),
                mode.to_owned(),
            ],
            cwd: env!("CARGO_MANIFEST_DIR").into(),
            environment: BTreeMap::new(),
            timeouts: McpTimeouts {
                startup: Duration::from_secs(2),
                catalog: Duration::from_secs(5),
                execution: Duration::from_secs(5),
            },
        };
        Ok(Self {
            root,
            registry: McpRegistry::new(
                bounds(),
                SessionBounds::conservative()?,
                "fixture",
                Arc::new(Clock),
            ),
            declared: BTreeMap::from([(name.to_owned(), declaration)]),
        })
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn required_first_turn_retries_one_timed_out_stateengine_handshake(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new("stateengine", "first-handshake")?;
    fixture
        .registry
        .with_continuity_status("project", &fixture.declared, None, |entries| {
            assert_eq!(
                entries["stateengine"].status_label(),
                "connected",
                "the first admitted turn must recover without another user input"
            );
            let published = tool_definitions(entries, &[]);
            assert!(published
                .routes
                .values()
                .any(|route| { route.tool.as_str() == "stateengine_resume" }));
        })
        .await;
    assert_eq!(fixture.registry.connect_attempts(), 2);
    Ok(())
}

#[tokio::test]
async fn optional_stateengine_does_not_retry_its_first_handshake(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new("stateengine", "first-handshake")?;
    fixture
        .registry
        .with_status("project", &fixture.declared, |entries| {
            assert!(matches!(
                &entries["stateengine"],
                ServerEntry::Unusable(
                    grapt_mcp::session::Unusable::TimedOut,
                    Some(StartPhase::Handshake),
                    _
                )
            ));
        })
        .await;
    assert_eq!(fixture.registry.connect_attempts(), 1);
    Ok(())
}

#[tokio::test]
async fn required_continuity_does_not_retry_an_unrelated_server(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new("other", "first-handshake")?;
    fixture
        .registry
        .with_continuity_status("project", &fixture.declared, None, |entries| {
            assert!(matches!(
                &entries["other"],
                ServerEntry::Unusable(
                    grapt_mcp::session::Unusable::TimedOut,
                    Some(StartPhase::Handshake),
                    _
                )
            ));
        })
        .await;
    assert_eq!(fixture.registry.connect_attempts(), 1);
    Ok(())
}

#[tokio::test]
async fn required_first_turn_keeps_a_persistent_timeout_visible_after_one_retry(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new("stateengine", "all-handshakes")?;
    fixture
        .registry
        .with_continuity_status("project", &fixture.declared, None, |entries| {
            assert!(matches!(
                &entries["stateengine"],
                ServerEntry::Unusable(
                    grapt_mcp::session::Unusable::TimedOut,
                    Some(StartPhase::Handshake),
                    _
                )
            ));
            assert!(tool_definitions(entries, &[]).routes.is_empty());
        })
        .await;
    assert_eq!(fixture.registry.connect_attempts(), 2);
    Ok(())
}

#[tokio::test]
async fn required_first_turn_does_not_replay_discovery_failures(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new("stateengine", "first-discovery")?;
    fixture
        .registry
        .with_continuity_status("project", &fixture.declared, None, |entries| {
            assert!(matches!(
                &entries["stateengine"],
                ServerEntry::Unusable(
                    grapt_mcp::session::Unusable::TimedOut,
                    Some(StartPhase::Discovery),
                    _
                )
            ));
        })
        .await;
    assert_eq!(fixture.registry.connect_attempts(), 1);
    Ok(())
}

#[tokio::test]
async fn cancelling_required_startup_allows_a_fresh_successor(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new("stateengine", "first-handshake")?;
    let mut starting = Box::pin(fixture.registry.with_continuity_status(
        "project",
        &fixture.declared,
        None,
        |_| (),
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                () = &mut starting => panic!("the silent child unexpectedly finished"),
                () = tokio::time::sleep(Duration::from_millis(10)) => {},
            }
            if fixture.root.join("started").exists() {
                break;
            }
        }
    })
    .await?;
    drop(starting);
    fixture
        .registry
        .with_continuity_status("project", &fixture.declared, None, |entries| {
            assert_eq!(entries["stateengine"].status_label(), "connected");
        })
        .await;
    assert_eq!(fixture.registry.connect_attempts(), 2);
    Ok(())
}
