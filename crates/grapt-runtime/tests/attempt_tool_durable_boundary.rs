//! Structural ownership witnesses for the governed tool boundary.

const SOURCE: &str = include_str!("../src/tool_protocol.rs");

fn executor_port() -> &'static str {
    let start = match SOURCE.find("pub trait GovernedToolExecutor") {
        Some(start) => start,
        None => panic!("governed executor port must exist"),
    };
    let tail = &SOURCE[start..];
    let end = match tail.find("pub struct GovernedToolRunner") {
        Some(end) => end,
        None => panic!("governed runner must follow the executor port"),
    };
    &tail[..end]
}

#[test]
fn runtime_helper_is_authority_neutral_and_accepts_only_a_resolved_coordinate() {
    assert!(
        SOURCE.contains("pub fn run_effect(\n        &self,\n        capability: CapabilityId,"),
        "the neutral helper must require a resolved capability coordinate"
    );
    assert!(
        !SOURCE.contains("AuthorityLease") && !SOURCE.contains("run_authorized"),
        "runtime must own neither the live lease nor a second authorized entry"
    );
}

#[test]
fn executor_port_requires_owned_bounded_carriers_and_closed_terminal() {
    let port = executor_port();
    assert!(
        port.contains("progress: BoundedProgressSink,"),
        "progress must be a distinct bounded carrier"
    );
    assert!(
        port.contains("cancellation: ToolCancellation,"),
        "cancellation must be an owned carrier"
    );
    assert!(
        port.contains("artifacts: BoundedArtifactWriter<S>,"),
        "artifact writer must be mandatory and owned"
    );
    assert!(
        port.contains(") -> BoxFuture<'static, ToolExecutionTerminal>;"),
        "executor may return only the closed runtime terminal"
    );
}

#[test]
fn executor_port_receives_capability_identity_not_provider_tool_text() {
    let port = executor_port();
    assert!(port.contains("capability: CapabilityId,"));
    assert!(
        !port.contains("tool: String") && !port.contains("tool: &str"),
        "provider-authored tool labels must never become executor authority"
    );
}

#[test]
fn artifact_capture_precedes_terminal_artifact_construction() {
    let capture = match SOURCE.find(".store\n            .capture(source") {
        Some(index) => index,
        None => panic!("bounded store capture must exist"),
    };
    let terminal = match SOURCE.find("ToolTerminalArtifact::new(") {
        Some(index) => index,
        None => panic!("terminal artifact construction must exist"),
    };
    assert!(
        capture < terminal,
        "artifact bytes must be durable before terminal reference construction"
    );
}

/// The production dispatch path must not reach the attempt-dropping default.
///
/// `ToolRunner::run_with_attempt` is defaulted so the ten test doubles that never
/// observe an attempt stay unchanged. That default FORWARDS to `run` and drops
/// the attempt — correct for a double, silent data loss for production. Nothing
/// in the type system says which implementors are which, so this asserts the two
/// facts that keep the production path off the default: the supervisor must call
/// the attempt-carrying entry point, and it must be given a real attempt to pass.
#[test]
fn the_supervisor_dispatches_through_the_attempt_carrying_entry_point() {
    const DRIVERS: &str = include_str!("../src/supervisor/task_drivers.rs");
    const LOOP: &str = include_str!("../src/supervisor/attempt_loop.rs");
    const PORT: &str = include_str!("../src/supervisor.rs");
    assert!(
        DRIVERS.contains("run_with_nested("),
        "the tool task must dispatch through the attempt-carrying entry point"
    );
    assert!(
        PORT.contains("self.run_with_attempt(attempt, call, tool, input)"),
        "the nested-aware default must forward the admitted attempt"
    );
    assert_eq!(
        DRIVERS.matches(".run(").count(),
        0,
        "no tool dispatch may call the attempt-dropping `run` directly; every call \
         site must carry the attempt, or a production runner silently loses it"
    );
    assert!(
        LOOP.contains("Some(attempt.clone()),"),
        "the loop that owns the attempt must hand it to the spawned tool rather \
         than passing None, which would reach the default by another route"
    );
}
