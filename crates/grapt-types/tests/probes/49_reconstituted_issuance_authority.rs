// Probe 49: a `SummaryCompatibility` rebuilt from persisted coordinates is
// inert DATA. It carries no catalog authority, it cannot answer the issuance
// question, and it cannot manufacture the live `ModelRef` that would.
//
// This is the structural half of the restart adversary, and a runtime test
// cannot witness it: the code that would witness it does not compile, so there
// is no assertion to execute and no mutant the harness could red. The compiler
// is the witness, which is why the carrier is a probe rather than a mutation
// row (the same reason probe 46 is a probe).
//
// Why it matters at exactly this seam: `from_validated_parts` is the route
// taken AFTER a strict codec has validated stored bytes. If a value on that
// route could speak issuance — by answering `is_same_issuance`, or by yielding
// a `ModelRef` — then stored bytes would speak with a live catalog's authority,
// which is precisely what the `Arc`-identity snapshot exists to prevent. The
// one thing a decoded value may do is be compared, coordinate for coordinate,
// against a target the caller already holds: `matches_target`.
//
// Every attempt below is method resolution, so E0599 is the sole error phase
// and nothing fires ahead of it. The arguments are `todo!()` because `!`
// coerces to any type: the missing METHOD is the only thing wrong here, not a
// value the probe failed to build.
#![allow(unreachable_code)]

use grapt_types::SummaryCompatibility;

fn main() {
    let reconstituted: SummaryCompatibility =
        SummaryCompatibility::from_validated_parts(todo!(), todo!(), todo!(), todo!());

    // 1. It cannot answer the issuance question that `ModelRef` answers.
    let _same = reconstituted.is_same_issuance(todo!());

    // 2. It cannot hand out the live ref that would answer it either.
    let _target = reconstituted.model_ref();

    // 3. Nor convert itself into one.
    let _converted = reconstituted.into_model_ref();
}
