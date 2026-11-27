// Probe 5: a match over the closed OperationKind that misses a variant must
// not compile — adding a kind must break every owner match site.
use grapt_types::OperationKind;

fn main() {
    let kind = OperationKind::Shell;
    match kind {
        OperationKind::Permission => {}
        OperationKind::Form => {}
        OperationKind::Subagent => {}
    }
}
