//! Witnesses for the terminal-vocabulary boundary.
//!
//! Everything here guards a RULED map or a RULED bound, not an implementation
//! detail. Each test names the exact substitution it refuses, because the
//! defect these replace was that any of these values could be swapped for any
//! other and nothing would notice: they were all `&'static str`.

use std::time::Duration;

use grapt_provider::{BoundedDetail, FinishReason, NormalizedFinish, ProviderFailureClass};
use grapt_types::ProviderCompletionDisposition as Disposition;
use grapt_types::ProviderFailureClass as Terminal;

/// Every oracle finish reason, with its ruled normalization.
///
/// Written as a table so the map is checked as a WHOLE. Two arms mapping to the
/// same value would still be caught: the table pairs each input with its own
/// expectation, and the assertions below prove the outputs are not all equal.
fn ruled_finish_map() -> Vec<(FinishReason, NormalizedFinish)> {
    vec![
        (
            FinishReason::Stop,
            NormalizedFinish::Completed {
                completion: Disposition::AssertedComplete,
            },
        ),
        (
            FinishReason::ToolCalls,
            NormalizedFinish::Completed {
                completion: Disposition::AssertedContinue,
            },
        ),
        (
            FinishReason::Length,
            NormalizedFinish::Completed {
                completion: Disposition::Truncated,
            },
        ),
        (
            FinishReason::Unknown,
            NormalizedFinish::Completed {
                completion: Disposition::Unstated,
            },
        ),
        (
            FinishReason::ContentFilter,
            NormalizedFinish::Failed {
                class: Terminal::ContentPolicy,
            },
        ),
        (
            FinishReason::Error,
            NormalizedFinish::Failed {
                class: Terminal::ProviderInternal,
            },
        ),
    ]
}

#[test]
fn every_finish_reason_normalizes_as_ruled() {
    let map = ruled_finish_map();
    assert_eq!(
        map.len(),
        6,
        "the oracle set has exactly six finish reasons; a new one needs a ruling, not a default"
    );
    for (finish, expected) in map {
        assert_eq!(
            finish.normalize(),
            expected,
            "finish {finish:?} must normalize to {expected:?}"
        );
    }
}

/// `Length` is the arm the runtime must not fall through on, so it gets its own
/// witness rather than living only inside the table.
///
/// The failure this refuses: `Length` normalizing to `AssertedComplete`, which
/// would make a provider that hit its output ceiling indistinguishable from one
/// that finished its answer.
#[test]
fn length_is_truncated_and_not_a_completion_claim() {
    assert_eq!(
        FinishReason::Length.normalize(),
        NormalizedFinish::Completed {
            completion: Disposition::Truncated
        }
    );
    assert_ne!(
        FinishReason::Length.normalize(),
        FinishReason::Stop.normalize(),
        "a truncated stream and a clean stop must not normalize to the same disposition"
    );
}

/// The two failure-shaped finishes are not dispositions.
#[test]
fn a_content_filter_finish_is_a_failure_not_a_disposition() {
    match FinishReason::ContentFilter.normalize() {
        NormalizedFinish::Failed { class } => assert_eq!(class, Terminal::ContentPolicy),
        other => panic!("a content-policy refusal is not an ordinary ending, got {other:?}"),
    }
    match FinishReason::Error.normalize() {
        NormalizedFinish::Failed { class } => assert_eq!(class, Terminal::ProviderInternal),
        other => panic!("an error finish is not an ordinary ending, got {other:?}"),
    }
}

/// Every transport class, with its ruled terminal classification.
fn ruled_class_map() -> Vec<(ProviderFailureClass, Terminal)> {
    vec![
        (
            ProviderFailureClass::RateLimit { retry_after: None },
            Terminal::RateLimit,
        ),
        (
            ProviderFailureClass::ProviderInternal { retry_after: None },
            Terminal::ProviderInternal,
        ),
        (ProviderFailureClass::Transport, Terminal::Transport),
        (
            ProviderFailureClass::Authentication,
            Terminal::Authentication,
        ),
        (ProviderFailureClass::QuotaExceeded, Terminal::Quota),
        (ProviderFailureClass::ContentPolicy, Terminal::ContentPolicy),
        (
            ProviderFailureClass::InvalidProviderOutput,
            Terminal::InvalidProviderOutput,
        ),
        (
            ProviderFailureClass::InvalidRequest,
            Terminal::InvalidRequest,
        ),
        (ProviderFailureClass::NoRoute, Terminal::NoRoute),
        (
            ProviderFailureClass::UnknownProvider,
            Terminal::UnknownProvider,
        ),
    ]
}

