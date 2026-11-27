//! Row-11 NEGATIVE FIXTURE. Deliberately fails its own lint gate.
//!
//! `trybuild` and rustc cannot reject a std API — `SystemTime::now` is
//! perfectly valid Rust. The enforceable gate is Clippy's `disallowed_methods`,
//! configured in the workspace `clippy.toml`. This crate exists so that gate
//! can be PROVEN to fire rather than assumed:
//!
//! ```sh
//! CLIPPY_CONF_DIR=. cargo clippy \
//!   --manifest-path fixtures/negative/ambient-clock/Cargo.toml -- -D warnings
//! ```
//!
//! must FAIL, and the failure must name `clippy::disallowed_methods` on the
//! call below. If it passes, the gate has been silently lost.
//!
//! WHY THIS IS NOT A CARGO FEATURE. It used to be `grapt-types`'
//! `probe-ambient-clock` feature. That was wrong twice over. Features are
//! additive, so any dependent could switch the forbidden call back on — a
//! feature is not an access-control boundary. And it put a deliberately
//! lint-failing code path inside the workspace's own feature space, which meant
//! `cargo clippy --workspace --all-targets --all-features -- -D warnings` could
//! never be green: the fixture and the gate it proves were in permanent
//! conflict.
//!
//! The crate is named in the root `Cargo.toml` `exclude` list, so no workspace
//! command reaches it and it is compiled only by the explicit command above.
//! `CLIPPY_CONF_DIR` is set explicitly so the fixture is judged by the
//! workspace's real `clippy.toml` rather than a second copy that could drift
//! away from the law it is meant to prove.
//!
//! There is deliberately no `#[allow]` anywhere in this file.

/// A durable owner reaching for ambient wall-clock time — exactly what the law
/// forbids outside a named production adapter.
pub fn forbidden_ambient_now() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
