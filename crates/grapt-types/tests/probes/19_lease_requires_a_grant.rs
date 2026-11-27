// Probe 8, third clause (sealed minting): an authority lease cannot be
// conjured. `AuthorityLease::new` no longer exists and `mint` is crate-private;
// the only route is `CapabilityGrant::lease`, which refuses unless a real grant
// covers the requested capability at the requested scope.
use grapt_types::{
    AuthorityLease, CapabilityId, EffectClass, EgressAuthority, FilesystemAuthority, GrantScope,
    RealizedEnforcement, ResolvedCapability,
};

fn main() {
    let capability = CapabilityId::derive(&ResolvedCapability::new(
        EffectClass::FilesystemRead,
        None,
        FilesystemAuthority::default(),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    ));
    let _by_new = AuthorityLease::new(capability.clone(), GrantScope::Once);
    let _by_mint = AuthorityLease::mint(capability, GrantScope::Once);
}
