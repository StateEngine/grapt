//! Account cooldown crosses root/child admission, without crossing accounts.
use super::*;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

type Server = tokio::task::JoinHandle<Result<(), String>>;

fn poll<F: Future + ?Sized>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

fn account(base: &str, name: &str) -> Result<AttemptOpener, String> {
    AttemptOpener::codex_loopback(
        deadlines(),
        CodexLimits::new(capture_capacity(1)),
        base,
        route(),
        credential(name),
    )
    .map(|opener| opener.with_child_capacity(capture_capacity(3)))
    .map_err(|error| error.to_string())
}

async fn fixture() -> Result<(AttemptOpener, Server), String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| e.to_string())?;
    let base = format!(
        "http://{}",
        listener.local_addr().map_err(|e| e.to_string())?
    );
    let server = tokio::spawn(async move {
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let mut request = [0_u8; 4096];
        let count = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut request))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        assert!(count > 0, "provider request missing");
        let body = r#"{"error":{"type":"rate_limit_exceeded"}}"#;
        let response = format!("HTTP/1.1 429 Too Many Requests\r\ncontent-type: application/json\r\ncontent-length: {}\r\nretry-after: 30\r\nconnection: close\r\n\r\n{body}", body.len());
        socket
            .write_all(response.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        socket.shutdown().await.map_err(|e| e.to_string())
    });
    Ok((account(&base, "acct_u2")?, server))
}

fn authority() -> PhysicalAttemptAuthoritySnapshot {
    authority_of(issue(
        &ModelRegistry::install(catalog("account-cooldown")),
        "acct_u2",
    ))
}

async fn refuse(opener: &AttemptOpener, child: bool) -> Result<(), String> {
    let capacity = if child {
        opener
            .reserve_child_capacity_observed(capture_capacity(3), |_, _| {})
            .await
    } else {
        opener.reserve_capacity().await
    }
    .map_err(|e| e.to_string())?;
    let authority = authority();
    let request = request_for(&authority);
    let (_, mut reader) = opener
        .open_reserved(attempt("pat_rate_limit"), authority, request, capacity)
        .map_err(|e| e.to_string())?
        .into_parts();
    let fragments = drain(&mut reader).await;
    assert!(
        fragments.iter().any(|fragment| matches!(
            fragment,
            AttemptFragment::ProviderFailure {
                class: crate::ProviderFailureClass::RateLimit { .. },
                ..
            }
        )),
        "actual HTTP 429 did not classify as rate limit"
    );
    Ok(())
}

async fn limited(child: bool) -> Result<AttemptOpener, String> {
    let (opener, server) = fixture().await?;
    refuse(&opener, child).await?;
    server.await.map_err(|e| e.to_string())??;
    // HTTP classification uses the real clock; admission is then advanced
    // explicitly, so sleeps or network scheduling cannot qualify assertions.
    tokio::time::pause();
    Ok(opener)
}

#[tokio::test]
async fn child_429_delays_a_new_root_until_the_account_cools_down() -> Result<(), String> {
    let opener = limited(true).await?;
    let mut root = Box::pin(opener.reserve_capacity());
    assert!(poll(root.as_mut()).is_pending(), "root bypassed child 429");
    tokio::time::advance(Duration::from_secs(30)).await;
    assert!(matches!(poll(root.as_mut()), Poll::Ready(Ok(_))));
    Ok(())
}

#[tokio::test]
async fn root_429_delays_a_new_child_until_the_account_cools_down() -> Result<(), String> {
    let opener = limited(false).await?;
    let mut child =
        Box::pin(opener.reserve_child_capacity_observed(capture_capacity(3), |_, _| {}));
    assert!(poll(child.as_mut()).is_pending(), "child bypassed root 429");
    tokio::time::advance(Duration::from_secs(30)).await;
    assert!(matches!(poll(child.as_mut()), Poll::Ready(Ok(_))));
    Ok(())
}

#[tokio::test]
async fn rate_limiting_one_account_leaves_another_accounts_pools_available() -> Result<(), String> {
    let limited = limited(true).await?;
    let mut waiting = Box::pin(limited.reserve_capacity());
    assert!(poll(waiting.as_mut()).is_pending());
    let other = account("http://127.0.0.1:1", "acct_unaffected")?;
    let mut root = Box::pin(other.reserve_capacity());
    let mut child = Box::pin(other.reserve_child_capacity_observed(capture_capacity(3), |_, _| {}));
    assert!(matches!(poll(root.as_mut()), Poll::Ready(Ok(_))));
    assert!(matches!(poll(child.as_mut()), Poll::Ready(Ok(_))));
    assert!(
        poll(waiting.as_mut()).is_pending(),
        "other account erased cooldown"
    );
    Ok(())
}

#[derive(Debug, Default)]
struct DispatchClock(AtomicUsize);
impl grapt_types::ClockAuthority for DispatchClock {
    fn now(&self) -> grapt_types::Timestamp {
        self.0.fetch_add(1, Ordering::SeqCst);
        grapt_types::Timestamp::from_millis(100)
    }
}

#[tokio::test]
async fn a_root_staged_before_429_rechecks_cooldown_at_dispatch() -> Result<(), String> {
    let (opener, server) = fixture().await?;
    let clock = Arc::new(DispatchClock::default());
    let (observer, _samples) = crate::DispatchObserver::bounded(clock.clone());
    let opener = opener.with_dispatch_observer(observer);
    let authority = authority();
    let request = request_for(&authority);
    let (_, mut reader) = opener
        .open(attempt("pat_previously_staged"), authority, request)
        .map_err(|e| e.to_string())?
        .into_parts();
    refuse(&opener, true).await?;
    server.await.map_err(|e| e.to_string())??;
    tokio::time::pause();
    let before = clock.0.load(Ordering::SeqCst);
    let mut next = Box::pin(reader.next_fragment());
    assert!(poll(next.as_mut()).is_pending());
    assert_eq!(
        clock.0.load(Ordering::SeqCst),
        before,
        "staged root dispatched during account cooldown"
    );
    tokio::time::advance(Duration::from_secs(30)).await;
    // Advance can return before the timer driver wakes this send. Drive the
    // same future with the executor's waker until dispatch, under a bound.
    tokio::time::timeout(
        Duration::from_secs(1),
        std::future::poll_fn(|context| {
            let _ = next.as_mut().poll(context);
            if clock.0.load(Ordering::SeqCst) > before {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }),
    )
    .await
    .map_err(|_| "staged root did not resume after cooldown".to_owned())?;
    Ok(())
}
