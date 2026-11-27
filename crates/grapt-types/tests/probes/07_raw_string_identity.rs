// Probe 7: a raw string cannot be used as a session, operation,
// effective-graph or service identity in a store-command position.
use grapt_types::{EffectiveGraphId, OperationId, ServiceInstanceId, SessionId};

fn store_command(_session: SessionId) {}
fn store_operation(_operation: OperationId) {}
fn store_graph(_graph: EffectiveGraphId) {}
fn store_service(_instance: ServiceInstanceId) {}

fn main() {
    store_command("ses_04b1327e9ffeovGoacE6KI7KVu");
    store_operation("op_1");
    store_graph("efg_1");
    store_service("inst_1");
}
