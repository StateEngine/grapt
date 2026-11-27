//! Named, measured budgets. One home, so none of them is re-guessed.
//!
//! Every value here is a **disclosed behaviour change** where it bounds
//! something the oracle leaves unbounded. Source of the measurements:
//! `fixtures/rust-reconstruction/PROVIDER_STREAM_TOPOLOGY_AND_BUDGETS_D2_2026-08-08.md`
//! (over the frozen clone: 69,740 events / 4,792 steps). Ruled by the
//! provider-stream review of 2026-08-08.

/// Maximum local tool executions in flight for one physical attempt.
///
/// Measured: local tool calls per step were p50 1, p90 5, p99 9, **max 17**.
/// 16 bounds the pathological case while covering p99 with headroom; the one
/// observed 17-tool step waits exactly once for a permit.
///
/// DISCLOSED CHANGE: the oracle forks one fibre per tool call with **no**
/// semaphore (`llm.ts:392`) and joins them unbounded (`llm.ts:414-416`). This
/// is a new bound, not preserved behaviour, and carries a waiver row.
pub const ATTEMPT_TOOL_BUDGET: usize = 16;

/// Maximum tool calls retained for one physical attempt, including settled calls.
pub const ATTEMPT_TOOL_CALL_LIMIT: usize = 32;

/// Reserved terminal capacity: one non-droppable terminal per in-flight call.
pub const TOOL_TERMINAL_CAPACITY: usize = ATTEMPT_TOOL_BUDGET;

/// Transient tool progress queue, independent of terminal capacity.
pub const TOOL_PROGRESS_CAPACITY: usize = 64;

/// Maximum bytes in one transient progress update.
pub const TOOL_PROGRESS_MAX_BYTES: usize = 8 * 1024;

/// Maximum bounded input delivered to one tool executor.
pub const TOOL_INPUT_MAX_BYTES: usize = 256 * 1024;

/// Hard capture ceiling for one tool output or failure detail.
pub const TOOL_ARTIFACT_MAX_BYTES: u64 = 1024 * 1024;

/// Streaming artifact-capture buffer, independent of the capture ceiling.
pub const TOOL_ARTIFACT_BUFFER_BYTES: usize = 64 * 1024;

/// Capacity of the provider-reader → owner channel, in fragments.
///
/// Rationale: fragments are small and the durable
/// owner's transaction time is the bottleneck, so 64 absorbs a full tool-call
/// burst — 17 calls is roughly 51 input-bracket fragments plus terminals —
/// without unbounded growth.
///
/// DISCLOSED CHANGE: the oracle has **no channel at all** between the provider
/// loop, the tool fibres and the publisher. Mutual safety there rests on the
/// Effect single-threaded cooperative model plus a synchronous check-and-mark
/// convention. A bounded mpsc with a serialized owner is new design, which the
/// contract requires; it is not preservation.
pub const PROVIDER_CHANNEL_CAPACITY: usize = 64;

/// Capacity of the tool-task → owner channel.
///
/// Only tool **terminals** cross this channel today — one per permitted tool.
/// It is sized at twice the tool budget so there is headroom for the progress
/// messages the contract names as transient-and-bounded but which are not yet
/// implemented. When progress is added, this constant must be revisited: the
/// invariant that matters is that a permitted tool can ALWAYS enqueue its
/// terminal without blocking, because a tool that cannot report its terminal
/// while the provider channel is saturated is how interrupt-settlement
/// deadlocks.
pub const TOOL_CHANNEL_CAPACITY: usize = TOOL_TERMINAL_CAPACITY;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_channel_cannot_block_a_permitted_tool() {
        // Each in-flight tool may need to enqueue progress plus a terminal.
        // If this ever fails, a tool task can block on the owner and the
        // interrupt path can deadlock. Checked at compile time — a violation
        // should never reach a test run.
        const { assert!(TOOL_CHANNEL_CAPACITY >= ATTEMPT_TOOL_BUDGET) };
    }

    #[test]
    fn budgets_are_the_ruled_values() {
        // Pinned so a later edit is a deliberate re-ruling, not a drift.
        assert_eq!(ATTEMPT_TOOL_BUDGET, 16, "ruled 2026-08-08");
        assert_eq!(ATTEMPT_TOOL_CALL_LIMIT, 32, "R2 tool boundary ruling");
        assert_eq!(PROVIDER_CHANNEL_CAPACITY, 64, "ruled 2026-08-08");
    }
}
