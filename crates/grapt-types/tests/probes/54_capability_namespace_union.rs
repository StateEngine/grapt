// Probe 54: model-target, ordinary-effect and checkpoint-restore capabilities
// are three different identities, and the compiler — not a review comment —
// refuses every substitution.
//
// Master §8.1 defines capability identity as a resolved multidimensional
// EXECUTION/EFFECT. A model target is not an effect; it is a route to call.
// Both used to be spelled `CapabilityId`, and the merge was not merely untidy:
// `CapabilityId` derives `PartialEq`, and equality IS the authorization
// comparison, so any two canonical forms that could ever coincide would compare
// equal ACROSS the boundary. A routing identity would have answered for an
// effect grant.
//
// The nominal split is what makes that unwritable. `capability::tests` proves
// the three canonical namespaces stay pairwise disjoint at runtime; this probe
// proves the two TYPES cannot be exchanged at all, which no runtime test can
// witness because the code that would witness it does not compile.
//
// The model-target value is taken as a FUNCTION ARGUMENT. Building one here is
// impossible — `ModelTargetCapabilityId::from_canonical` is crate-private and
// there is no other producer — and attempting it would fail on the mint in an
// earlier error phase, witnessing a different law and hiding these three.
//
// One error phase: every line below is E0308, so rustc reports all of them
// rather than stopping at an earlier resolution or privacy failure.
use grapt_types::{
    CapabilityGrant, CapabilityId, EffectClass, EgressAuthority, FilesystemAuthority, GrantScope,
    CheckpointRestoreCapabilityId, ModelTargetCapabilityId, RealizedEnforcement,
    ResolvedCapability,
};

fn requires_effect(_: &CapabilityId) {}
fn requires_model_target(_: &ModelTargetCapabilityId) {}
fn requires_checkpoint_restore(_: &CheckpointRestoreCapabilityId) {}

fn substitutions(
    grant: &CapabilityGrant,
    target: &ModelTargetCapabilityId,
    restore: &CheckpointRestoreCapabilityId,
) {
    let effect = CapabilityId::derive(&ResolvedCapability::new(
        EffectClass::FilesystemRead,
        None,
        FilesystemAuthority::new(Vec::new(), Vec::new(), Vec::new()),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    ));

    // A route to call is not a governed effect.
    requires_effect(target);
    // And a governed effect is not a route to call.
    requires_model_target(&effect);
    // A compound restore is neither an ordinary effect nor a model route.
    requires_effect(restore);
    requires_model_target(restore);
    requires_checkpoint_restore(&effect);
    requires_checkpoint_restore(target);
    // The one that mattered: a grant approved for an exact effect cannot be
    // answered with a routing identity.
    let _ = grant.authorizes(target, &GrantScope::Once);
    let _ = grant.authorizes(restore, &GrantScope::Once);
}

fn main() {
    let _ = substitutions;
}
