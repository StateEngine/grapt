// A bounded identity wrapper cannot be built from a raw String.
//
// R2 finding Z2: the wrapper must own the ALREADY-VALIDATED identity, never a
// String. A String-owning wrapper would be a second identity — it could hold a
// value `PhysicalAttemptId` never validated — so the receipt would carry
// something that is not the id. This probe is that seal.
use grapt_types::BoundedAttemptId;

fn main() {
    let maximum = match std::num::NonZeroU32::new(8) {
        Some(value) => value,
        None => return,
    };
    // Not a PhysicalAttemptId. Must not compile.
    let _bounded = BoundedAttemptId::new(String::from("att_1"), maximum);
}
