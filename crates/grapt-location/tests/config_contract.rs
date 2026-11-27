//! The bind-intent contract: what may be stated, and what is refused.
//!
//! These tests touch no socket. Everything here is about keeping three
//! different bind intents apart and refusing a bound that is not a bound.

use std::net::{IpAddr, Ipv4Addr};
use std::num::NonZeroU16;

use grapt_location::{
    AdapterUnavailableReason, BindHost, BindHostName, BindLimits, BindPort,
    DeterministicNameResolver, HostNameError, LimitError, LimitField, ListenerBindPlan,
    LiteralAddressResolver, MaxBindAttempts, MaxResolvedAddresses, NameResolver, ResolveError,
    ResolvedAddresses, MAX_HOST_NAME_BYTES,
};

fn fixed(port: u16) -> BindPort {
    match NonZeroU16::new(port) {
        Some(port) => BindPort::Fixed(port),
        None => panic!("this test asked for a zero fixed port, which cannot exist"),
    }
}

fn addresses(count: usize) -> MaxResolvedAddresses {
    match MaxResolvedAddresses::new(count) {
        Ok(bound) => bound,
        Err(error) => panic!("bound of {count} refused: {error}"),
    }
}

fn attempts(count: usize) -> MaxBindAttempts {
    match MaxBindAttempts::new(count) {
        Ok(bound) => bound,
        Err(error) => panic!("bound of {count} refused: {error}"),
    }
}

fn name(value: &str) -> BindHostName {
    match BindHostName::new(value) {
        Ok(name) => name,
        Err(error) => panic!("hostname {value:?} refused: {error}"),
    }
}

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

#[test]
fn ephemeral_is_the_only_intent_that_asks_for_port_zero() {
    assert_eq!(BindPort::Ephemeral.requested(), 0);
    assert!(BindPort::Ephemeral.is_ephemeral());

    // Every representable fixed port asks for itself. There is no port value
    // that maps to zero, and none that maps to a search base either: the
    // oracle's search-from-4096 policy is not reachable from this type.
    for port in [1u16, 80, 4096, 8080, 65535] {
        let intent = fixed(port);
        assert_eq!(
            intent.requested(),
            port,
            "fixed({port}) must ask for {port}"
        );
        assert!(!intent.is_ephemeral(), "fixed({port}) is not ephemeral");
    }
}

#[test]
fn a_positive_bound_refuses_zero_and_names_which_field_it_was() {
    assert_eq!(
        MaxResolvedAddresses::new(0),
        Err(LimitError::NotPositive {
            field: LimitField::MaxResolvedAddresses
        })
    );
    assert_eq!(
        MaxBindAttempts::new(0),
        Err(LimitError::NotPositive {
            field: LimitField::MaxBindAttempts
        })
    );

    // The two fields are distinct types, so the refusal can name which one was
    // zero rather than reporting "a limit" and leaving the caller to guess.
    assert_eq!(addresses(1).get(), 1);
    assert_eq!(attempts(7).get(), 7);
}

#[test]
fn a_hostname_is_validated_at_construction() {
    assert_eq!(BindHostName::new(""), Err(HostNameError::Empty));

    let long = "a".repeat(MAX_HOST_NAME_BYTES + 1);
    assert_eq!(
        BindHostName::new(&long),
        Err(HostNameError::TooLong {
            bytes: MAX_HOST_NAME_BYTES + 1,
            limit: MAX_HOST_NAME_BYTES,
        })
    );

    assert_eq!(
        BindHostName::new("exam ple.test"),
        Err(HostNameError::InvalidByte {
            index: 4,
            byte: b' ',
        })
    );

    let boundary = "a".repeat(MAX_HOST_NAME_BYTES);
    match BindHostName::new(&boundary) {
        Ok(accepted) => assert_eq!(accepted.as_str().len(), MAX_HOST_NAME_BYTES),
        Err(error) => panic!("a name of exactly the bound must be accepted, got {error}"),
    }

    assert_eq!(
        name("api.internal-host.test").as_str(),
        "api.internal-host.test"
    );
}

