// Probe 56 (L2 S1, generation non-authorability): a `CatalogGeneration` says
// which release is current, and the staleness refusal is a comparison against
// it. A caller that could author one could hand a retired issuance a generation
// equal to the live one and walk it past the gate.
//
// So the inner ordinal is private and there is no public constructor. The only
// routes to a generation are `CatalogGeneration::FIRST`, which starts a lineage,
// and `succ`, which advances one — neither of which lets a caller CHOOSE a value
// for a target it holds.
use grapt_types::CatalogGeneration;

fn main() {
    let _authored = CatalogGeneration(7);
}
