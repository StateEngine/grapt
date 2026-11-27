// Probe 26 (routing/adapter provenance, subagent routing row): no foreign type
// may declare itself the user-selection authority.
//
// Amendment §7.5: "no project policy, catalog, adapter or parent model may
// cross the model-family or provider boundary while `agent default` is
// selected". If a policy layer could implement `UserSelectionAuthority`, it
// would mint selections and cross the boundary itself.
//
// `UserSelectionAuthority` requires a supertrait declared in a private module,
// so it is sealed: `UserSelectionSurface` is the only implementation, and the
// settings/transport owner is the only component that holds one.
use grapt_types::{AuthenticatedClientId, SelectorVersion, UserSelectionAuthority};

#[derive(Debug)]
struct ProjectPolicyLayer {
    client: AuthenticatedClientId,
    selector: SelectorVersion,
}

impl UserSelectionAuthority for ProjectPolicyLayer {
    fn authenticated_client(&self) -> &AuthenticatedClientId {
        &self.client
    }

    fn selector_version(&self) -> &SelectorVersion {
        &self.selector
    }
}

fn main() {}
