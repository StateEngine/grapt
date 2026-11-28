//! Which built-in tools a Session advertises, recorded once, with its
//! instruction baseline. Sessions that started before the record existed keep
//! advertising what they started with. Every epoch's calls are still served.
use grapt_instruction::{InstructionValue, SourceObservation};

pub(super) const KEY: &str = "core/builtin-tool-epoch";
const CURRENT: &str = "2";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::dogfood) enum BuiltinEpoch {
    /// Started before the record: `read_lines` and `subagent` stay advertised.
    Legacy,
    /// `read` and the agent tools cover them, so neither is advertised.
    Current,
}

impl BuiltinEpoch {
    /// Recorded only when a Session's baseline is created.
    pub(super) fn observation() -> Result<SourceObservation, String> {
        Ok(SourceObservation::AtBaseline {
            key: super::key(KEY)?,
            value: InstructionValue::Text(CURRENT.to_owned()),
        })
    }

    /// The epoch a Session's current instruction values record.
    pub(super) fn recorded(value: Option<&str>) -> Self {
        if value == Some(CURRENT) {
            Self::Current
        } else {
            Self::Legacy
        }
    }
}
