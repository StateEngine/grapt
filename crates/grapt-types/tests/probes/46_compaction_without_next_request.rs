// Probe 46: Amendment §4.2 lists the "Step/request-context reference required
// to reassemble the next request" among the coordinates one atomic compaction
// descriptor must contain or reference. `next_request` is therefore a REQUIRED
// field of `CompactionDescriptorParts`, not an option, and requiredness is a
// type law: a compaction that commits successfully while unable to say which
// Step resumes, or from which prepared context, has ended one context epoch
// without enabling the next.
//
// This is the half of the law no mutation can witness. Every single edit that
// makes the coordinate omissible — deleting the field, or making it `Option` —
// stops the crate compiling, and the mutation harness correctly refuses a
// mutant that does not compile, because a compile failure attributes nothing to
// a runtime assertion. The compiler is the witness, so the probe is the carrier.
//
// The other ten coordinates are `todo!()` on purpose. `!` coerces to every one
// of their types, so the ONLY thing wrong with this expression is the missing
// field, and E0063 is the sole error phase. The `unreachable_code` warning that
// chain of `todo!()`s would otherwise emit is allowed here so the recorded
// stderr is exactly the one error this probe exists to pin.
#![allow(unreachable_code)]

use grapt_types::CompactionDescriptorParts;

fn main() {
    let _parts = CompactionDescriptorParts {
        id: todo!(),
        source_session: todo!(),
        source_epoch: todo!(),
        target_window: todo!(),
        target_epoch: todo!(),
        trigger: todo!(),
        retention_policy: todo!(),
        summary: todo!(),
        retained: todo!(),
        baseline: todo!(),
    };
}
