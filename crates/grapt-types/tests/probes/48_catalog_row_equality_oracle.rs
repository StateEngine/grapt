// Probe 48: two catalog rows cannot be compared.
//
// `ModelCatalogRecord` is the value that HOLDS the two sealed facts, and it
// derived `PartialEq`/`Eq`. That impl was reachable with a genuine row on one
// side, because `ModelRef`'s equality delegated straight to it: author a row
// for a genuine endpoint guessing a family, load it into a catalog of your own,
// convert it, and compare against a ref the runtime's catalog vouched for.
// `true` confirmed the guess.
//
// Sealing that at the `ModelRef` level alone would leave the oracle armed for
// the next lane that hands out a row — the model registry's picker projection,
// for instance. Row identity is now the crate-private `same_row`, and the
// catalog's own load-time ordering is by lookup coordinate.
//
// The probe is written as the attack rather than as a trait bound: two rows for
// one endpoint that differ ONLY in the family they claim.
use grapt_types::{BaseModelId, ModelCatalogRecord, ModelFamilyId, ModelId, ProviderId};

fn row(family: &str) -> ModelCatalogRecord {
    ModelCatalogRecord::new(
        ProviderId::new("anthropic").expect("provider"),
        ModelId::new("dated-model-20260801").expect("model"),
        BaseModelId::new("undated-base").expect("base"),
        ModelFamilyId::new(family).expect("family"),
        None,
    )
}

fn main() {
    // The step that must not exist: one bit of the sealed routing fact.
    let _guessed_right = row("premium-tier") == row("economy-tier");
}
