//! The closed normalized fragment alphabet.
//!
//! Law: `SECODE_RUST_PROVIDER_STREAM_RUNTIME_CONTRACT_2026-08-08.md`
//! §"Normalized fragment vocabulary". Adapters normalize provider bytes into
//! this alphabet before anything crosses into the runtime. The alphabet is
//! closed, versioned, and independent of any SDK's event names — a new
//! provider does not get to widen it.
//!
//! Oracle evidence: the oracle's `events.ts:82-315`. Where this
//! module deliberately departs from the oracle, the departure is named in a
//! `DISCLOSED` note rather than made silently.

use std::time::Duration;

use grapt_types::ToolCallId;

/// Ordinal of a text or reasoning block within one attempt.
///
/// The contract permits sequential blocks and requires each to retain an
/// ordinal, so a transcript can distinguish "second paragraph" from "the same
/// paragraph re-opened".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockOrdinal(u32);

impl BlockOrdinal {
    pub fn new(value: u32) -> Self {
        Self(value)
    }

    pub fn value(self) -> u32 {
        self.0
    }

    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// Who runs a tool call.
///
/// DISCLOSED (behaviour-preserving, representation-correcting): the oracle
/// carries this as `providerExecuted: Schema.optional(Schema.Boolean)`
/// (`events.ts:167`, `:179`, `:189`). An absent optional boolean cannot
/// distinguish "local" from "unstated", so the contract requires a closed
/// enum — "`execution` is a closed enum (`Local` or `ProviderHosted`), never
/// an optional boolean". Adapters must decide; the runtime never infers.
pub use grapt_types::ToolExecution;

/// Adapter-owned opaque metadata attached to a fragment.
///
/// Contract: "`state` is adapter-owned bounded opaque metadata; it cannot
/// change authority or cause a runtime branch." It is not parsed here. The
/// owner may capture its bytes as an opaque artifact; interpretation stays at
/// the adapter boundary. The capture accessor does not confer authority.
///
/// RULED: the inline retention cap is 256 KiB (2026-08-08),
/// set together with the provider-to-owner channel
/// capacity and the attempt tool budget. Metadata above the cap spills to an
/// artifact rather than riding inline — that spill path is a named residual
/// and is NOT implemented here, so oversize metadata is refused for now.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct AdapterState(Option<Box<str>>);

impl std::fmt::Debug for AdapterState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AdapterState([REDACTED])")
    }
}

impl AdapterState {
    /// Ruled inline retention cap (2026-08-08). Above this, the intended
    /// behaviour is artifact spill; until that path exists,
    /// [`AdapterState::retain`] refuses rather than silently truncating.
    pub const MAX_BYTES: usize = 256 * 1024;

    pub fn none() -> Self {
        Self(None)
    }

    /// Retain adapter metadata, rejecting anything over the provisional bound
    /// rather than silently truncating it. Truncation would make an audit
    /// trail quietly lossy; refusal is visible.
    pub fn retain(raw: impl Into<Box<str>>) -> Result<Self, AdapterStateTooLarge> {
        let raw = raw.into();
        if raw.len() > Self::MAX_BYTES {
            return Err(AdapterStateTooLarge {
                bytes: raw.len(),
                limit: Self::MAX_BYTES,
            });
        }
        Ok(Self(Some(raw)))
    }

    pub fn is_present(&self) -> bool {
        self.0.is_some()
    }

    pub fn byte_len(&self) -> usize {
        self.0.as_ref().map_or(0, |value| value.len())
    }

