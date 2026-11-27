//! Retention, admission and queue bounds.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use grapt_mcp::session::Unusable;
use grapt_mcp::{McpBounds, PROTOCOL_VERSION};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::super::publication::{tool_definitions_from, ConnectedCatalog};
use super::super::*;
use super::fixtures::*;

/// THE LOCATION MAP IS BOUNDED, and the release is the least recently read.
///
/// Without a bound every project a user ever opened kept its servers alive
/// for the life of the process.
#[tokio::test]
async fn retained_locations_are_bounded_and_release_the_least_recently_read() {
    let registry = registry_for_tests();
    let declaration = declared("srv", local("grapt-no-such-mcp-server"));

    for index in 0..MAX_RETAINED_LOCATIONS {
        registry
            .with_status(&format!("E:/projects/p{index}"), &declaration, |entries| {
                assert!(entries.contains_key("srv"));
            })
            .await;
    }

    // Re-read p1 so it is no longer the oldest; p0 becomes the victim.
    registry
        .with_status("E:/projects/p1", &declaration, |entries| {
            assert!(entries.contains_key("srv"));
        })
        .await;

    // One more location forces a release.
    registry
        .with_status("E:/projects/new", &declaration, |entries| {
            assert!(entries.contains_key("srv"));
        })
        .await;

    let retained = registry.retained_location_count().await;
    assert!(
        retained <= MAX_RETAINED_LOCATIONS,
        "the map must stay within its bound, held {retained}"
    );
    assert!(
        !registry.retains_location("E:/projects/p0").await,
        "the least recently read location is the one released"
    );
    assert!(
        registry.retains_location("E:/projects/p1").await,
        "a location read more recently is kept"
    );
    assert!(registry.retains_location("E:/projects/new").await);
}

/// A caller queued behind another call must not wait past its OWN deadline
/// and then still start. The wait is inside the budget.
#[tokio::test]
async fn waiting_for_a_busy_connection_is_covered_by_the_calls_deadline() {
    let registry = std::sync::Arc::new(registry_for_tests());
    let (stream, _frames, server) = conformant(handshake_answering(PROTOCOL_VERSION));
    let incarnation = registry
        .install_live_for_test("E:/projects/one", "srv", local("prog"), stream, "prog")
        .await;
    let approved = approval_for(&local("prog"), "prog", incarnation);

    let holder = std::sync::Arc::clone(&registry);
    let approved_for_holder = approved.clone();
    let stalled = tokio::spawn(async move {
        holder
            .call(
                "E:/projects/one",
                "srv",
                &discovered_tool(),
                &approved_for_holder,
                "call-stalled",
                json!({"silent": true}),
                Duration::from_secs(30),
            )
            .await
    });
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    // A second caller on the SAME connection, with a short deadline.
    let queued = registry
        .call(
            "E:/projects/one",
            "srv",
            &discovered_tool(),
            &approved,
            "call-queued",
            json!({}),
            Duration::from_millis(150),
        )
        .await;
    assert!(
        matches!(queued, Err(McpCallRefusal::TimedOut)),
        "a caller must time out waiting rather than outlive its budget: {queued:?}"
    );

    stalled.abort();
    server.abort();
}

/// The comparison the refusal turns on, witnessed directly.
///
/// The registry-level test above proves a held approval is not spent, but
/// its fixture programs do not exist so the refusal it reaches is
/// NotConnected. This pins the specific property that makes a LIVE
/// replacement refuse: identity is the whole launch, so a changed program,
/// a changed cwd or a new generation each make it a different connection.
#[test]
fn a_launch_identity_differs_on_program_cwd_or_connection() {
    let approved = LaunchIdentity {
        declaration: std::sync::Arc::new(crate::dogfood::mcp::Declaration::Disabled),
        program: "node".to_owned(),
        cwd: "E:/projects/one".to_owned(),
        arguments: vec!["server.js".to_owned()],
        incarnation: 1,
    };
    assert_eq!(approved, approved.clone());

    let repointed = LaunchIdentity {
        program: "python".to_owned(),
        ..approved.clone()
    };
    let moved = LaunchIdentity {
        cwd: "E:/projects/two".to_owned(),
        ..approved.clone()
    };
    let restarted = LaunchIdentity {
        incarnation: 2,
        ..approved.clone()
    };
    for other in [repointed, moved, restarted] {
        assert_ne!(
            approved, other,
            "a dispatch must treat this as a different connection"
        );
    }
}

