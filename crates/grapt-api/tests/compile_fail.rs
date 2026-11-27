//! Compile-time seals for the transport trust boundary. The compiler must
//! REJECT each probe file; a probe that starts compiling is a broken law.
//!
//! | Probe | Law |
//! | --- | --- |
//! | `01_principal_has_no_public_mint` | the sealed principal cannot be assembled from outside the crate |
//! | `02_verified_subject_has_no_public_mint` | nor can the input the mint consumes — sealing only the principal would be a half-seal |
//! | `03_takeover_lease_is_not_clone` | one live lease per recipient; asserted through a `T: Clone` BOUND, because autoref makes a `.clone()` call compile on `&T` and pass for the wrong reason |
//! | `04_seat_principal_is_not_an_authority_principal` | answer Q2's ratified permanent non-conversion between the two principal namespaces |
//! | `05_dto_cannot_carry_a_principal` | no caller-authored value yields an identity |
//! | `06_sealed_router_admits_no_further_route` | `seal` consumes the builder, so the sealed route set is immutable by construction |
//! | `07_redacted_value_has_no_public_equality` | a holder cannot confirm guesses of secret material through `PartialEq` |
//!
//! R4H-H adds four. The preflight numbered them 04-07, which was written before
//! this crate existed; at the accepted base 01-07 are taken, so they are 08-11.
//!
//! | Probe | Law |
//! | --- | --- |
//! | `08_host_limits_have_no_default` | eight bounds, none of which can materialise on its own |
//! | `09_listener_use_after_host_transfer` | the host takes the accepted listener BY VALUE, so there is no second accept owner |
//! | `10_running_host_not_clone` | one owner stops a host once; asserted through a `T: Clone` bound, not a `.clone()` call |
//! | `11_raw_router_cannot_start_host` | a bare `axum::Router` — even one unsealed from A's own `ApiRouter` — cannot become the authenticated service |
//!
//! H inherits A's principal-forging and non-Serde probes, and L's non-clone
//! listener probe, by accepted dependency receipt rather than by copying them
//! into new source.
//!
//! Probes are split one **error phase** per file, following
//! `grapt-types/tests/compile_fail.rs`. rustc stops after a name-resolution or
//! privacy failure and never reaches trait selection or borrow checking, so two
//! seals in one file silently witness only the first.
//!
//! # Two probes the preflight proposed, and why they are not here
//!
//! **"the principal is not `Deserialize`"** has no subject to probe. This crate
//! takes no `serde` dependency in any table, so there is no trait in scope for a
//! probe to fail against, and a probe that referenced one would be testing the
//! probe's own dev-dependency rather than this crate. The property is stronger
//! than a probe anyway: it is structural, held by the `allowed_external_dependencies`
//! gate, which turns red if `serde` is ever added to any table.
//!
//! **"a runtime refusal for registering after seal"** was replaced by probe 06,
//! which witnesses the stronger property. See that file.
//!
//! # What CANNOT be witnessed here, stated rather than skipped
//!
//! There is no probe for "authorization cannot be allowed without a capability
//! proof", because that is not a compile-time property — `authorize` accepts an
//! `Option<SeatCapabilityProof>` and refuses `None` at runtime. The seal on the
//! POSITIVE path is that a `SeatCapabilityProof` borrows a real
//! `grapt_types::CapabilityGrant`, and `CapabilityGrant::from_live` requires a
//! `LivePermissionDecision` whose mint is `pub(crate)` inside `grapt-types`. No
//! crate in this workspace can mint one today, including this one and its tests.
//!
//! That is ruling R4HA-04 working as written — production A authenticates and
//! refuses, and an allow requires a proof supplied through the later composition
//! seam — and it has a cost this lane discloses rather than hides: the allow
//! path and the two proof-mismatch branches in `authorize` are unreachable from
//! any witness A can write. `R4HA_RESIDUALS.md` records it as the composition
//! lane's obligation.

#[test]
fn transport_boundary_seals_are_rejected_by_the_compiler() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/probes/*.rs");
}
