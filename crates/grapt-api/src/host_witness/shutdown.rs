use core::time::Duration;
use std::net::SocketAddr;
use std::sync::Arc;

use super::doubles::{CountingAcceptSource, FailingAcceptSource, ManualDrain, NeverFiringDrain};
use super::fixtures::{
    fixture_service, limits_of, loopback_listener, start_host, two_seat_boundary, RouteLog,
};
use super::http1_client::{authenticated_get, RawConnection};
use crate::deadline::{DeadlineDriver, TokioDeadlineDriver};
use crate::host::{AcceptFailure, ApiHostExit, PreparedApiHost, RetainedInventory};
use crate::limits::ShutdownDrainAllowance;

/// How many sequential connections the retained-inventory witnesses drive.
///
/// Strictly more than `max_connections`, and by a wide margin. The defect this
/// witnesses could not appear at a population of one — with no earlier completed
/// connection there is nothing retained to miscount — and the first version of
/// the exactness witness drove exactly one connection for that reason. A law
/// about accumulation has to be witnessed where accumulation can happen.
const SEQUENTIAL_CONNECTIONS: usize = 40;

/// The connection bound the two retained-inventory witnesses run under.
const RETAINED_BOUND: usize = 4;

/// Drive `SEQUENTIAL_CONNECTIONS` connections, one at a time, each fully
/// completed and closed by the client before the next opens.
///
/// Sequential and fully completed is the whole point: at every instant at most
/// ONE connection is live, so a truthful host can never report more than one as
/// active, and `max_connections` is never approached let alone reached.
async fn drive_sequential_connections(address: SocketAddr, log: &RouteLog) {
    for index in 0..SEQUENTIAL_CONNECTIONS {
        let mut client = RawConnection::open(address).await;
        client.send(&authenticated_get("alpha")).await;
        let response = client.read_head().await;
        assert_eq!(
            response.status, 200,
            "connection {index} of {SEQUENTIAL_CONNECTIONS} was not served: {}",
            response.head
        );
        // Closed before the next opens, so the previous task is finished and
        // its permit returned well before another connection is admitted.
        drop(client);
    }
    assert_eq!(
        log.invocations(),
        SEQUENTIAL_CONNECTIONS,
        "every sequential connection must have routed exactly once"
    );
}

#[tokio::test]
async fn the_retained_connection_task_set_stays_within_the_connection_bound() {
    // R1 HOLD-1, measured where the law is decided.
    //
    // The reap sat behind an always-ready permit branch in a `biased` select, so
    // it ran only while the semaphore was saturated — never, under sequential
    // load. The root owner then retained one entry for every connection it had
    // ever admitted, and `drain` reported that inventory as the count still LIVE
    // at the deadline.
    //
    // The terminal receipt cannot witness this. At any population a test can
    // drive, a completed backlog is reaped far faster than a drain allowance can
    // expire, so the receipt reads `StoppedGracefully` and says nothing: measured
    // directly, the same run that retained FORTY tasks against a bound of four
    // returned a clean graceful exit. Review needed three thousand connections
    // before the deadline could beat the join loop. So this asserts the retained
    // set itself, through a `#[cfg(test)]` observer the root owner updates once
    // per accept iteration.
    let log = Arc::new(RouteLog::default());
    let limits = limits_of(RETAINED_BOUND, 8, 8, 1024);
    let inventory = Arc::new(RetainedInventory::default());
    let listener = loopback_listener().await;
    let service = fixture_service(Arc::clone(&log), two_seat_boundary(), &limits, false);
    let host = PreparedApiHost::new(
        listener,
        service,
        limits,
        Arc::new(TokioDeadlineDriver::new()),
    )
    .unwrap_or_else(|error| panic!("fixture host limits were refused: {error}"))
    .observe_retained(Arc::clone(&inventory))
    .start();
    let address = host.ready().observed_address();

    drive_sequential_connections(address, &log).await;

    let exit = host.stop().await;
    let high_watermark = inventory.high_watermark();
    assert!(
        high_watermark <= RETAINED_BOUND,
        "the root owner retained {high_watermark} connection tasks against a bound \
         of {RETAINED_BOUND}, after {SEQUENTIAL_CONNECTIONS} sequential connections \
         of which at most one was ever live at a time — completed tasks are not \
         being reaped, and whatever is still retained at a stop is reported as the \
         count LIVE at the deadline"
    );

    // Exact accounting must survive the reap moving: every connection is still
    // recorded exactly once, through the same path, wherever it was joined.
    match exit {
        ApiHostExit::StoppedGracefully {
            admitted_connections,
            completed_connections,
            failed_connections,
            admitted_requests,
            refused_requests,
        } => {
            assert_eq!(admitted_connections, SEQUENTIAL_CONNECTIONS as u64);
            assert_eq!(
                completed_connections, SEQUENTIAL_CONNECTIONS as u64,
                "every connection must be counted exactly once, whether it was \
                 reaped in the accept loop or in the drain"
            );
            assert_eq!(failed_connections, 0);
            assert_eq!(admitted_requests, SEQUENTIAL_CONNECTIONS as u64);
            assert_eq!(refused_requests, 0);
        }
        other => panic!(
            "a stop after {SEQUENTIAL_CONNECTIONS} clean connections must be graceful: {other:?}"
        ),
    }
}