#[test]
fn every_transport_class_maps_to_its_ruled_terminal_class() {
    let map = ruled_class_map();
    assert_eq!(
        map.len(),
        10,
        "the transport vocabulary has ten classes; each needs its own ruled target"
    );
    for (class, expected) in &map {
        assert_eq!(
            class.to_terminal(),
            *expected,
            "class {class:?} must map to {expected:?}"
        );
    }

    // The map must be INJECTIVE. Without this, an implementation that returned
    // one constant for everything would satisfy every pairing above only if the
    // table were wrong — but an implementation that collapsed two neighbouring
    // arms would not be caught by the pairings alone.
    let mut seen: Vec<Terminal> = map.iter().map(|(class, _)| class.to_terminal()).collect();
    let total = seen.len();
    seen.sort_by_key(|terminal| format!("{terminal:?}"));
    seen.dedup();
    assert_eq!(
        seen.len(),
        total,
        "two transport classes collapsed onto one terminal class"
    );
}

/// The one arm whose NAME changes across the boundary, so a careless map would
/// have to invent something.
#[test]
fn quota_exceeded_becomes_quota() {
    assert_eq!(
        ProviderFailureClass::QuotaExceeded.to_terminal(),
        Terminal::Quota
    );
}

/// `retry_after` is a scheduling input, consumed before terminality. It must
/// not change the terminal class.
#[test]
fn a_retry_after_does_not_change_the_terminal_class() {
    let bare = ProviderFailureClass::RateLimit { retry_after: None };
    let timed = ProviderFailureClass::RateLimit {
        retry_after: Some(Duration::from_secs(30)),
    };
    assert_ne!(bare, timed, "the two inputs must actually differ");
    assert_eq!(bare.to_terminal(), timed.to_terminal());
}

// ---------------------------------------------------------------------------
// The bounded diagnostic.
// ---------------------------------------------------------------------------

#[test]
fn a_short_detail_is_kept_verbatim_and_unmarked() {
    let detail = BoundedDetail::new("HTTP 429 from the provider");
    assert_eq!(detail.as_str(), "HTTP 429 from the provider");
    assert!(!detail.is_truncated());
}

/// EXACT-BOUND GREEN. Input of exactly the cap is kept whole: the bound is a
/// ceiling, not a budget the marker eats into when it does not have to.
#[test]
fn a_detail_of_exactly_the_cap_is_kept_whole() {
    let raw = "x".repeat(BoundedDetail::MAX_BYTES);
    let detail = BoundedDetail::new(raw.clone());
    assert_eq!(detail.byte_len(), BoundedDetail::MAX_BYTES);
    assert_eq!(detail.as_str(), raw);
    assert!(
        !detail.is_truncated(),
        "input at the cap is not over the cap"
    );
}

/// OVER-BOUND RED. One byte past the cap truncates, and the RETAINED bytes —
/// marker included — still fit under the cap. A marker appended after the cut
/// without budgeting would overshoot here.
#[test]
fn one_byte_over_the_cap_truncates_within_the_cap() {
    let raw = "x".repeat(BoundedDetail::MAX_BYTES + 1);
    let detail = BoundedDetail::new(raw);
    assert!(detail.is_truncated());
    assert!(
        detail.byte_len() <= BoundedDetail::MAX_BYTES,
        "retained {} bytes exceeds the {} byte cap; the marker was not budgeted",
        detail.byte_len(),
        BoundedDetail::MAX_BYTES
    );
    assert!(detail.as_str().ends_with(BoundedDetail::TRUNCATION_MARKER));
}

/// A provider message far over the cap is still retained within the bound.
/// The carrier must truncate rather than hold an unbounded `message`.
#[test]
fn a_wildly_oversized_detail_is_still_bounded() {
    let detail = BoundedDetail::new("y".repeat(BoundedDetail::MAX_BYTES * 4));
    assert!(detail.is_truncated());
    assert!(detail.byte_len() <= BoundedDetail::MAX_BYTES);
}

/// The cut must land on a character boundary. A byte-slice truncation would
/// panic here, or produce invalid UTF-8 if done unchecked.
#[test]
fn truncation_never_splits_a_multibyte_character() {
    // Three-byte characters do not divide evenly into the budget, so the cut
    // lands mid-character unless the implementation walks back.
    let one = "€"; // 3 bytes
    let raw = one.repeat(BoundedDetail::MAX_BYTES);
    let detail = BoundedDetail::new(raw);
    assert!(detail.is_truncated());
    assert!(detail.byte_len() <= BoundedDetail::MAX_BYTES);
    // Reaching here at all means the value is a valid `str`; check the retained
    // prefix is whole characters rather than a salvaged byte run.
    let body = detail
        .as_str()
        .strip_suffix(BoundedDetail::TRUNCATION_MARKER)
        .unwrap_or_else(|| panic!("a truncated detail must carry the marker"));
    assert_eq!(
        body.len() % one.len(),
        0,
        "the cut landed inside a character"
    );
}

/// `is_truncated` is recorded at construction, not sniffed from the suffix.
#[test]
fn a_detail_that_merely_ends_with_the_marker_is_not_truncated() {
    let detail = BoundedDetail::new(format!(
        "provider said {}",
        BoundedDetail::TRUNCATION_MARKER
    ));
    assert!(detail.as_str().ends_with(BoundedDetail::TRUNCATION_MARKER));
    assert!(
        !detail.is_truncated(),
        "truncation is a fact about construction, not about the text"
    );
}
