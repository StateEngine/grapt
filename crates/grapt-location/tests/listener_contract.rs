//! The acquisition contract, proved against real sockets.
//!
//! Every test here binds loopback or a documentation-range address. Nothing
//! reaches the network, nothing resolves a name through the platform, and no
//! test guesses a "probably free" port: where a specific port is needed, it is
//! obtained by binding ephemerally first and reading the receipt.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::num::NonZeroU16;
use std::sync::Arc;

use grapt_location::{
    AdapterUnavailableReason, BindError, BindHost, BindHostName, BindLimits, BindPort,
    BoundListener, DeterministicNameResolver, ListenerBindPlan, ListenerBinder,
    LiteralAddressResolver, MaxBindAttempts, MaxResolvedAddresses, ResolveError,
};
use tokio::net::TcpStream;

mod support;

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// Compile-time proof that every owner the acceptance corpus names is a real
/// zero-argument function in THIS target.
///
/// `#[tokio::test] async fn f()` expands to `#[test] fn f()`, so each of these
/// coerces to `fn()`. Rename or delete any of them and this file stops
/// compiling — which is a kill, not a silent pass. A helper cannot be
/// substituted here either: the helpers in this file all take arguments or
/// return values, so they do not have this type.
///
/// This is the structural half of the R1 correction. The string half — that the
/// inventory's `owner` values are these same names, and that they carry a test
/// attribute rather than merely existing — is proved by
/// `the_corpus_owners_are_real_tests_in_this_target` below.
const _CORPUS_OWNERS_EXIST_IN_THIS_TARGET: [fn(); support::OWNED_SCENARIO_COUNT] = [
    a_fixed_loopback_bind_receives_exactly_the_port_it_asked_for,
    an_ephemeral_bind_observes_a_real_port_and_keeps_the_ephemeral_intent,
    a_held_port_is_refused_as_address_in_use_and_its_owner_is_undamaged,
    a_resolution_over_its_bound_is_refused_before_any_bind_is_attempted,
];

/// Every owner the corpus names is a test in this target, and is one of the
/// functions bound above.
///
/// The compile-time array proves those four identifiers exist. This proves the
/// inventory's `owner` strings are exactly those identifiers AND that each
/// carries a test attribute, so a scenario can never be owned by a helper.
#[test]
fn the_corpus_owners_are_real_tests_in_this_target() {
    let source = include_str!("listener_contract.rs");
    let tests = support::test_function_names(source);
    assert!(
        tests.len() >= support::OWNED_SCENARIO_COUNT,
        "the target parser found {} tests, which cannot cover {} owners",
        tests.len(),
        support::OWNED_SCENARIO_COUNT
    );
    for scenario in support::OWNED_SCENARIOS.iter() {
        assert!(
            tests.iter().any(|name| name == scenario.owner),
            "scenario {:?} names owner {:?}, which is not a test in target {:?}; \
             tests found: {:?}",
            scenario.id,
            scenario.owner,
            support::CORPUS_TEST_TARGET,
            tests
        );
    }
    // A helper must not be reachable through this parser, or the guarantee
    // above is weaker than it reads.
    assert!(
        !tests.iter().any(|name| name == "bind_or_panic"),
        "the target parser admitted a helper as a test; the attribute walk is broken"
    );
}

fn limits(resolved: usize, attempts: usize) -> BindLimits {
    let resolved = match MaxResolvedAddresses::new(resolved) {
        Ok(bound) => bound,
        Err(error) => panic!("resolution bound refused: {error}"),
    };
    let attempts = match MaxBindAttempts::new(attempts) {
        Ok(bound) => bound,
        Err(error) => panic!("attempt bound refused: {error}"),
    };
    BindLimits::new(resolved, attempts)
}

fn fixed(port: u16) -> BindPort {
    match NonZeroU16::new(port) {
        Some(port) => BindPort::Fixed(port),
        None => panic!("this test asked for a zero fixed port, which cannot exist"),
    }
}

fn name(value: &str) -> BindHostName {
    match BindHostName::new(value) {
        Ok(name) => name,
        Err(error) => panic!("hostname {value:?} refused: {error}"),
    }
}

fn literal_binder() -> ListenerBinder {
    ListenerBinder::new(Arc::new(LiteralAddressResolver))
}

fn loopback_plan(port: BindPort) -> ListenerBindPlan {
    ListenerBindPlan::new(BindHost::ip(LOOPBACK), port, limits(4, 4))
}

