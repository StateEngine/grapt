use super::*;

#[test]
fn accept_failures_are_classified_by_kind_and_never_by_text() {
    // The kinds a real socket will not produce on demand, asserted through
    // the pure classifier. A policy that could only be exercised by
    // exhausting a machine is a policy nobody exercises.
    let cases = [
        (
            io::ErrorKind::PermissionDenied,
            AcceptFailure::PermissionDenied,
        ),
        (io::ErrorKind::OutOfMemory, AcceptFailure::ResourceExhausted),
        (io::ErrorKind::StorageFull, AcceptFailure::ResourceExhausted),
        (
            io::ErrorKind::ConnectionAborted,
            AcceptFailure::ConnectionAborted,
        ),
        (
            io::ErrorKind::BrokenPipe,
            AcceptFailure::OtherIo {
                kind: io::ErrorKind::BrokenPipe,
            },
        ),
    ];
    for (kind, expected) in cases {
        assert_eq!(
            AcceptFailure::from_kind(kind),
            expected,
            "the {kind:?} accept failure was misclassified"
        );
    }
}

#[tokio::test]
async fn a_host_whose_root_handle_is_gone_reports_the_loss_rather_than_an_inventory() {
    // Unreachable through the public API — `stop` consumes the owner — so the
    // state is constructed directly, exactly as the accepted location crate
    // witnesses `AcceptError::Closed`. Without this the variant would be a
    // reason with no producer, and the alternative it replaced was worse: the
    // first candidate reported a join failure as `LiveInventory`, which is a
    // COUNT, and a task that never reported has told us nothing to count.
    let (stop, _receiver) = watch::channel(false);
    let host = RunningApiHost {
        stop,
        root: None,
        ready: ApiHostReady {
            observed_address: match "127.0.0.1:1".parse::<SocketAddr>() {
                Ok(address) => address,
                Err(error) => panic!("the fixture address did not parse: {error}"),
            },
            limits: fixture_limits(),
        },
    };
    match host.stop().await {
        ApiHostExit::Failed { stage, cause, .. } => {
            assert_eq!(stage, HostFailureStage::Root);
            assert_eq!(cause, HostFailureCause::RootTaskLost(RootLoss::Detached));
        }
        other => panic!("a host with no root task must report the loss: {other:?}"),
    }
}

/// The smallest admitted bound set, for the constructed-state witness above.
fn fixture_limits() -> ApiHostLimits {
    let bound = |what: &str| -> ! { panic!("the fixture bound for {what} was refused") };
    let Ok(max_connections) = crate::limits::MaxConnections::try_new(1) else {
        bound("max_connections")
    };
    let Ok(max_requests_per_connection) = crate::limits::MaxRequestsPerConnection::try_new(1)
    else {
        bound("max_requests_per_connection")
    };
    let Ok(max_inflight_requests) = crate::limits::MaxInflightRequests::try_new(1) else {
        bound("max_inflight_requests")
    };
    let Ok(max_request_head_bytes) = crate::limits::MaxRequestHeadBytes::try_new(8192) else {
        bound("max_request_head_bytes")
    };
    let Ok(max_headers) = crate::limits::MaxHeaders::try_new(1) else {
        bound("max_headers")
    };
    let Ok(max_request_body_bytes) = crate::limits::MaxRequestBodyBytes::try_new(1) else {
        bound("max_request_body_bytes")
    };
    let Ok(header_read_allowance) =
        crate::limits::HeaderReadAllowance::try_new(core::time::Duration::from_secs(1))
    else {
        bound("header_read_allowance")
    };
    let Ok(shutdown_drain_allowance) =
        crate::limits::ShutdownDrainAllowance::try_new(core::time::Duration::from_secs(1))
    else {
        bound("shutdown_drain_allowance")
    };
    let Ok(stream_write_allowance) =
        crate::limits::StreamWriteAllowance::try_new(core::time::Duration::from_secs(1))
    else {
        bound("stream_write_allowance")
    };
    ApiHostLimits {
        max_connections,
        max_requests_per_connection,
        max_inflight_requests,
        max_inflight_streams: crate::MaxInflightStreams::disabled(),
        max_request_head_bytes,
        max_headers,
        max_request_body_bytes,
        header_read_allowance,
        shutdown_drain_allowance,
        stream_write_allowance,
    }
}

#[test]
fn a_closed_listener_is_its_own_accept_class() {
    // `AcceptError::Closed` is not an I/O kind and must not be given a
    // fabricated one. The accepted location crate states it as a distinct
    // typed refusal, and folding it into `OtherIo` would invent a kind the
    // operating system never reported.
    let local = match "127.0.0.1:1".parse::<SocketAddr>() {
        Ok(address) => address,
        Err(error) => panic!("the fixture address did not parse: {error}"),
    };
    assert_eq!(
        AcceptFailure::classify(AcceptError::Closed { local }),
        AcceptFailure::ListenerClosed
    );
}
