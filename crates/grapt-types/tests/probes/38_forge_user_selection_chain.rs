// Probe 38: the COMPLETE predecessor chain to cross-family subagent authority.
//
// Probe 25 proved `UserModelSelection`'s fields are private. Probe 26 proved the
// authority trait is sealed. Both were true, both were green, and both missed
// this: sealing a trait stops a foreign type IMPLEMENTING it and does nothing
// about CALLING the public methods of the implementation that already ships.
//
// The chain was: author an `AuthenticatedClientId` and a `SelectorVersion`
// (opaque ids, public `new`), call `UserSelectionSurface::establish`, then call
// the trait's public `select` to mint the one value amendment section 7.5 lets
// cross the model-family and provider boundary while `agent default` is chosen.
// No human ever clicked anything.
//
// `establish` is now crate-private, so there is no surface to call `select` on.
use grapt_types::{
    AuthenticatedClientId, DecisionNonce, RequestId, SelectorVersion, SubagentModelPreference,
    UserSelectionAuthority, UserSelectionSurface,
};

fn main() {
    let client = AuthenticatedClientId::new("client-invented").expect("client");
    let selector = SelectorVersion::new("selector-invented").expect("selector");

    // The step that must not exist.
    let surface = UserSelectionSurface::establish(client, selector);

    let selection = surface.select(
        RequestId::new("req-invented").expect("request"),
        DecisionNonce::new("nonce-invented").expect("nonce"),
        todo!("any ModelRef the attacker resolved from its own catalog"),
    );
    let _crosses_families = SubagentModelPreference::user_selected(selection);
}
