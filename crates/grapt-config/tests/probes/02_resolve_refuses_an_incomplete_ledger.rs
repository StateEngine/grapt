//! Resolution cannot be reached from a ledger that was never checked for
//! completeness.
//!
//! ONE law per probe file, deliberately. Four of these laws were first written
//! into a single probe, and rustc stopped after three: the privacy error was
//! never emitted, so the file silently proved less than it claimed while
//! trybuild reported it as passing. A compile-fail probe that asserts several
//! laws at once witnesses only the ones the compiler reaches.
//!
//! Scoreable: widening `resolve` to take a `ConfigLedger` makes this compile,
//! trybuild reports an expected-failure that succeeded, and the test turns red.

use grapt_config::{admit, resolve, ConfigBounds};

fn resolving_a_raw_ledger_is_rejected(census: &grapt_config::SourceCensus, bounds: ConfigBounds) {
    let ledger = admit(census, bounds).expect("fixture");
    let _ = resolve(&ledger);
}

fn main() {}
