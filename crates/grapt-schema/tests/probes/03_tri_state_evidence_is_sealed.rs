// Probe 3: a consumer cannot attest that its own field is oracle-proven.
//
// This probe ATTEMPTS the implementation, so the recorded error is the sealed
// supertrait refusing it. A missing-impl error would prove nothing: an open
// trait produces that too. Tuple-state privacy is a separate law with its own
// probe, so neither can mask the other.
use grapt_schema::TriStateEvidence;

struct MyField;

impl TriStateEvidence for MyField {
    const LEDGER_ROW: &'static str = "trust me, this field is tri-state";
}

fn main() {}
