// Probe 53: a registration version has no byte order.
//
// `opaque_id!` derives `PartialOrd`/`Ord`, and for this type that derivation is
// a lexicographic order over the recorded bytes — under which `"0.10.0"` sorts
// BELOW `"0.9.0"`. A caller reaching for `>` to decide "is the candidate newer"
// would compile and silently answer backwards, which is exactly the mistake
// that makes an upgrade indistinguishable from a downgrade.
//
// `ServiceRegistrationVersion` is therefore declared without those derives, so
// the comparison has no meaning to borrow. Semantic precedence is reachable
// only through `semantic_precedence`, which refuses a value it cannot order.
use grapt_types::ServiceRegistrationVersion;

fn is_newer(
    incumbent: &ServiceRegistrationVersion,
    candidate: &ServiceRegistrationVersion,
) -> bool {
    candidate > incumbent
}

fn main() {
    let _ = is_newer;
}
