// Probe 24 (routing/adapter provenance, ModelRef row): a catalog entry is
// issued by a loaded catalog, never assembled by its consumer.
//
// With `ModelRef::new` gone, the next shortest path is to forge the thing
// `ModelRef::from_catalog` consumes. `ModelCatalogEntry`'s fields are private
// and it has no public constructor, so the only producer stays
// `ModelCatalog::entry` — a lookup that can only return a row the snapshot
// actually contains.
use grapt_types::ModelCatalogEntry;

fn main() {
    let _forged = ModelCatalogEntry {
        snapshot: todo!(),
        record: todo!(),
    };
}
