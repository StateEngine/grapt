//! The J02 witness corpus for the activation boundary.
//!
//! Every refusal reason the contract matrix owes (revision 3: R01..R22
//! minus the unreachable R12) is exercised here through the full
//! activate path against a real git worktree, real archive bytes on
//! disk, and a fixed clock. The corpus carries the positive witnesses
//! (fresh, reuse, upgrade, re-cut, tamper, undecodable record), the
//! crash-recovery and concurrency witnesses, and the failure
//! counterexamples (one per refusal reason), and each asserts
//! the layout invariants the law owes: nothing staged after a refusal,
//! the lock always released, the previous state retained or restored
//! exactly as the refusal names.
//!
//! R19 (staged-copy-incomplete) is witnessed by the mutation battery
//! (prefix TMA) rather than here: no portable short-copy trigger
//! exists on the shipped filesystems, and the harness refuses to claim
//! a mutation it did not run.
//!
//! The corpus is split by theme across sibling modules: `infra`
//! carries the shared scene and fixture machinery; `witnesses` the
//! positive witnesses; `crash_recovery` the crash-window and
//! concurrency witnesses; `refusals_inputs`, `refusals_fetch` and
//! `refusals_activation` one refusal family each.

mod crash_recovery;
mod infra;
mod refusals_activation;
mod refusals_fetch;
mod refusals_inputs;
mod witnesses;
