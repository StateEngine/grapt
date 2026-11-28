//! Which built-in tools a Session advertises, and the base guidance that names
//! them. A Session keeps the epoch it began in for life, across compaction,
//! moves, rewinds and forks, so its cached prompt prefix never shifts: a
//! Session that began before the record existed keeps both the retired tools
//! and the base guidance bytes it began with. Every epoch's calls are served.
use grapt_instruction::{InstructionService, InstructionValue, RecordedValue, SourceObservation};
use grapt_store::EventStore;
use grapt_types::SessionId;

pub(super) const KEY: &str = "core/builtin-tool-epoch";
const CURRENT: &str = "2";
/// How the base guidance named file reading before `read_lines` was retired.
const LEGACY_READING: &str = "read_lines reads source with line numbers.";
const READING: &str = "read reads files with line numbers.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::dogfood) enum BuiltinEpoch {
    /// Began before the record: `read_lines` and `subagent` stay advertised.
    Legacy,
    /// `read` and the agent tools cover them, so neither is advertised.
    Current,
}

impl BuiltinEpoch {
    /// The epoch the Session began in; a Session with no instructions yet
    /// begins in the current one.
    pub(super) fn of(store: &EventStore, session: &SessionId) -> Result<Self, String> {
        let recorded = InstructionService::recorded(store, session, &super::key(KEY)?)
            .map_err(|error| error.to_string())?;
        Ok(match recorded {
            RecordedValue::Never => Self::Current,
            RecordedValue::Present(value) if value == CURRENT => Self::Current,
            RecordedValue::Present(_) | RecordedValue::Absent => Self::Legacy,
        })
    }

    /// The base guidance, in the bytes this epoch's Sessions began with.
    pub(super) fn base(self) -> Result<SourceObservation, String> {
        let text = match self {
            Self::Legacy => legacy_base(),
            Self::Current => super::BASE.to_owned(),
        };
        Ok(SourceObservation::Available {
            key: super::key("core/base")?,
            value: InstructionValue::Text(text),
        })
    }

    /// The record itself: kept by current Sessions, never added to legacy ones.
    pub(super) fn marker(self) -> Result<SourceObservation, String> {
        let key = super::key(KEY)?;
        Ok(match self {
            Self::Legacy => SourceObservation::Removed { key },
            Self::Current => SourceObservation::Available {
                key,
                value: InstructionValue::Text(CURRENT.to_owned()),
            },
        })
    }
}

pub(super) fn legacy_base() -> String {
    super::BASE.replacen(READING, LEGACY_READING, 1)
}