#[tokio::test]
async fn the_terminal_receipt_never_reports_more_live_than_the_connection_bound() {
    // R1 HOLD-1. The root owner retained one JoinSet entry per connection it had
    // ever admitted, because the reap sat behind an always-ready permit branch in
    // a `biased` select and was polled only while the semaphore was saturated.
    // `drain` then snapshots `tasks.len()` as `active_at_deadline`, whose stated
    // meaning is "the exact number still live when the deadline fired" — so a
    // shutdown in which every response had already been delivered reported a
    // large backlog of connections aborted and joined, none of which was live.
    //
    // Forty sequential connections against a bound of four. At most one is ever
    // live, so ANY report above the bound is false by construction.
    let log = Arc::new(RouteLog::default());
    let limits = limits_of(RETAINED_BOUND, 8, 8, 1024);
    let drain = Arc::new(ManualDrain::new());
    let host = start_host(
        Arc::clone(&log),
        limits,
        false,
        Arc::clone(&drain) as Arc<dyn DeadlineDriver>,
    )
    .await;
    let address = host.ready().observed_address();

    drive_sequential_connections(address, &log).await;

    // Stop and exhaust the allowance immediately. With the defect the drain finds
    // a backlog of completed-but-unreaped tasks and the deadline beats the join
    // loop; with the reap in the accept loop there is nothing retained to find.
    // Exhaust the allowance BEFORE the stop, so the drain finds it already
    // spent on its very first poll. There is no race to lose: whatever the
    // drain then reports as live is what it genuinely believed was live.
    drain.arm();
    let exit = host.stop().await;

    match exit {
        ApiHostExit::StoppedGracefully {
            admitted_connections,
            completed_connections,
            failed_connections,
            admitted_requests,
            ..
        } => {
            assert_eq!(admitted_connections, SEQUENTIAL_CONNECTIONS as u64);
            assert_eq!(
                completed_connections, SEQUENTIAL_CONNECTIONS as u64,
                "completed accounting must stay exact once the reap moved"
            );
            assert_eq!(failed_connections, 0);
            assert_eq!(admitted_requests, SEQUENTIAL_CONNECTIONS as u64);
        }
        ApiHostExit::ForcedClose {
            admitted_connections,
            active_at_deadline,
            aborted_and_joined,
            ..
        } => {
            assert_eq!(admitted_connections, SEQUENTIAL_CONNECTIONS as u64);
            assert!(
                active_at_deadline <= RETAINED_BOUND as u64,
                "the receipt claims {active_at_deadline} connections were live at the \
                 deadline, but the bound is {RETAINED_BOUND} and every connection had \
                 already completed and closed — the retained task inventory is being \
                 counted as live"
            );
            assert!(
                aborted_and_joined <= RETAINED_BOUND as u64,
                "the receipt claims {aborted_and_joined} connections were aborted and \
                 joined, but at most {RETAINED_BOUND} could ever have been live"
            );
        }
        other => panic!("a stop after {SEQUENTIAL_CONNECTIONS} clean connections must reach a terminal, not fail: {other:?}"),
    }
}