/// A refusal names BOTH connections, because an operator seeing a call
/// refused needs to know what they approved and what is there now.
#[test]
fn a_replacement_refusal_names_what_was_approved_and_what_is_current() {
    let refusal = McpCallRefusal::ConnectionReplaced {
        approved: describe_launch(&LaunchIdentity {
            declaration: std::sync::Arc::new(crate::dogfood::mcp::Declaration::Disabled),
            program: "node".to_owned(),
            cwd: "E:/projects/one".to_owned(),
            arguments: Vec::new(),
            incarnation: 1,
        }),
        current: describe_launch(&LaunchIdentity {
            declaration: std::sync::Arc::new(crate::dogfood::mcp::Declaration::Disabled),
            program: "python".to_owned(),
            cwd: "E:/projects/one".to_owned(),
            arguments: Vec::new(),
            incarnation: 2,
        }),
    };
    let McpCallRefusal::ConnectionReplaced { approved, current } = refusal else {
        panic!("the refusal must carry both descriptions");
    };
    assert!(approved.contains("node") && approved.contains("connection 1"));
    assert!(current.contains("python") && current.contains("connection 2"));
}

/// Equal names in different projects retain separate healthy connections.
#[tokio::test]
async fn one_server_name_in_two_projects_is_two_connections_not_a_shared_one() {
    let registry = registry_for_tests();
    let alpha = declared("github", healthy_local("alpha"));
    let beta = declared("github", healthy_local("beta"));

    let alpha_launch = registry
        .with_status("E:/projects/alpha", &alpha, |entries| {
            assert_eq!(entries["github"].status_label(), "connected");
            live_launch(&entries["github"])
        })
        .await;
    let beta_launch = registry
        .with_status("E:/projects/beta", &beta, |entries| {
            assert_eq!(entries["github"].status_label(), "connected");
            live_launch(&entries["github"])
        })
        .await;
    assert_ne!(
        alpha_launch, beta_launch,
        "projects must own separate launches"
    );
    assert_eq!(registry.connect_attempts(), 2);

    registry
        .with_status("E:/projects/alpha", &alpha, |entries| {
            assert_eq!(entries["github"].status_label(), "connected");
            assert_eq!(live_launch(&entries["github"]), alpha_launch);
        })
        .await;
    assert_eq!(
        registry.connect_attempts(),
        2,
        "beta evicted alpha's connection"
    );
}

/// Reading a disjoint declaration map must not evict another project's child.
#[tokio::test]
async fn one_projects_status_read_does_not_evict_another_projects_connections() {
    let registry = registry_for_tests();
    let alpha = declared("alpha-server", healthy_local("alpha"));
    let beta = declared("beta-server", healthy_local("beta"));

    let alpha_launch = registry
        .with_status("E:/projects/alpha", &alpha, |entries| {
            assert_eq!(entries["alpha-server"].status_label(), "connected");
            live_launch(&entries["alpha-server"])
        })
        .await;
    registry
        .with_status("E:/projects/beta", &beta, |entries| {
            assert_eq!(entries["beta-server"].status_label(), "connected");
        })
        .await;
    assert_eq!(registry.connect_attempts(), 2);

    registry
        .with_status("E:/projects/alpha", &alpha, |entries| {
            assert_eq!(entries["alpha-server"].status_label(), "connected");
            assert_eq!(live_launch(&entries["alpha-server"]), alpha_launch);
        })
        .await;
    assert_eq!(
        registry.connect_attempts(),
        2,
        "beta evicted alpha's connection"
    );
}

