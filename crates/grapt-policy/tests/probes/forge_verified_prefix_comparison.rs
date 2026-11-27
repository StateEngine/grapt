use grapt_policy::VerifiedPrefixComparison;
use grapt_types::CommittedPrefixId;

fn forge(committed: CommittedPrefixId) -> VerifiedPrefixComparison {
    VerifiedPrefixComparison {
        committed,
        outcome: todo!(),
    }
}

fn main() {}
