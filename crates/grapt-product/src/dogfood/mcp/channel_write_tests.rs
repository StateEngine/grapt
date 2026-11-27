//! A cancelled partial frame must not become the prefix of another request.
use super::*;
use grapt_mcp::McpBounds;
use std::future::{poll_fn, Future};
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::AsyncWrite;

#[derive(Default)]
struct Wire {
    bytes: Mutex<Vec<u8>>,
    unblocked: AtomicBool,
}

struct PartialWriter(Arc<Wire>);

impl AsyncWrite for PartialWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let mut wire = match self.0.bytes.lock() {
            Ok(wire) => wire,
            Err(_) => return Poll::Ready(Err(std::io::Error::other("fixture lock poisoned"))),
        };
        let remaining = if self.0.unblocked.load(Ordering::Acquire) {
            4096usize.saturating_sub(wire.len())
        } else {
            5usize.saturating_sub(wire.len())
        };
        if remaining == 0 {
            return Poll::Pending;
        }
        let take = remaining.min(bytes.len());
        wire.extend_from_slice(&bytes[..take]);
        Poll::Ready(Ok(take))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn interrupted_partial_request_cannot_corrupt_the_next_request(
) -> Result<(), Box<dyn std::error::Error>> {
    let bounds = McpBounds::conservative()?;
    let wire = Arc::new(Wire::default());
    let reader = std::io::Cursor::new(
        b"{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[]}}\n".to_vec(),
    );
    let mut channel = McpChannel::new(reader, PartialWriter(wire.clone()), bounds);
    let request = grapt_mcp::tools_list_request(channel.sequence(), bounds)?;
    let mut first = Box::pin(channel.exchange(request, Duration::from_secs(1)));
    assert!(poll_fn(|cx| Poll::Ready(first.as_mut().poll(cx)))
        .await
        .is_pending());
    drop(first);
    let partial = wire.bytes.lock().map_err(|_| "fixture lock")?.clone();
    assert_eq!(partial.len(), 5, "the real frame must be partly written");
    wire.unblocked.store(true, Ordering::Release);

    let next = grapt_mcp::tools_list_request(channel.sequence(), bounds)?;
    let result = channel.exchange(next, Duration::from_secs(1)).await;
    let sent = wire.bytes.lock().map_err(|_| "fixture lock")?.clone();
    assert!(
        matches!(result, Err(ChannelError::Disconnected(_))),
        "an interrupted write must refuse reuse, not accept a reply after appending a corrupt frame: {result:?}; wire={sent:?}"
    );
    assert_eq!(
        sent, partial,
        "no next request may follow the partial frame"
    );
    Ok(())
}

#[tokio::test]
async fn interrupted_notification_also_refuses_further_writes(
) -> Result<(), Box<dyn std::error::Error>> {
    let bounds = McpBounds::conservative()?;
    let wire = Arc::new(Wire::default());
    let mut channel = McpChannel::new(
        std::io::Cursor::new(Vec::new()),
        PartialWriter(wire.clone()),
        bounds,
    );
    let notification = grapt_mcp::initialized_notification(bounds)?;
    let mut first = Box::pin(channel.notify(notification, Duration::from_secs(1)));
    assert!(poll_fn(|cx| Poll::Ready(first.as_mut().poll(cx)))
        .await
        .is_pending());
    drop(first);
    let partial = wire.bytes.lock().map_err(|_| "fixture lock")?.clone();
    assert_eq!(partial.len(), 5);
    wire.unblocked.store(true, Ordering::Release);
    let notification = grapt_mcp::initialized_notification(bounds)?;
    assert!(matches!(
        channel.notify(notification, Duration::from_secs(1)).await,
        Err(ChannelError::Disconnected(_))
    ));
    assert_eq!(*wire.bytes.lock().map_err(|_| "fixture lock")?, partial);
    Ok(())
}

#[tokio::test]
async fn interrupted_partial_call_releases_its_lease_without_replaying(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_mcp::session::{CallOutcome, McpSession, SessionBounds};
    let bounds = McpBounds::conservative()?;
    let wire = Arc::new(Wire::default());
    let mut channel = McpChannel::new(
        std::io::Cursor::new(Vec::new()),
        PartialWriter(wire.clone()),
        bounds,
    );
    let tool = grapt_mcp::McpToolName::new("fixture_read").ok_or("fixture tool")?;
    let mut session = McpSession::declared(SessionBounds::new(1, 32)?);
    session.start()?;
    session.ready(vec![tool.clone()])?;
    let mut call = Box::pin(super::super::run_call(
        &mut channel,
        &mut session,
        bounds,
        &tool,
        "cancelled",
        serde_json::json!({}),
        Duration::from_secs(1),
    ));
    assert!(poll_fn(|cx| Poll::Ready(call.as_mut().poll(cx)))
        .await
        .is_pending());
    drop(call);
    assert_eq!(
        session.outcome("cancelled"),
        Some(CallOutcome::Disconnected)
    );
    let partial = wire.bytes.lock().map_err(|_| "fixture lock")?.clone();
    assert_eq!(partial.len(), 5);
    wire.unblocked.store(true, Ordering::Release);
    let result = super::super::run_call(
        &mut channel,
        &mut session,
        bounds,
        &tool,
        "successor",
        serde_json::json!({}),
        Duration::from_secs(1),
    )
    .await;
    assert!(
        matches!(result, Err(super::super::McpCallRefusal::Disconnected(_))),
        "{result:?}"
    );
    assert_eq!(
        session.outcome("successor"),
        Some(CallOutcome::Disconnected)
    );
    assert_eq!(*wire.bytes.lock().map_err(|_| "fixture lock")?, partial);
    Ok(())
}