async fn bind_or_panic(binder: &ListenerBinder, plan: &ListenerBindPlan) -> BoundListener {
    match binder.bind(plan).await {
        Ok(listener) => listener,
        Err(error) => panic!("bind of {plan:?} failed: {error}"),
    }
}

/// Binds ephemerally and reports the port the operating system chose.
///
/// This is how every "specific port" test gets a port that is genuinely free:
/// asking the OS beats guessing a constant that some other process owns.
async fn borrow_a_free_port(binder: &ListenerBinder) -> u16 {
    let listener = bind_or_panic(binder, &loopback_plan(BindPort::Ephemeral)).await;
    let port = listener.receipt().observed().port();
    let _ = listener.close();
    port
}

#[tokio::test]
async fn a_fixed_loopback_bind_receives_exactly_the_port_it_asked_for() {
    let binder = literal_binder();
    let port = borrow_a_free_port(&binder).await;
    let plan = loopback_plan(fixed(port));

    let listener = bind_or_panic(&binder, &plan).await;
    let receipt = listener.receipt();

    assert_eq!(receipt.requested_host(), &BindHost::Ip(LOOPBACK));
    assert_eq!(receipt.requested_port(), fixed(port));
    assert_eq!(receipt.observed(), SocketAddr::new(LOOPBACK, port));
}

#[tokio::test]
async fn an_ephemeral_bind_observes_a_real_port_and_keeps_the_ephemeral_intent() {
    let binder = literal_binder();
    let listener = bind_or_panic(&binder, &loopback_plan(BindPort::Ephemeral)).await;
    let receipt = listener.receipt();

    // The requested port is zero and the observed port never is. A receipt that
    // echoed the request would be wrong in precisely the case it is consulted.
    assert_eq!(receipt.requested_port(), BindPort::Ephemeral);
    assert_eq!(BindPort::Ephemeral.requested(), 0);
    assert_ne!(
        receipt.observed().port(),
        0,
        "an ephemeral receipt must record the port the OS assigned, not the zero that was asked for"
    );
    assert_eq!(receipt.observed().ip(), LOOPBACK);
}

#[tokio::test]
async fn two_ephemeral_binds_receive_two_different_ports() {
    let binder = literal_binder();
    let first = bind_or_panic(&binder, &loopback_plan(BindPort::Ephemeral)).await;
    let second = bind_or_panic(&binder, &loopback_plan(BindPort::Ephemeral)).await;

    // This is what "ephemeral" MEANS, and it is the assertion that a fixed
    // sentinel cannot satisfy: any implementation that turns `Ephemeral` into
    // one constant port — 4096, say, as the oracle's search base — fails the
    // second bind outright instead of receiving a second free port.
    assert_ne!(
        first.receipt().observed().port(),
        second.receipt().observed().port(),
        "two ephemeral binds must not land on the same port"
    );
}

#[tokio::test]
async fn a_held_port_is_refused_as_address_in_use_and_its_owner_is_undamaged() {
    let binder = literal_binder();
    let held = bind_or_panic(&binder, &loopback_plan(BindPort::Ephemeral)).await;
    let address = held.receipt().observed();

    let conflict = binder
        .bind(&loopback_plan(fixed(address.port())))
        .await
        .err();
    assert_eq!(conflict, Some(BindError::AddressInUse { address }));

    // "Undamaged" is proved against the socket, not against the cached receipt:
    // a live listener still completes a TCP connection from its backlog, which
    // it could not do if the refused attempt had disturbed it. Accepting the
    // connection is the API host's job, not this crate's.
    match TcpStream::connect(address).await {
        Ok(stream) => drop(stream),
        Err(error) => panic!("the first listener must survive a conflicting bind, got {error}"),
    }
}

#[tokio::test]
async fn an_explicit_close_releases_the_port() {
    let binder = literal_binder();
    let listener = bind_or_panic(&binder, &loopback_plan(BindPort::Ephemeral)).await;
    let address = listener.receipt().observed();

    let closed = listener.close();
    assert_eq!(closed.receipt().observed(), address);

    // The port is genuinely free afterwards, which a receipt alone cannot show.
    let rebound = bind_or_panic(&binder, &loopback_plan(fixed(address.port()))).await;
    assert_eq!(rebound.receipt().observed(), address);
}

#[tokio::test]
async fn dropping_the_owner_releases_the_port() {
    let binder = literal_binder();
    let address = {
        let listener = bind_or_panic(&binder, &loopback_plan(BindPort::Ephemeral)).await;
        listener.receipt().observed()
    };

    // Separate evidence from the explicit-close path on purpose. The backstop
    // and the observable shutdown are two mechanisms, and one passing is not
    // evidence for the other.
    let rebound = bind_or_panic(&binder, &loopback_plan(fixed(address.port()))).await;
    assert_eq!(rebound.receipt().observed(), address);
}