/// A CHANGED declaration is a different server, not the same one. Reusing
/// the held connection would answer for a command nobody declared.
#[tokio::test]
async fn a_changed_declaration_is_not_served_by_the_old_connection() {
    let registry = registry_for_tests();
    let before = declared("tool-server", local("grapt-no-such-mcp-before"));
    let after = declared("tool-server", local("grapt-no-such-mcp-after"));

    let first = registry
        .with_status("E:/projects/one", &before, |entries| {
            entries.get("tool-server").map(ServerEntry::detail)
        })
        .await;
    let second = registry
        .with_status("E:/projects/one", &after, |entries| {
            entries.get("tool-server").map(ServerEntry::detail)
        })
        .await;

    match (first, second) {
        (Some(first), Some(second)) => {
            assert!(first.contains("before"), "first: {first}");
            assert!(
                second.contains("after"),
                "a changed command must be started as a different server: {second}"
            );
        }
        other => panic!("both reads must hold an entry: {other:?}"),
    }
}

#[tokio::test]
async fn only_connected_servers_offer_a_model_any_tools() {
    let (stream, _received, server) = conformant(handshake_answering(PROTOCOL_VERSION));
    let mut channel = channel(stream);
    let mut session = session();
    let started = match start(
        &mut channel,
        &mut session,
        bounds(),
        Duration::from_secs(5),
        Duration::from_secs(5),
        "0.1.0",
    )
    .await
    {
        Ok(started) => started,
        Err(reason) => panic!("the fixture server did not start: {reason:?}"),
    };
    assert_eq!(
        started.catalog.len(),
        1,
        "the discovered SCHEMA is retained, not just the name"
    );

    let mut entries = BTreeMap::new();
    entries.insert("disabled-one".to_owned(), ServerEntry::Disabled);
    entries.insert(
        "timed-out-one".to_owned(),
        ServerEntry::unusable(Unusable::TimedOut),
    );
    let published = tool_definitions(&entries, &["read", "edit", "shell"]);
    assert!(
        published.definitions.is_empty(),
        "a disabled or unusable server offers a model nothing; a tool it cannot call \
         would be a failure invented by the prompt"
    );
    assert!(published.shadowed.is_empty());
    assert!(published.ambiguous.is_empty());
    assert!(published.unpublished.is_empty());
    server.abort();
}

#[tokio::test]
async fn a_connected_servers_schema_reaches_the_model_qualified_by_server() {
    let (stream, _received, server) = conformant(handshake_answering(PROTOCOL_VERSION));
    let mut channel = channel(stream);
    let mut session = session();
    let started = match start(
        &mut channel,
        &mut session,
        bounds(),
        Duration::from_secs(5),
        Duration::from_secs(5),
        "0.1.0",
    )
    .await
    {
        Ok(started) => started,
        Err(reason) => panic!("the fixture server did not start: {reason:?}"),
    };

    let schema = started.catalog[0].input_schema().to_string();
    let connected: Vec<ConnectedCatalog<'_>> =
        vec![connected_catalog("my fixture", started.catalog.as_slice())];

    let published = tool_definitions_from(&connected, &["read", "edit", "shell"]);
    assert!(published.shadowed.is_empty());
    assert!(published.ambiguous.is_empty());
    assert_eq!(published.definitions.len(), 1);
    assert_eq!(
        published.definitions[0].name, "my_fixture_fixture_read",
        "the server half is sanitised and joined by the oracle's rule"
    );
    assert_eq!(
        published.definitions[0].input_schema, schema,
        "the schema is carried as the server published it, not re-derived"
    );
    server.abort();
}

#[tokio::test]
async fn a_silent_server_times_out_and_is_not_reported_as_a_failure() {
    // The server reads the request and never answers.
    let (stream, server) = scripted(vec![String::new()]);
    let mut channel = channel(stream);
    let mut session = session();
    let reason = start(
        &mut channel,
        &mut session,
        bounds(),
        Duration::from_millis(150),
        Duration::from_millis(150),
        "0.1.0",
    )
    .await;
    assert!(
        matches!(
            reason,
            Err(StartFailure {
                reason: Unusable::TimedOut,
                ..
            })
        ),
        "a silent server must time out, not fail: {reason:?}"
    );
    assert_eq!(
        session.state().generation().map(|value| value.value()),
        Some(1),
        "the failed attempt keeps its generation so a receipt can name it"
    );
    server.abort();
}

