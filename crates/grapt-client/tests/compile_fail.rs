//! The Core-bypass refusal, witnessed by the compiler.
//!
//! Every other witness in this crate asserts that some call RETURNS the right
//! thing. This one asserts that a whole class of call DOES NOT EXIST, and that
//! is a claim no runtime test can make: `assert!` needs the forbidden
//! expression to compile, and the forbidden expression compiling IS the
//! defect. A green test that reads a `SessionView` and then asserts prose
//! about boundaries proves only that the view works.
//!
//! ## What the probes pin
//!
//! The ratified law names the witness directly — "embedded TUI Core-bypass
//! refusal" and "embedded-client Core-bypass compile failure" — because the
//! superseded reading ("embedded TUI may call Core/runtime directly for
//! speed") is a genuinely tempting optimisation. An embedded client sits in
//! the same process as the engine. Reaching for `grapt_runtime` there is one
//! `use` line away and would be measurably faster.
//!
//! ## Why the probes are written as ADDITIONS
//!
//! A seal is undone by adding something back, not by deleting a check. So each
//! probe is the code a future author would most plausibly write: a `use` of
//! the engine crate and a call into it. Today those crates are not
//! dependencies, the paths do not resolve, and the probes fail to build —
//! which is the pass.
//!
//! Add `grapt-runtime` to `Cargo.toml` for speed and the probe COMPILES, its
//! expected diagnostic disappears, and this test turns RED. That is the
//! mutant, and it is the one that matters: `dependency_direction.rs` catches
//! the manifest edit, and this catches the call, so neither can be defeated
//! by doing only the other half.

#[test]
fn no_public_route_reaches_the_engine_from_the_client() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/probes/*.rs");
}
