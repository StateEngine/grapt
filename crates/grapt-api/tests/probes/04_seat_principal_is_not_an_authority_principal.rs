// Probe 04 (R4H-A, answer Q2): two principal namespaces, permanently separate.
//
// `grapt-types::AuthorityPrincipal` is closed over
// `{Human, System, PolicyLayer, Hook, AutomatedReviewer}` and `is_human()` is
// the single decision point for "can this ratify". An
// `AuthenticatedSeatPrincipal` is a different thing entirely: transport
// provenance for one seat connection, not permission authority.
//
// The answer to Q2 RATIFIES the permanent non-conversion in both directions.
// Silence here would have left the strongest bypass in the lane available — a
// transport-authenticated seat presenting itself as `AuthorityPrincipal::Human`
// — so it is held by the compiler rather than by a review comment.
//
// No value is constructed: the conversion is probed at the signature, which is
// also why one error phase fires rather than a privacy error first.
use grapt_api::AuthenticatedSeatPrincipal;
use grapt_types::AuthorityPrincipal;

fn upgrade(seat: AuthenticatedSeatPrincipal) -> AuthorityPrincipal {
    seat.into()
}

fn main() {
    let _ = upgrade;
}
