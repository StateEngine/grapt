use grapt_types::{AttemptUsage, UsageField, UsageObservationPhase};

fn forge() -> AttemptUsage {
    AttemptUsage {
        input_total_tokens: UsageField::ABSENT,
        input_tokens: UsageField::ABSENT,
        output_tokens: UsageField::ABSENT,
        reasoning_tokens: UsageField::ABSENT,
        cache_read_input_tokens: UsageField::ABSENT,
        cache_write_input_tokens: UsageField::ABSENT,
        total_tokens: UsageField::ABSENT,
        phase: UsageObservationPhase::Terminal,
        legacy_input_tokens: UsageField::ABSENT,
    }
}

fn main() {}
