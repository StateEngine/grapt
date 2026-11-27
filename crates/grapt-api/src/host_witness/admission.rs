use core::time::Duration;
use std::sync::Arc;

use super::doubles::ManualDrain;
use super::fixtures::{limits_of, start_host, RouteLog};
use super::http1_client::{authenticated_get, RawConnection};
use crate::deadline::{DeadlineDriver, TokioDeadlineDriver};
use crate::host::ApiHostExit;
use crate::http1::SEAT_CREDENTIAL_HEADER;

// ---------------------------------------------------------------------------
// The witnesses.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_real_http1_request_reaches_the_route_under_a_server_derived_principal() {
    let log = Arc::new(RouteLog::default());
    let limits = limits_of(4, 8, 4, 1024);
    let host = start_host(
        Arc::clone(&log),
        limits,
        false,
        Arc::new(TokioDeadlineDriver::new()),
    )
    .await;
    let address = host.ready().observed_address();

    // Readiness is a property of the receipt, and it names the bounds in force
    // rather than a health state.
    assert_eq!(host.ready().limits(), &limits);

    let mut client = RawConnection::open(address).await;
    client.send(&authenticated_get("alpha")).await;
    let response = client.read_head().await;

    assert_eq!(response.status, 200, "head was: {}", response.head);
    assert_eq!(log.invocations(), 1, "the route must run exactly once");
    assert_eq!(
        log.recipients(),
        vec!["seat-alpha".to_owned()],
        "the route must see the SERVER-derived recipient"
    );

    drop(client);
    match host.stop().await {
        ApiHostExit::StoppedGracefully {
            admitted_connections,
            admitted_requests,
            refused_requests,
            ..
        } => {
            assert_eq!(admitted_connections, 1);
            assert_eq!(admitted_requests, 1);
            assert_eq!(refused_requests, 0);
        }
        other => panic!("an explicit stop with no live work must be graceful: {other:?}"),
    }
}

#[tokio::test]
async fn a_forged_identity_in_request_data_never_replaces_the_sealed_principal() {
    let log = Arc::new(RouteLog::default());
    let limits = limits_of(4, 8, 4, 1024);
    let host = start_host(
        Arc::clone(&log),
        limits,
        false,
        Arc::new(TokioDeadlineDriver::new()),
    )
    .await;
    let address = host.ready().observed_address();

    // Authenticated as alpha, while every caller-authored channel names beta:
    // a header, a query string and a path-shaped value.
    let mut client = RawConnection::open(address).await;
    client
        .send(
            format!(
                "GET /witness?recipient=seat-beta HTTP/1.1\r\nHost: witness\r\n\
                 {SEAT_CREDENTIAL_HEADER}: alpha\r\nX-Seat-Recipient: seat-beta\r\n\
                 X-Authenticated-Client: cli-1\r\n\r\n"
            )
            .as_bytes(),
        )
        .await;
    let response = client.read_head().await;
    assert_eq!(response.status, 200, "head was: {}", response.head);
    assert_eq!(
        log.recipients(),
        vec!["seat-alpha".to_owned()],
        "a caller-authored recipient must never reach the route's principal"
    );

    // And an unauthenticated request does not invoke the route at all.
    let before = log.invocations();
    let mut anonymous = RawConnection::open(address).await;
    anonymous
        .send(b"GET /witness HTTP/1.1\r\nHost: witness\r\nConnection: close\r\n\r\n")
        .await;
    let refused = anonymous.read_head().await;
    assert_eq!(refused.status, 401, "head was: {}", refused.head);
    assert!(refused
        .head
        .to_ascii_lowercase()
        .contains("content-type: application/json"));
    let body = anonymous.drain_to_close().await;
    let decoded: serde_json::Value = serde_json::from_slice(&body)
        .unwrap_or_else(|error| panic!("real HTTP authentication refusal was not JSON: {error}"));
    assert_eq!(
        decoded,
        serde_json::json!({"_tag":"UnauthorizedError","message":"authentication required"})
    );
    assert_eq!(
        log.invocations(),
        before,
        "an unauthenticated request must not invoke the route"
    );

    drop(client);
    drop(anonymous);
    let _ = host.stop().await;
}

