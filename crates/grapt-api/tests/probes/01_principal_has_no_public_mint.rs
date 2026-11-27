// Probe 01 (R4H-A): the sealed principal has no public mint.
//
// This is the whole trust boundary in one line. If `mint` were reachable from
// outside the crate, a caller could assemble a principal naming any recipient
// it liked and every downstream authorization would believe it — the exact
// bypass `grapt-types::authority` documents when it warns that a public
// constructor "under any name, including an `elect`-style trust root" defeats
// the seal.
//
// One error phase: the privacy failure fires at path resolution, so the
// argument expressions are never type-checked and no second error can mask it.
use grapt_api::AuthenticatedSeatPrincipal;

fn main() {
    let _forged = AuthenticatedSeatPrincipal::mint(todo!(), todo!(), todo!(), todo!());
}
