//! The one JSON object this binary prints, and the rules about what it says.
//!
//! Exactly one JSON object reaches stdout per invocation, carrying the
//! operation, every supplied bound, the observed outcome and the typed receipt
//! or refusal. Human prose goes to stderr. A caller that reads stdout gets
//! either one parseable object or nothing at all.
//!
//! # This is not a compatibility surface
//!
//! E1a preserves no existing machine-readable contract, so this receipt is not
//! master hard-compatibility item 7 and must never be described as one. It is
//! a new surface with no consumer yet, and saying so here keeps a later reader
//! from treating it as frozen.
//!
//! # The one bound this crate owns, declared as the exception it is
//!
//! [`ServeBounds`] says below that this module re-validates nothing, because
//! the newtypes that validate each bound are owned by `grapt-api` and
//! `grapt-location` and a second carrier of an existing law is a defect.
//! [`ServeWindow`] is a deliberate, named exception: **no crate owns a process
//! run window**, so there is no first carrier to duplicate. The doctrine
//! forbids becoming the *second* carrier of a law, not owning a law nobody
//! holds — and an unannounced exception to a stated doctrine is how doctrines
//! rot, so it is announced here and in the crate doc rather than discovered.
//!
//! It is validated *here*, by a local constructor. Adding it to
//! `ApiHostLimits` would be an edit to another crate, which this lane refuses.
//!
//! # Why the JSON is hand-rendered
//!
//! The frozen dependency closure for this crate is `grapt-api`,
//! `grapt-location`, `grapt-service`, `grapt-types` and `tokio`. A
//! serialization crate is not in it, and adding one to save thirty lines of
//! escaping would be a dependency edge bought for convenience — the exact
//! trade the closure exists to refuse. The writer below is deliberately small
//! and total.

use grapt_api::ApiHostExit;
use std::fmt::Write as _;

/// Every bound `serve` was given, in the order the receipt prints them.
///
/// The bounds arrive as plain integers because this struct's whole job is to
/// echo what the operator supplied. The typed newtypes that validate them are
/// owned by `grapt-api` and `grapt-location`; re-validating here would make
/// this a second carrier of a law that already has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeBounds {
    pub max_connections: usize,
    pub max_inflight: usize,
    pub max_headers: usize,
    pub max_request_head_bytes: usize,
    pub max_request_body_bytes: usize,
    pub max_requests_per_connection: usize,
    pub header_read_allowance_ms: u64,
    pub shutdown_drain_allowance_ms: u64,
    pub bind_attempts: usize,
    pub max_resolved_addresses: usize,
    /// How long the host serves before it stops itself.
    ///
    /// A bound like every other one here: required, never defaulted, and
    /// echoed. An operator interrupt still stops the host sooner — this is the
    /// bound on an unattended run, not the only way out of one.
    pub serve_window: ServeWindow,
}

/// The bounded lifetime of one `serve` process.
///
/// This binary already refuses to run unbounded in every dimension it has: ten
/// numeric bounds, none defaulted, over types that deliberately implement no
/// `Default`. Its *lifetime* was the one dimension left unbounded, and closing
/// it is that same law rather than a new one. It is emphatically not an
/// affordance for a witness — a bound justified by its test is a hook, and this
/// one is justified by the rule the other ten already obey.
///
/// # A duration, never a clock
///
/// The window is a relative duration driven through the deadline/timer seam the
/// host already owns. It is not a wall-clock read, and nothing derived from it
/// ever becomes a timestamp: the receipt echoes the configured milliseconds —
/// a number the operator supplied — and carries no `started_at` and no
/// `will_stop_at`. Durable wall-clock attribution is exactly the authority that
/// keeps `service ensure|stop` refused at E1a, and a timestamp smuggled into
/// the serve receipt would unblock it in the one place nobody is looking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServeWindow(std::num::NonZeroU64);

impl ServeWindow {
    /// Refuse zero, exactly as `ShutdownDrainAllowance` refuses `Duration::ZERO`.
    ///
    /// A zero window is a host that is asked to serve for no time at all, which
    /// is not a smaller serve but a different verb. The sibling allowance in
    /// the accepted host limits refuses zero for the same reason, and a bound
    /// this crate owns must not be laxer than the bounds it borrows.
    pub fn try_new(milliseconds: u64) -> Option<Self> {
        std::num::NonZeroU64::new(milliseconds).map(Self)
    }

    /// The configured window, in milliseconds, exactly as supplied.
    pub fn milliseconds(self) -> u64 {
        self.0.get()
    }

