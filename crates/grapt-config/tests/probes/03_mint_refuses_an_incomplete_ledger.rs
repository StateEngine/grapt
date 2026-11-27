//! The authority graph cannot be reached from a ledger that was never checked
//! for completeness.
//!
//! Scoreable: widening `mint_layers` to take a `ConfigLedger` makes this
//! compile and the test turns red. See `02` for why each law is its own file.

use grapt_config::{admit, mint_layers, ConfigBounds};
use grapt_policy::ContributionAuthority;

fn minting_from_a_raw_ledger_is_rejected(
    census: &grapt_config::SourceCensus,
    bounds: ConfigBounds,
    authority: &ContributionAuthority,
) {
    let ledger = admit(census, bounds).expect("fixture");
    let _ = mint_layers(&ledger, authority);
}

fn main() {}
