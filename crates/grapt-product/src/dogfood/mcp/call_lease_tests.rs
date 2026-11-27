//! Exercise the production call adapter, not only the domain reservation API.
use super::CallLeaseGuard;
use grapt_mcp::session::{CallOutcome, McpSession, SessionBounds, SessionError, Unusable};
use grapt_mcp::{McpBounds, McpToolName};
use serde_json::{json, Value};
use std::future::{poll_fn, Future};
use std::task::Poll;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn tool() -> McpToolName {
    McpToolName::new("fixture_read").unwrap_or_else(|| panic!("fixture tool name"))
}

fn session() -> McpSession {
    let bounds = SessionBounds::new(1, 32).unwrap_or_else(|error| panic!("bounds: {error}"));
    let mut session = McpSession::declared(bounds);
    session
        .start()
        .unwrap_or_else(|error| panic!("start: {error}"));
    session
        .ready(vec![tool()])
        .unwrap_or_else(|error| panic!("ready: {error}"));
    session
}

#[test]
fn explicit_terminal_is_not_replaced_by_drop() -> Result<(), SessionError> {
    for outcome in [
        CallOutcome::Completed,
        CallOutcome::Refused,
        CallOutcome::TimedOut,
        CallOutcome::Disconnected,
    ] {
        let mut session = session();
        let lease = session.begin_call("terminal", &tool())?;
        CallLeaseGuard::new(&mut session, lease).finish(outcome)?;
        assert_eq!(session.outcome("terminal"), Some(outcome));
        let next = session.begin_call("next", &tool())?;
        session.settle(next, CallOutcome::Completed)?;
    }
    Ok(())
}

#[test]
fn dropped_guard_cannot_settle_a_successor_generation() -> Result<(), SessionError> {
    let mut session = session();
    let lease = session.begin_call("old", &tool())?;
    let old_generation = lease.generation();
    let guard = CallLeaseGuard::new(&mut session, lease);
    // Exercise the domain fence with a retained old lease. Production retains
    // exclusive access until drop; this deterministic cut proves the fallback
    // cannot overwrite a later generation even if ownership changes later.
    guard.session.unusable(Unusable::TimedOut);
    let successor = guard.session.start()?;
    assert_ne!(successor, old_generation);
    guard.session.ready(vec![tool()])?;
    let next = guard.session.begin_call("new", &tool())?;
    drop(guard);
    assert_eq!(session.state().generation(), Some(successor));
    assert_eq!(session.outcome("old"), Some(CallOutcome::TimedOut));
    assert_eq!(session.outcome("new"), None);
    session.settle(next, CallOutcome::Completed)?;
    Ok(())
}

#[tokio::test]
async fn dropped_ordinary_calls_release_slots_and_preserve_late_reply_correlation() {
    let bounds = McpBounds::conservative().unwrap_or_else(|error| panic!("bounds: {error}"));
    let (ours, peer) = tokio::io::duplex(64 * 1024);
    let (read, write) = tokio::io::split(ours);
    let mut channel = super::super::McpChannel::new(read, write, bounds);
    let (read, mut write) = tokio::io::split(peer);
    let mut peer = BufReader::new(read);
    let mut session = session();
    let tool = tool();

    // More cancellations than the production in-flight ceiling (eight), on
    // the stricter one-slot session. A leaked lease fails the very next call.
    for index in 0..12 {
        let call_id = format!("cancelled-{index}");
        let mut call = Box::pin(super::super::run_call(
            &mut channel,
            &mut session,
            bounds,
            &tool,
            &call_id,
            json!({"path":"fixture.txt"}),
            Duration::from_secs(5),
        ));
        assert!(poll_fn(|cx| Poll::Ready(call.as_mut().poll(cx)))
            .await
            .is_pending());
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(1), peer.read_line(&mut line))
            .await
            .unwrap_or_else(|error| panic!("request deadline: {error}"))
            .unwrap_or_else(|error| panic!("request read: {error}"));
        let request: Value =
            serde_json::from_str(&line).unwrap_or_else(|error| panic!("request JSON: {error}"));
        assert_eq!(request["method"], "tools/call");
        assert_eq!(request["params"]["name"], "fixture_read");
        drop(call);
        assert_eq!(session.outcome(&call_id), Some(CallOutcome::Disconnected));
        assert!(matches!(
            session.begin_call(&call_id, &tool),
            Err(SessionError::DuplicateTerminal(_))
        ));
        let late = json!({"jsonrpc":"2.0","id":request["id"],
            "result":{"content":[{"type":"text","text":"late"}]}});
        write
            .write_all(format!("{late}\n").as_bytes())
            .await
            .unwrap_or_else(|error| panic!("late reply write: {error}"));
    }

    let mut call = Box::pin(super::super::run_call(
        &mut channel,
        &mut session,
        bounds,
        &tool,
        "completed",
        json!({}),
        Duration::from_secs(5),
    ));
    assert!(poll_fn(|cx| Poll::Ready(call.as_mut().poll(cx)))
        .await
        .is_pending());
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(1), peer.read_line(&mut line))
        .await
        .unwrap_or_else(|error| panic!("final request deadline: {error}"))
        .unwrap_or_else(|error| panic!("final request read: {error}"));
    let request: Value =
        serde_json::from_str(&line).unwrap_or_else(|error| panic!("final request: {error}"));
    let reply = json!({"jsonrpc":"2.0","id":request["id"],
        "result":{"content":[{"type":"text","text":"current"}]}});
    write
        .write_all(format!("{reply}\n").as_bytes())
        .await
        .unwrap_or_else(|error| panic!("reply write: {error}"));
    let result = call.await.unwrap_or_else(|error| panic!("call: {error:?}"));
    assert_eq!(result.text(), "current");
    assert_eq!(session.outcome("completed"), Some(CallOutcome::Completed));
}
