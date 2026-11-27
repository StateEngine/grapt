// Probe 03: the session domain is unreachable.
//
// `grapt-session` owns session creation and history. The client must observe
// both through `v2.session.create` and `v2.message.list`; a domain call would
// bypass the DTO surface whose exact shape is the compatibility contract.
use grapt_session::SessionStore;

fn main() {
    let _bypass: Option<SessionStore> = None;
}
