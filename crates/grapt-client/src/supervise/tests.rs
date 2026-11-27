use super::{Connection, Event};
use std::time::Duration;
use tokio::time::Instant;

fn states(at: Instant) -> Vec<Connection> {
    vec![
        Connection::Live,
        Connection::Resyncing { since: at },
        Connection::Replacing { attempt: 2 },
        Connection::Disconnected {
            attempt: 3,
            reason: "refused".into(),
        },
        Connection::Refused {
            reason: "other product".into(),
        },
    ]
}

fn events() -> Vec<Event> {
    vec![
        Event::Lost,
        Event::Closed,
        Event::Replacing,
        Event::Attached,
        Event::Failed {
            reason: "connect".into(),
        },
        Event::Unverified {
            reason: "mismatch".into(),
        },
    ]
}

/// Every state against every event, with the expected successor spelled out.
#[test]
fn every_transition_is_explicit() {
    let at = Instant::now();
    let now = at + Duration::from_secs(1);
    let resyncing = Connection::Resyncing { since: at };
    let failed = |attempt| Connection::Disconnected {
        attempt,
        reason: "connect".into(),
    };
    let refused = Connection::Refused {
        reason: "other product".into(),
    };
    let unverified = Connection::Refused {
        reason: "mismatch".into(),
    };
    let expected: [[Connection; 6]; 5] = [
        // Live
        [
            Connection::Resyncing { since: now },
            Connection::Replacing { attempt: 1 },
            Connection::Live,
            Connection::Live,
            failed(1),
            unverified.clone(),
        ],
        // Resyncing
        [
            resyncing.clone(),
            Connection::Replacing { attempt: 1 },
            resyncing.clone(),
            Connection::Live,
            failed(1),
            unverified.clone(),
        ],
        // Replacing
        [
            Connection::Replacing { attempt: 2 },
            Connection::Replacing { attempt: 2 },
            Connection::Replacing { attempt: 2 },
            Connection::Live,
            failed(2),
            unverified.clone(),
        ],
        // Disconnected
        [
            Connection::Disconnected {
                attempt: 3,
                reason: "refused".into(),
            },
            Connection::Disconnected {
                attempt: 3,
                reason: "refused".into(),
            },
            Connection::Replacing { attempt: 4 },
            Connection::Live,
            failed(3),
            unverified.clone(),
        ],
        // Refused
        [
            refused.clone(),
            refused.clone(),
            refused.clone(),
            Connection::Live,
            refused.clone(),
            unverified,
        ],
    ];
    for (state, row) in states(at).iter().zip(expected) {
        for (event, successor) in events().into_iter().zip(row) {
            let label = format!("{state:?} on {event:?}");
            assert_eq!(state.next(event, now), successor, "{label}");
        }
    }
}

#[test]
fn a_gap_never_blocks_input() {
    let now = Instant::now();
    let resyncing = Connection::Live.next(Event::Lost, now);
    assert!(resyncing.accepts_commands());
    // Repeated loss while resynchronising keeps input open and the first
    // loss time, so the view can say how long it has been catching up.
    assert_eq!(
        resyncing.next(Event::Lost, now + Duration::from_secs(5)),
        resyncing
    );
}

#[test]
fn multi_step_reads_wait_for_any_replacement() {
    let at = Instant::now();
    for state in states(at) {
        assert_eq!(
            state.accepts_incremental(),
            state == Connection::Live,
            "{state:?}"
        );
    }
}

#[test]
fn a_closed_or_unverified_service_refuses_commands() {
    let at = Instant::now();
    for state in states(at) {
        for event in [
            Event::Closed,
            Event::Failed {
                reason: "connect".into(),
            },
            Event::Unverified {
                reason: "mismatch".into(),
            },
        ] {
            let next = state.next(event.clone(), at);
            assert!(
                !next.accepts_commands(),
                "{state:?} on {event:?} -> {next:?}"
            );
            assert!(next.recovering(), "{state:?} on {event:?} -> {next:?}");
        }
    }
}

#[test]
fn only_a_verified_replacement_restores_everything() {
    let at = Instant::now();
    for state in states(at) {
        let next = state.next(Event::Attached, at);
        assert_eq!(next, Connection::Live, "{state:?}");
        assert!(next.accepts_commands() && next.accepts_incremental() && next.streaming());
        assert!(!next.recovering());
    }
}

#[test]
fn attempts_count_up_across_failed_replacements() {
    let at = Instant::now();
    let mut state = Connection::Live.next(Event::Closed, at);
    for attempt in 1..=4 {
        assert_eq!(state, Connection::Replacing { attempt });
        state = state.next(
            Event::Failed {
                reason: "connect".into(),
            },
            at,
        );
        state = state.next(Event::Replacing, at);
    }
}

#[test]
fn attempts_carry_over_at_any_count() {
    let at = Instant::now();
    let failed = || Event::Failed {
        reason: "connect".into(),
    };
    assert_eq!(
        Connection::Replacing { attempt: 9 }.next(failed(), at),
        Connection::Disconnected {
            attempt: 9,
            reason: "connect".into()
        }
    );
    let disconnected = Connection::Disconnected {
        attempt: 7,
        reason: "connect".into(),
    };
    assert_eq!(
        disconnected.next(Event::Replacing, at),
        Connection::Replacing { attempt: 8 }
    );
    assert_eq!(disconnected.next(failed(), at), disconnected);
    assert_eq!(
        Connection::Disconnected {
            attempt: u32::MAX,
            reason: "connect".into()
        }
        .next(Event::Replacing, at),
        Connection::Replacing { attempt: u32::MAX }
    );
}

#[test]
fn a_refusal_and_its_reason_survive_retries_until_a_replacement_verifies() {
    let at = Instant::now();
    let refused = Connection::Refused {
        reason: "other product".into(),
    };
    let mut state = refused.clone();
    for _ in 0..3 {
        state = state.next(Event::Replacing, at);
        assert_eq!(state, refused);
        state = state.next(
            Event::Failed {
                reason: "connect".into(),
            },
            at,
        );
        assert_eq!(state, refused);
    }
    assert_eq!(state.next(Event::Attached, at), Connection::Live);
}