#[test]
fn a_candidate_sequence_is_non_empty_and_bounded() {
    assert_eq!(
        ResolvedAddresses::new(Vec::new(), addresses(4)),
        Err(ResolveError::NoAddresses)
    );

    let three = vec![LOOPBACK, LOOPBACK, LOOPBACK];
    assert_eq!(
        ResolvedAddresses::new(three.clone(), addresses(2)),
        Err(ResolveError::TooManyAddresses {
            produced: 3,
            limit: 2,
        })
    );

    // Over-length is REFUSED, never truncated. A truncating constructor would
    // turn "this resolver is misbehaving" into "we tried the first two", and
    // the caller could never tell the two apart.
    match ResolvedAddresses::new(three, addresses(3)) {
        Ok(admitted) => {
            assert_eq!(admitted.len(), 3);
            assert!(!admitted.is_empty());
        }
        Err(error) => panic!("exactly the bound must be admitted, got {error}"),
    }
}

#[test]
fn a_plan_carries_the_intent_it_was_given() {
    let limits = BindLimits::new(addresses(4), attempts(2));
    let plan = ListenerBindPlan::new(BindHost::ip(LOOPBACK), BindPort::Ephemeral, limits);

    assert_eq!(plan.host(), &BindHost::Ip(LOOPBACK));
    assert_eq!(plan.port(), BindPort::Ephemeral);
    assert_eq!(plan.limits().max_resolved_addresses().get(), 4);
    assert_eq!(plan.limits().max_bind_attempts().get(), 2);
}

#[tokio::test]
async fn the_production_resolver_states_its_gap_instead_of_guessing() {
    let resolver = LiteralAddressResolver;
    let outcome = resolver
        .resolve(&name("api.internal.test"), addresses(4))
        .await;

    // Not an empty success, not a silent fall back to loopback, and not an
    // ambient platform lookup: a typed statement that this build has no DNS.
    assert_eq!(
        outcome,
        Err(ResolveError::AdapterUnavailable {
            reason: AdapterUnavailableReason::NoProductionDnsAdapter,
        })
    );
}

#[tokio::test]
async fn the_deterministic_resolver_answers_only_what_was_declared() {
    let declared = name("declared.test");
    let resolver = DeterministicNameResolver::new().with_answer(
        declared.clone(),
        vec![LOOPBACK, IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2))],
    );

    assert_eq!(resolver.calls(), 0);

    match resolver.resolve(&declared, addresses(4)).await {
        Ok(found) => assert_eq!(found.len(), 2),
        Err(error) => panic!("a declared name must resolve, got {error}"),
    }
    assert_eq!(resolver.calls(), 1);

    assert_eq!(
        resolver
            .resolve(&name("undeclared.test"), addresses(4))
            .await,
        Err(ResolveError::NoAddresses)
    );
    assert_eq!(resolver.calls(), 2);
}

#[tokio::test]
async fn the_deterministic_resolver_is_bounded_by_the_limit_it_is_handed() {
    let declared = name("wide.test");
    let resolver = DeterministicNameResolver::new().with_answer(
        declared.clone(),
        vec![
            LOOPBACK,
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)),
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 3)),
        ],
    );

    // The bound travels INTO the adapter, so the adapter is bounded at its own
    // boundary rather than trusted to be modest.
    assert_eq!(
        resolver.resolve(&declared, addresses(2)).await,
        Err(ResolveError::TooManyAddresses {
            produced: 3,
            limit: 2,
        })
    );

    match resolver.resolve(&declared, addresses(3)).await {
        Ok(found) => assert_eq!(found.len(), 3),
        Err(error) => panic!("the same answer within its bound must resolve, got {error}"),
    }
}
