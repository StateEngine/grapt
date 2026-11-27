// Probe 74: no public or test-only admitted continuation exists downstream.

use grapt_runtime::attempt_admission::AttemptPreparation;

fn main() {
    let _ = AttemptPreparation::<()>::admitted;
}