    /// Bytes for opaque artifact capture, never runtime interpretation.
    pub fn capture_bytes(&self) -> Option<&[u8]> {
        self.0.as_ref().map(|value| value.as_bytes())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("adapter state is {bytes} bytes, over the {limit}-byte retention bound")]
pub struct AdapterStateTooLarge {
    pub bytes: usize,
    pub limit: usize,
}

/// A bounded, non-authoritative diagnostic string carried beside a typed
/// terminal reason.
///
/// # Why this is bounded, and why it is not typed
///
/// Once the terminal reason is a closed enum, no `detail` datum is
/// load-bearing: nothing reads it, matches it, or branches on it. Typing every
/// detail would duplicate the enums' payloads and freeze prose. But one of its
/// inputs is **provider-controlled and unbounded** — the `message` field of a
/// [`AttemptFragment::ProviderFailure`] fragment — and it reaches this carrier
/// directly. An unbounded provider string riding inside a terminal fact is a
/// resource claim nobody made.
///
/// # Truncate, do not refuse — and why that differs from [`AdapterState`]
///
/// [`AdapterState::retain`] REFUSES oversize input, because adapter metadata is
/// audit evidence and a silently shortened audit trail is worse than a visible
/// rejection. The opposite is true here: this value exists only to explain a
/// failure that has ALREADY happened, and refusing it would destroy the typed
/// reason it accompanies in order to protect a diagnostic. So this truncates,
/// deterministically, and says so.
///
/// The cap is the same ruled 256 KiB inline ceiling.
///
/// Truncation is UTF-8 safe: the cut lands on a character boundary, never
/// mid-codepoint, so the result is always a valid `str`. The retained bytes
/// **including the marker** never exceed the cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedDetail {
    text: Box<str>,
    truncated: bool,
}

impl BoundedDetail {
    /// The same ruled inline ceiling [`AdapterState`] uses (2026-08-08).
    pub const MAX_BYTES: usize = AdapterState::MAX_BYTES;

    /// Appended when input was cut, so a reader can never mistake a truncated
    /// diagnostic for a complete one.
    pub const TRUNCATION_MARKER: &'static str = "…[truncated]";

    /// The only way to build one. Infallible by design: a diagnostic may not
    /// be able to fail the terminal reason it describes.
    pub fn new(raw: impl Into<String>) -> Self {
        let raw = raw.into();
        if raw.len() <= Self::MAX_BYTES {
            return Self {
                text: raw.into_boxed_str(),
                truncated: false,
            };
        }
        // Budget the marker FIRST so the total never exceeds the cap, then walk
        // back to a character boundary. `floor_char_boundary` is unstable, so
        // the walk is explicit; it moves at most three bytes.
        let budget = Self::MAX_BYTES - Self::TRUNCATION_MARKER.len();
        let mut end = budget.min(raw.len());
        while end > 0 && !raw.is_char_boundary(end) {
            end -= 1;
        }
        let mut text = String::with_capacity(end + Self::TRUNCATION_MARKER.len());
        text.push_str(&raw[..end]);
        text.push_str(Self::TRUNCATION_MARKER);
        Self {
            text: text.into_boxed_str(),
            truncated: true,
        }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn byte_len(&self) -> usize {
        self.text.len()
    }

    /// Whether input was cut. Recorded rather than derived from the marker
    /// suffix: a genuine diagnostic could legitimately end with that text.
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }
}

impl std::fmt::Display for BoundedDetail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl From<&str> for BoundedDetail {
    fn from(raw: &str) -> Self {
        Self::new(raw)
    }
}

impl From<String> for BoundedDetail {
    fn from(raw: String) -> Self {
        Self::new(raw)
    }
}

/// Normalized terminal reason.
///
/// Oracle set, taken from the frozen generated client rather than from prose:
/// `"stop" | "length" | "tool-calls" | "content-filter" | "error" | "unknown"`
/// (frozen `promise/generated/types.ts:785`, compatibility artifact). Adapter
/// mappers such as `anthropic-messages.ts:673` collapse vendor strings into
/// exactly this set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Error,
    Unknown,
}

/// What a provider finish means once normalized.
///
/// Two of the six oracle finish reasons are not dispositions at all: they are
/// failures wearing a terminal's shape. Keeping them in the same closed return
/// type is what stops a caller from quietly treating a content-policy refusal
/// as an ordinary ending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalizedFinish {
    /// The stream ended and the model's completion claim is known.
    Completed {
        completion: grapt_types::ProviderCompletionDisposition,
    },
    /// The stream "finished" in a way that is really a terminal failure.
    Failed {
        class: grapt_types::ProviderFailureClass,
    },
}

