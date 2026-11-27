//! Compile-time probes for the provider stream contract, and for the two laws
//! that can only be witnessed from a CONSUMER crate. The compiler must REJECT
//! each probe file; a probe that starts compiling is a broken law.
//!
//! | Probe | Law |
//! | --- | --- |
//! | `04`, `09` | a provider-specific parameter is unnameable in another family's request |
//! | `08` | `NoVisibleOutput` cannot be forged |
//! | `62b` | a Responses-only prompt-cache coordinate is unnameable on generic chat-completions requests; Anthropic's equivalent is a compile-fail doctest on `AnthropicMessagesRequest` |
//! | `46_rogue_catalog_reaches_policy_authority` | a self-authored catalog cannot enter policy authority: `SnapshotBuilder::model` requires the registry-issued `ResolvedModelTarget`. Cross-crate, so it lives here rather than in `grapt-types` |
//! | `49_snapshot_clone_duplicates_authority`, `49b_snapshot_equality_oracle`, `49c_snapshot_debug_prints_authority` | the authority snapshot is not `Clone` (one admission is one attempt), not comparable (equality over sealed routing facts is an oracle), and not `Debug` (it printed them verbatim) |
//! | `56`…`61` | the §8.4 secret seam, SPLIT one clause per probe: an identity cannot be minted from bytes (`56`), read back out as bytes (`57`), or minted by a caller (`58`); the receipt does not publish its own preimage (`59`); the unkeyed digest constructor is unreachable (`60`); and a contributor does not hand out the identity it holds (`61`) |
//!
//! # Why `56`-`61` are six files and not one
//!
//! Because rustc gives up after name resolution. A probe carrying a resolution
//! error and a type error only ever witnesses the resolution error, so a single
//! "the secret seam holds" probe would have proven whichever clause happened to
//! fail earliest and silently covered for the other five. The six here fail with
//! six distinct codes — `E0423`, `E0624`, `E0624`, `E0599`, `E0433`, `E0616` —
//! and the two sharing a code fail on different items. Each `.stderr` was read,
//! and each contains exactly ONE `error[` line; more than one would mean a probe
//! had started witnessing something incidental.
//!
//! They are also six laws rather than one restated six ways: a crate with a
//! public mint passes `56` and `57`; one whose identity is readable but
//! unforgeable passes `56` and `58`; one that publishes its preimage passes all
//! of `56`-`58` and `60`-`61`. None of them subsumes another.
//!
//! # Probes retired, and why — a deleted probe is a receipt too
//!
//! `10`, `50` and `50b` asserted properties of `PreparedPhysicalAttemptRequest`, which
//! was deleted from the foundational boundary: it was live request-validation
//! and receipt-fusion implementation, not an identity. `50` and `50b` are worth a second
//! sentence, because they were green, on-law and delta-sensitive and STILL did not prove
//! what they were credited with — they witnessed that two accessors were gone, while
//! `PhysicalAttemptAuthoritySnapshot::receipt` is public, so the pair they were meant to
//! prevent was already obtainable before `prepare` was ever called.
//!
//! `51`, `51b`, `52`, `53`, `54` and `55` were retired with the
//! synchronous dispatch seam they described. `51`
//! and `51b` had a second defect worth keeping: a non-`Clone` request is cosmetic
//! while every concrete per-route request keeps public `Clone`/`Copy` fields, so a
//! borrow is rebuilt field by field.
//!
//! `46b` was removed for a different reason: it was NOT
//! delta-sensitive — reverting the seal left it failing, because it actually tested
//! a pre-existing type mismatch. A probe that fails both before and after the fix
//! witnesses nothing.
//!
//! Two probe-authoring hazards from those rounds outlive the probes:
//!
//! - a diverging `unimplemented!()`/`todo!()` coerces to ANY type, so a probe
//!   sourcing its value that way compiles and proves nothing;
//! - a variance probe must use the relabelling the WRONG variance would permit.
//!   Relabelling between two UNRELATED lifetimes fails under covariance too, so
//!   that form is insensitive; the delta-sensitive shape is
//!   `fn shorten<'long: 'short, 'short>(x: T<'long>) -> T<'short>`. A mutation pass
//!   caught it, not the `.stderr`.
//!
//! Every `.stderr` in this directory was read, not just regenerated.
//!
#[test]
fn provider_probes_are_rejected_by_the_compiler() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/probes/*.rs");
}
