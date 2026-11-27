// Probe 75: a public source cannot manufacture an admitted preparation.

use grapt_runtime::{AttemptPreparation, AttemptSource, HistoryGeneration, ProviderReader};
use grapt_types::StepNumber;

struct Reader;

impl ProviderReader for Reader {
    async fn next(&mut self) -> Option<grapt_provider::AttemptFragment> {
        None
    }
}

struct Source;

impl AttemptSource for Source {
    type Reader = Reader;

    fn prepare(
        &mut self,
        _step: StepNumber,
        _ordinal: u32,
        _generation: HistoryGeneration,
    ) -> AttemptPreparation<Self::Reader> {
        AttemptPreparation::admitted()
    }
}

fn main() {}
