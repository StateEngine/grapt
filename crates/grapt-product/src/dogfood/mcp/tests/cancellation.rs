//! Cancel real registry discovery/call futures, including queued server names.
use std::collections::BTreeMap;
use std::future::{poll_fn, Future};
use std::task::Poll;
use std::time::Duration;

use grapt_mcp::session::{CallOutcome, SessionBounds};
use serde_json::json;

use super::super::*;
use super::fixtures::{bounds, discovered_tool};

#[derive(Debug)]
struct Clock;
impl grapt_types::ClockAuthority for Clock {
    fn now(&self) -> grapt_types::Timestamp {
        grapt_types::Timestamp::from_millis(1)
    }
}

struct TestDirectory(std::path::PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "grapt-mcp-cancellation-{}-{}",
            std::process::id(),
            next_read_ordinal()
        ));
        std::fs::create_dir(&path).unwrap_or_else(|error| panic!("tempdir: {error}"));
        Self(path)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn declaration(ready: &std::path::Path) -> Declaration {
    let python = spawn::resolve_program(if cfg!(windows) { "python" } else { "python3" })
        .unwrap_or_else(|| panic!("MCP cancellation witness requires Python on PATH"));
    Declaration::Local {
        command: vec![
            python.to_string_lossy().into_owned(),
            format!(
                "{}/tests/support/mcp-cancellation-peer.py",
                env!("CARGO_MANIFEST_DIR")
            ),
            ready.to_string_lossy().into_owned(),
        ],
        cwd: env!("CARGO_MANIFEST_DIR").to_owned(),
        environment: BTreeMap::new(),
        timeouts: McpTimeouts::default(),
    }
}