    /// The window as a duration, for the timer that enforces it.
    pub fn as_duration(self) -> std::time::Duration {
        std::time::Duration::from_millis(self.0.get())
    }
}

/// Why a `serve` process stopped, as a closed vocabulary.
///
/// A bool would say "did the window fire" and lose which of two real events
/// ended the run; prose would let the answer drift. The match below is
/// exhaustive with no wildcard, so a third trigger cannot be added without
/// naming its own rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopTrigger {
    /// The configured run window elapsed.
    WindowElapsed,
    /// An operator interrupted the process.
    OperatorInterrupt,
}

impl StopTrigger {
    /// The stable machine label for this trigger.
    pub fn label(self) -> &'static str {
        match self {
            StopTrigger::WindowElapsed => "window_elapsed",
            StopTrigger::OperatorInterrupt => "operator_interrupt",
        }
    }
}

/// The posture every request meets at E1a.
///
/// The host is composed with an authenticated service and E1a issues no
/// credential, so every request is answered 401. That is a deliberate product
/// state, not an accident, and the ready receipt discloses it: an operator who
/// starts this daemon and sees a healthy bind must not have to discover the
/// posture by sending a request.
pub const CREDENTIAL_REFUSES_EVERYTHING: &str = "every_request_answers_401_no_credential_is_issued";

/// The ready receipt persisted once the host is bound and listening.
///
/// It is written to the settled custody root as a durable artifact rather than
/// printed, because `serve` is a long-lived process with exactly one JSON
/// object on stdout and that object is the *exit* receipt. A ready receipt
/// that were only computed and dropped would make every law about its contents
/// a law about a value no consumer can observe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeReceipt {
    /// The address the operating system actually assigned, taken from the
    /// host's own readiness rather than from the requested bind: a plan that
    /// asked for port 0 and a listener that got 53 211 are different facts, and
    /// the observed one is the only one a client can connect to.
    pub observed_address: String,
    pub max_connections: usize,
    pub max_inflight: usize,
    /// The standalone composition installs no event-stream routes.
    pub max_inflight_streams: usize,
    pub max_headers: usize,
    pub max_request_head_bytes: usize,
    pub max_request_body_bytes: usize,
    pub max_requests_per_connection: usize,
    pub header_read_allowance_ms: u64,
    pub shutdown_drain_allowance_ms: u64,
    pub bind_attempts: usize,
    pub max_resolved_addresses: usize,
    pub serve_window_ms: u64,
    pub credential_posture: &'static str,
}

impl ServeReceipt {
    /// Echo every supplied bound, and disclose the credential posture.
    ///
    /// Every field of [`ServeBounds`] appears here. A bound that is accepted on
    /// the command line and then not echoed is a bound the operator cannot
    /// confirm took effect, which makes the receipt a claim rather than
    /// evidence.
    pub fn from_bounds(bounds: &ServeBounds, observed_address: std::net::SocketAddr) -> Self {
        Self {
            observed_address: observed_address.to_string(),
            max_connections: bounds.max_connections,
            max_inflight: bounds.max_inflight,
            max_inflight_streams: grapt_api::MaxInflightStreams::disabled().value(),
            max_headers: bounds.max_headers,
            max_request_head_bytes: bounds.max_request_head_bytes,
            max_request_body_bytes: bounds.max_request_body_bytes,
            max_requests_per_connection: bounds.max_requests_per_connection,
            header_read_allowance_ms: bounds.header_read_allowance_ms,
            shutdown_drain_allowance_ms: bounds.shutdown_drain_allowance_ms,
            bind_attempts: bounds.bind_attempts,
            max_resolved_addresses: bounds.max_resolved_addresses,
            serve_window_ms: bounds.serve_window.milliseconds(),
            credential_posture: CREDENTIAL_REFUSES_EVERYTHING,
        }
    }

