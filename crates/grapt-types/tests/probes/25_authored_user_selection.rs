// Probe 25 (routing/adapter provenance, subagent routing row): a user model
// selection is authority, not data anything may write.
//
// Amendment §7.5: "The GUI Subagent model selector (and exactly equivalent
// explicit user API/CLI input) is the authority for cross-family/provider
// routing". It is the ONE thing that may cross the boundary, so an adapter, a
// policy layer or the parent model writing `UserSelected { .. }` inline is a
// full bypass.
//
// `UserModelSelection`'s fields are private and it has no public constructor:
// the only producer is `UserSelectionAuthority::select`. The sealing clause —
// that no foreign type can BE that authority — lives in
// `26_foreign_selection_authority.rs`, because this file fails during privacy
// checking and would never reach trait selection.
use grapt_types::{SubagentModelPreference, UserModelSelection};

fn main() {
    let _crossing = SubagentModelPreference::user_selected(UserModelSelection {
        client: todo!(),
        request: todo!(),
        nonce: todo!(),
        selector: todo!(),
        target: todo!(),
    });
}
