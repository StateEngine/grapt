//! Reconnecting a declared server preserves consent, not an old byte stream.
use super::super::*;
use super::fixtures::{conformant, discovered_tool, handshake_answering, live_launch, local};
use grapt_mcp::PROTOCOL_VERSION;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const LOCATION: &str = "rebind-fixture";
const SERVER: &str = "srv";
const BUDGET: Duration = Duration::from_secs(5);

struct Peer {
    approved: LaunchIdentity,
    frames: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Peer {
    async fn install(registry: &McpRegistry, declaration: Declaration) -> Self {
        let (stream, frames, task) = conformant(handshake_answering(PROTOCOL_VERSION));
        registry
            .install_live_for_test(LOCATION, SERVER, declaration, stream, "prog")
            .await;
        let locations = registry.entries.lock().await;
        let approved = live_launch(&locations[LOCATION].entries[SERVER]);
        Self {
            approved,
            frames,
            task,
        }
    }
    fn calls(&self) -> usize {
        self.frames
            .lock()
            .unwrap_or_else(|_| panic!("recorder poisoned"))
            .iter()
            .filter(|frame| frame["method"] == "tools/call")
            .count()
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn queued<F: std::future::Future>(call: &mut std::pin::Pin<Box<F>>) {
    let waker = std::task::Waker::noop();
    let mut context = std::task::Context::from_waker(waker);
    assert!(
        call.as_mut().poll(&mut context).is_pending(),
        "call did not queue on held I/O"
    );
}

#[tokio::test]
async fn sc_mcp03_changed_command_arguments_or_environment_refuse_the_old_grant() {
    for field in 0..3 {
        let registry = super::fixtures::registry_for_tests();
        let original = local("prog");
        let first = Peer::install(&registry, original.clone()).await;
        let mut changed = original;
        let Declaration::Local {
            command,
            environment,
            ..
        } = &mut changed
        else {
            panic!("local fixture")
        };
        match field {
            0 => command[0] = "another-alias-for-prog".into(),
            1 => command.push("--different-arguments".into()),
            _ => {
                environment.insert("PRIVATE_SETTING".into(), "SCMCP03-private-secret".into());
            }
        }
        let replacement = Peer::install(&registry, changed).await;
        assert!(!format!("{:?}", replacement.approved).contains("SCMCP03-private-secret"));
        let result = registry
            .call(
                LOCATION,
                SERVER,
                &discovered_tool(),
                &first.approved,
                "old-grant",
                json!({}),
                BUDGET,
            )
            .await;
        assert!(
            matches!(result, Err(McpCallRefusal::ConnectionReplaced { .. })),
            "SCMCP03 changed declaration must refuse before writing: {result:?}"
        );
        assert_eq!(first.calls(), 0);
        assert_eq!(
            replacement.calls(),
            0,
            "SCMCP03 changed peer received an unapproved call"
        );
        let fresh = registry
            .call(
                LOCATION,
                SERVER,
                &discovered_tool(),
                &replacement.approved,
                "fresh-grant",
                json!({}),
                BUDGET,
            )
            .await;
        assert!(
            fresh.is_ok(),
            "the new declaration's own grant must work: {fresh:?}"
        );
        assert_eq!(replacement.calls(), 1);
    }
}

#[tokio::test]
async fn sc_mcp03_remote_endpoint_or_headers_cannot_reuse_the_old_grant() {
    for field in 0..2 {
        let registry = super::fixtures::registry_for_tests();
        let original = Declaration::Remote {
            url: "https://example.invalid/approved".into(),
            headers: BTreeMap::new(),
            oauth: false,
            timeouts: McpTimeouts::default(),
        };
        let first = Peer::install(&registry, original.clone()).await;
        let mut changed = original;
        let Declaration::Remote { url, headers, .. } = &mut changed else {
            panic!("remote fixture")
        };
        if field == 0 {
            *url = "https://example.invalid/another-endpoint".into();
        } else {
            headers.insert("Authorization".into(), "SCMCP03-private-secret".into());
        }
        let replacement = Peer::install(&registry, changed).await;
        assert!(!format!("{:?}", replacement.approved).contains("SCMCP03-private-secret"));
        let result = registry
            .call(
                LOCATION,
                SERVER,
                &discovered_tool(),
                &first.approved,
                "remote-old-grant",
                json!({}),
                BUDGET,
            )
            .await;
        assert!(
            matches!(result, Err(McpCallRefusal::ConnectionReplaced { .. })),
            "SCMCP03 changed remote declaration must refuse: {result:?}"
        );
        assert_eq!(replacement.calls(), 0);
    }
}

#[tokio::test]
async fn sc_mcp03_queued_call_rebinds_without_waiting_for_retired_io() {
    let registry = super::fixtures::registry_for_tests();
    let declaration = local("prog");
    let first = Peer::install(&registry, declaration.clone()).await;
    let handle = registry
        .io_handle_for_test(LOCATION, SERVER)
        .await
        .unwrap_or_else(|| panic!("installed connection missing"));
    let owner = handle.io().lock().await;
    let tool = discovered_tool();
    let mut call = Box::pin(registry.call(
        LOCATION,
        SERVER,
        &tool,
        &first.approved,
        "queued-reconnect",
        json!({}),
        BUDGET,
    ));
    queued(&mut call);
    let replacement = Peer::install(&registry, declaration).await;
    let result = call.await;
    assert!(
        result.is_ok(),
        "SCMCP03 queued call must rebind under its original grant: {result:?}"
    );
    assert_eq!(
        first.calls(),
        0,
        "SCMCP03 retired peer received the queued request"
    );
    assert_eq!(replacement.calls(), 1);
    drop(owner);
}

#[tokio::test]
async fn sc_mcp03_queued_call_refuses_a_changed_declaration() {
    let registry = super::fixtures::registry_for_tests();
    let declaration = local("prog");
    let first = Peer::install(&registry, declaration.clone()).await;
    let handle = registry
        .io_handle_for_test(LOCATION, SERVER)
        .await
        .unwrap_or_else(|| panic!("installed connection missing"));
    let owner = handle.io().lock().await;
    let tool = discovered_tool();
    let mut call = Box::pin(registry.call(
        LOCATION,
        SERVER,
        &tool,
        &first.approved,
        "queued-changed",
        json!({}),
        BUDGET,
    ));
    queued(&mut call);
    let mut changed = declaration;
    let Declaration::Local { environment, .. } = &mut changed else {
        panic!("local fixture")
    };
    environment.insert("MODE".into(), "unapproved".into());
    let replacement = Peer::install(&registry, changed).await;
    let result = call.await;
    assert!(
        matches!(result, Err(McpCallRefusal::ConnectionReplaced { .. })),
        "SCMCP03 queued changed declaration must refuse: {result:?}"
    );
    assert_eq!(first.calls(), 0);
    assert_eq!(replacement.calls(), 0);
    drop(owner);
}

#[tokio::test]
async fn sc_mcp03_dispatched_call_is_never_replayed_after_reconnect() {
    let registry = super::fixtures::registry_for_tests();
    let declaration = local("prog");
    let first = Peer::install(&registry, declaration.clone()).await;
    let tool = discovered_tool();
    let mut running = Box::pin(registry.call(
        LOCATION,
        SERVER,
        &tool,
        &first.approved,
        "already-sent",
        json!({"silent":true}),
        BUDGET,
    ));
    queued(&mut running);
    for _ in 0..64 {
        if first.calls() == 1 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        first.calls(),
        1,
        "the peer must observe the effect-bearing request first"
    );
    let replacement = Peer::install(&registry, declaration).await;
    let result = running.await;
    assert!(
        matches!(result, Err(McpCallRefusal::ConnectionReplaced { .. })),
        "an in-flight retirement keeps its honest failure: {result:?}"
    );
    assert_eq!(
        replacement.calls(),
        0,
        "SCMCP03 an already dispatched call was replayed"
    );
}
