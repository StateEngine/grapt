// Probe 4: a presence state cannot be forged by tuple construction.
//
// This probe tests tuple-state privacy ONLY, on a category a consumer can
// actually name, so it never depends on the sealed-evidence law.
use grapt_schema::Optional;

fn main() {
    let _ = Optional(Some(true));
}