    /// The receipt as one JSON object, keys in declaration order.
    pub fn to_json(&self, operation: &str) -> String {
        let mut out = String::from("{");
        push_string(&mut out, "operation", operation);
        out.push(',');
        push_string(&mut out, "observed_address", &self.observed_address);
        out.push(',');
        push_number(&mut out, "max_connections", self.max_connections as u64);
        out.push(',');
        push_number(&mut out, "max_inflight", self.max_inflight as u64);
        out.push(',');
        push_number(
            &mut out,
            "max_inflight_streams",
            self.max_inflight_streams as u64,
        );
        out.push(',');
        push_number(&mut out, "max_headers", self.max_headers as u64);
        out.push(',');
        push_number(
            &mut out,
            "max_request_head_bytes",
            self.max_request_head_bytes as u64,
        );
        out.push(',');
        push_number(
            &mut out,
            "max_request_body_bytes",
            self.max_request_body_bytes as u64,
        );
        out.push(',');
        push_number(
            &mut out,
            "max_requests_per_connection",
            self.max_requests_per_connection as u64,
        );
        out.push(',');
        push_number(
            &mut out,
            "header_read_allowance_ms",
            self.header_read_allowance_ms,
        );
        out.push(',');
        push_number(
            &mut out,
            "shutdown_drain_allowance_ms",
            self.shutdown_drain_allowance_ms,
        );
        out.push(',');
        push_number(&mut out, "bind_attempts", self.bind_attempts as u64);
        out.push(',');
        push_number(
            &mut out,
            "max_resolved_addresses",
            self.max_resolved_addresses as u64,
        );
        out.push(',');
        push_number(&mut out, "serve_window_ms", self.serve_window_ms);
        out.push(',');
        push_string(&mut out, "credential_posture", self.credential_posture);
        out.push('}');
        out
    }
}

/// The exit disposition, rendered.
///
/// A forced close is a different fact from a graceful stop: connections were
/// still live when the drain allowance expired and were aborted. Rendering it
/// as `stopped_gracefully` would tell an operator the system settled when it
/// did not, which is the one thing an exit receipt exists to prevent.
///
/// The match is exhaustive with no wildcard, so a fourth `ApiHostExit` variant
/// stops this crate compiling until it names its own rendering rather than
/// inheriting one.
/// It carries no `#[allow(dead_code)]`, and that absence is the point: `serve`
/// calls it on the host's own terminal receipt, so the arm below is the one an
/// operator's exit code and exit receipt are rendered from. A mutation of any
/// arm changes what the binary prints. While this function had no caller in
/// `src/`, a witness that reddened when it was mutated proved a reference and
/// not a product law.
pub fn render_exit(exit: &ApiHostExit) -> &'static str {
    match exit {
        ApiHostExit::StoppedGracefully { .. } => "stopped_gracefully",
        ApiHostExit::ForcedClose { .. } => "forced_close",
        ApiHostExit::Failed { .. } => "failed",
    }
}

/// One JSON string member.
fn push_string(out: &mut String, key: &str, value: &str) {
    out.push('"');
    push_escaped(out, key);
    out.push_str("\":\"");
    push_escaped(out, value);
    out.push('"');
}

/// One JSON number member.
fn push_number(out: &mut String, key: &str, value: u64) {
    out.push('"');
    push_escaped(out, key);
    out.push_str("\":");
    // `write!` to a String is infallible; the result is consumed so the
    // workspace `unused_must_use` deny has nothing to complain about and no
    // `unwrap` is reachable.
    let _ = write!(out, "{value}");
}

/// JSON string escaping, total over every `char`.
fn push_escaped(out: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", control as u32);
            }
            other => out.push(other),
        }
    }
}

/// One JSON object carrying an operation and a single outcome string.
pub fn simple_json(operation: &str, key: &str, value: &str) -> String {
    let mut out = String::from("{");
    push_string(&mut out, "operation", operation);
    out.push(',');
    push_string(&mut out, key, value);
    out.push('}');
    out
}

/// The observation receipt: what `status` saw, and who holds the slot.
///
/// `state` is carried as its own member rather than folded into `outcome`,
/// because the two answer different questions — which verb settled, and what it
/// found — and a consumer keyed on `outcome` keeps working while an operator
/// finally learns whether the slot was empty, held, or undecodable.
///
/// **An absent owner is an ABSENT MEMBER, never an empty string.** `""` would be
/// a name no owner can have and every consumer must special-case; leaving the
/// key out says "there is no owner" in the one way JSON already has.
pub fn observation_json(outcome: &str, state: &str, owner: Option<&str>) -> String {
    let mut out = String::from("{");
    push_string(&mut out, "operation", "service");
    out.push(',');
    push_string(&mut out, "outcome", outcome);
    out.push(',');
    push_string(&mut out, "state", state);
    if let Some(owner) = owner {
        out.push(',');
        push_string(&mut out, "owner", owner);
    }
    out.push('}');
    out
}

/// One JSON object carrying an operation, an outcome and one named detail.
pub fn detailed_json(
    operation: &str,
    key: &str,
    value: &str,
    detail_key: &str,
    detail_value: &str,
) -> String {
    let mut out = String::from("{");
    push_string(&mut out, "operation", operation);
    out.push(',');
    push_string(&mut out, key, value);
    out.push(',');
    push_string(&mut out, detail_key, detail_value);
    out.push('}');
    out
}
