//! Connection identity, dispatch, approval binding and concurrency.

use std::time::Duration;

use grapt_mcp::session::Unusable;
use grapt_mcp::{McpToolName, PROTOCOL_VERSION};
use serde_json::json;

use super::super::*;
use super::fixtures::*;

#[tokio::test]
async fn a_discovered_tool_runs_and_its_result_comes_back() {
    let (mut channel, mut session, server) = connected().await;
    let outcome = run_call(
        &mut channel,
        &mut session,
        bounds(),
        &discovered_tool(),
        "call-1",
        json!({"id": "42"}),
        Duration::from_secs(5),
    )
    .await;
    match outcome {
        Ok(outcome) => {
            assert!(!outcome.is_error());
            assert!(
                outcome.text().contains("fixture_read ran with"),
                "the server's own result is returned: {}",
                outcome.text()
            );
        }
        Err(refusal) => panic!("a discovered tool must run: {refusal:?}"),
    }
    server.abort();
}

/// A tool that RAN and reported failure is a completed call the caller
/// continues from, not a refusal. Collapsing them would leave a model
/// unable to tell a non-zero exit from a server that is gone.
#[tokio::test]
async fn a_tool_reporting_its_own_failure_is_a_result_not_a_refusal() {
    let (mut channel, mut session, server) = connected().await;
    let outcome = run_call(
        &mut channel,
        &mut session,
        bounds(),
        &discovered_tool(),
        "call-fails",
        json!({"fail": true}),
        Duration::from_secs(5),
    )
    .await;
    match outcome {
        Ok(outcome) => assert!(outcome.is_error(), "the server's own verdict is carried"),
        Err(refusal) => panic!("a tool failure is not a call refusal: {refusal:?}"),
    }
    server.abort();
}

/// NO DUPLICATE EXECUTION. A call id that already reached a terminal is
/// refused before anything is written, so a retry cannot run the tool twice.
#[tokio::test]
async fn a_call_id_that_already_settled_never_reaches_the_wire_again() {
    let (mut channel, mut session, server) = connected().await;
    let first = run_call(
        &mut channel,
        &mut session,
        bounds(),
        &discovered_tool(),
        "call-1",
        json!({"id": "once"}),
        Duration::from_secs(5),
    )
    .await;
    assert!(first.is_ok(), "the first call runs: {first:?}");

    let repeat = run_call(
        &mut channel,
        &mut session,
        bounds(),
        &discovered_tool(),
        "call-1",
        json!({"id": "twice"}),
        Duration::from_secs(5),
    )
    .await;
    match repeat {
        Err(McpCallRefusal::Session(detail)) => assert!(
            detail.contains("call-1"),
            "the refusal names the repeated call: {detail}"
        ),
        other => panic!("a settled call id must not run again: {other:?}"),
    }
    server.abort();
}

/// Discovery is the authority for what exists. A well-formed name this
/// connection never published is still refused.
#[tokio::test]
async fn an_undiscovered_tool_is_refused_before_anything_is_sent() {
    let (mut channel, mut session, server) = connected().await;
    let unknown = match McpToolName::new("never_discovered") {
        Some(name) => name,
        None => panic!("the name must be usable"),
    };
    let outcome = run_call(
        &mut channel,
        &mut session,
        bounds(),
        &unknown,
        "call-unknown",
        json!({}),
        Duration::from_secs(5),
    )
    .await;
    assert!(
        matches!(outcome, Err(McpCallRefusal::Session(_))),
        "an undiscovered tool must be refused: {outcome:?}"
    );
    server.abort();
}

