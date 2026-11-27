//! Row-11 NEGATIVE FIXTURE. Deliberately fails its own lint gate.
//!
//! The sibling of `fixtures/negative/ambient-clock`, for the other half of the
//! row-11 law. `trybuild` and rustc cannot reject `getrandom::fill` — it is
//! perfectly valid Rust. The enforceable gate is Clippy's `disallowed_methods`,
//! configured in the workspace `clippy.toml`. This crate exists so that gate can
//! be PROVEN to fire rather than assumed:
//!
//! ```sh
//! CLIPPY_CONF_DIR=. cargo clippy \
//!   --manifest-path fixtures/negative/ambient-randomness/Cargo.toml -- -D warnings
//! ```
//!
//! must FAIL, and the failure must name `clippy::disallowed_methods` on the call
//! below. If it passes, the gate has been silently lost — which matters more
//! here than for most lints, because the entry it proves is what keeps OS
//! randomness confined to one named adapter. A writer identity that is not
//! unique over time makes an ownership transfer look like a re-election by the
//! incumbent, and the epoch advance that fences the previous owner never runs.
//!
//! The crate is named in the root `Cargo.toml` `exclude` list, so no workspace
//! command reaches it and it is compiled only by the explicit command above.
//! `CLIPPY_CONF_DIR` is set explicitly so the fixture is judged by the
//! workspace's real `clippy.toml` rather than a second copy that could drift
//! away from the law it is meant to prove.
//!
//! There is deliberately no `#[allow]` anywhere in this file.

/// A durable owner reaching for ambient OS randomness — exactly what the law
/// forbids outside the named production adapter.
pub fn forbidden_ambient_randomness() -> [u8; 16] {
    let mut bytes = [0u8; 16];
    let _ = getrandom::fill(&mut bytes);
    bytes
}
