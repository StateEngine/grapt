// Probe 31: a persistent grant cannot be assembled around a self-resolved
// capability.
//
// `CapabilityId::derive` is public by design — resolving a capability is not
// authorization, and the identity must be derivable to be compared. The defect
// was that `CapabilityGrant::new(capability, scope)` was public too, so any
// caller could resolve an effect of its choosing, wrap it in a grant and lease
// it as often as it liked with no permission decision anywhere in the chain.
//
// Amendment §8.2: "Persistent grants are built by the authority from the
// resolved capability." The only route is now `CapabilityGrant::from_live`,
// which reads both coordinates out of an authenticated decision.
use grapt_types::{
    CapabilityGrant, CapabilityId, EffectClass, EgressAuthority, FilesystemAuthority, GrantScope,
    RealizedEnforcement, ResolvedCapability,
};

fn main() {
    let capability = CapabilityId::derive(&ResolvedCapability::new(
        EffectClass::ProcessSpawn,
        None,
        FilesystemAuthority::default(),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    ));

    let _grant = CapabilityGrant::new(capability, GrantScope::Attempt);
}
