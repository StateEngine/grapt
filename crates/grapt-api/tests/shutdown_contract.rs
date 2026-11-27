//! Stop, listener linearity, and what a terminated host gives back.
//!
//! These need no principal: they are about the socket and the root task, not
//! about who was allowed to talk to them.

mod host_support;

use std::sync::Arc;
use std::time::Duration;

use grapt_api::{ApiHostExit, PreparedApiHost, TokioDeadlineDriver};
use grapt_location::{
    BindHost, BindLimits, BindPort, ListenerBindPlan, ListenerBinder, LiteralAddressResolver,
    MaxBindAttempts, MaxResolvedAddresses,
};
use host_support::{limits_with, loopback_listener, production_service, start, RawConnection};

#[tokio::test]
async fn a_stop_cancels_a_pending_accept_and_joins() {
    let limits = limits_with(64, 8192, Duration::from_secs(5));
    let host = start(limits).await;
    let address = host.ready().observed_address();

    // The control FIRST: prove the address really is being served, so the
    // silence asserted at the end is a consequence of the stop rather than of a
    // host that never worked.
    let mut live = RawConnection::open(address).await;
    live.send(b"GET /witness HTTP/1.1\r\nHost: witness\r\n\r\n")
        .await;
    assert_eq!(
        live.read_status().await,
        Some(401),
        "the host must be serving before the stop is meaningful"
    );
    drop(live);

    // The client is gone, so the root task is parked inside `accept` again. A
    // stop must CANCEL that accept rather than wait for a connection that will
    // never arrive — which is what this witness would time out on if the stop
    // were not part of the accept select.
    let exit = tokio::time::timeout(Duration::from_secs(10), host.stop()).await;
    match exit {
        Ok(ApiHostExit::StoppedGracefully {
            admitted_connections,
            admitted_requests,
            refused_requests,
            ..
        }) => {
            assert_eq!(
                admitted_connections, 1,
                "the control connection was admitted"
            );
            assert_eq!(admitted_requests, 1);
            assert_eq!(refused_requests, 0);
        }
        Ok(other) => panic!("a stop with nothing in flight must be graceful: {other:?}"),
        Err(_) => panic!("a stop did not cancel a pending accept; the root never joined"),
    }

    // And no later socket is admitted. The listener was dropped BEFORE the
    // drain, so there is no longer anything listening: the connect itself is
    // refused. If a connect somehow succeeds — a stray handle, or a backlog
    // entry left behind — the request must still go unanswered.
    assert_unserved(address).await;
}

/// The address serves nothing: connect is refused, or nothing answers.
async fn assert_unserved(address: std::net::SocketAddr) {
    let Some(mut late) = RawConnection::try_open(address).await else {
        // Refused at the handshake. The listening socket is gone, which is the
        // strongest form of "no later socket is admitted".
        return;
    };
    late.send(b"GET /witness HTTP/1.1\r\nHost: witness\r\n\r\n")
        .await;
    let answered = tokio::time::timeout(Duration::from_millis(250), late.read_status()).await;
    assert!(
        matches!(answered, Ok(None) | Err(_)),
        "a socket was served after the host reached its terminal: {answered:?}"
    );
}

#[tokio::test]
async fn the_observed_address_can_be_rebound_after_the_host_has_joined() {
    // The linearity of the listener, stated as an OBSERVABLE consequence rather
    // than only as a compile-fail probe. The host consumed the listener; when it
    // joins, the socket is released, and a new location owner can take the same
    // address. If the host had kept a second handle, this bind would fail.
    let limits = limits_with(64, 8192, Duration::from_secs(5));
    let listener = loopback_listener().await;
    let address = listener.receipt().observed();
    let service = production_service(&limits);
    let host = PreparedApiHost::new(
        listener,
        service,
        limits,
        Arc::new(TokioDeadlineDriver::new()),
    )
    .unwrap_or_else(|error| panic!("fixture host limits were refused: {error}"))
    .start();
    assert_eq!(host.ready().observed_address(), address);

    match host.stop().await {
        ApiHostExit::StoppedGracefully { .. } => {}
        other => panic!("an idle stop must be graceful: {other:?}"),
    }

    // Rebinding the EXACT observed port, which is only possible because nothing
    // still holds it.
    let Some(port) = std::num::NonZeroU16::new(address.port()) else {
        panic!("an ephemeral bind must not report port zero");
    };
    let (Ok(addresses), Ok(attempts)) = (MaxResolvedAddresses::new(4), MaxBindAttempts::new(4))
    else {
        panic!("the fixture bind limits must be admitted");
    };
    let plan = ListenerBindPlan::new(
        BindHost::Ip(address.ip()),
        BindPort::Fixed(port),
        BindLimits::new(addresses, attempts),
    );
    let binder = ListenerBinder::new(Arc::new(LiteralAddressResolver));
    match binder.bind(&plan).await {
        Ok(rebound) => {
            assert_eq!(
                rebound.receipt().observed(),
                address,
                "the rebound listener must hold the same address the host released"
            );
            let closed = rebound.close();
            assert_eq!(closed.receipt().observed(), address);
        }
        Err(error) => panic!(
            "the address the host released could not be rebound: {error}. The host \
             is holding a socket it was supposed to have given up."
        ),
    }
}

#[tokio::test]
async fn a_dropped_host_stops_serving_and_returns_no_receipt() {
    // `Drop` is a backstop and nothing more. It has no receipt to give — a drain
    // that needs I/O cannot be assumed to complete inside `Drop` — but it must
    // still stop the host, or a forgotten owner would leave a listening socket
    // and a live task behind.
    let limits = limits_with(64, 8192, Duration::from_secs(5));
    let host = start(limits).await;
    let address = host.ready().observed_address();

    drop(host);

    // The root is aborted, so nothing answers. Yielding first gives the abort a
    // chance to land, which is what makes this a statement about `Drop` rather
    // than about scheduling.
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    assert_unserved(address).await;
}