/// A connection that has gone unusable refuses a call rather than
/// attempting one.
///
/// NAMED FOR WHAT IT ACTUALLY PROVES. It was called a restart/grant
/// witness, which it is not: it never admits a call before the transition
/// and never brings up a fresh READY replacement, so it cannot show an
/// approval failing to carry across one. The approval-held-across-
/// replacement property is witnessed separately, through the registry.
#[tokio::test]
async fn a_connection_that_went_unusable_refuses_a_call() {
    let (mut channel, mut session, server) = connected().await;
    session.unusable(Unusable::Disconnected);

    let outcome = run_call(
        &mut channel,
        &mut session,
        bounds(),
        &discovered_tool(),
        "call-after-restart",
        json!({}),
        Duration::from_secs(5),
    )
    .await;
    match outcome {
        Err(McpCallRefusal::Session(_)) => {}
        other => panic!("a retired connection must refuse a call: {other:?}"),
    }
    server.abort();
}

/// TIMED OUT IS NOT REFUSED. A deadline says nothing about whether the
/// server ran the tool, and a caller treating it as "did not run" would
/// retry an effect that may already have happened.
#[tokio::test]
async fn a_call_that_passes_its_deadline_is_timed_out_and_not_a_refusal() {
    let (mut channel, mut session, server) = connected().await;
    let outcome = run_call(
        &mut channel,
        &mut session,
        bounds(),
        &discovered_tool(),
        "call-slow",
        json!({"silent": true}),
        Duration::from_millis(150),
    )
    .await;
    assert!(
        matches!(outcome, Err(McpCallRefusal::TimedOut)),
        "a deadline is not a refusal: {outcome:?}"
    );
    server.abort();
}

/// A grant survives a new connection of the same declared server. Both
/// connections complete discovery, and the replacement receives the call.
#[tokio::test]
async fn an_approval_for_one_declaration_survives_its_reconnection() {
    let registry = registry_for_tests();
    let (stream, _, first) = conformant(handshake_answering(PROTOCOL_VERSION));
    let incarnation = registry
        .install_live_for_test("one", "srv", local("prog"), stream, "prog")
        .await;
    let approved = approval_for(&local("prog"), "prog", incarnation);
    let (stream, frames, second) = conformant(handshake_answering(PROTOCOL_VERSION));
    let replacement = registry
        .install_live_for_test("one", "srv", local("prog"), stream, "prog")
        .await;
    assert_ne!(incarnation, replacement);
    let outcome = registry
        .call(
            "one",
            "srv",
            &discovered_tool(),
            &approved,
            "call-reconnected",
            json!({}),
            Duration::from_secs(5),
        )
        .await;
    assert!(
        outcome.is_ok(),
        "SCMCP03 unchanged declaration must keep its grant: {outcome:?}"
    );
    assert!(frames
        .lock()
        .unwrap_or_else(|_| panic!("recorder poisoned"))
        .iter()
        .any(|frame| frame["method"] == "tools/call"));
    first.abort();
    second.abort();
}

/// The same connection, unreplaced, still runs under its original grant.
#[tokio::test]
async fn an_approval_for_the_current_connection_still_runs() {
    let registry = registry_for_tests();
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
            "call-ok",
            json!({"id": "1"}),
            Duration::from_secs(5),
        )
        .await;
    assert!(
        outcome.is_ok(),
        "the approved connection must run: {outcome:?}"
    );
    server.abort();
}

/// A CHANGED `cwd` IS A DIFFERENT SERVER, so the old connection is retired
/// rather than reused.
///
/// This is why every configured field belongs in `Declaration`: reuse is
/// decided by comparing declarations, so a field the declaration does not
/// carry cannot end a connection that no longer matches it. Before this work
/// `cwd` and `environment` were not carried, and editing either left the OLD
/// child serving calls under the NEW configuration -- a tool running in a
/// directory, or with credentials, the current configuration does not
/// describe.
#[tokio::test]
async fn changing_the_working_directory_retires_the_connection() {
    let registry = registry_for_tests();
    let (stream, _frames, server) = conformant(handshake_answering(PROTOCOL_VERSION));
    let first = registry
        .install_live_for_test(
            "E:/projects/one",
            "srv",
            local_with_cwd("prog", "before"),
            stream,
            "prog",
        )
        .await;

    // The same name, the same command, a different directory.
    let declared = declared("srv", local_with_cwd("prog", "after"));
    let held = registry
        .with_status("E:/projects/one", &declared, |entries| {
            entries.get("srv").map(ServerEntry::status_label)
        })
        .await;

    assert_ne!(
        held,
        Some("connected"),
        "the connection for the previous cwd must not be reused"
    );
    let _ = first;
    server.abort();
}