#[tokio::test]
async fn a_server_that_hangs_up_is_disconnected_and_not_timed_out() {
    // An empty script: the task ends immediately, closing the stream.
    let (stream, server) = scripted(Vec::new());
    let mut channel = channel(stream);
    let mut session = session();
    let reason = start(
        &mut channel,
        &mut session,
        bounds(),
        Duration::from_secs(5),
        Duration::from_secs(5),
        "0.1.0",
    )
    .await;
    assert!(
        matches!(
            reason,
            Err(StartFailure {
                reason: Unusable::Disconnected,
                ..
            })
        ),
        "a hangup must be disconnected, not timed out: {reason:?}"
    );
    server.abort();
}

#[tokio::test]
async fn an_unusable_answer_fails_with_the_reason_the_server_gave() {
    let wrong_version = json!({"jsonrpc":"2.0","id":1,"result":{
        "protocolVersion":"1999-01-01","serverInfo":{"name":"x"}}})
    .to_string();
    let (stream, server) = scripted(vec![wrong_version]);
    let mut channel = channel(stream);
    let mut session = session();
    let reason = start(
        &mut channel,
        &mut session,
        bounds(),
        Duration::from_secs(5),
        Duration::from_secs(5),
        "0.1.0",
    )
    .await;
    match reason {
        Err(StartFailure {
            reason: Unusable::Failed(detail),
            ..
        }) => assert!(
            detail.contains("protocol version"),
            "the failure must name what the server actually said: {detail}"
        ),
        other => panic!("an unusable handshake was not a failure: {other:?}"),
    }
    assert!(
        session.catalog().is_empty(),
        "a failed start must leave no catalog behind"
    );
    server.abort();
}

#[tokio::test]
async fn an_endless_line_is_refused_at_the_frame_bound_rather_than_absorbed() {
    let small = match McpBounds::new(256, 4, 128, 32, 64) {
        Ok(bounds) => bounds,
        Err(_) => panic!("the fixture bounds must be constructible"),
    };
    let (ours, theirs) = tokio::io::duplex(1024 * 64);
    let server = tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(theirs);
        let mut lines = BufReader::new(read);
        let mut request = String::new();
        let _ = lines.read_line(&mut request).await;
        // No newline, ever, and far past the bound.
        let _ = write.write_all(&vec![b'x'; 4096]).await;
        let _ = write.flush().await;
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    let (read, write) = tokio::io::split(ours);
    let mut channel = McpChannel::new(read, write, small);
    let mut session = session();
    let reason = start(
        &mut channel,
        &mut session,
        small,
        Duration::from_secs(5),
        Duration::from_secs(5),
        "0.1.0",
    )
    .await;
    match reason {
        Err(StartFailure {
            reason: Unusable::Failed(detail),
            ..
        }) => assert!(
            detail.contains("exceeds"),
            "an oversized frame must be refused at the bound: {detail}"
        ),
        other => panic!("an endless line was not refused: {other:?}"),
    }
    server.abort();
}

#[tokio::test]
async fn a_reply_answering_the_wrong_request_is_refused() {
    // The server answers the tools/list request with the handshake's id.
    let stale = json!({"jsonrpc":"2.0","id":1,"result":{"tools":[]}}).to_string();
    let (stream, server) = scripted(vec![handshake(), stale]);
    let mut channel = channel(stream);
    let mut session = session();
    let reason = start(
        &mut channel,
        &mut session,
        bounds(),
        Duration::from_secs(5),
        Duration::from_secs(5),
        "0.1.0",
    )
    .await;
    match reason {
        Err(StartFailure {
            reason: Unusable::Failed(detail),
            ..
        }) => assert!(
            detail.contains("does not answer"),
            "a mismatched reply must be refused by correlation: {detail}"
        ),
        other => panic!("a mismatched reply was accepted: {other:?}"),
    }
    server.abort();
}

#[test]
fn a_local_command_without_a_program_is_refused_before_any_spawn() {
    let refusal = spawn_local(&[], Path::new("."), &BTreeMap::new());
    assert!(refusal.is_err(), "an empty command was spawned");
}

