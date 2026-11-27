// Probe 1: a SessionId cannot stand in for any other identity kind. All four
// named targets are checked, not just Permission.
use grapt_types::{FormId, OperationId, PermissionId, ServiceInstanceId, SessionId};

fn requires_permission(_: PermissionId) {}
fn requires_form(_: FormId) {}
fn requires_operation(_: OperationId) {}
fn requires_service_instance(_: ServiceInstanceId) {}

fn main() {
    let session: SessionId = match "ses_1".parse() {
        Ok(id) => id,
        Err(_) => return,
    };
    requires_permission(session.clone());
    requires_form(session.clone());
    requires_operation(session.clone());
    requires_service_instance(session);
}