#[tokio::test]
async fn a_real_exhausted_drain_allowance_reports_no_fabricated_backlog() {
    // The same law under the PRODUCTION driver and a genuinely short allowance,
    // rather than a witness-fired deadline. The review reproduction used exactly
    // this shape — an ordinary admitted allowance, no custom driver, no hostile
    // client — and it is the configuration a real operator would meet.
    let log = Arc::new(RouteLog::default());
    let mut limits = limits_of(RETAINED_BOUND, 8, 8, 1024);
    let Ok(exhausted) = ShutdownDrainAllowance::try_new(Duration::from_millis(1)) else {
        panic!("a one millisecond drain allowance must be admitted");
    };
    limits.shutdown_drain_allowance = exhausted;

    let host = start_host(
        Arc::clone(&log),
        limits,
        false,
        Arc::new(TokioDeadlineDriver::new()),
    )
    .await;
    let address = host.ready().observed_address();

    drive_sequential_connections(address, &log).await;

    match host.stop().await {
        ApiHostExit::StoppedGracefully {
            admitted_connections,
            completed_connections,
            ..
        } => {
            assert_eq!(admitted_connections, SEQUENTIAL_CONNECTIONS as u64);
            assert_eq!(completed_connections, SEQUENTIAL_CONNECTIONS as u64);
        }
        ApiHostExit::ForcedClose {
            active_at_deadline,
            aborted_and_joined,
            ..
        } => {
            // A forced close is admissible here — a 1 ms allowance may genuinely
            // expire — but only if what it names is genuinely live.
            assert!(
                active_at_deadline <= RETAINED_BOUND as u64
                    && aborted_and_joined <= RETAINED_BOUND as u64,
                "a run in which every response completed reported a forced-close \
                 backlog of {active_at_deadline} active and {aborted_and_joined} \
                 aborted against a bound of {RETAINED_BOUND}"
            );
        }
        other => panic!("an exhausted allowance must still reach a terminal: {other:?}"),
    }
}

#[tokio::test]
async fn a_stop_with_a_completing_response_drains_gracefully() {
    let log = Arc::new(RouteLog::default());
    let limits = limits_of(4, 8, 8, 1024);
    // A drain deadline that never fires within this witness, so "graceful" is
    // decided by the connection settling and not by a race.
    let host = start_host(Arc::clone(&log), limits, true, Arc::new(NeverFiringDrain)).await;
    let address = host.ready().observed_address();

    let mut client = RawConnection::open(address).await;
    client.send(&authenticated_get("alpha")).await;
    let response = client.read_head().await;
    assert_eq!(response.status, 200, "head was: {}", response.head);

    // Stop while the response body is still open, then let it finish. The
    // connection must keep being polled after `graceful_shutdown` or it never
    // settles.
    let stopping = tokio::spawn(host.stop());
    // Let the stop REACH the connection before the body completes, and the
    // ordering is the whole witness rather than tidiness. Releasing first lets
    // the connection settle on its own, so the "already signalled, keep polling
    // the same future" branch is never entered at all — and a mutation that
    // deleted that branch stayed GREEN here until this loop existed. On the
    // current-thread runtime a yield lets every ready task run once, so this
    // orders the stop ahead of the release rather than hoping for it, and QH15
    // going red is the standing proof that the branch really is reached.
    for _ in 0..128 {
        tokio::task::yield_now().await;
    }
    log.release_all();
    let tail = client.drain_to_close().await;

    // The body ARRIVED, complete. `held` is the frame the route emitted before
    // the stop; the zero-length chunk is what a connection writes only if it was
    // still being polled after its graceful shutdown was signalled. A connection
    // that stopped being polled would close having written the first and never
    // the second, and every count below would still be exactly right.
    assert!(
        tail.windows(4).any(|window| window == b"held"),
        "the response body was never delivered: {:?}",
        String::from_utf8_lossy(&tail)
    );
    assert!(
        tail.windows(5).any(|window| window == b"0\r\n\r\n"),
        "the chunked response never reached its terminating chunk, so the \
         connection was cut rather than drained: {:?}",
        String::from_utf8_lossy(&tail)
    );

    let joined = tokio::time::timeout(Duration::from_secs(10), stopping).await;
    let Ok(joined) = joined else {
        panic!(
            "the graceful drain never settled; a connection that is not polled \
             after `graceful_shutdown` never resolves"
        )
    };
    match joined {
        Ok(ApiHostExit::StoppedGracefully {
            admitted_connections,
            completed_connections,
            failed_connections,
            admitted_requests,
            refused_requests,
        }) => {
            assert_eq!(admitted_connections, 1, "exactly one socket was admitted");
            assert_eq!(
                completed_connections, 1,
                "the drained connection must be counted as COMPLETED; a connection \
                 that was cut, cancelled or never settled is a failure and must not \
                 be folded into the graceful count"
            );
            assert_eq!(
                failed_connections, 0,
                "no connection failed in a drain that completed"
            );
            assert_eq!(admitted_requests, 1, "exactly one request was admitted");
            assert_eq!(refused_requests, 0, "no bound refused anything");
        }
        Ok(other) => panic!("a response that completed before the deadline must drain: {other:?}"),
        Err(error) => panic!("the stop task did not join: {error}"),
    }
}