/// The same, for a changed environment entry. Separately witnessed because
/// `cwd` and `environment` are different fields and a fix could carry one
/// without the other.
#[tokio::test]
async fn changing_an_environment_entry_retires_the_connection() {
    let registry = registry_for_tests();
    let (stream, _frames, server) = conformant(handshake_answering(PROTOCOL_VERSION));
    let mut before = std::collections::BTreeMap::new();
    before.insert("TOKEN_NAME".to_owned(), "one".to_owned());
    registry
        .install_live_for_test(
            "E:/projects/one",
            "srv",
            local_with_environment("prog", before),
            stream,
            "prog",
        )
        .await;

    let mut after = std::collections::BTreeMap::new();
    after.insert("TOKEN_NAME".to_owned(), "two".to_owned());
    let declared = declared("srv", local_with_environment("prog", after));
    let held = registry
        .with_status("E:/projects/one", &declared, |entries| {
            entries.get("srv").map(ServerEntry::status_label)
        })
        .await;

    assert_ne!(
        held,
        Some("connected"),
        "a connection built with different environment must not be reused"
    );
    server.abort();
}

/// THE CONTROL, without which the two witnesses above are vacuous.
///
/// They assert that a CHANGED declaration is not reported connected. That is
/// satisfied by a registry which never reports anything connected -- by a
/// broken fixture, a wrong location, or a status label that changed name. This
/// holds every input identical and requires `connected`, so the two refusals
/// are refusals about the CHANGE rather than about the setup.
#[tokio::test]
async fn an_unchanged_declaration_keeps_its_connection() {
    let registry = registry_for_tests();
    let (stream, _frames, server) = conformant(handshake_answering(PROTOCOL_VERSION));
    registry
        .install_live_for_test(
            "E:/projects/one",
            "srv",
            local_with_cwd("prog", "same"),
            stream,
            "prog",
        )
        .await;

    let declared = declared("srv", local_with_cwd("prog", "same"));
    let held = registry
        .with_status("E:/projects/one", &declared, |entries| {
            entries.get("srv").map(ServerEntry::status_label)
        })
        .await;

    assert_eq!(
        held,
        Some("connected"),
        "an unchanged declaration must keep its live connection"
    );
    server.abort();
}

/// AN APPROVAL BOUND TO ONE WORKING DIRECTORY IS REFUSED AGAINST ANOTHER.
///
/// `cwd` became part of the declaration, so changing it retires the old
/// connection — witnessed separately. This is the OTHER half: the user's
/// consent named a program running in one directory, and a call carrying that
/// consent must not run against a connection working somewhere else.
///
/// It matters because `cwd` decides what a filesystem-shaped MCP server can
/// see. Spending an approval granted for one project on a server rooted in
/// another is exactly the substitution the launch identity exists to prevent.
#[tokio::test]
async fn an_approval_naming_one_cwd_is_refused_against_a_connection_in_another() {
    let registry = registry_for_tests();
    let (stream, frames, server) = conformant(handshake_answering(PROTOCOL_VERSION));
    let incarnation = registry
        .install_live_for_test(
            "E:/projects/one",
            "srv",
            local_with_cwd("prog", "after"),
            stream,
            "prog",
        )
        .await;

    // The consent the user gave: same program, same arguments, same
    // connection identity -- and the directory it was granted for.
    let approved = approval_for(&local_with_cwd("prog", "before"), "prog", incarnation);

    let before = match frames.lock() {
        Ok(frames) => frames.len(),
        Err(_) => panic!("the recorder was poisoned"),
    };

    let refusal = registry
        .call(
            "E:/projects/one",
            "srv",
            &discovered_tool(),
            &approved,
            "call-elsewhere",
            json!({}),
            Duration::from_secs(5),
        )
        .await;

    match refusal {
        Err(McpCallRefusal::ConnectionReplaced { approved, current }) => {
            assert!(
                approved.contains("before"),
                "the consent names its cwd: {approved}"
            );
            assert!(
                current.contains("after"),
                "the connection names its own: {current}"
            );
        }
        other => panic!("a differing cwd must refuse the approval: {other:?}"),
    }

    let after = match frames.lock() {
        Ok(frames) => frames.len(),
        Err(_) => panic!("the recorder was poisoned"),
    };
    assert_eq!(
        before, after,
        "the server must receive NOTHING: refused before any write"
    );
    server.abort();
}

