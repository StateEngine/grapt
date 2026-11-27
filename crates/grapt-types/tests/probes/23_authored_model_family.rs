// Probe 23 (routing/adapter provenance, ModelRef row): base model and family
// are CATALOG truth and cannot be caller-asserted.
//
// Amendment §7.5 makes the model family the boundary `agent default` may not
// cross. A caller who can author the family simply declares that a frontier
// model and its own cheap tier are one family, and `same_family` then waves the
// child through with no user selection at all.
//
// This is the attacker's shortest path: the old public constructor. It no
// longer exists — `ModelRef::from_catalog` is the only route, and it takes a
// `ModelCatalogEntry` that only a loaded `ModelCatalog` hands out.
//
// The entry-forging clause lives in `24_forge_catalog_entry.rs`: this file
// fails during method resolution, which aborts before privacy checking.
use grapt_types::{BaseModelId, ModelFamilyId, ModelId, ModelRef, ProviderId};

fn main() {
    let _forged = ModelRef::new(
        ProviderId::new("anthropic").unwrap(),
        ModelId::new("claude-opus-5").unwrap(),
        BaseModelId::new("claude-opus-5").unwrap(),
        // The lie: a frontier model placed in the caller's own cheap tier.
        ModelFamilyId::new("cheap-tier").unwrap(),
        None,
    );
}
