// Probe 2, first clause: a SessionGrant cannot be minted from outside the
// crate at all. `new` no longer exists and `mint` is crate-private; the only
// route is `PermissionSettlement::session_grant`, which requires a settlement
// whose decision actually granted Session scope.
//
// The non-persistability clause lives in `02b_session_grant_not_persistable.rs`
// because these are privacy/name-resolution errors, which abort before trait
// selection runs.
use grapt_types::{
    CapabilityId, EffectClass, EgressAuthority, FilesystemAuthority, RealizedEnforcement,
    ResolvedCapability, SessionGrant, SessionId,
};

fn main() {
    let session: SessionId = match "ses_1".parse() {
        Ok(id) => id,
        Err(_) => return,
    };
    let capability = CapabilityId::derive(&ResolvedCapability::new(
        EffectClass::FilesystemRead,
        None,
        FilesystemAuthority::default(),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    ));
    let _by_new = SessionGrant::new(session.clone(), capability.clone());
    let _by_mint = SessionGrant::mint(session, capability);
}
