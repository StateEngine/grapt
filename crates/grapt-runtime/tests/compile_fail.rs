//! Compile-time probes for the R2A terminal-reason carriers.
//!
//! The compiler must REJECT each probe file; a probe that starts compiling is a
//! broken law.
//!
//! | Probe | Law |
//! | --- | --- |
//! | `70_attempt_reason_names_a_step_terminal` | an attempt's terminal reason cannot be written into a Step terminal — the two vocabularies are distinct types |
//! | `71_a_string_names_a_step_terminal` | no string can name a terminal reason, so no reason literal can be compared or constructed |
//! | `72_public_prepare_bypasses_attempt_admission` | downstream code cannot invoke the raw provider-running continuation |
//! | `73_public_reader_factory_bypasses_attempt_admission` | downstream code cannot import the removed raw provider-reader factory alias |
//! | `74_mint_attempt_admission_outside_owner` | downstream code cannot mint an admitted preparation from an unopened attempt and sealed receipt |
//! | `75_public_attempt_source_mints_identity` | a public source cannot manufacture an admitted attempt identity/opening path |
//! | `76_public_unopened_attempt_constructor` | parsing an inert ID cannot name the unopened-attempt constructor |
//!
//! # Why these are two files and not one
//!
//! rustc reports what it reaches, so a probe carrying two faults witnesses only
//! the first and silently covers for the second. These fail at the same phase
//! but on DIFFERENT items, and neither law implies the other: a crate with two
//! separate string newtypes passes 70 and fails 71; a crate with a single
//! shared enum passes 71 and fails 70. Each `.stderr` was read and contains
//! exactly one `error[` line.
//!
//! # Sensitivity to the delta, stated honestly
//!
//! Neither probe is delta-sensitive against the BASE commit, and saying so
//! would be a false receipt. At the base there was no `grapt_runtime::
//! StepFailure` at all — the type lived in `grapt-provider`, was shared by
//! both levels, and was never re-exported here — so both probes fail there with
//! an unresolved import, which witnesses nothing.
//!
//! What they witness is REQUIREDNESS, which is a compile-time seam rather
//! than a mutation-attributed law: the crate does not compile at all with
//! `reason` widened back to a string, so there is no runnable mutant for the
//! harness to score. They are carried by structure, and the receipt is that
//! each was shown to fail for its OWN reason — each `.stderr` was read and
//! carries exactly one `error[` line naming the reason argument, not an
//! incidental import or arity fault.
//!
//! They still separate from each other, which is why both exist: `reason`
//! widened to a string would make 71 compile while 70 still fails, and the
//! two vocabularies merged into one enum would make 70 compile while 71
//! still fails.

#[test]
fn runtime_probes_are_rejected_by_the_compiler() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/probes/*.rs");
}
