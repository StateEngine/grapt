//! Compile-time seals for the service controller. The compiler must REJECT
//! each probe file; a probe that starts compiling is a broken law.
//!
//! | Probe | Law |
//! | --- | --- |
//! | `01_rollback_after_commit` | a commit consumes the candidate, so "rolled back" is unnameable once the candidate became current |
//! | `02_raw_store_cas_requires_elected_authority` | an external dependent cannot obtain a mutation permit for raw-store CAS |
//!
//! Why this law needs a compile-fail witness rather than a runtime one: it is
//! an **absence**. Master forbids a caller from *printing a rollback message
//! after the candidate became current*, and no runtime test can prove that no
//! caller anywhere ever does. What is provable is that the value such a message
//! would have to come from does not exist after a commit —
//! `ServiceController::commit` takes the `PreparedCandidate` by value, and
//! `PreparedCandidate` is deliberately not `Clone`, so there is no second
//! handle to keep.
//!
//! Probes are one **error phase** per file, following the convention in
//! `grapt-api/tests/compile_fail.rs` and `grapt-types/tests/compile_fail.rs`:
//! rustc stops after a name-resolution or privacy failure and never reaches
//! borrow checking, so two seals in one file would silently witness only the
//! first. This probe's failure is a borrow-check use-after-move, which is why
//! every name it touches must resolve.

#[test]
fn a_rollback_after_a_commit_is_rejected_by_the_compiler() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/probes/*.rs");
}