#[tokio::test]
async fn repeated_discovery_interrupts_release_all_names_and_chatty_servers_answer() {
    let directory = TestDirectory::new();
    let ready = directory.path().join("ready");
    let declared: BTreeMap<_, _> = ["a", "b", "c"]
        .into_iter()
        .map(|name| (name.to_owned(), declaration(&ready)))
        .collect();
    let registry = McpRegistry::new(
        bounds(),
        SessionBounds::conservative().unwrap_or_else(|error| panic!("bounds: {error}")),
        "fixture",
        std::sync::Arc::new(Clock),
    );
    for attempt in 1..=12 {
        let mut discovery = Box::pin(registry.with_status("project", &declared, |_| ()));
        assert!(poll_fn(|cx| Poll::Ready(discovery.as_mut().poll(cx)))
            .await
            .is_pending());
        assert_eq!(registry.connect_attempts(), attempt);
        // A concurrent reader coalesces while the first still owns its starts.
        registry
            .with_status("project", &declared, |entries| assert!(entries.is_empty()))
            .await;
        assert_eq!(registry.connect_attempts(), attempt);
        drop(discovery);
        assert_eq!(
            registry
                .connections_for_test()
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }
    std::fs::write(ready, "ready").unwrap_or_else(|error| panic!("ready: {error}"));
    tokio::time::timeout(
        Duration::from_secs(15),
        registry.with_status("project", &declared, |entries| {
            assert_eq!(entries.len(), 3, "unattempted names must not stay stranded");
            for entry in entries.values() {
                assert_eq!(entry.status_label(), "connected", "{entry:?}");
            }
        }),
    )
    .await
    .unwrap_or_else(|error| panic!("chatty server startup: {error}"));
    assert_eq!(registry.connect_attempts(), 15);
}

#[tokio::test]
async fn ordinary_registry_calls_can_be_interrupted_more_than_the_in_flight_bound() {
    let directory = TestDirectory::new();
    let ready = directory.path().join("ready");
    std::fs::write(&ready, "ready").unwrap_or_else(|error| panic!("ready: {error}"));
    let declared = BTreeMap::from([("ordinary".to_owned(), declaration(&ready))]);
    let registry = McpRegistry::new(
        bounds(),
        SessionBounds::new(1, 32).unwrap_or_else(|error| panic!("bounds: {error}")),
        "fixture",
        std::sync::Arc::new(Clock),
    );
    let approved = registry
        .with_status("project", &declared, |entries| {
            match entries.get("ordinary") {
                Some(ServerEntry::Live(live)) => live.launch().clone(),
                other => panic!("chatty ordinary server: {other:?}"),
            }
        })
        .await;
    let tool = discovered_tool();
    for index in 0..12 {
        let call_id = format!("interrupted-{index}");
        let mut call = Box::pin(registry.call(
            "project",
            "ordinary",
            &tool,
            &approved,
            &call_id,
            json!({"silent":true}),
            Duration::from_secs(5),
        ));
        assert!(poll_fn(|cx| Poll::Ready(call.as_mut().poll(cx)))
            .await
            .is_pending());
        drop(call);
        let io = registry
            .io_handle_for_test("project", "ordinary")
            .await
            .unwrap_or_else(|| panic!("held ordinary connection"));
        assert_eq!(
            io.io().lock().await.session.outcome(&call_id),
            Some(CallOutcome::Disconnected)
        );
    }
    let result = registry
        .call(
            "project",
            "ordinary",
            &tool,
            &approved,
            "final",
            json!({}),
            Duration::from_secs(5),
        )
        .await
        .unwrap_or_else(|error| panic!("final call: {error:?}"));
    assert_eq!(result.text(), "current");
}

#[tokio::test]
async fn reconnect_retires_pending_startup_before_replacement() {
    let directory = TestDirectory::new();
    let ready = directory.path().join("ready");
    let declared = BTreeMap::from([("server".to_owned(), declaration(&ready))]);
    let registry = super::fixtures::registry_for_tests();
    let mut starting = Box::pin(registry.with_status("project", &declared, |_| ()));
    assert!(poll_fn(|cx| Poll::Ready(starting.as_mut().poll(cx)))
        .await
        .is_pending());
    registry
        .reconnect("project", "server", &declared)
        .await
        .unwrap_or_else(|error| panic!("reconnect: {error}"));
    tokio::time::timeout(Duration::from_secs(1), starting)
        .await
        .unwrap_or_else(|error| panic!("retired startup remained live: {error}"));
    assert_eq!(
        registry
            .connections_for_test()
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    std::fs::write(ready, "ready").unwrap_or_else(|error| panic!("ready: {error}"));
    registry
        .with_status("project", &declared, |entries| {
            assert_eq!(entries["server"].status_label(), "connected");
        })
        .await;
    assert_eq!(registry.connect_attempts(), 2);
}

#[tokio::test]
async fn superseded_start_cannot_clear_its_successors_reservation() {
    let directory = TestDirectory::new();
    let old_ready = directory.path().join("old");
    let new_ready = directory.path().join("new");
    let declared = BTreeMap::from([("server".to_owned(), declaration(&old_ready))]);
    let registry = McpRegistry::new(
        bounds(),
        SessionBounds::conservative().unwrap_or_else(|error| panic!("bounds: {error}")),
        "fixture",
        std::sync::Arc::new(Clock),
    );
    let mut old = Box::pin(registry.with_status("project", &declared, |_| ()));
    assert!(poll_fn(|cx| Poll::Ready(old.as_mut().poll(cx)))
        .await
        .is_pending());
    registry
        .register_runtime("project", "server".to_owned(), declaration(&new_ready))
        .await
        .unwrap_or_else(|error| panic!("replacement: {error}"));
    let mut successor = Box::pin(registry.with_status("project", &declared, |entries| {
        assert_eq!(entries.len(), 1);
        assert_eq!(entries["server"].status_label(), "connected");
    }));
    assert!(poll_fn(|cx| Poll::Ready(successor.as_mut().poll(cx)))
        .await
        .is_pending());
    assert_eq!(registry.connect_attempts(), 2);
    std::fs::write(&old_ready, "ready").unwrap_or_else(|error| panic!("old ready: {error}"));
    tokio::time::timeout(Duration::from_secs(5), old)
        .await
        .unwrap_or_else(|error| panic!("old startup: {error}"));
    registry
        .with_status("project", &declared, |entries| assert!(entries.is_empty()))
        .await;
    assert_eq!(
        registry.connect_attempts(),
        2,
        "old completion must not release successor's name"
    );
    std::fs::write(&new_ready, "ready").unwrap_or_else(|error| panic!("new ready: {error}"));
    tokio::time::timeout(Duration::from_secs(5), successor)
        .await
        .unwrap_or_else(|error| panic!("successor startup: {error}"));
}

#[tokio::test]
async fn unsent_private_read_and_send_reconnect_once_after_retirement() {
    let directory = TestDirectory::new();
    let ready = directory.path().join("ready");
    let log = directory.path().join("calls");
    std::fs::write(&ready, "ready").unwrap_or_else(|error| panic!("ready: {error}"));
    let mut declaration = declaration(&ready);
    let Declaration::Local { environment, .. } = &mut declaration else {
        panic!("local fixture")
    };
    environment.insert(
        "MCP_CALL_LOG".to_owned(),
        log.to_string_lossy().into_owned(),
    );
    let declared = BTreeMap::from([("stateengine".to_owned(), declaration)]);
    let registry = super::fixtures::registry_for_tests();
    let approved = registry
        .with_status("fixture", &declared, |entries| {
            match &entries["stateengine"] {
                ServerEntry::Live(live) => live.launch().clone(),
                other => panic!("local startup: {other:?}"),
            }
        })
        .await;
    let held = registry
        .io_handle_for_test("fixture", "stateengine")
        .await
        .unwrap_or_else(|| panic!("installed connection"));
    let occupied = held.io().lock().await;
    let tool = discovered_tool();
    let mut read = Box::pin(registry.call_with(
        "fixture",
        "stateengine",
        &tool,
        &approved,
        "queued-read",
        json!({}),
        Duration::from_secs(10),
        None,
        None,
        Some(json!({"private":"fixture"})),
    ));
    let mut send = Box::pin(registry.call_with(
        "fixture",
        "stateengine",
        &tool,
        &approved,
        "queued-send",
        json!({}),
        Duration::from_secs(10),
        None,
        None,
        Some(json!({"private":"fixture"})),
    ));
    for call in [&mut read, &mut send] {
        assert!(poll_fn(|cx| Poll::Ready(call.as_mut().poll(cx)))
            .await
            .is_pending());
    }
    held.retire();
    drop(occupied);
    let (read, send) = tokio::join!(read, send);
    assert!(read.is_ok(), "queued read: {read:?}");
    assert!(send.is_ok(), "queued send: {send:?}");
    assert_eq!(
        registry.connect_attempts(),
        2,
        "one shared replacement, no retry loop"
    );
    let calls =
        std::fs::read_to_string(log).unwrap_or_else(|error| panic!("dispatch log: {error}"));
    assert_eq!(
        calls.lines().count(),
        2,
        "each unsent request dispatched once"
    );
}
