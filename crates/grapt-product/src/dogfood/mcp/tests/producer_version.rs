//! Real child replacement with a controlled peer; the real producer journey is separate.
use super::super::*;
use super::fixtures::{bounds, live_launch, session_bounds_for_tests};
use grapt_types::{ClockAuthority, Timestamp};
use serde_json::json;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

type TestResult = Result<(), Box<dyn std::error::Error>>;
#[derive(Debug)]
struct Clock;
impl ClockAuthority for Clock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1000)
    }
}

struct Fixture {
    root: PathBuf,
    registry: McpRegistry,
}
impl Fixture {
    async fn new(always_refuse: bool) -> Result<Self, Box<dyn std::error::Error>> {
        Self::with_key_support(always_refuse, None).await
    }
    async fn with_key_support(
        always_refuse: bool,
        support: Option<&str>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "grapt-producer-version-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("generation"), "old")?;
        if let Some(support) = support {
            std::fs::write(root.join("key-support"), support)?;
        }
        if always_refuse {
            std::fs::write(root.join("always-refuse"), "yes")?;
        }
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/support/mcp-producer-version-peer.py");
        let declaration = Declaration::Local {
            command: vec![
                "python3".into(),
                script.to_string_lossy().into_owned(),
                root.to_string_lossy().into_owned(),
            ],
            cwd: ".".into(),
            environment: BTreeMap::new(),
            timeouts: McpTimeouts {
                startup: Duration::from_secs(5),
                catalog: Duration::from_secs(5),
                execution: Duration::from_secs(5),
            },
        };
        let registry = McpRegistry::new(
            bounds(),
            session_bounds_for_tests(),
            "0.1.0",
            Arc::new(Clock),
        );
        let declared = BTreeMap::from([("stateengine".into(), declaration)]);
        registry
            .with_status("bound-session", &declared, |entries| {
                assert_eq!(entries["stateengine"].status_label(), "connected");
            })
            .await;
        Ok(Self { root, registry })
    }
    async fn approval(&self) -> LaunchIdentity {
        let locations = self.registry.entries.lock().await;
        live_launch(&locations["bound-session"].entries["stateengine"])
    }
    async fn call(
        &self,
        approved: &LaunchIdentity,
        id: &str,
        action: &str,
        bound: bool,
    ) -> Result<ToolCallOutcome, McpCallRefusal> {
        self.call_tool(approved, id, "reader", json!({"action":action}), bound)
            .await
    }
    async fn call_tool(
        &self,
        approved: &LaunchIdentity,
        id: &str,
        tool: &str,
        arguments: serde_json::Value,
        bound: bool,
    ) -> Result<ToolCallOutcome, McpCallRefusal> {
        self.call_with_attempt(approved, id, tool, arguments, bound, None)
            .await
    }
    async fn call_with_attempt(
        &self,
        approved: &LaunchIdentity,
        id: &str,
        tool: &str,
        arguments: serde_json::Value,
        bound: bool,
        attempt: Option<&grapt_types::PhysicalAttemptId>,
    ) -> Result<ToolCallOutcome, McpCallRefusal> {
        self.registry
            .call_with(
                "bound-session",
                "stateengine",
                &grapt_mcp::McpToolName::new(tool).unwrap_or_else(|| panic!("valid fixture tool")),
                approved,
                id,
                arguments,
                Duration::from_secs(5),
                None,
                attempt,
                bound.then(
                    || json!({"stateengine.session-capability-v1":"private-fixture-binding"}),
                ),
            )
            .await
    }
    fn count(&self, file: &str) -> Result<usize, std::io::Error> {
        Ok(std::fs::read_to_string(self.root.join(file))?
            .lines()
            .count())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn producer_version_change_refreshes_child_under_same_approval_without_replaying(
) -> TestResult {
    let fixture = Fixture::new(false).await?;
    let approved = fixture.approval().await;
    assert!(fixture
        .call(&approved, "before", "read", true)
        .await
        .is_ok());
    std::fs::write(fixture.root.join("generation"), "new")?;
    assert!(matches!(
        fixture.call(&approved, "stale", "read", true).await,
        Err(McpCallRefusal::ProducerVersionChanged { recovered: true })
    ));
    assert_eq!(fixture.count("starts")?, 2);
    assert_eq!(fixture.count("calls")?, 2, "refused exchange was replayed");
    assert!(fixture.call(&approved, "after", "read", true).await.is_ok());
    assert_eq!(fixture.count("calls")?, 3);
    assert_eq!(fixture.count("starts")?, 2);
    let calls: Vec<serde_json::Value> = std::fs::read_to_string(fixture.root.join("calls"))?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert_eq!(calls[0]["pid"], calls[1]["pid"]);
    assert_ne!(calls[1]["pid"], calls[2]["pid"]);
    assert!(calls.iter().all(|call| call["binding_present"] == true));
    Ok(())
}

#[tokio::test]
async fn producer_version_text_after_an_effect_never_repeats_that_effect() -> TestResult {
    let fixture = Fixture::new(false).await?;
    let approved = fixture.approval().await;
    assert!(matches!(
        fixture
            .call(&approved, "effect", "effect-then-error", true)
            .await,
        Err(McpCallRefusal::ProducerVersionChanged { recovered: true })
    ));
    assert_eq!(fixture.count("effects")?, 1);
    assert_eq!(fixture.count("calls")?, 1);
    assert_eq!(fixture.count("starts")?, 2);
    Ok(())
}

#[tokio::test]
async fn producer_version_refusal_is_bounded_and_unbound_tools_do_not_respawn() -> TestResult {
    let fixture = Fixture::new(true).await?;
    let approved = fixture.approval().await;
    let unbound = fixture
        .call(&approved, "unbound", "read", false)
        .await
        .map_err(|error| format!("{error:?}"))?;
    assert!(unbound.is_error());
    assert_eq!(fixture.count("starts")?, 1);
    assert!(matches!(
        fixture.call(&approved, "bounded", "read", true).await,
        Err(McpCallRefusal::ProducerVersionChanged { recovered: true })
    ));
    assert_eq!(
        fixture.count("starts")?,
        2,
        "persistent refusal caused a respawn loop"
    );
    assert_eq!(fixture.count("calls")?, 2);
    Ok(())
}

#[tokio::test]
async fn producer_version_change_moves_only_unsent_queued_calls_to_one_new_child() -> TestResult {
    let fixture = Fixture::new(false).await?;
    let approved = fixture.approval().await;
    let held = fixture
        .registry
        .io_handle_for_test("bound-session", "stateengine")
        .await
        .ok_or("missing fixture connection")?;
    let occupied = held.io().lock().await;
    std::fs::write(fixture.root.join("generation"), "new")?;
    let mut stale = Box::pin(fixture.call(&approved, "stale", "read", true));
    let mut read = Box::pin(fixture.call(&approved, "queued-read", "read", true));
    let mut send = Box::pin(fixture.call(&approved, "queued-send", "send", true));
    for call in [&mut stale, &mut read, &mut send] {
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(call.as_mut().poll(&mut context).is_pending());
    }
    assert_eq!(held.waiting(), 3, "fixture did not queue all three calls");
    drop(occupied);
    let (stale, read, send) = tokio::join!(stale, read, send);
    assert!(matches!(
        stale,
        Err(McpCallRefusal::ProducerVersionChanged { recovered: true })
    ));
    assert!(read.is_ok(), "queued read: {read:?}");
    assert!(send.is_ok(), "queued send: {send:?}");
    assert_eq!(
        fixture.count("starts")?,
        2,
        "queued calls did not share recovery"
    );
    assert_eq!(
        fixture.count("calls")?,
        3,
        "an exchange was repeated or lost"
    );
    Ok(())
}

#[tokio::test]
async fn producer_version_failed_reconnect_returns_once_without_repeating_the_request() -> TestResult
{
    let fixture = Fixture::new(false).await?;
    let approved = fixture.approval().await;
    std::fs::write(fixture.root.join("generation"), "new")?;
    std::fs::write(fixture.root.join("fail-start"), "yes")?;
    assert!(matches!(
        fixture.call(&approved, "stale", "read", true).await,
        Err(McpCallRefusal::ProducerVersionChanged { recovered: false })
    ));
    assert_eq!(fixture.count("starts")?, 2);
    assert_eq!(fixture.count("calls")?, 1);
    Ok(())
}

#[tokio::test]
async fn stale_bound_reads_retry_once_without_acknowledging_or_repeating_writes() -> TestResult {
    for (tool, arguments) in [
        ("stateengine_msg_read", json!({"msg_id":"m"})),
        (
            "stateengine_msg_list",
            json!({"unread_only":true,"dismiss":false}),
        ),
        ("stateengine_resume", json!({"boot_packet":true})),
    ] {
        let fixture = Fixture::new(false).await?;
        let approved = fixture.approval().await;
        std::fs::write(fixture.root.join("generation"), "new")?;
        let result = fixture
            .call_tool(&approved, "read", tool, arguments, true)
            .await;
        assert!(
            result.is_ok_and(|reply| !reply.is_error()),
            "stale read was not retried: {tool}"
        );
        assert_eq!(
            fixture.count("starts")?,
            2,
            "read did not use one replacement"
        );
        assert_eq!(fixture.count("calls")?, 2, "read replay count changed");
        let calls: Vec<serde_json::Value> = std::fs::read_to_string(fixture.root.join("calls"))?
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        assert!(calls
            .iter()
            .all(|call| call["binding_present"] == true && call["name"] == tool));
    }
    for (tool, arguments) in [
        (
            "stateengine_resume",
            json!({"packet_id":"p","action":"effect-then-error"}),
        ),
        (
            "stateengine_msg_list",
            json!({"dismiss":true,"action":"effect-then-error"}),
        ),
        (
            "stateengine_msg_list",
            json!({"dismiss":"true","action":"effect-then-error"}),
        ),
        ("reader", json!({"action":"effect-then-error"})),
    ] {
        let fixture = Fixture::new(false).await?;
        let approved = fixture.approval().await;
        let result = fixture
            .call_tool(&approved, "write", tool, arguments, true)
            .await;
        assert!(matches!(
            result,
            Err(McpCallRefusal::ProducerVersionChanged { recovered: true })
        ));
        assert_eq!(
            fixture.count("effects")?,
            1,
            "a write was transparently repeated"
        );
        assert_eq!(fixture.count("calls")?, 1);
    }
    let fixture = Fixture::new(true).await?;
    let approved = fixture.approval().await;
    let anonymous = fixture
        .call_tool(
            &approved,
            "anonymous",
            "stateengine_resume",
            json!({}),
            false,
        )
        .await;
    assert!(anonymous.is_ok_and(|reply| reply.is_error()));
    assert_eq!(fixture.count("starts")?, 1);
    assert!(matches!(
        fixture
            .call_tool(
                &approved,
                "persistent",
                "stateengine_resume",
                json!({}),
                true
            )
            .await,
        Err(McpCallRefusal::ProducerReadRetryRefused)
    ));
    assert_eq!(
        fixture.count("starts")?,
        2,
        "persistent refusal started a third child"
    );
    assert_eq!(fixture.count("calls")?, 3);
    Ok(())
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn location_churn_keeps_a_bound_producer_and_unsent_routes_recover_safely() -> TestResult {
    let mut fixture = Fixture::new(false).await?;
    fixture.registry.retained_locations = 2;
    let session = "ses_retention_revision".parse::<grapt_types::SessionId>()?;
    let mut custody = crate::dogfood::context::SessionCapabilities::default();
    custody.put(session.clone(), "private-fixture-binding")?;
    let first_binding = custody
        .producer_bindings
        .get(&session)
        .cloned()
        .ok_or("initial binding")?;
    let approved = fixture.approval().await;
    let declared = BTreeMap::from([(
        "stateengine".to_owned(),
        approved.declaration.as_ref().clone(),
    )]);
    fixture
        .registry
        .with_continuity_status("bound-session", &declared, Some(&first_binding), |_| ())
        .await;
    custody.put(session.clone(), "private-fixture-binding")?;
    let successor = custody
        .producer_bindings
        .get(&session)
        .cloned()
        .ok_or("same-key binding")?;
    fixture
        .registry
        .with_continuity_status("bound-session", &declared, Some(&successor), |_| ())
        .await;
    fixture
        .registry
        .with_continuity_status("bound-session", &declared, Some(&first_binding), |_| ())
        .await;
    for index in 0..9 {
        fixture
            .registry
            .with_status(&format!("subagent-{index}"), &BTreeMap::new(), |_| ())
            .await;
    }
    assert!(
        fixture.registry.retains_location("bound-session").await,
        "location churn evicted the bound producer"
    );
    assert!(fixture
        .call(&approved, "after-churn", "read", true)
        .await
        .is_ok());
    assert_eq!(
        fixture.count("starts")?,
        1,
        "parent producer restarted during churn"
    );
    assert_eq!(fixture.registry.retained_location_count().await, 2);
    let active = fixture
        .registry
        .hold_location("active-turn")
        .await
        .ok_or("active location")?;
    fixture
        .registry
        .with_status("over-capacity", &BTreeMap::new(), |_| ())
        .await;
    assert!(!fixture.registry.retains_location("over-capacity").await);
    assert!(fixture.registry.retains_location("active-turn").await);
    drop(active);
    fixture
        .registry
        .with_status("after-turn", &BTreeMap::new(), |_| ())
        .await;
    assert!(!fixture.registry.retains_location("active-turn").await);

    // Real capability replacement must release superseded revision pins,
    // while an old execution can retain its own key until its drain ends.
    let old_drain = fixture
        .registry
        .hold_location("bound-session")
        .await
        .ok_or("old drain")?;
    for revision in 1..=8 {
        custody.put(session.clone(), "private-fixture-binding")?;
        let key =
            crate::dogfood::session_config::key_for_revision(&session, "fixture", Some(revision));
        let producer = custody
            .producer_bindings
            .get(&session)
            .ok_or("current binding")?;
        fixture
            .registry
            .with_continuity_status(&key, &declared, Some(producer), |entries| {
                assert_eq!(
                    entries["stateengine"].status_label(),
                    "connected",
                    "superseded revision exhausted retention"
                );
            })
            .await;
        if revision == 1 {
            assert!(fixture.registry.retains_location("bound-session").await);
        }
        if revision > 1 {
            assert!(fixture.registry.retained_location_count().await <= 2);
        }
    }
    drop(old_drain);
    fixture
        .registry
        .with_status("release-parent", &BTreeMap::new(), |_| ())
        .await;
    assert!(!fixture.registry.retains_location("bound-session").await);
    let before_recovery = fixture.count("starts")?;
    let human_paused = fixture
        .registry
        .readmit_missing(
            "bound-session",
            "bound-session",
            "stateengine",
            &approved,
            &declared,
            Duration::from_secs(5),
        )
        .await
        .map_err(|error| format!("re-admission: {error:?}"))?;
    assert_eq!(human_paused, Duration::ZERO);
    assert!(fixture
        .call(&approved, "re-admitted", "read", true)
        .await
        .is_ok());
    assert_eq!(fixture.count("starts")?, before_recovery + 1);
    assert_eq!(fixture.count("calls")?, 2);
    let disabled = BTreeMap::from([("stateengine".to_owned(), Declaration::Disabled)]);
    let revoked = fixture
        .registry
        .readmit_missing(
            "bound-session",
            "bound-session",
            "stateengine",
            &approved,
            &disabled,
            Duration::from_secs(5),
        )
        .await;
    if revoked.is_ok() {
        let _ = fixture
            .call(&approved, "revoked-effect", "effect-then-error", true)
            .await;
    }
    assert!(
        !fixture.root.join("effects").try_exists()?,
        "re-admission resurrected a revoked effect"
    );
    assert!(matches!(
        revoked,
        Err(McpCallRefusal::ConnectionReplaced { .. })
    ));
    assert!(matches!(
        fixture
            .registry
            .readmit_missing(
                "bound-session",
                "bound-session",
                "stateengine",
                &approved,
                &declared,
                Duration::ZERO
            )
            .await,
        Err(McpCallRefusal::TimedOut)
    ));
    assert_eq!(
        fixture.count("starts")?,
        before_recovery + 1,
        "refused recovery spawned a child"
    );
    assert_eq!(fixture.count("calls")?, 2, "recovery replayed an exchange");
    custody.retire_producer(&session);
    fixture
        .registry
        .with_status("retired-session", &BTreeMap::new(), |_| ())
        .await;
    let last = crate::dogfood::session_config::key_for_revision(&session, "fixture", Some(8));
    assert!(
        !fixture.registry.retains_location(&last).await,
        "retired binding retained its producer"
    );
    Ok(())
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn a_keyed_send_replays_once_only_with_advertised_support_and_retains_its_receipt(
) -> TestResult {
    for (support, bound, supplied, attempted, always_refuse, expected_calls) in [
        (Some("all"), true, None, true, false, 2),
        (Some("all"), true, Some("caller-key"), true, false, 2),
        (None, true, None, true, false, 1),
        (Some("all"), false, None, true, false, 1),
        (Some("all"), true, Some("msg_id"), true, false, 1),
        (Some("all"), true, None, false, false, 1),
        (Some("old"), true, None, true, false, 1),
        (Some("all"), true, None, true, true, 2),
    ] {
        let fixture = Fixture::with_key_support(always_refuse, support).await?;
        let approved = fixture.approval().await;
        std::fs::write(fixture.root.join("generation"), "new")?;
        let attempt = grapt_types::PhysicalAttemptId::new("attempt-keyed-send")
            .ok_or("invalid fixture attempt")?;
        let mut arguments = json!({"action":"effect-then-error", "body":"send once"});
        match supplied {
            Some("msg_id") => arguments["msg_id"] = json!("existing-message"),
            Some(key) => arguments["idempotency_key"] = json!(key),
            None => {}
        }
        let outcome = fixture
            .call_with_attempt(
                &approved,
                "send",
                "stateengine_msg_send",
                arguments.clone(),
                bound,
                attempted.then_some(&attempt),
            )
            .await;
        let calls: Vec<serde_json::Value> = std::fs::read_to_string(fixture.root.join("calls"))?
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        assert_eq!(
            calls.len(),
            expected_calls,
            "support={support:?}, supplied={supplied:?}, bound={bound}, attempted={attempted}"
        );
        assert_eq!(fixture.count("effects")?, 1, "send effect repeated");
        if expected_calls == 2 {
            assert_eq!(
                calls[0]["arguments"], calls[1]["arguments"],
                "replay changed payload or key"
            );
            assert!(
                calls[0]["arguments"]["idempotency_key"]
                    .as_str()
                    .is_some_and(|key| !key.is_empty()),
                "first send had no key"
            );
            assert_eq!(
                fixture.count("starts")?,
                2,
                "recovery exceeded one reconnect"
            );
            if let Some(key) = supplied {
                assert_eq!(calls[0]["arguments"]["idempotency_key"], key);
            }
            if always_refuse {
                assert!(
                    matches!(outcome, Err(McpCallRefusal::ProducerWriteRetryRefused)),
                    "exhausted send lost its recovery cause"
                );
            } else {
                let result = outcome.map_err(|e| format!("{e:?}"))?;
                assert!(!result.is_error());
                assert_eq!(result.text(), "durable-message-1");
                if supplied.is_some() {
                    let mut conflicting = arguments;
                    conflicting["body"] = json!("different payload");
                    let conflict = fixture
                        .call_with_attempt(
                            &approved,
                            "conflict",
                            "stateengine_msg_send",
                            conflicting,
                            true,
                            Some(&attempt),
                        )
                        .await
                        .map_err(|e| format!("{e:?}"))?;
                    assert!(conflict.is_error());
                    assert!(conflict.text().contains("durable-message-1"));
                    assert_eq!(fixture.count("effects")?, 1);
                }
            }
        } else {
            if support != Some("old") {
                assert!(
                    calls[0]["arguments"].get("idempotency_key").is_none(),
                    "unsupported send gained a key"
                );
            }
            if bound {
                assert!(matches!(
                    outcome,
                    Err(McpCallRefusal::ProducerVersionChanged { recovered: true })
                ));
            } else {
                assert!(outcome.map_err(|e| format!("{e:?}"))?.is_error());
            }
        }
    }
    Ok(())
}
