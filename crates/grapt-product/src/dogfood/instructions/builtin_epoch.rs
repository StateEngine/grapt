//! Which built-in tools a Session advertises, and the base guidance that names
//! them. A Session keeps the epoch it began in for life, across compaction,
//! moves, rewinds and forks, so its cached prompt prefix never shifts: a
//! Session that began before the record existed keeps both the retired tools
//! and the base guidance bytes it began with. Every epoch's calls are served.
use grapt_instruction::{InstructionValue, RecordedValue, SourceObservation};
use grapt_types::InstructionKey;

pub(super) const KEY: &str = "core/builtin-tool-epoch";
const CURRENT: &str = "2";
/// How the base guidance named file reading before `read_lines` was retired.
const LEGACY_READING: &str = "read_lines reads source with line numbers.";
const READING: &str = "read reads files with line numbers.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::dogfood) enum BuiltinEpoch {
    /// Began before the record: `read_lines` and `subagent` stay advertised.
    Legacy,
    /// `read` and the agent tools cover them, so neither is advertised.
    Current,
    /// Recorded by a newer Grapt; its record and base are kept as they are.
    Newer(String),
    /// The Session's journal could not be read whole, so nothing about its
    /// epoch is written and every built-in tool stays advertised.
    Unreadable,
}

impl BuiltinEpoch {
    pub(super) fn key() -> Result<InstructionKey, String> {
        super::key(KEY)
    }

    /// The epoch a Session's record names; one with none yet begins now.
    pub(super) fn from_recorded(recorded: &RecordedValue) -> Self {
        match recorded {
            RecordedValue::Never => Self::Current,
            RecordedValue::Present(value) if value == CURRENT => Self::Current,
            RecordedValue::Present(value) => Self::Newer(value.clone()),
            RecordedValue::Absent => Self::Legacy,
            RecordedValue::Unreadable => Self::Unreadable,
        }
    }

    /// Whether `read_lines` and `subagent` go unadvertised.
    pub(in crate::dogfood) fn retires_tools(&self) -> bool {
        matches!(self, Self::Current | Self::Newer(_))
    }

    /// The base guidance and record this epoch keeps. A newer or unreadable
    /// Session keeps whatever base and record it holds; only one a move or
    /// rewind cleared takes them again.
    pub(super) fn observations(&self) -> Result<Vec<SourceObservation>, String> {
        let key = super::key("core/base")?;
        let mut observations = vec![match self {
            Self::Legacy => SourceObservation::Available {
                key,
                value: text(legacy_base()),
            },
            Self::Current => SourceObservation::Available {
                key,
                value: text(super::BASE.to_owned()),
            },
            Self::Newer(_) | Self::Unreadable => SourceObservation::AvailableOnce {
                key,
                value: text(super::BASE.to_owned()),
            },
        }];
        let record = match self {
            Self::Current => Some(CURRENT.to_owned()),
            Self::Newer(value) => Some(value.clone()),
            Self::Legacy | Self::Unreadable => None,
        };
        if let Some(record) = record {
            observations.push(SourceObservation::AvailableOnce {
                key: Self::key()?,
                value: text(record),
            });
        }
        Ok(observations)
    }
}

fn text(value: String) -> InstructionValue {
    InstructionValue::Text(value)
}

pub(super) fn legacy_base() -> String {
    super::BASE.replacen(READING, LEGACY_READING, 1)
}