impl FinishReason {
    /// Normalize the provider's finish into the foundational vocabulary.
    ///
    /// Ruled map:
    ///
    /// | finish | normalized |
    /// |---|---|
    /// | `Stop` | `AssertedComplete` |
    /// | `ToolCalls` | `AssertedContinue` |
    /// | `Length` | `Truncated` |
    /// | `Unknown` | `Unstated` |
    /// | `ContentFilter` | failure `ContentPolicy` |
    /// | `Error` | failure `ProviderInternal` |
    ///
    /// `Stop` is `AssertedComplete`, NOT `ModelAssertedComplete`: the latter is
    /// a Step-level reason meaning "the model claimed it was done and that
    /// claim is recorded, not believed". A clean provider stop is the ordinary
    /// end of a Step chain and carries its disposition.
    ///
    /// `Length` is `Truncated` and never ordinary idle — that is mandatory law
    /// (provider contract rule 11), and the runtime must route it rather than
    /// fall through.
    ///
    /// `Error` maps to `ProviderInternal` only when no more specific failure
    /// fragment arrived; the caller prefers a real `ProviderFailure` fragment's
    /// class when it has one.
    pub fn normalize(self) -> NormalizedFinish {
        use grapt_types::ProviderCompletionDisposition as Disposition;
        use grapt_types::ProviderFailureClass as Terminal;
        match self {
            Self::Stop => NormalizedFinish::Completed {
                completion: Disposition::AssertedComplete,
            },
            Self::ToolCalls => NormalizedFinish::Completed {
                completion: Disposition::AssertedContinue,
            },
            Self::Length => NormalizedFinish::Completed {
                completion: Disposition::Truncated,
            },
            Self::Unknown => NormalizedFinish::Completed {
                completion: Disposition::Unstated,
            },
            Self::ContentFilter => NormalizedFinish::Failed {
                class: Terminal::ContentPolicy,
            },
            Self::Error => NormalizedFinish::Failed {
                class: Terminal::ProviderInternal,
            },
        }
    }
}

pub use crate::usage::TokenUsage;

/// Why a provider stream failed, in the vocabulary that decides retry.
///
/// Oracle evidence: the oracle's `retry.ts:18-37`
/// enumerates exactly these reasons and classifies the first three as
/// retryable. Retry *eligibility* lives with the class so no caller can
/// invent a policy for a reason it does not own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderFailureClass {
    RateLimit { retry_after: Option<Duration> },
    ProviderInternal { retry_after: Option<Duration> },
    Transport,
    Authentication,
    QuotaExceeded,
    ContentPolicy,
    InvalidProviderOutput,
    InvalidRequest,
    NoRoute,
    UnknownProvider,
}

impl ProviderFailureClass {
    /// Retry eligibility by class alone. This is a necessary condition, never
    /// a sufficient one: the caller must also enforce the pre-visible-output
    /// rule and the five-attempt bound. See [`crate::normalize::StreamValidator::visible_output`].
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::RateLimit { .. } | Self::ProviderInternal { .. } | Self::Transport => true,
            Self::Authentication
            | Self::QuotaExceeded
            | Self::ContentPolicy
            | Self::InvalidProviderOutput
            | Self::InvalidRequest
            | Self::NoRoute
            | Self::UnknownProvider => false,
        }
    }

    /// The foundational terminal classification this transport class names.
    ///
    /// TOTAL by construction — every variant maps, and the compiler enforces
    /// that a new provider variant cannot be added without deciding its
    /// terminal image. The only datum dropped is `retry_after`, and dropping it
    /// HERE is correct: it is a retry-scheduling input consumed strictly before
    /// terminality (`classify()` hands it to `StepPolicy::delay_for`), never a
    /// terminal fact. A terminal reason that carried a retry delay would be
    /// describing a decision that had already been made.
    ///
    /// `QuotaExceeded` becomes `Quota`: a rename across the boundary, not a
    /// semantic change.
    ///
    /// The foundational enum additionally has `NoProgressTimeout`, which has no
    /// provider counterpart on purpose — a stalled stream is observed by the
    /// runtime's watchdog, not reported by an adapter, so no adapter can name
    /// it and this map never produces it.
    pub fn to_terminal(&self) -> grapt_types::ProviderFailureClass {
        use grapt_types::ProviderFailureClass as Terminal;
        match self {
            Self::RateLimit { .. } => Terminal::RateLimit,
            Self::ProviderInternal { .. } => Terminal::ProviderInternal,
            Self::Transport => Terminal::Transport,
            Self::Authentication => Terminal::Authentication,
            Self::QuotaExceeded => Terminal::Quota,
            Self::ContentPolicy => Terminal::ContentPolicy,
            Self::InvalidProviderOutput => Terminal::InvalidProviderOutput,
            Self::InvalidRequest => Terminal::InvalidRequest,
            Self::NoRoute => Terminal::NoRoute,
            Self::UnknownProvider => Terminal::UnknownProvider,
        }
    }

    /// A provider-declared minimum delay, when the class carries one.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimit { retry_after } | Self::ProviderInternal { retry_after } => {
                *retry_after
            }
            _ => None,
        }
    }
}

