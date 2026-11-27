// Probe 32b: a live permission decision cannot be assembled field by field.
//
// Probe 32 proves the mint is crate-private. Sealing only the mint would leave
// the chain open at its last link, so this proves the struct's fields are
// private too: there is no way to skip `mint`'s coherence checks — the
// grant/scope agreement, the refusal of a `Reject`, the clock-stamped answer
// time — by building the value directly.
use grapt_types::LivePermissionDecision;

fn main() {
    let _assembled = LivePermissionDecision {
        permission: todo!(),
        decision: todo!(),
        principal: todo!(),
        source: todo!(),
        capability: todo!(),
        scope: todo!(),
        policy: todo!(),
        effective_graph: todo!(),
        enforcement: todo!(),
        client: todo!(),
        request: todo!(),
        nonce: todo!(),
        answered_at: todo!(),
    };
}