#[tokio::test]
async fn a_connection_routes_exactly_its_budget_and_then_refuses_and_closes() {
    let log = Arc::new(RouteLog::default());
    // Two requests per connection, so the third is the refusal.
    let limits = limits_of(4, 2, 8, 1024);
    let host = start_host(
        Arc::clone(&log),
        limits,
        false,
        Arc::new(TokioDeadlineDriver::new()),
    )
    .await;
    let address = host.ready().observed_address();

    let mut client = RawConnection::open(address).await;
    for request in 1..=2 {
        client.send(&authenticated_get("alpha")).await;
        let response = client.read_head().await;
        assert_eq!(
            response.status, 200,
            "request {request} of the budget must route"
        );
    }
    assert_eq!(log.invocations(), 2, "exactly the budget must have routed");

    client.send(&authenticated_get("alpha")).await;
    let refused = client.read_head().await;
    assert_eq!(
        refused.status, 503,
        "request 3 of 2 must be a bounded overload refusal, not a route"
    );
    assert!(
        refused.says_close(),
        "a connection that can never serve again must be told to close: {}",
        refused.head
    );
    assert_eq!(
        log.invocations(),
        2,
        "the refused request must not have reached the route"
    );

    drop(client);
    let _ = host.stop().await;
}

#[tokio::test]
async fn global_overload_refuses_immediately_and_recovers_when_a_body_ends() {
    let log = Arc::new(RouteLog::default());
    // One in-flight request globally, and generous per-connection budgets, so
    // the ONLY bound in play is the global one.
    let limits = limits_of(4, 8, 1, 1024);
    let host = start_host(
        Arc::clone(&log),
        limits,
        true,
        Arc::new(TokioDeadlineDriver::new()),
    )
    .await;
    let address = host.ready().observed_address();

    // One request whose response body is held open. Its head has arrived, so a
    // bound that released on headers would already have let the next one in.
    let mut holder = RawConnection::open(address).await;
    holder.send(&authenticated_get("alpha")).await;
    let held = holder.read_head().await;
    assert_eq!(held.status, 200, "head was: {}", held.head);

    let mut second = RawConnection::open(address).await;
    second.send(&authenticated_get("alpha")).await;
    let refused = second.read_head().await;
    assert_eq!(
        refused.status, 503,
        "a second request must be refused while the first body is live: {}",
        refused.head
    );
    assert!(
        !refused.says_close(),
        "transient overload must leave the connection usable: {}",
        refused.head
    );
    assert_eq!(
        log.invocations(),
        1,
        "the refused request must not have reached the route"
    );

    // Releasing the held body returns the permit, and later work is admitted.
    log.release_all();
    let _ = holder.drain_to_close().await;

    let mut third = RawConnection::open(address).await;
    third.send(&authenticated_get("alpha")).await;
    let admitted = third.read_head().await;
    assert_eq!(
        admitted.status, 200,
        "a returned permit must admit later work: {}",
        admitted.head
    );

    log.release_all();
    drop(holder);
    drop(second);
    drop(third);
    let _ = host.stop().await;
}

#[tokio::test]
async fn a_streaming_body_over_the_bound_is_refused_and_the_route_sees_no_more_than_the_bound() {
    let log = Arc::new(RouteLog::default());
    // A sixteen byte body bound, and a chunked body carrying more.
    let limits = limits_of(4, 8, 8, 16);
    let host = start_host(
        Arc::clone(&log),
        limits,
        false,
        Arc::new(TokioDeadlineDriver::new()),
    )
    .await;
    let address = host.ready().observed_address();

    let mut client = RawConnection::open(address).await;
    client
        .send(
            format!(
                "POST /sink HTTP/1.1\r\nHost: witness\r\n{SEAT_CREDENTIAL_HEADER}: alpha\r\n\
                 Transfer-Encoding: chunked\r\n\r\n20\r\n\
                 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n0\r\n\r\n"
            )
            .as_bytes(),
        )
        .await;
    let response = client.read_head().await;
    assert_eq!(
        response.status, 413,
        "an over-limit streaming body must be refused: {}",
        response.head
    );
    assert!(
        log.body_bytes_seen() <= 16,
        "the route observed {} bytes past a sixteen byte bound",
        log.body_bytes_seen()
    );

    // And a known oversized `Content-Length` is refused ahead of the route
    // entirely — a different mechanism, and a different count.
    let before = log.invocations();
    let mut declared = RawConnection::open(address).await;
    declared
        .send(
            format!(
                "POST /sink HTTP/1.1\r\nHost: witness\r\n{SEAT_CREDENTIAL_HEADER}: alpha\r\n\
                 Content-Length: 4096\r\n\r\n"
            )
            .as_bytes(),
        )
        .await;
    let pre_refused = declared.read_head().await;
    assert_eq!(
        pre_refused.status, 413,
        "a known oversized Content-Length must be refused: {}",
        pre_refused.head
    );
    assert_eq!(
        log.invocations(),
        before,
        "a known oversized body must not invoke the route at all"
    );

    drop(client);
    drop(declared);
    let _ = host.stop().await;
}

