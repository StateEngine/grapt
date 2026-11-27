// Probe 55 (L2 S3, receipt non-constructibility): a `CatalogReissuanceReceipt`
// is a PROOF, and the only party that may mint one is the registry that verified
// the four things it stands for — same coordinates, same base model, same family,
// new generation.
//
// If a caller could author one, the carry-forward path would accept a proof
// nobody proved, and a config reload that moved a model to another family would
// carry the user's Subagent selection straight across the boundary `agent
// default` may not cross. `ModelRegistry::reissue` is the only producer.
use grapt_types::CatalogReissuanceReceipt;

fn main() {
    let _forged = CatalogReissuanceReceipt {
        from: todo!(),
        to: todo!(),
    };
}