#[tokio::test]
async fn runtime_registration_deduplicates_and_changed_config_replaces() {
    let registry = registry_for_tests();
    let location = "E:/projects/runtime";
    let first = healthy_local("runtime-first");
    if let Err(error) = registry
        .register_runtime(location, "tools".to_owned(), first.clone())
        .await
    {
        panic!("the first runtime declaration must register: {error}");
    }
    let original = registry
        .with_status(location, &std::collections::BTreeMap::new(), |entries| {
            assert_eq!(entries["tools"].status_label(), "connected");
            live_launch(&entries["tools"])
        })
        .await;
    assert_eq!(
        registry.connect_attempts(),
        1,
        "runtime declaration did not reach ordinary MCP admission"
    );

    if let Err(error) = registry
        .register_runtime(location, "tools".to_owned(), first)
        .await
    {
        panic!("the duplicate runtime declaration must reconcile: {error}");
    }
    registry
        .with_status(location, &std::collections::BTreeMap::new(), |entries| {
            assert_eq!(entries["tools"].status_label(), "connected");
            assert_eq!(live_launch(&entries["tools"]), original);
        })
        .await;
    assert_eq!(
        registry.connect_attempts(),
        1,
        "duplicate registration reconnected"
    );

    let changed = healthy_local("runtime-replaced");
    if let Err(error) = registry
        .register_runtime(location, "tools".to_owned(), changed)
        .await
    {
        panic!("the changed runtime declaration must register: {error}");
    }
    let replacement = registry
        .with_status(location, &std::collections::BTreeMap::new(), |entries| {
            assert_eq!(entries["tools"].status_label(), "connected");
            live_launch(&entries["tools"])
        })
        .await;
    assert_ne!(replacement, original);
    assert_eq!(
        registry.connect_attempts(),
        2,
        "replacement reused stale state"
    );

    for ordinal in 0..MAX_RETAINED_LOCATIONS {
        registry
            .with_status(
                &format!("E:/projects/evict-{ordinal}"),
                &std::collections::BTreeMap::new(),
                |_| (),
            )
            .await;
    }
    assert!(
        !registry.retains_location(location).await,
        "fixture did not evict the runtime location"
    );
    registry
        .with_status(location, &std::collections::BTreeMap::new(), |entries| {
            assert_eq!(entries["tools"].status_label(), "connected");
            assert_ne!(live_launch(&entries["tools"]), replacement);
        })
        .await;
    assert_eq!(
        registry.connect_attempts(),
        3,
        "connection eviction erased a process-lifetime runtime registration"
    );
}

#[test]
fn declaration_debug_never_renders_mcp_credentials() {
    let declaration = Declaration::Remote {
        url: "https://example.com/mcp".to_owned(),
        headers: std::collections::BTreeMap::from([(
            "Authorization".to_owned(),
            "Bearer scc-secret".to_owned(),
        )]),
        oauth: false,
        timeouts: super::super::McpTimeouts::default(),
    };
    let rendered = format!("{declaration:?}");
    assert!(rendered.contains("Authorization"));
    assert!(!rendered.contains("scc-secret"));
}
