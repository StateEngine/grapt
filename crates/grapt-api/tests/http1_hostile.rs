//! Hostile HTTP/1 heads, refused by the configured bounds before the service.
//!
//! Each witness here is a CONTRAST rather than a bare assertion. Production A
//! refuses every credential, so every request that reaches the authenticated
//! service receives `401`; a request refused earlier receives `431`, or no
//! response at all. Asserting the hostile status alone would not say WHERE the
//! refusal happened, and "where" is the entire content of a transport bound.
//!
//! The control in each test is the same well-formed request that gets `401`. It
//! is not decoration: without it, a host that refused everything for any reason
//! would satisfy every hostile case.

mod host_support;

use std::time::Duration;

use host_support::{limits_with, start, RawConnection};

/// A well-formed request, which must reach the service and be refused there.
const WELL_FORMED: &[u8] = b"GET /witness HTTP/1.1\r\nHost: witness\r\n\r\n";

#[tokio::test]
async fn a_head_with_more_headers_than_the_bound_is_refused_before_the_service() {
    // Four headers admitted. `Host` plus five more is six, which is over.
    let limits = limits_with(4, 8192, Duration::from_secs(5));
    let host = start(limits).await;
    let address = host.ready().observed_address();

    let mut hostile_head = String::from("GET /witness HTTP/1.1\r\nHost: witness\r\n");
    for index in 0..5 {
        hostile_head.push_str(&format!("X-Filler-{index}: v\r\n"));
    }
    hostile_head.push_str("\r\n");

    let mut hostile = RawConnection::open(address).await;
    hostile.send(hostile_head.as_bytes()).await;
    assert_eq!(
        hostile.read_status().await,
        Some(431),
        "a head over the header-count bound must be refused by the parser"
    );

    // The control. It travels the same path and is refused one layer LATER, so
    // the 431 above is a statement about the bound and not about the host
    // refusing everything.
    let mut control = RawConnection::open(address).await;
    control.send(WELL_FORMED).await;
    assert_eq!(
        control.read_status().await,
        Some(401),
        "a well-formed request must reach the authenticated service"
    );

    drop(hostile);
    drop(control);
    let _ = host.stop().await;
}

#[tokio::test]
async fn a_head_larger_than_the_read_buffer_is_refused_before_the_service() {
    // The minimum buffer, and a head deliberately larger than it. The header
    // COUNT bound is set high enough that it cannot be what refuses this — the
    // two bounds must not be able to stand in for one another.
    let limits = limits_with(64, 8192, Duration::from_secs(5));
    let host = start(limits).await;
    let address = host.ready().observed_address();

    let mut oversized = String::from("GET /witness HTTP/1.1\r\nHost: witness\r\n");
    // Ten headers of roughly 1 KiB each: over the 8192-byte buffer, under the
    // 64-header count bound.
    for index in 0..10 {
        oversized.push_str(&format!("X-Big-{index}: {}\r\n", "p".repeat(1000)));
    }
    oversized.push_str("\r\n");

    let mut hostile = RawConnection::open(address).await;
    hostile.send(oversized.as_bytes()).await;
    let status = hostile.read_status().await;
    assert_ne!(
        status,
        Some(401),
        "an oversized head reached the authenticated service, so the read buffer \
         was not the bound in force"
    );
    assert!(
        matches!(status, Some(431) | None),
        "an oversized head must be refused or closed by the parser, not served: \
         {status:?}"
    );

    let mut control = RawConnection::open(address).await;
    control.send(WELL_FORMED).await;
    assert_eq!(
        control.read_status().await,
        Some(401),
        "a well-formed request must reach the authenticated service"
    );

    drop(hostile);
    drop(control);
    let _ = host.stop().await;
}

#[tokio::test(start_paused = true)]
async fn an_incomplete_head_is_closed_when_its_allowance_is_exhausted() {
    // Deterministic time: the allowance is advanced by this witness, never
    // waited out. A wall-clock sleep here would measure the machine.
    let allowance = Duration::from_secs(30);
    let limits = limits_with(64, 8192, allowance);
    let host = start(limits).await;
    let address = host.ready().observed_address();

    let mut slow = RawConnection::open(address).await;
    // A head that has begun and will never finish: no terminating blank line.
    slow.send(b"GET /witness HTTP/1.1\r\nHost: witness\r\n")
        .await;

    // Let the server admit the connection and arm its header deadline before the
    // clock moves. Advancing first would exhaust an allowance that had not
    // started, which is a different fact.
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(allowance + Duration::from_secs(1)).await;

    // BOUNDED, and the bound is the point. If the header deadline is not armed
    // at all, nothing ever closes this connection and an unbounded read would
    // hang the whole battery until its step timeout rather than reporting the
    // missing deadline. Under a paused clock a timeout fires exactly when the
    // runtime can make no other progress — which IS the hang condition — so this
    // is a hang detector rather than a race against the server.
    let closed = tokio::time::timeout(Duration::from_secs(60), slow.read_status()).await;
    match closed {
        Ok(None) => {}
        Ok(Some(status)) => panic!(
            "an incomplete head must close WITHOUT a response once its allowance \
             is exhausted; status {status} means the deadline never applied"
        ),
        Err(_) => panic!(
            "an incomplete head was neither closed nor answered after its \
             allowance was exhausted, so no header-read deadline was in force"
        ),
    }

    // The control, on the same paused clock: a complete head is served
    // immediately and never touches the deadline.
    let mut control = RawConnection::open(address).await;
    control.send(WELL_FORMED).await;
    assert_eq!(
        control.read_status().await,
        Some(401),
        "a complete head must be answered without waiting on the header allowance"
    );

    drop(slow);
    drop(control);
    let _ = host.stop().await;
}