/// Secondary classification carried alongside a failure.
///
/// The oracle models this as a separate axis from the retry reason:
/// `ProviderFailureClassification = Schema.Literal("context-overflow")`
/// (the oracle's `errors.ts:5`) attached to `ProviderErrorEvent`
/// (`events.ts:217-223`). Two axes is faithful to the source, so it is
/// preserved rather than tidied into one enum — overflow drives compaction
/// rebuild, which is a different decision from retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureClassification {
    ContextOverflow,
    UsageLimit {
        reset: Option<grapt_types::provider_limit::ProviderLimitReset>,
    },
    /// The credential owner installed a replacement after an HTTP 401. The
    /// failed attempt stays failed; a retry needs its own identity and receipt.
    CredentialRefreshed,
}

/// The closed alphabet an adapter may emit for one physical attempt.
///
/// Ordering rules are not expressible in the type alone; they are enforced by
/// [`crate::normalize::StreamValidator`], which rejects violations as typed
/// errors rather than panicking.
#[derive(Debug, Clone, PartialEq)]
pub enum AttemptFragment {
    AttemptOpened,

    TextOpened {
        block: BlockOrdinal,
        state: AdapterState,
    },
    TextDelta {
        block: BlockOrdinal,
        text: String,
        state: AdapterState,
    },
    TextClosed {
        block: BlockOrdinal,
        state: AdapterState,
    },

    ReasoningOpened {
        block: BlockOrdinal,
        state: AdapterState,
    },
    ReasoningDelta {
        block: BlockOrdinal,
        text: String,
        state: AdapterState,
    },
    ReasoningClosed {
        block: BlockOrdinal,
        state: AdapterState,
    },

    ToolInputOpened {
        call: ToolCallId,
        tool: String,
        execution: ToolExecution,
        state: AdapterState,
    },
    ToolInputDelta {
        call: ToolCallId,
        tool: String,
        text: String,
    },
    ToolInputClosed {
        call: ToolCallId,
        tool: String,
        state: AdapterState,
    },
    ToolInputInvalid {
        call: ToolCallId,
        tool: String,
        raw: String,
    },
    ToolCalled {
        call: ToolCallId,
        tool: String,
        input: String,
        execution: ToolExecution,
        state: AdapterState,
    },

    HostedToolResult {
        call: ToolCallId,
        tool: String,
        result: String,
        state: AdapterState,
    },
    HostedToolFailed {
        call: ToolCallId,
        tool: String,
        error: String,
        state: AdapterState,
    },

    Usage {
        usage: TokenUsage,
    },
    ProviderTerminal {
        finish: FinishReason,
        usage: Option<TokenUsage>,
        state: AdapterState,
    },
    ProviderFailure {
        class: ProviderFailureClass,
        classification: Option<FailureClassification>,
        diagnostic: Option<grapt_types::failure_diagnostic::UsageDiagnostic>,
        message: String,
        state: AdapterState,
    },
}

impl AttemptFragment {
    /// Maximum emitted bytes in one coalesced text or reasoning delta.
    ///
    /// Small enough that the coalesced delta still fits ONE live event: a
    /// control character escapes to six JSON bytes, so 32 KiB of raw text is
    /// at most 192 KiB escaped, well inside the product's 256 KiB SSE event
    /// bound with room for the envelope. At 256 KiB, a burst coalesced under
    /// load overflowed that bound and ended every live stream (the at-bound
    /// ACP refusal); the product pins the relationship in a test.
    pub const MAX_COALESCED_DELTA_BYTES: usize = 32 * 1024;