#[tokio::test]
async fn the_attempt_bound_stops_before_the_candidate_list_is_exhausted() {
    // TEST-NET-1 (RFC 5737) is never assigned to a host, so every one of these
    // candidates fails to bind on every platform this runs on.
    let unusable = name("unusable.test");
    let resolver = DeterministicNameResolver::new().with_answer(
        unusable.clone(),
        vec![
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2)),
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 3)),
        ],
    );
    let binder = ListenerBinder::new(Arc::new(resolver));
    let plan = ListenerBindPlan::new(BindHost::name(unusable), BindPort::Ephemeral, limits(3, 2));

    match binder.bind(&plan).await {
        Err(BindError::AttemptsExhausted {
            attempted, limit, ..
        }) => {
            assert_eq!(attempted, 2, "exactly the bound may be attempted");
            assert_eq!(limit, 2);
        }
        other => panic!("the attempt bound must stop the third candidate, got {other:?}"),
    }
}

#[tokio::test]
async fn a_resolution_over_its_bound_is_refused_before_any_bind_is_attempted() {
    let wide = name("wide.test");
    let resolver = DeterministicNameResolver::new()
        .with_answer(wide.clone(), vec![LOOPBACK, LOOPBACK, LOOPBACK]);
    let binder = ListenerBinder::new(Arc::new(resolver));
    let plan = ListenerBindPlan::new(BindHost::name(wide), BindPort::Ephemeral, limits(2, 4));

    // A refusal naming the RESOLUTION bound — rather than any bind outcome — is
    // what proves nothing was attempted: had a candidate been tried, this
    // loopback plan would have succeeded and returned a listener.
    assert_eq!(
        binder.bind(&plan).await.err(),
        Some(BindError::Resolve(ResolveError::TooManyAddresses {
            produced: 3,
            limit: 2,
        }))
    );
}

#[tokio::test]
async fn the_production_binder_refuses_a_hostname_plan_with_a_typed_gap() {
    let binder = literal_binder();
    let plan = ListenerBindPlan::new(
        BindHost::name(name("api.internal.test")),
        BindPort::Ephemeral,
        limits(4, 4),
    );

    assert_eq!(
        binder.bind(&plan).await.err(),
        Some(BindError::Resolve(ResolveError::AdapterUnavailable {
            reason: AdapterUnavailableReason::NoProductionDnsAdapter,
        }))
    );
}

#[tokio::test]
async fn a_literal_address_plan_reaches_no_resolver_at_all() {
    let resolver = Arc::new(DeterministicNameResolver::new());
    let binder = ListenerBinder::new(resolver.clone());

    let listener = bind_or_panic(&binder, &loopback_plan(BindPort::Ephemeral)).await;
    assert_ne!(listener.receipt().observed().port(), 0);

    // Binding loopback must not acquire a dependency on name resolution being
    // installed, configured or reachable. Zero calls is the whole claim.
    assert_eq!(
        resolver.calls(),
        0,
        "a literal address must not consult the resolver"
    );
}

#[tokio::test]
async fn an_unassignable_address_is_classified_by_kind_not_by_message() {
    let binder = literal_binder();
    let unassignable = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
    let plan = ListenerBindPlan::new(
        BindHost::ip(unassignable),
        BindPort::Ephemeral,
        limits(1, 1),
    );

    match binder.bind(&plan).await {
        Err(BindError::AddressNotAvailable { address }) => {
            assert_eq!(address, SocketAddr::new(unassignable, 0));
        }
        // Some hosts refuse an unassigned bind on authority grounds instead;
        // both are structural classifications, and neither reads OS text.
        Err(BindError::PermissionDenied { address }) => {
            assert_eq!(address, SocketAddr::new(unassignable, 0));
        }
        other => panic!("an unassignable address must classify structurally, got {other:?}"),
    }
}

#[test]
fn every_classified_kind_is_a_distinct_error() {
    // The classification is a total function over the kinds this crate names;
    // anything it does not name lands in `Io` carrying its kind rather than
    // being flattened into a nearby variant.
    let address = SocketAddr::new(LOOPBACK, 1);
    let distinct = [
        BindError::AddressInUse { address },
        BindError::PermissionDenied { address },
        BindError::AddressNotAvailable { address },
        BindError::Io {
            address,
            kind: io::ErrorKind::Other,
        },
    ];
    for (index, left) in distinct.iter().enumerate() {
        for (offset, right) in distinct.iter().enumerate() {
            if index != offset {
                assert_ne!(left, right, "classified failures must stay distinguishable");
            }
        }
    }
}