#[tokio::test]
async fn a_known_oversized_content_length_is_refused_without_reading_the_body() {
    // The body bound is 1024 in the shared fixture. A declared 64 KiB body is
    // refused on its declaration; the bytes are never sent and never read.
    let limits = limits_with(64, 8192, Duration::from_secs(5));
    let host = start(limits).await;
    let address = host.ready().observed_address();

    let mut hostile = RawConnection::open(address).await;
    hostile
        .send(b"POST /witness HTTP/1.1\r\nHost: witness\r\nContent-Length: 65536\r\n\r\n")
        .await;
    // `401`, not `413`: this lane authenticates before it measures a body, so an
    // unauthenticated oversized upload is refused WITHOUT its body being read at
    // all. That is at least as strong as answering `413`, and it tells an
    // unauthenticated caller nothing about the host's bounds.
    assert_eq!(
        hostile.read_status().await,
        Some(401),
        "an unauthenticated request must be refused before its body is considered"
    );

    drop(hostile);
    let _ = host.stop().await;
}

#[tokio::test]
async fn only_the_two_exact_packaged_renderer_origins_receive_cors_authorization() {
    let host = start(limits_with(64, 8192, Duration::from_secs(5))).await;
    let address = host.ready().observed_address();
    for (origin, admitted) in [
        ("http://tauri.localhost", true),
        ("oc://renderer", true),
        ("oc://other", false),
        ("http://evil", false),
        ("oc://renderer.evil", false),
        ("oc://renderer/", false),
        ("OC://renderer", false),
        ("null", false),
        ("*", false),
        ("oc://renderer, http://tauri.localhost", false),
        ("oc://renderer\r\nOrigin: oc://renderer", false),
        ("oc://renderer\r\nOrigin: http://tauri.localhost", false),
    ] {
        let mut connection = RawConnection::open(address).await;
        connection.send(format!(
            "OPTIONS /witness HTTP/1.1\r\nHost: witness\r\nOrigin: {origin}\r\nAccess-Control-Request-Method: GET\r\nAccess-Control-Request-Headers: authorization, content-type\r\n\r\n"
        ).as_bytes()).await;
        let head = connection
            .read_head()
            .await
            .unwrap_or_else(|| panic!("no origin response: {origin}"));
        assert_eq!(
            head.lines().next(),
            Some(if admitted {
                "HTTP/1.1 204 No Content"
            } else {
                "HTTP/1.1 401 Unauthorized"
            }),
            "{origin}"
        );
        if admitted {
            assert!(
                head.contains(&format!("access-control-allow-origin: {origin}\r\n")),
                "{head}"
            );
            assert!(head.contains("vary: Origin\r\n"), "{head}");
        } else {
            assert!(
                !head.contains("access-control-allow-origin"),
                "foreign or repeated origin received CORS authorization: {head}"
            );
        }
    }
    let _ = host.stop().await;
}

#[tokio::test]
async fn renderer_preflights_validate_every_method_and_header_before_cors_authorization() {
    let host = start(limits_with(64, 8192, Duration::from_secs(5))).await;
    let address = host.ready().observed_address();
    for origin in ["http://tauri.localhost", "oc://renderer"] {
        for headers in [
            "Access-Control-Request-Method: CONNECT\r\n",
            "Access-Control-Request-Method: GET\r\nAccess-Control-Request-Method: CONNECT\r\n",
            "Access-Control-Request-Method: GET\r\nAccess-Control-Request-Headers: authorization, x-forbidden\r\n",
            "Access-Control-Request-Method: GET\r\nAccess-Control-Request-Headers: authorization\r\nAccess-Control-Request-Headers: x-forbidden\r\n",
            "Access-Control-Request-Headers: authorization\r\n",
        ] {
            let mut connection = RawConnection::open(address).await;
            connection.send(format!("OPTIONS /witness HTTP/1.1\r\nHost: witness\r\nOrigin: {origin}\r\n{headers}\r\n").as_bytes()).await;
            let head = connection.read_head().await.unwrap_or_else(|| panic!("no invalid preflight response"));
            assert_eq!(head.lines().next(), Some("HTTP/1.1 403 Forbidden"), "{head}");
            assert!(!head.contains("access-control-allow-"), "invalid preflight received CORS authorization: {head}");
        }
    }
    let _ = host.stop().await;
}