    /// Whether this fragment ends the stream. Contract: "`ProviderTerminal`
    /// occurs once; `ProviderFailure` is terminal for the stream; no fragment
    /// follows either terminal fragment."
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::ProviderTerminal { .. } | Self::ProviderFailure { .. }
        )
    }

    /// Whether accepting this fragment constitutes *visible output*.
    ///
    /// Contract definition: "a normalized text, reasoning, tool-input, or
    /// tool-call fragment accepted by the Session owner. Once visible output
    /// exists, that Step is not silently retried and is not transparently
    /// overflow-rebuilt."
    ///
    /// `Usage` and the terminal fragments are deliberately excluded: usage
    /// accounting is not user-visible output, and a terminal fragment cannot
    /// retroactively make a silent stream visible.
    ///
    /// Tool-input fragments are excluded too (ruled 2026-10-18), narrowing
    /// the contract's set: the Session owner neither
    /// stores nor publishes arguments that are still streaming, and no effect
    /// starts before `ToolCalled` commits. A retryable failure, stall or
    /// overflow mid-arguments therefore retries instead of ending the chain.
    /// `ToolCalled` stays visible: its tool starts during the stream, so a
    /// retry past it could repeat a side effect.
    ///
    /// Unfinished text and reasoning are excluded as well (ruled
    /// 2026-10-18). An unfinished item is never durable, so a failure that
    /// retries discards it (see [`Self::may_retry`]) and the retry is built
    /// from the same history. So is FINISHED reasoning (ruled
    /// 2026-10-19): the Session owner holds it
    /// until visible output follows, and a retry discards it and re-thinks
    /// rather than replaying it. Finished text stays visible.
    pub fn is_visible_output(&self) -> bool {
        matches!(self, Self::TextClosed { .. } | Self::ToolCalled { .. })
    }

    /// Whether this is a failure terminal the Step may answer with another
    /// attempt, by class alone: a retryable class, a context overflow (a
    /// rebuild) or a refreshed credential. The Step still refuses once visible
    /// output exists or its bound is spent. An unfinished block is discarded
    /// on this path, never closed into a durable item; every other failure
    /// keeps today's close.
    pub fn may_retry(&self) -> bool {
        match self {
            Self::ProviderFailure {
                class,
                classification,
                ..
            } => {
                class.is_retryable()
                    || *classification == Some(FailureClassification::ContextOverflow)
                    || (*class == ProviderFailureClass::Authentication
                        && *classification == Some(FailureClassification::CredentialRefreshed))
            }
            _ => false,
        }
    }

    /// Whether two adjacent fragments may coalesce before ownership transfer.
    ///
    /// Contract: "Only same-block text/reasoning deltas may coalesce before
    /// ownership transfer; coalescing may never cross a block, tool, terminal,
    /// usage, or failure boundary." Everything else answers `false`, so the
    /// default is always the safe one.
    pub fn coalesces_with(&self, next: &Self) -> bool {
        match (self, next) {
            (Self::TextDelta { block: a, .. }, Self::TextDelta { block: b, .. }) => a == b,
            (Self::ReasoningDelta { block: a, .. }, Self::ReasoningDelta { block: b, .. }) => {
                a == b
            }
            _ => false,
        }
    }

    /// Whether this fragment is eligible to become the reader pump's one pending delta.
    pub fn is_coalescible_delta(&self) -> bool {
        matches!(self, Self::TextDelta { .. } | Self::ReasoningDelta { .. })
    }

    /// Absorb one same-block delta without crossing the 256 KiB emitted-batch bound.
    ///
    /// On success the latest adapter state wins and `None` confirms that `next`
    /// was absorbed. On refusal `Some(next)` returns the fragment unchanged so
    /// the caller can flush it separately and remain lossless.
    pub fn coalesce_or_return(&mut self, next: Self) -> Option<Self> {
        match (self, next) {
            (
                Self::TextDelta {
                    block: current_block,
                    text: current_text,
                    state: current_state,
                },
                Self::TextDelta {
                    block: next_block,
                    text: next_text,
                    state: next_state,
                },
            ) if *current_block == next_block
                && current_text
                    .len()
                    .checked_add(next_text.len())
                    .is_some_and(|bytes| bytes <= Self::MAX_COALESCED_DELTA_BYTES) =>
            {
                current_text.push_str(&next_text);
                *current_state = next_state;
                None
            }
            (
                Self::ReasoningDelta {
                    block: current_block,
                    text: current_text,
                    state: current_state,
                },
                Self::ReasoningDelta {
                    block: next_block,
                    text: next_text,
                    state: next_state,
                },
            ) if *current_block == next_block
                && current_text
                    .len()
                    .checked_add(next_text.len())
                    .is_some_and(|bytes| bytes <= Self::MAX_COALESCED_DELTA_BYTES) =>
            {
                current_text.push_str(&next_text);
                *current_state = next_state;
                None
            }
            (_, next) => Some(next),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str) -> ToolCallId {
        match ToolCallId::new(id) {
            Some(value) => value,
            None => panic!("test fixture id must be valid"),
        }
    }

    #[test]
    fn retryable_classes_match_the_oracle_split() {
        // retry.ts:20-22 — exactly these three are retryable.
        assert!(ProviderFailureClass::RateLimit { retry_after: None }.is_retryable());
        assert!(ProviderFailureClass::ProviderInternal { retry_after: None }.is_retryable());
        assert!(ProviderFailureClass::Transport.is_retryable());
        // retry.ts:23-30 — these seven are terminal.
        for class in [
            ProviderFailureClass::Authentication,
            ProviderFailureClass::QuotaExceeded,
            ProviderFailureClass::ContentPolicy,
            ProviderFailureClass::InvalidProviderOutput,
            ProviderFailureClass::InvalidRequest,
            ProviderFailureClass::NoRoute,
            ProviderFailureClass::UnknownProvider,
        ] {
            assert!(!class.is_retryable(), "{class:?} must be terminal");
        }
    }

    #[test]
    fn retry_after_only_exists_where_the_oracle_carries_it() {
        let delay = Duration::from_millis(1500);
        assert_eq!(
            ProviderFailureClass::RateLimit {
                retry_after: Some(delay)
            }
            .retry_after(),
            Some(delay)
        );
        assert_eq!(ProviderFailureClass::Transport.retry_after(), None);
        assert_eq!(ProviderFailureClass::Authentication.retry_after(), None);
    }

    #[test]
    fn usage_is_not_visible_output() {
        // A usage-only stream must stay silently retryable.
        assert!(!AttemptFragment::Usage {
            usage: TokenUsage::default()
        }
        .is_visible_output());
        assert!(!AttemptFragment::AttemptOpened.is_visible_output());
    }

    #[test]
    fn unfinished_text_and_reasoning_are_not_visible_output() {
        // An unfinished block is discarded when the Step retries.
        let block = BlockOrdinal::new(0);
        for fragment in [
            AttemptFragment::TextOpened {
                block,
                state: AdapterState::none(),
            },
            AttemptFragment::TextDelta {
                block,
                text: "hi".into(),
                state: AdapterState::none(),
            },
            AttemptFragment::ReasoningOpened {
                block,
                state: AdapterState::none(),
            },
            AttemptFragment::ReasoningDelta {
                block,
                text: "hm".into(),
                state: AdapterState::none(),
            },
        ] {
            assert!(!fragment.is_visible_output(), "{fragment:?}");
        }
    }

    #[test]
    fn only_a_failure_the_step_may_answer_may_retry() {
        let failure = |class, classification| AttemptFragment::ProviderFailure {
            class,
            classification,
            diagnostic: None,
            message: String::new(),
            state: AdapterState::none(),
        };
        assert!(failure(ProviderFailureClass::Transport, None).may_retry());
        assert!(failure(
            ProviderFailureClass::InvalidRequest,
            Some(FailureClassification::ContextOverflow)
        )
        .may_retry());
        assert!(failure(
            ProviderFailureClass::Authentication,
            Some(FailureClassification::CredentialRefreshed)
        )
        .may_retry());
        assert!(!failure(ProviderFailureClass::Authentication, None).may_retry());
        assert!(!failure(ProviderFailureClass::InvalidProviderOutput, None).may_retry());
        assert!(!AttemptFragment::AttemptOpened.may_retry());
    }

    #[test]
    fn finished_text_and_tool_calls_are_visible_output() {
        assert!(AttemptFragment::TextClosed {
            block: BlockOrdinal::new(0),
            state: AdapterState::none(),
        }
        .is_visible_output());
        assert!(!AttemptFragment::ReasoningClosed {
            block: BlockOrdinal::new(0),
            state: AdapterState::none(),
        }
        .is_visible_output());
        assert!(AttemptFragment::ToolCalled {
            call: call("call_1"),
            tool: "read".into(),
            input: "{}".into(),
            execution: ToolExecution::Local,
            state: AdapterState::none(),
        }
        .is_visible_output());
    }

    #[test]
    fn coalescing_never_crosses_a_boundary() {
        let a = AttemptFragment::TextDelta {
            block: BlockOrdinal::new(0),
            text: "a".into(),
            state: AdapterState::none(),
        };
        let same_block = AttemptFragment::TextDelta {
            block: BlockOrdinal::new(0),
            text: "b".into(),
            state: AdapterState::none(),
        };
        let other_block = AttemptFragment::TextDelta {
            block: BlockOrdinal::new(1),
            text: "b".into(),
            state: AdapterState::none(),
        };
        let reasoning = AttemptFragment::ReasoningDelta {
            block: BlockOrdinal::new(0),
            text: "b".into(),
            state: AdapterState::none(),
        };
        let terminal = AttemptFragment::ProviderTerminal {
            finish: FinishReason::Stop,
            usage: None,
            state: AdapterState::none(),
        };

        assert!(a.coalesces_with(&same_block));
        assert!(!a.coalesces_with(&other_block), "block boundary");
        assert!(!a.coalesces_with(&reasoning), "text/reasoning boundary");
        assert!(!a.coalesces_with(&terminal), "terminal boundary");
        assert!(!reasoning.coalesces_with(&a));
    }

    #[test]
    fn same_block_deltas_coalesce_with_latest_state_and_exact_bytes() {
        let mut current = AttemptFragment::TextDelta {
            block: BlockOrdinal::new(0),
            text: "ab".into(),
            state: AdapterState::retain("a").unwrap_or_else(|error| panic!("state: {error}")),
        };
        let next = AttemptFragment::TextDelta {
            block: BlockOrdinal::new(0),
            text: "cd".into(),
            state: AdapterState::retain("latest").unwrap_or_else(|error| panic!("state: {error}")),
        };
        assert_eq!(current.coalesce_or_return(next), None);
        match current {
            AttemptFragment::TextDelta { text, state, .. } => {
                assert_eq!(text, "abcd");
                assert_eq!(state.byte_len(), "latest".len());
            }
            _ => panic!("text delta must remain a text delta"),
        }
    }

    #[test]
    fn coalescing_flushes_losslessly_at_the_byte_cap() {
        let mut current = AttemptFragment::ReasoningDelta {
            block: BlockOrdinal::new(0),
            text: "x".repeat(AttemptFragment::MAX_COALESCED_DELTA_BYTES),
            state: AdapterState::none(),
        };
        let next = AttemptFragment::ReasoningDelta {
            block: BlockOrdinal::new(0),
            text: "y".into(),
            state: AdapterState::none(),
        };
        let refused = match current.coalesce_or_return(next) {
            Some(refused) => refused,
            None => panic!("over-cap delta must flush"),
        };
        match refused {
            AttemptFragment::ReasoningDelta { text, .. } => assert_eq!(text, "y"),
            _ => panic!("refused delta must be returned unchanged"),
        }
    }

    #[test]
    fn adapter_state_refuses_oversize_rather_than_truncating() {
        let oversize = "x".repeat(AdapterState::MAX_BYTES + 1);
        match AdapterState::retain(oversize) {
            Err(error) => {
                assert_eq!(error.limit, AdapterState::MAX_BYTES);
                assert_eq!(error.bytes, AdapterState::MAX_BYTES + 1);
            }
            Ok(_) => panic!("oversize adapter state must be refused, not retained"),
        }

        let at_limit = "x".repeat(AdapterState::MAX_BYTES);
        assert!(
            AdapterState::retain(at_limit).is_ok(),
            "exactly at the bound is allowed"
        );
    }

    #[test]
    fn terminal_fragments_are_the_only_terminals() {
        assert!(AttemptFragment::ProviderTerminal {
            finish: FinishReason::Stop,
            usage: None,
            state: AdapterState::none(),
        }
        .is_terminal());
        assert!(AttemptFragment::ProviderFailure {
            class: ProviderFailureClass::Transport,
            classification: None,
            diagnostic: None,
            message: "reset".into(),
            state: AdapterState::none(),
        }
        .is_terminal());
        assert!(!AttemptFragment::Usage {
            usage: TokenUsage::default()
        }
        .is_terminal());
    }
}