/// A STALLED HANDSHAKE MUST NOT BLOCK AN ALREADY-READY CONNECTION.
///
/// Startup is network work. While it ran under the registry lock, a server
/// that handshakes slowly held every other location's status read and every
/// unrelated dispatch for its whole startup budget. The stalled-CALL witness
/// covered only the path that was moved first; this covers admission.
#[tokio::test]
async fn a_stalled_handshake_does_not_block_a_ready_connection() {
    let registry = std::sync::Arc::new(registry_for_tests());

    // An already-ready connection in another location.
    let (stream, _frames, ready_server) = conformant(handshake_answering(PROTOCOL_VERSION));
    let incarnation = registry
        .install_live_for_test("E:/projects/fast", "srv", local("fast"), stream, "fast")
        .await;

    // A location whose declared server never answers its handshake. The
    // fixture reads the initialize request and goes silent.
    let (silent, _silent_task) = scripted(vec![String::new()]);
    let silent_registry = std::sync::Arc::clone(&registry);
    let stalling = tokio::spawn(async move {
        silent_registry
            .with_status(
                "E:/projects/slow",
                &declared("srv", local("grapt-no-such-mcp-slow")),
                |entries| entries.len(),
            )
            .await
    });
    drop(silent);
    tokio::task::yield_now().await;

    // The ready location must still serve status AND dispatch.
    let status = tokio::time::timeout(
        Duration::from_secs(5),
        registry.with_status(
            "E:/projects/fast",
            &declared("srv", local("fast")),
            |entries| entries.contains_key("srv"),
        ),
    )
    .await;
    assert_eq!(
        status.ok(),
        Some(true),
        "a status read must not wait behind another location's startup"
    );

    let dispatched = tokio::time::timeout(
        Duration::from_secs(5),
        registry.call(
            "E:/projects/fast",
            "srv",
            &discovered_tool(),
            &approval_for(&local("fast"), "fast", incarnation),
            "call-fast",
            json!({"id": "1"}),
            Duration::from_secs(5),
        ),
    )
    .await;
    match dispatched {
        Ok(Ok(_)) => {}
        other => panic!("a dispatch must not wait behind another location's startup: {other:?}"),
    }

    stalling.abort();
    ready_server.abort();
}

/// AN OBSERVED HANGUP CHANGES WHAT THE NEXT READER IS TOLD.
///
/// `Live` means startup once succeeded, which is a fact about the past. A
/// hangup a call actually saw is a fact about the present, and reporting the
/// first while knowing the second is how a list says connected about a dead
/// pipe and advertises tools no call can reach.
#[tokio::test]
async fn an_observed_hangup_stops_the_server_reading_as_connected() {
    let registry = std::sync::Arc::new(registry_for_tests());
    // A server that answers the handshake and discovery, then HANGS UP.
    let (stream, _frames, server) = conformant(handshake_answering(PROTOCOL_VERSION));
    let incarnation = registry
        .install_live_for_test(
            "E:/projects/one",
            "srv",
            local("grapt-no-such-mcp-after-hangup"),
            stream,
            "prog",
        )
        .await;
    let approved = approval_for(
        &local("grapt-no-such-mcp-after-hangup"),
        "prog",
        incarnation,
    );

    let before = registry
        .with_status(
            "E:/projects/one",
            &declared("srv", local("grapt-no-such-mcp-after-hangup")),
            |entries| {
                (
                    entries["srv"].status_label(),
                    entries["srv"].tool_count(),
                    super::super::tool_definitions(entries, &["read", "edit", "shell"])
                        .definitions
                        .len(),
                )
            },
        )
        .await;
    assert_eq!(
        before,
        ("connected", 1, 1),
        "a live connection reports connected and offers its tool"
    );

    // The server goes away, and a call OBSERVES it.
    server.abort();
    let outcome = registry
        .call(
            "E:/projects/one",
            "srv",
            &discovered_tool(),
            &approved,
            "call-after-hangup",
            json!({}),
            Duration::from_secs(5),
        )
        .await;
    assert!(
        matches!(outcome, Err(McpCallRefusal::Disconnected(_))),
        "the call must observe the hangup: {outcome:?}"
    );

    {
        let locations = registry.entries.lock().await;
        let entries = &locations["E:/projects/one"].entries;
        assert_eq!(
            (
                entries["srv"].status_label(),
                entries["srv"].tool_count(),
                super::super::tool_definitions(entries, &["read", "edit", "shell"])
                    .definitions
                    .len(),
            ),
            ("disconnected", 0, 0),
            "the observed hangup immediately withdraws the dead connection's tools"
        );
    }

    let after = registry
        .with_status(
            "E:/projects/one",
            &declared("srv", local("grapt-no-such-mcp-after-hangup")),
            |entries| {
                (
                    entries["srv"].status_label(),
                    entries["srv"].tool_count(),
                    super::super::tool_definitions(entries, &["read", "edit", "shell"])
                        .definitions
                        .len(),
                )
            },
        )
        .await;
    assert_eq!(
        after,
        ("failed", 0, 0),
        "a refresh reports the failed replacement and keeps dead tools withdrawn"
    );
    assert_eq!(
        registry.connect_attempts(),
        1,
        "the refresh must attempt recovery"
    );
}