#[tokio::test]
async fn an_exhausted_drain_allowance_forces_close_with_an_exact_inventory() {
    let log = Arc::new(RouteLog::default());
    let limits = limits_of(4, 8, 8, 1024);
    // The witness owns the deadline and fires it explicitly.
    let drain = Arc::new(ManualDrain::new());
    let host = start_host(
        Arc::clone(&log),
        limits,
        true,
        Arc::clone(&drain) as Arc<dyn DeadlineDriver>,
    )
    .await;
    let address = host.ready().observed_address();

    let mut client = RawConnection::open(address).await;
    client.send(&authenticated_get("alpha")).await;
    let response = client.read_head().await;
    assert_eq!(response.status, 200, "head was: {}", response.head);

    // Exhaust the allowance with the response body still held and never
    // released, so the connection is genuinely live when the drain begins.
    drain.arm();

    match host.stop().await {
        ApiHostExit::ForcedClose {
            admitted_connections,
            completed_before_deadline,
            active_at_deadline,
            aborted_and_joined,
        } => {
            assert_eq!(admitted_connections, 1);
            assert_eq!(completed_before_deadline, 0);
            assert_eq!(
                active_at_deadline, 1,
                "the exact live count at the deadline must be reported"
            );
            assert_eq!(
                aborted_and_joined, active_at_deadline,
                "every task live at the deadline must be aborted AND joined"
            );
        }
        other => panic!(
            "a drain that ran out of allowance must never be reported as graceful: {other:?}"
        ),
    }

    log.release_all();
    drop(client);
}

#[tokio::test]
async fn a_classified_accept_failure_stops_the_host_without_a_retry_loop() {
    let log = Arc::new(RouteLog::default());
    let limits = limits_of(4, 8, 8, 1024);
    let source = Arc::new(FailingAcceptSource::new(AcceptFailure::PermissionDenied));
    let service = fixture_service(Arc::clone(&log), two_seat_boundary(), &limits, false);
    let host = PreparedApiHost::with_source(
        Box::new(CountingAcceptSource::new(Arc::clone(&source))),
        service,
        limits,
        Arc::new(TokioDeadlineDriver::new()),
    )
    .start();

    // Wait for the accept to have been ATTEMPTED before stopping. Stopping
    // first would set the stop flag before the loop's first iteration, and the
    // host would report a graceful stop — a green that says nothing about
    // accept-failure handling. This is the difference between witnessing the
    // law and witnessing the scheduler.
    while source.attempts() == 0 {
        tokio::task::yield_now().await;
    }

    match host.stop().await {
        ApiHostExit::Failed { stage, cause, .. } => {
            assert_eq!(stage, crate::host::HostFailureStage::Accept);
            assert_eq!(
                cause,
                crate::host::HostFailureCause::Accept(AcceptFailure::PermissionDenied)
            );
        }
        other => panic!("a classified accept failure must fail the host typed: {other:?}"),
    }
    assert_eq!(
        source.attempts(),
        1,
        "the accept loop must be fail-fast: a second accept is a retry policy \
         nobody measured"
    );
}
