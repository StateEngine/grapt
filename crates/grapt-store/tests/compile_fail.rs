//! Compile-fail probes: the store's laws that are carried by the TYPE rather
//! than by a runtime check.
//!
//! `governed_writes.rs` and `projection_watermark.rs` show what the store
//! refuses to DO. These show what a consumer cannot even WRITE:
//!
//! * 01 — a database guard may not be held across an `.await` (F3B1b);
//! * 02 — a `CommittedNotices` carrier may not be built beside its commit;
//! * 03 — a notice may not be pushed into an open transaction;
//! * 04 — a campaign append may not carry a JSON `data` payload (F3B3);
//! * 05 — a campaign append may not omit its transition envelope (F3B3).
//!
//! 04 and 05 are the `CampaignAppendInput` carrier. A campaign fact IS its
//! envelope: the session row contract owns `data`, and a campaign row stores
//! exact canonical bytes with no second representation of the same fact. Both
//! laws are therefore unwritable rather than refused at runtime.
//!
//! 02 and 03 together are the commit-before-notify attribution. That law has no
//! source-order mutation to stage, because the ordering is not a line's position
//! — it is the fact that the only producer of the carrier is the `Ok` arm of
//! `commit`. `F3B2_RESIDUALS.md` records that accounting.
//!
//! Each probe reaches exactly one error phase: rustc aborts early, so a probe
//! carrying two laws would silently witness only the first.
#[test]
fn the_stores_type_level_laws_reject_their_counterexamples() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/probes/*.rs");
}
