// Probe 76: parsing an inert attempt ID cannot construct an unopened opener.

use grapt_runtime::attempt_admission::UnopenedAttempt;

fn main() {
    let _ = UnopenedAttempt::<()>::new;
}
