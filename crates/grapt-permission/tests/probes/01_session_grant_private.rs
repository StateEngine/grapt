// The transitional approval record is private to grapt-permission and is NOT
// authority. `policy` is a private module, `UnresolvedApproval::record` is
// crate-private, and the type deliberately does not exist as a `SessionGrant`.
fn main() {
    let action = grapt_permission::policy::UnresolvedAction::new("fs.read");
    let _approval = grapt_permission::policy::UnresolvedApproval::record(action);
}
