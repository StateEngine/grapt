//! Completeness cannot be asserted from outside the module that checks it.
//!
//! `CompleteLedger`'s field is private, so a caller cannot build one around a
//! ledger it never checked. Without this, the typestate would be decoration:
//! every caller could construct the proof itself.
//!
//! Scoreable: making the field `pub` makes this compile and the test turns red.

use grapt_config::{admit, CompleteLedger, ConfigBounds};

fn constructing_a_complete_ledger_directly_is_rejected(
    census: &grapt_config::SourceCensus,
    bounds: ConfigBounds,
) {
    let ledger = admit(census, bounds).expect("fixture");
    let _ = CompleteLedger { ledger };
}

fn main() {}
