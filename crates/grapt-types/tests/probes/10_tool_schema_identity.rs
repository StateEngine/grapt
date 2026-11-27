// Probe 10, F1A clause: a tool's definition identity and its schema version are
// distinct nominal types, so a schema version can never stand in for the
// definition it describes (or the reverse) when a caller wires a tool up.
//
// The rest of law row 10 — "constructing a tool/provider schema independently
// of its authoritative Rust argument/request type" — cannot be witnessed here:
// no schema type exists in any crate yet. Its owner is the tool/schema lane
// (survey row G3: the Rust argument type is authoritative and the generated
// JSON/provider schema is a checked projection). That clause is recorded as
// owed in `tests/compile_fail.rs`, not silently skipped.
use grapt_types::{ToolDefinitionId, ToolSchemaVersion};

fn register(_definition: ToolDefinitionId, _schema: ToolSchemaVersion) {}

fn main() {
    let definition = match ToolDefinitionId::new("read") {
        Some(id) => id,
        None => return,
    };
    let schema = match ToolSchemaVersion::new("1") {
        Some(v) => v,
        None => return,
    };
    register(schema, definition);
}