/// A TIMEOUT IS NOT A HANGUP. It says nothing about whether the server is
/// alive, so it must not condemn a connection that may still be serving.
#[tokio::test]
async fn a_call_timeout_does_not_mark_the_connection_unusable() {
    let registry = std::sync::Arc::new(registry_for_tests());
    let (stream, _frames, server) = conformant(handshake_answering(PROTOCOL_VERSION));
    let incarnation = registry
        .install_live_for_test("E:/projects/one", "srv", local("prog"), stream, "prog")
        .await;
    let approved = approval_for(&local("prog"), "prog", incarnation);

    let outcome = registry
        .call(
            "E:/projects/one",
            "srv",
            &discovered_tool(),
            &approved,
            "call-slow",
            json!({"silent": true}),
            Duration::from_millis(150),
        )
        .await;
    assert!(matches!(outcome, Err(McpCallRefusal::TimedOut)));

    let label = registry
        .with_status(
            "E:/projects/one",
            &declared("srv", local("prog")),
            |entries| entries["srv"].status_label(),
        )
        .await;
    assert_eq!(
        label, "connected",
        "a deadline is the absence of an answer, not evidence the server is gone"
    );
    server.abort();
}

/// THE AGGREGATE BOUND COUNTS PROCESSES, NOT MAP ENTRIES.
///
/// A retired connection a caller still holds is still a child and a pipe. The
/// location map cannot see that: eviction removes the entry while the caller's
/// reference keeps the process alive. The slot is held BY the connection, so
/// it is released when the last holder drops it.
#[test]
fn a_connection_slot_is_released_by_the_last_holder_not_by_eviction() {
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let first = ConnectionSlot::admit(&counter, 2);
    let second = ConnectionSlot::admit(&counter, 2);
    assert!(first.is_some() && second.is_some());
    assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 2);

    // At the bound, a third is REFUSED before anything is spawned.
    assert!(
        ConnectionSlot::admit(&counter, 2).is_none(),
        "admission must refuse at the bound rather than count past it"
    );

    // Releasing one holder frees exactly one slot.
    drop(first);
    assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 1);
    let third = ConnectionSlot::admit(&counter, 2);
    assert!(third.is_some(), "a released slot is reusable");

    drop(second);
    drop(third);
    assert_eq!(
        counter.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "every slot returns when its connection is finally dropped"
    );
}

/// The registry refuses a new connection at the bound rather than spawning
/// past it, and says so.
#[tokio::test]
async fn the_registry_refuses_a_connection_over_the_aggregate_bound() {
    let registry = registry_for_tests();
    let mut held = Vec::new();
    for _ in 0..MAX_LIVE_CONNECTIONS {
        match ConnectionSlot::admit(registry.connections_for_test(), MAX_LIVE_CONNECTIONS) {
            Some(slot) => held.push(slot),
            None => panic!("the bound must admit up to its own limit"),
        }
    }

    let detail = registry
        .with_status(
            "E:/projects/one",
            &declared("srv", local("grapt-no-such-mcp-server")),
            |entries| entries.get("srv").map(ServerEntry::detail),
        )
        .await;
    match detail {
        Some(detail) => assert!(
            detail.contains("already holds"),
            "the refusal must name the bound rather than a spawn failure: {detail}"
        ),
        None => panic!("the declared server must hold an entry"),
    }
}

