//! Closed, payload-free explanations of provider usage refusal and retry exhaustion.
use crate::usage::UsageFieldKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageIssue {
    Null,
    WrongType,
    OutOfRange,
    PartitionExceedsTotal,
    TotalMismatch,
    ConflictingSnapshots,
}
impl UsageIssue {
    pub const fn word(self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::WrongType => "wrong-type",
            Self::OutOfRange => "out-of-range",
            Self::PartitionExceedsTotal => "partition-exceeds-total",
            Self::TotalMismatch => "total-mismatch",
            Self::ConflictingSnapshots => "conflicting-snapshots",
        }
    }
    pub fn from_word(word: &str) -> Option<Self> {
        [
            Self::Null,
            Self::WrongType,
            Self::OutOfRange,
            Self::PartitionExceedsTotal,
            Self::TotalMismatch,
            Self::ConflictingSnapshots,
        ]
        .into_iter()
        .find(|value| value.word() == word)
    }
}

/// No counter value, provider word, or arbitrary detail can enter this carrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageDiagnostic {
    pub field: Option<UsageFieldKind>,
    pub issue: UsageIssue,
}
impl UsageDiagnostic {
    pub fn field_word(self) -> String {
        self.field
            .map_or_else(|| "usage".into(), |field| field.to_string())
    }
    pub fn from_words(field: &str, issue: &str) -> Option<Self> {
        let field = if field == "usage" {
            None
        } else {
            Some(
                [
                    UsageFieldKind::InputTotal,
                    UsageFieldKind::InputFresh,
                    UsageFieldKind::LegacyInputAmbiguous,
                    UsageFieldKind::Output,
                    UsageFieldKind::Reasoning,
                    UsageFieldKind::CacheRead,
                    UsageFieldKind::CacheWrite,
                    UsageFieldKind::Total,
                ]
                .into_iter()
                .find(|known| known.to_string() == field)?,
            )
        };
        Some(Self {
            field,
            issue: UsageIssue::from_word(issue)?,
        })
    }
}

/// The only provider retry classes exposed in a public diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryFailureClass {
    RateLimited,
    ServerError,
    Transport,
}
impl RetryFailureClass {
    pub const fn word(self) -> &'static str {
        match self {
            Self::RateLimited => "rate-limited",
            Self::ServerError => "server-error",
            Self::Transport => "transport",
        }
    }
    pub fn from_word(word: &str) -> Option<Self> {
        [Self::RateLimited, Self::ServerError, Self::Transport]
            .into_iter()
            .find(|value| value.word() == word)
    }
}
/// One initial attempt plus at most four retries, matching the ruled cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryExhaustion {
    classes: [RetryFailureClass; 5],
    attempts: usize,
}
impl RetryExhaustion {
    pub fn new(classes: &[RetryFailureClass]) -> Option<Self> {
        if classes.is_empty() || classes.len() > 5 {
            return None;
        }
        let mut held = [RetryFailureClass::Transport; 5];
        held[..classes.len()].copy_from_slice(classes);
        Some(Self {
            classes: held,
            attempts: classes.len(),
        })
    }
    pub fn classes(&self) -> &[RetryFailureClass] {
        &self.classes[..self.attempts]
    }
    pub fn attempts(&self) -> u32 {
        self.attempts as u32
    }
    pub fn last(&self) -> RetryFailureClass {
        self.classes[self.attempts - 1]
    }
}
/// The closed admission-refusal classes that end a Step before any provider
/// byte. Carried by the committed-Step diagnostic, never by the terminal.
///
/// The campaign words are no longer produced, but stored notices may carry
/// them, so they stay readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionRefusalWord {
    NoIssuer,
    MissingCampaignTruth,
    StaleCoordinate,
    MissingStepTerminal,
    PlanRefused,
    OpenRefused,
    Saturated,
    StoreRefused,
}
impl AdmissionRefusalWord {
    pub const fn word(self) -> &'static str {
        match self {
            Self::NoIssuer => "no-issuer",
            Self::MissingCampaignTruth => "missing-campaign-truth",
            Self::StaleCoordinate => "stale-coordinate",
            Self::MissingStepTerminal => "missing-step-terminal",
            Self::PlanRefused => "plan-refused",
            Self::OpenRefused => "open-refused",
            Self::Saturated => "saturated",
            Self::StoreRefused => "store-refused",
        }
    }
    pub fn from_word(word: &str) -> Option<Self> {
        [
            Self::NoIssuer,
            Self::MissingCampaignTruth,
            Self::StaleCoordinate,
            Self::MissingStepTerminal,
            Self::PlanRefused,
            Self::OpenRefused,
            Self::Saturated,
            Self::StoreRefused,
        ]
        .into_iter()
        .find(|value| value.word() == word)
    }
}

/// The exact campaign coordinate named by a stale-coordinate refusal. No
/// longer produced; kept so stored notices stay readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleCoordinateWord {
    Campaign,
    PlanVersion,
    Node,
    NodeRevision,
    AssistantTurn,
    NodeReservation,
    PlanRead,
    SpendReservation,
    EffectiveGraph,
}
impl StaleCoordinateWord {
    pub const fn word(self) -> &'static str {
        match self {
            Self::Campaign => "campaign",
            Self::PlanVersion => "plan-version",
            Self::Node => "node",
            Self::NodeRevision => "node-revision",
            Self::AssistantTurn => "assistant-turn",
            Self::NodeReservation => "node-reservation",
            Self::PlanRead => "plan-read",
            Self::SpendReservation => "spend-reservation",
            Self::EffectiveGraph => "effective-graph",
        }
    }
    pub fn from_word(word: &str) -> Option<Self> {
        [
            Self::Campaign,
            Self::PlanVersion,
            Self::Node,
            Self::NodeRevision,
            Self::AssistantTurn,
            Self::NodeReservation,
            Self::PlanRead,
            Self::SpendReservation,
            Self::EffectiveGraph,
        ]
        .into_iter()
        .find(|value| value.word() == word)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepFailureDiagnostic {
    Usage(UsageDiagnostic),
    RetryExhausted(RetryExhaustion),
    ProviderLimit {
        reset: Option<crate::provider_limit::ProviderLimitReset>,
    },
    AdmissionRefused {
        refusal: AdmissionRefusalWord,
        coordinate: Option<StaleCoordinateWord>,
    },
}
impl StepFailureDiagnostic {
    /// The admission diagnostic with its coordinate shape enforced once, in
    /// the carrier both notice directions own: the stale-coordinate class
    /// names its coordinate, and no other class carries one. The notice
    /// writer refuses a diagnostic this rejects before any durable append,
    /// and the notice reader keeps the same check on the rows it reads back.
    pub fn admission_refused(
        refusal: AdmissionRefusalWord,
        coordinate: Option<StaleCoordinateWord>,
    ) -> Option<Self> {
        (coordinate.is_some() == (refusal == AdmissionRefusalWord::StaleCoordinate)).then_some(
            Self::AdmissionRefused {
                refusal,
                coordinate,
            },
        )
    }
}
