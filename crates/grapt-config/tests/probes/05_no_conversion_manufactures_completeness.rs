//! There is no conversion that turns an unchecked ledger into a complete one.
//!
//! The private field closes the struct literal; this closes the other door a
//! caller would reach for. `ConfigLedger::complete` returning a `Result` is the
//! only way across, and a `From`/`Into` impl would be a second way that skipped
//! the check.
//!
//! Scoreable: adding `impl From<ConfigLedger> for CompleteLedger` makes this
//! compile and the test turns red.

use grapt_config::{admit, CompleteLedger, ConfigBounds};

fn converting_into_a_complete_ledger_is_rejected(
    census: &grapt_config::SourceCensus,
    bounds: ConfigBounds,
) {
    let ledger = admit(census, bounds).expect("fixture");
    let _: CompleteLedger = ledger.into();
}

fn main() {}