#[tokio::test]
async fn the_connection_bound_is_never_exceeded_and_a_release_admits_the_next_client() {
    let log = Arc::new(RouteLog::default());
    // Exactly one connection, and held bodies so the first never settles.
    let limits = limits_of(1, 8, 8, 1024);
    let host = start_host(
        Arc::clone(&log),
        limits,
        true,
        Arc::new(TokioDeadlineDriver::new()),
    )
    .await;
    let address = host.ready().observed_address();

    let mut first = RawConnection::open(address).await;
    first.send(&authenticated_get("alpha")).await;
    let served = first.read_head().await;
    assert_eq!(served.status, 200, "head was: {}", served.head);

    // A second client connects — the kernel accepts it into the backlog — but
    // the host must not ADMIT it into a task while the bound is full. The
    // measurable consequence is that its request is not routed.
    let mut second = RawConnection::open(address).await;
    second.send(&authenticated_get("alpha")).await;
    let blocked = tokio::time::timeout(Duration::from_millis(250), second.read_head()).await;
    assert!(
        blocked.is_err(),
        "a second connection was admitted past a bound of one"
    );
    assert_eq!(
        log.invocations(),
        1,
        "only the admitted connection's request may route"
    );

    // Releasing the first connection returns its permit; the waiting client is
    // then admitted and served.
    log.release_all();
    let _ = first.drain_to_close().await;
    drop(first);
    let admitted = second.read_head().await;
    assert_eq!(
        admitted.status, 200,
        "a returned connection permit must admit the waiting client: {}",
        admitted.head
    );

    log.release_all();
    drop(second);
    let _ = host.stop().await;
}

#[tokio::test]
async fn a_connection_permit_is_held_until_its_task_ends() {
    // The other half of the connection bound, and a DIFFERENT observable.
    // `the_connection_bound_is_never_exceeded_and_a_release_admits_the_next_client`
    // measures whether a second client is served; this measures whether it was
    // ever ADMITTED, by reading the terminal receipt's admitted-connection
    // count. The distinction matters because a permit released when the response
    // was written — rather than when the task ended — lets a replacement socket
    // into a task while the first is still open, and the receipt is where that
    // shows up as a number.
    let log = Arc::new(RouteLog::default());
    let limits = limits_of(1, 8, 8, 1024);
    let drain = Arc::new(ManualDrain::new());
    let host = start_host(
        Arc::clone(&log),
        limits,
        true,
        Arc::clone(&drain) as Arc<dyn DeadlineDriver>,
    )
    .await;
    let address = host.ready().observed_address();

    let mut first = RawConnection::open(address).await;
    first.send(&authenticated_get("alpha")).await;
    let served = first.read_head().await;
    assert_eq!(served.status, 200, "head was: {}", served.head);

    // A second client, whose request must never be admitted into a task while
    // the first connection is still live.
    let mut second = RawConnection::open(address).await;
    second.send(&authenticated_get("alpha")).await;
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }

    drain.arm();
    let exit = host.stop().await;

    let admitted = match exit {
        ApiHostExit::ForcedClose {
            admitted_connections,
            ..
        } => admitted_connections,
        ApiHostExit::StoppedGracefully {
            admitted_connections,
            ..
        } => admitted_connections,
        other => panic!("the host failed rather than reaching a terminal: {other:?}"),
    };
    assert_eq!(
        admitted, 1,
        "a second socket was admitted while the first connection task was still \
         live, so its permit was released before the task ended"
    );

    log.release_all();
    drop(first);
    drop(second);
}