#[tokio::test]
async fn an_accepted_connection_carries_the_observed_addresses_and_leaves_the_listener_owned() {
    // The single-owner accept seam R4H-H depends on. `accept` borrows, so this
    // owner still holds the listener and its receipt afterwards — the seam does
    // not consume the listener and does not erase the location receipt.
    let binder = literal_binder();
    let bound = bind_or_panic(&binder, &loopback_plan(BindPort::Ephemeral)).await;
    let observed = bound.receipt().observed();

    let client = TcpStream::connect(observed)
        .await
        .unwrap_or_else(|error| panic!("the client must reach the bound listener: {error}"));
    let client_local = client
        .local_addr()
        .unwrap_or_else(|error| panic!("the client must have a local address: {error}"));

    let accepted = match bound.accept().await {
        Ok(accepted) => accepted,
        Err(error) => panic!("the listener must admit the waiting connection: {error}"),
    };

    assert_eq!(
        accepted.local(),
        observed,
        "the connection must report the address the listener was OBSERVED on, not the requested one"
    );
    assert_eq!(
        accepted.peer(),
        client_local,
        "the connection must report the peer that actually connected"
    );
    assert_ne!(
        accepted.local().port(),
        0,
        "an ephemeral request asks for port 0; no connection is ever admitted on it"
    );

    // The listener is still owned here, and its receipt is unchanged.
    assert_eq!(
        bound.receipt().observed(),
        observed,
        "accepting must not disturb the bind receipt"
    );
    assert_eq!(
        bound.receipt().requested_port(),
        BindPort::Ephemeral,
        "accepting must not rewrite the requested intent"
    );

    // And it still accepts again: one accept admits one connection, no more.
    let second = TcpStream::connect(observed)
        .await
        .unwrap_or_else(|error| panic!("a second client must also reach it: {error}"));
    match bound.accept().await {
        Ok(next) => assert_eq!(
            next.local(),
            observed,
            "the second admission is on the same observed address"
        ),
        Err(error) => panic!("the listener must still accept after the first: {error}"),
    }
    drop(second);
    drop(client);

    // Closing still consumes the owner and yields the same receipt.
    let closed = bound.close();
    assert_eq!(
        closed.receipt().observed(),
        observed,
        "close must still return the receipt of the listener that accepted"
    );
}

#[tokio::test]
async fn a_closed_listener_can_no_longer_be_reached() {
    // The accept seam does not weaken the close contract: after an explicit
    // close the port is released, which is what proves accept borrowed rather
    // than took ownership of the socket's lifetime.
    let binder = literal_binder();
    let bound = bind_or_panic(&binder, &loopback_plan(BindPort::Ephemeral)).await;
    let observed = bound.receipt().observed();
    let closed = bound.close();
    assert_eq!(closed.receipt().observed(), observed);

    let rebound = bind_or_panic(&binder, &loopback_plan(fixed(observed.port()))).await;
    assert_eq!(
        rebound.receipt().observed(),
        observed,
        "the released port must be bindable again"
    );
}

#[tokio::test]
async fn an_accepted_connection_names_the_peer_that_dialled_in_not_its_own_address() {
    // Deliberately separate from the observed-address test above, and the
    // separation is evidence rather than tidiness. A mutation that SWAPS peer
    // and local trips that test's local assertion first, so both laws would rest
    // on one record and the peer law would never be independently witnessed. The
    // mutation harness's duplicate-evidence guard refused exactly that, and it
    // was right to: two laws cannot share one record.
    let binder = literal_binder();
    let bound = bind_or_panic(&binder, &loopback_plan(BindPort::Ephemeral)).await;
    let observed = bound.receipt().observed();

    let client = TcpStream::connect(observed)
        .await
        .unwrap_or_else(|error| panic!("the client must reach the bound listener: {error}"));
    let client_local = client
        .local_addr()
        .unwrap_or_else(|error| panic!("the client must have a local address: {error}"));

    let accepted = match bound.accept().await {
        Ok(accepted) => accepted,
        Err(error) => panic!("the listener must admit the waiting connection: {error}"),
    };

    // Peer first, and peer only. Asserting the local address here would put this
    // law back on the same record as the test above.
    assert_eq!(
        accepted.peer(),
        client_local,
        "the connection must name the peer that dialled in"
    );
    assert_ne!(
        accepted.peer(),
        observed,
        "the peer is the far end of the connection, never the address this listener holds"
    );
    drop(client);
}