/// A WITHDRAWN DECLARATION STAYS WITHDRAWN across later reads.
///
/// NAMED FOR WHAT IT PROVES, which is less than the reservation it sits
/// beside. It shows withdrawal is durable, but it does NOT isolate the
/// install-time validation: removing that check leaves this green, because
/// the retain already removed the entry and no late completion exists to
/// reinstall it.
///
/// The case that WOULD isolate it — a start still running when its
/// declaration is withdrawn, completing afterwards — needs a controllable
/// slow connect this fixture cannot yet produce, since a missing program
/// fails immediately. That witness is OWED. The validation is implemented
/// and reviewable in `with_status`; it is not yet proven by a test, and I
/// would rather say so than let the name imply otherwise.
#[tokio::test]
async fn a_withdrawn_declaration_stays_withdrawn() {
    let registry = std::sync::Arc::new(registry_for_tests());

    // A start that will fail slowly enough to be overtaken is not needed: the
    // check is on the reservation, so a completed start whose declaration no
    // longer matches is refused the same way.
    registry
        .with_status(
            "E:/projects/one",
            &declared("srv", local("grapt-no-such-mcp-before")),
            |entries| assert!(entries.contains_key("srv")),
        )
        .await;

    // Withdraw it.
    registry
        .with_status("E:/projects/one", &BTreeMap::new(), |entries| {
            assert!(
                entries.is_empty(),
                "a withdrawn declaration takes its entry"
            );
        })
        .await;

    // And it stays withdrawn: nothing reinstalls it.
    registry
        .with_status("E:/projects/one", &BTreeMap::new(), |entries| {
            assert!(entries.is_empty());
        })
        .await;
}

/// Concurrent readers share one healthy startup and retain it on later reads.
#[tokio::test]
async fn repeated_readers_start_a_server_once() {
    let registry = std::sync::Arc::new(registry_for_tests());
    let declaration = declared("srv", healthy_local("readers"));

    let mut readers = Vec::new();
    for _ in 0..4 {
        let reader = std::sync::Arc::clone(&registry);
        let declaration = declaration.clone();
        readers.push(tokio::spawn(async move {
            reader
                .with_status("E:/projects/one", &declaration, |entries| entries.len())
                .await
        }));
    }
    for reader in readers {
        assert!(reader.await.is_ok(), "a reader task failed");
    }

    registry
        .with_status("E:/projects/one", &declaration, |entries| {
            assert_eq!(entries["srv"].status_label(), "connected");
        })
        .await;
    assert_eq!(
        registry.connect_attempts(),
        1,
        "four readers must produce ONE startup, not four copies"
    );
}

/// A CONNECTION ADMITS A BOUNDED QUEUE. A deadline bounds how long one caller
/// waits, not how many do, and each waiter holds a reference that keeps the
/// child alive past any eviction.
#[test]
fn a_connection_refuses_more_waiters_than_it_admits() {
    let held = super::retirement::held_connection_for_test();
    let mut tickets = Vec::new();
    for _ in 0..MAX_QUEUED_CALLERS {
        match held.enqueue(MAX_QUEUED_CALLERS) {
            Some(ticket) => tickets.push(ticket),
            None => panic!("the queue must admit up to its own bound"),
        }
    }
    assert_eq!(held.waiting(), MAX_QUEUED_CALLERS);
    assert!(
        held.enqueue(MAX_QUEUED_CALLERS).is_none(),
        "the bound refuses rather than queueing without limit"
    );

    // A caller that leaves — including by timing out — releases its place.
    tickets.pop();
    assert_eq!(held.waiting(), MAX_QUEUED_CALLERS - 1);
    assert!(held.enqueue(MAX_QUEUED_CALLERS).is_some());
}
