//! Typed outbound event closure for the Dogfood-0 SSE stream.

use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::dto::NonNegativeU64;
use super::utf16_text::Utf16Text;
use super::{
    BoundedString, DtoString, FiniteF64, MoneyUSDDto, SessionStructuredErrorDto, TokenUsageInfoDto,
    WireId,
};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ServerStoppingData {
    pub reason: BoundedString<64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionTitleChangedData {
    #[serde(rename = "sessionID")]
    pub session_id: WireId<grapt_types::SessionId>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpCatalogChangedData {
    pub scope: BoundedString<64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SkillsChangedData {}

mod continuity_attention;
mod mail_attention;
pub use continuity_attention::{
    ContinuityAttentionData, ContinuityAttentionInfo, ContinuityDegradedReason,
};
pub use mail_attention::{MailAttentionData, MailAttentionInfo, MailPendingCount};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct AggregateIdDto(BoundedString<256>);

impl AggregateIdDto {
    pub fn try_new(value: impl Into<String>) -> Result<Self, &'static str> {
        BoundedString::try_new(value).map(Self)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DurableEventRef {
    #[serde(rename = "aggregateID")]
    aggregate_id: AggregateIdDto,
    seq: NonNegativeU64,
    version: CompatibilityEventVersion,
}

impl DurableEventRef {
    pub fn new(aggregate_id: AggregateIdDto, seq: NonNegativeU64) -> Self {
        Self {
            aggregate_id,
            seq,
            version: CompatibilityEventVersion::One,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompatibilityEventVersion {
    One,
    Two,
}
impl Serialize for CompatibilityEventVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(match self {
            Self::One => 1,
            Self::Two => 2,
        })
    }
}
impl DurableEventRef {
    pub fn version_two(aggregate_id: AggregateIdDto, seq: NonNegativeU64) -> Self {
        Self {
            aggregate_id,
            seq,
            version: CompatibilityEventVersion::Two,
        }
    }

    pub fn tool_terminal(aggregate_id: AggregateIdDto, seq: NonNegativeU64) -> Self {
        Self::version_two(aggregate_id, seq)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiveEvent<D> {
    id: WireId<grapt_types::EventId>,
    created: FiniteF64,
    data: D,
}

impl<D> LiveEvent<D> {
    pub fn new(id: WireId<grapt_types::EventId>, created: FiniteF64, data: D) -> Self {
        Self { id, created, data }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DurableEvent<D> {
    id: WireId<grapt_types::EventId>,
    created: FiniteF64,
    durable: DurableEventRef,
    data: D,
}

impl<D> DurableEvent<D> {
    pub fn new(
        id: WireId<grapt_types::EventId>,
        created: FiniteF64,
        durable: DurableEventRef,
        data: D,
    ) -> Self {
        Self {
            id,
            created,
            durable,
            data,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServerConnectedEvent {
    id: WireId<grapt_types::EventId>,
}

impl ServerConnectedEvent {
    pub fn new(id: WireId<grapt_types::EventId>) -> Self {
        Self { id }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ServerConnectedData {}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StepStartedData {
    #[serde(rename = "sessionID")]
    session_id: WireId<grapt_types::SessionId>,
    #[serde(rename = "assistantMessageID")]
    assistant_message_id: WireId<grapt_types::MessageId>,
    agent: WireId<grapt_types::AgentId>,
    model: super::ModelRefDto,
}

impl StepStartedData {
    pub fn new(
        session_id: WireId<grapt_types::SessionId>,
        assistant_message_id: WireId<grapt_types::MessageId>,
        agent: WireId<grapt_types::AgentId>,
        model: super::ModelRefDto,
    ) -> Self {
        Self {
            session_id,
            assistant_message_id,
            agent,
            model,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SliceStartedData {
    #[serde(rename = "sessionID")]
    session_id: WireId<grapt_types::SessionId>,
    #[serde(rename = "assistantMessageID")]
    assistant_message_id: WireId<grapt_types::MessageId>,
    ordinal: NonNegativeU64,
}

impl SliceStartedData {
    pub fn new(
        session_id: WireId<grapt_types::SessionId>,
        assistant_message_id: WireId<grapt_types::MessageId>,
        ordinal: NonNegativeU64,
    ) -> Self {
        Self {
            session_id,
            assistant_message_id,
            ordinal,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SliceDeltaData {
    #[serde(rename = "sessionID")]
    session_id: WireId<grapt_types::SessionId>,
    #[serde(rename = "assistantMessageID")]
    assistant_message_id: WireId<grapt_types::MessageId>,
    ordinal: NonNegativeU64,
    delta: DtoString,
}

impl SliceDeltaData {
    pub fn new(
        session_id: WireId<grapt_types::SessionId>,
        assistant_message_id: WireId<grapt_types::MessageId>,
        ordinal: NonNegativeU64,
        delta: DtoString,
    ) -> Self {
        Self {
            session_id,
            assistant_message_id,
            ordinal,
            delta,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SliceEndedData {
    #[serde(rename = "sessionID")]
    session_id: WireId<grapt_types::SessionId>,
    #[serde(rename = "assistantMessageID")]
    assistant_message_id: WireId<grapt_types::MessageId>,
    ordinal: NonNegativeU64,
    text: DtoString,
    /// Set only on an elided frame: the block's raw text length in bytes.
    #[serde(rename = "textBytes", skip_serializing_if = "Option::is_none")]
    text_bytes: Option<NonNegativeU64>,
    /// Set only on an elided frame: the lowercase hex SHA-256 of the block's
    /// raw text.
    #[serde(rename = "textSha256", skip_serializing_if = "Option::is_none")]
    text_sha256: Option<DtoString>,
}

impl SliceEndedData {
    pub fn new(
        session_id: WireId<grapt_types::SessionId>,
        assistant_message_id: WireId<grapt_types::MessageId>,
        ordinal: NonNegativeU64,
        text: DtoString,
    ) -> Self {
        Self {
            session_id,
            assistant_message_id,
            ordinal,
            text,
            text_bytes: None,
            text_sha256: None,
        }
    }

    /// The ended frame for a block whose whole text would not fit the live
    /// event bound: an empty `text`, plus the length and digest a client
    /// checks its accumulated deltas against. An accepted deviation from the
    /// frozen record, for overflow only
    /// (fixtures/rust-reconstruction/compat/openapi/DEVIATIONS.md).
    pub fn elided(
        session_id: WireId<grapt_types::SessionId>,
        assistant_message_id: WireId<grapt_types::MessageId>,
        ordinal: NonNegativeU64,
        text_bytes: NonNegativeU64,
        text_sha256: DtoString,
    ) -> Self {
        Self {
            session_id,
            assistant_message_id,
            ordinal,
            text: DtoString::default(),
            text_bytes: Some(text_bytes),
            text_sha256: Some(text_sha256),
        }
    }
}

/// The oracle's ephemeral `websearch.updated` notice; it carries no data.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct WebSearchUpdatedData {}
/// The retry affordance the frozen `session.status` offers while a provider
/// retry is in flight: what an operator can act on from the notice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionStatusAction {
    reason: String,
    provider: String,
    title: String,
    message: String,
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    link: Option<String>,
    #[serde(
        default,
        rename = "queuePosition",
        skip_serializing_if = "Option::is_none"
    )]
    queue_position: Option<NonNegativeU64>,
}

impl SessionStatusAction {
    pub fn reason(&self) -> &str {
        &self.reason
    }

    pub fn new(
        reason: String,
        provider: String,
        title: String,
        message: String,
        label: String,
        link: Option<String>,
    ) -> Self {
        Self {
            reason,
            provider,
            title,
            message,
            label,
            link,
            queue_position: None,
        }
    }

    /// Volatile transport FIFO position; absent for ordinary provider retries.
    pub fn with_queue_position(mut self, position: NonNegativeU64) -> Self {
        self.queue_position = Some(position);
        self
    }
}

/// The quiescence state the frozen `session.status` event reports for one
/// session: idle, busy, or a provider retry in flight.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SessionStatusInfo {
    Idle,
    Busy,
    Retry {
        attempt: NonNegativeU64,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        action: Option<SessionStatusAction>,
        next: NonNegativeU64,
    },
}

impl<'de> Deserialize<'de> for SessionStatusInfo {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        /// `deny_unknown_fields` is not honored by serde for internally
        /// tagged enums, so the frozen `additionalProperties: false` contract
        /// is enforced field by field: idle and busy carry no other field,
        /// and retry carries exactly attempt, message, next and optional
        /// action.
        struct StatusInfoVisitor;

        impl<'de> Visitor<'de> for StatusInfoVisitor {
            type Value = SessionStatusInfo;

            fn expecting(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                formatter.write_str("an object with a \"type\" field of idle, busy or retry")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut kind: Option<&'static str> = None;
                let mut attempt: Option<NonNegativeU64> = None;
                let mut message: Option<String> = None;
                let mut action: Option<SessionStatusAction> = None;
                let mut next: Option<NonNegativeU64> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "type" => {
                            if kind.is_some() {
                                return Err(serde::de::Error::duplicate_field("type"));
                            }
                            let raw = map.next_value::<String>()?;
                            kind = match raw.as_str() {
                                "idle" => Some("idle"),
                                "busy" => Some("busy"),
                                "retry" => Some("retry"),
                                other => {
                                    return Err(serde::de::Error::unknown_variant(
                                        other,
                                        &["idle", "busy", "retry"],
                                    ))
                                }
                            };
                        }
                        "attempt" => {
                            if attempt.is_some() {
                                return Err(serde::de::Error::duplicate_field("attempt"));
                            }
                            attempt = Some(map.next_value()?);
                        }
                        "message" => {
                            if message.is_some() {
                                return Err(serde::de::Error::duplicate_field("message"));
                            }
                            message = Some(map.next_value()?);
                        }
                        "action" => {
                            if action.is_some() {
                                return Err(serde::de::Error::duplicate_field("action"));
                            }
                            action = Some(map.next_value()?);
                        }
                        "next" => {
                            if next.is_some() {
                                return Err(serde::de::Error::duplicate_field("next"));
                            }
                            next = Some(map.next_value()?);
                        }
                        other => {
                            map.next_value::<serde::de::IgnoredAny>()?;
                            return Err(serde::de::Error::unknown_field(
                                other,
                                &["type", "attempt", "message", "action", "next"],
                            ));
                        }
                    }
                }
                let Some(kind) = kind else {
                    return Err(serde::de::Error::missing_field("type"));
                };
                match kind {
                    "idle" | "busy" => {
                        if attempt.is_some()
                            || message.is_some()
                            || action.is_some()
                            || next.is_some()
                        {
                            return Err(serde::de::Error::custom(format!(
                                "session.status {kind} carries no other field"
                            )));
                        }
                        Ok(if kind == "idle" {
                            SessionStatusInfo::Idle
                        } else {
                            SessionStatusInfo::Busy
                        })
                    }
                    "retry" => Ok(SessionStatusInfo::Retry {
                        attempt: attempt
                            .ok_or_else(|| serde::de::Error::missing_field("attempt"))?,
                        message: message
                            .ok_or_else(|| serde::de::Error::missing_field("message"))?,
                        action,
                        next: next.ok_or_else(|| serde::de::Error::missing_field("next"))?,
                    }),
                    _ => unreachable!("kind matched above"),
                }
            }
        }

        deserializer.deserialize_map(StatusInfoVisitor)
    }
}

impl SessionStatusInfo {
    pub const fn idle() -> Self {
        Self::Idle
    }

    pub const fn busy() -> Self {
        Self::Busy
    }

    pub fn retry(
        attempt: NonNegativeU64,
        message: String,
        action: Option<SessionStatusAction>,
        next: NonNegativeU64,
    ) -> Self {
        Self::Retry {
            attempt,
            message,
            action,
            next,
        }
    }
}

/// Optional advisory detail; quiescence remains owned by `status.type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatusPhase {
    Compacting,
}

/// The oracle's ephemeral `session.status` notice: one session's last-known
/// quiescence state, never durable.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionStatusData {
    #[serde(rename = "sessionID")]
    session_id: WireId<grapt_types::SessionId>,
    status: SessionStatusInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<SessionStatusPhase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<super::SessionStateDto>,
}

impl SessionStatusData {
    pub fn new(session_id: WireId<grapt_types::SessionId>, status: SessionStatusInfo) -> Self {
        Self {
            session_id,
            status,
            phase: None,
            state: None,
        }
    }
    pub fn with_phase(mut self, phase: Option<SessionStatusPhase>) -> Self {
        self.phase = phase;
        self
    }
    pub fn with_state(mut self, state: super::SessionStateDto) -> Self {
        self.state = Some(state);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsageUpdatedData {
    #[serde(rename = "sessionID")]
    session_id: WireId<grapt_types::SessionId>,
    cost: MoneyUSDDto,
    tokens: TokenUsageInfoDto,
}

impl UsageUpdatedData {
    pub fn new(
        session_id: WireId<grapt_types::SessionId>,
        cost: MoneyUSDDto,
        tokens: TokenUsageInfoDto,
    ) -> Self {
        Self {
            session_id,
            cost,
            tokens,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExecutionSucceededData {
    #[serde(rename = "sessionID")]
    session_id: WireId<grapt_types::SessionId>,
    #[serde(rename = "executionID", skip_serializing_if = "Option::is_none")]
    execution_id: Option<WireId<grapt_types::ExecutionId>>,
}

impl ExecutionSucceededData {
    pub fn new(
        session_id: WireId<grapt_types::SessionId>,
        execution_id: Option<WireId<grapt_types::ExecutionId>>,
    ) -> Self {
        Self {
            session_id,
            execution_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExecutionFailedData {
    #[serde(rename = "sessionID")]
    session_id: WireId<grapt_types::SessionId>,
    #[serde(rename = "executionID", skip_serializing_if = "Option::is_none")]
    execution_id: Option<WireId<grapt_types::ExecutionId>>,
    error: SessionStructuredErrorDto,
}

impl ExecutionFailedData {
    pub fn new(
        session_id: WireId<grapt_types::SessionId>,
        execution_id: Option<WireId<grapt_types::ExecutionId>>,
        error: SessionStructuredErrorDto,
    ) -> Self {
        Self {
            session_id,
            execution_id,
            error,
        }
    }
}

/// Why a compaction ran, as the frozen `session.compaction.*` events name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CompactionReason {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "manual")]
    Manual,
}

/// A committed compaction. `recent` is the frozen oracle's bounded recent
/// value for the compacted window: the tail of the serialized conversation
/// the walk keeps, published empty only on the summarize-recent branch (no
/// previous recent and no head). The product materializes it from the
/// committed lineage at publication and at replay, while the descriptor
/// itself retains history by reference.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CompactionEndedData {
    #[serde(rename = "sessionID")]
    session_id: WireId<grapt_types::SessionId>,
    reason: CompactionReason,
    text: String,
    recent: Utf16Text,
}

impl CompactionEndedData {
    pub fn new(
        session_id: WireId<grapt_types::SessionId>,
        reason: CompactionReason,
        text: String,
        recent: Utf16Text,
    ) -> Self {
        Self {
            session_id,
            reason,
            text,
            recent,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum InterruptReason {
    #[serde(rename = "user")]
    User,
    #[serde(rename = "shutdown")]
    Shutdown,
    #[serde(rename = "serve-window-expired")]
    ServeWindowExpired,
    #[serde(rename = "superseded")]
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExecutionInterruptedData {
    #[serde(rename = "sessionID")]
    session_id: WireId<grapt_types::SessionId>,
    #[serde(rename = "executionID", skip_serializing_if = "Option::is_none")]
    execution_id: Option<WireId<grapt_types::ExecutionId>>,
    reason: InterruptReason,
}

impl ExecutionInterruptedData {
    pub fn new(
        session_id: WireId<grapt_types::SessionId>,
        execution_id: Option<WireId<grapt_types::ExecutionId>>,
        reason: InterruptReason,
    ) -> Self {
        Self {
            session_id,
            execution_id,
            reason,
        }
    }
}

macro_rules! dogfood_event_closure {
    ($( $variant:ident($payload:ty) => $kind:literal ),+ $(,)?) => {
        /// The exact SSE event closure admitted for Dogfood-0.
        #[derive(Debug, Clone, PartialEq)]
        pub enum DogfoodEvent {
            $( $variant($payload), )+
        }

        impl DogfoodEvent {
            pub const VARIANT_COUNT: usize = [$(stringify!($variant)),+].len();

            pub const fn kind(&self) -> &'static str {
                match self {
                    $( Self::$variant(_) => $kind, )+
                }
            }
        }
    };
}

dogfood_event_closure! {
    ServerConnected(ServerConnectedEvent) => "server.connected",
    PermissionAsked(DurableEvent<super::PermissionAskedData>) => "permission.asked",
    PermissionReplied(DurableEvent<super::PermissionRepliedData>) => "permission.replied",
    PermissionCancelled(DurableEvent<super::PermissionCancelledData>) => "permission.cancelled",
    FormCreated(DurableEvent<super::FormCreatedData>) => "form.created",
    FormReplied(DurableEvent<super::FormRepliedData>) => "form.replied",
    FormCancelled(DurableEvent<super::FormCancelledData>) => "form.cancelled",
    FormSuspended(DurableEvent<super::FormSuspendedData>) => "form.suspended",
    SubagentModelSelected(DurableEvent<super::SessionSubagentModelSelectedData>) => "session.subagent-model.selected",
    ModelSelected(DurableEvent<super::SessionModelSelectedData>) => "session.model.selected",
    AgentSelected(DurableEvent<super::SessionAgentSelectedData>) => "session.agent.selected",
    SubagentModelCleared(DurableEvent<super::SessionSubagentModelClearedData>) => "session.subagent-model.cleared",
    InputAdmitted(DurableEvent<super::InputAdmittedData>) => "session.input.admitted",
    InputPromoted(DurableEvent<super::InputPromotedData>) => "session.input.promoted",
    StepStarted(DurableEvent<StepStartedData>) => "session.step.started",
    ReasoningStarted(DurableEvent<SliceStartedData>) => "session.reasoning.started",
    ReasoningDelta(LiveEvent<SliceDeltaData>) => "session.reasoning.delta",
    ReasoningEnded(DurableEvent<SliceEndedData>) => "session.reasoning.ended",
    TextStarted(DurableEvent<SliceStartedData>) => "session.text.started",
    TextDelta(LiveEvent<SliceDeltaData>) => "session.text.delta",
    TextEnded(DurableEvent<SliceEndedData>) => "session.text.ended",
    UsageUpdated(LiveEvent<UsageUpdatedData>) => "session.usage.updated",
    SessionStatus(LiveEvent<SessionStatusData>) => "session.status",
    SessionTitleChanged(LiveEvent<SessionTitleChangedData>) => "session.title.changed",
    ServerStopping(LiveEvent<ServerStoppingData>) => "server.stopping",
    McpCatalogChanged(LiveEvent<McpCatalogChangedData>) => "mcp.catalog.changed",
    McpResourceChanged(LiveEvent<McpCatalogChangedData>) => "mcp.resource.changed",
    SkillsChanged(LiveEvent<SkillsChangedData>) => "skills.changed",
    MailAttention(LiveEvent<MailAttentionData>) => "session.mail.attention",
    ContinuityAttention(LiveEvent<ContinuityAttentionData>) => "session.continuity.attention",
    WebSearchUpdated(LiveEvent<WebSearchUpdatedData>) => "websearch.updated",
    ToolProgress(LiveEvent<super::ToolProgressData>) => "session.tool.progress",
    ToolInputStarted(DurableEvent<super::ToolInputStartedData>) => "session.tool.input.started",
    ToolCalled(DurableEvent<super::ToolCalledData>) => "session.tool.called",
    ToolSuccess(DurableEvent<super::ToolSuccessData>) => "session.tool.success",
    ToolFailed(DurableEvent<super::ToolFailedData>) => "session.tool.failed",

    ExecutionStarted(DurableEvent<ExecutionSucceededData>) => "session.execution.started",
    ExecutionSucceeded(DurableEvent<ExecutionSucceededData>) => "session.execution.succeeded",
    ExecutionFailed(DurableEvent<ExecutionFailedData>) => "session.execution.failed",
    ExecutionInterrupted(DurableEvent<ExecutionInterruptedData>) => "session.execution.interrupted",
    CompactionEnded(DurableEvent<CompactionEndedData>) => "session.compaction.ended",
}

impl DogfoodEvent {
    /// The owner's location for a per-Session attention level, which is
    /// scoped to it rather than to the publisher's.
    pub(super) fn attention_location(&self) -> Option<&grapt_types::LocationRef> {
        match self {
            Self::MailAttention(event) => Some(event.data.location()),
            Self::ContinuityAttention(event) => Some(event.data.location()),
            _ => None,
        }
    }

    /// A live delta as a positions subscriber receives it; `None` for every
    /// other kind, which a positions subscriber receives unchanged.
    pub(super) fn positioned_delta(&self, from: u64) -> Option<PositionedDelta<'_>> {
        match self {
            Self::TextDelta(event) | Self::ReasoningDelta(event) => Some(PositionedDelta {
                kind: self.kind(),
                id: &event.id,
                created: &event.created,
                data: SliceDeltaPositionedData {
                    delta: &event.data,
                    from: from.into(),
                },
            }),
            _ => None,
        }
    }
}

/// A delta's fields plus `from`: the block's UTF-8 byte offset before it.
/// Sent only to subscribers that opted into positions; `SliceDeltaData` and
/// every legacy frame are unchanged.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SliceDeltaPositionedData<'a> {
    #[serde(flatten)]
    delta: &'a SliceDeltaData,
    from: NonNegativeU64,
}

/// The live envelope, field for field, around a positioned delta.
#[derive(Debug, Serialize)]
pub(super) struct PositionedDelta<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    id: &'a WireId<grapt_types::EventId>,
    created: &'a FiniteF64,
    data: SliceDeltaPositionedData<'a>,
}

macro_rules! serialize_live {
    ($serializer:ident, $event:expr, $kind:expr) => {{
        let mut state = $serializer.serialize_struct("DogfoodEvent", 4)?;
        state.serialize_field("type", $kind)?;
        state.serialize_field("id", &$event.id)?;
        state.serialize_field("created", &$event.created)?;
        state.serialize_field("data", &$event.data)?;
        state.end()
    }};
}

macro_rules! serialize_durable {
    ($serializer:ident, $event:expr, $kind:expr) => {{
        let mut state = $serializer.serialize_struct("DogfoodEvent", 5)?;
        state.serialize_field("type", $kind)?;
        state.serialize_field("id", &$event.id)?;
        state.serialize_field("created", &$event.created)?;
        state.serialize_field("durable", &$event.durable)?;
        state.serialize_field("data", &$event.data)?;
        state.end()
    }};
}

impl Serialize for DogfoodEvent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::ServerConnected(event) => {
                let mut state = serializer.serialize_struct("DogfoodEvent", 3)?;
                state.serialize_field("type", self.kind())?;
                state.serialize_field("id", &event.id)?;
                state.serialize_field("data", &ServerConnectedData {})?;
                state.end()
            }
            Self::ReasoningStarted(event) => serialize_durable!(serializer, event, self.kind()),
            Self::PermissionAsked(event) => serialize_durable!(serializer, event, self.kind()),
            Self::PermissionReplied(event) => serialize_durable!(serializer, event, self.kind()),
            Self::PermissionCancelled(event) => serialize_durable!(serializer, event, self.kind()),
            Self::FormCreated(event) => serialize_durable!(serializer, event, self.kind()),
            Self::FormReplied(event) => serialize_durable!(serializer, event, self.kind()),
            Self::FormCancelled(event) => serialize_durable!(serializer, event, self.kind()),
            Self::FormSuspended(event) => serialize_durable!(serializer, event, self.kind()),
            Self::SubagentModelSelected(event) => {
                serialize_durable!(serializer, event, self.kind())
            }
            Self::SubagentModelCleared(event) => serialize_durable!(serializer, event, self.kind()),
            Self::ModelSelected(event) => serialize_durable!(serializer, event, self.kind()),
            Self::AgentSelected(event) => serialize_durable!(serializer, event, self.kind()),
            Self::InputAdmitted(event) => serialize_durable!(serializer, event, self.kind()),
            Self::InputPromoted(event) => serialize_durable!(serializer, event, self.kind()),
            Self::StepStarted(event) => serialize_durable!(serializer, event, self.kind()),
            Self::ReasoningDelta(event) => serialize_live!(serializer, event, self.kind()),
            Self::ReasoningEnded(event) => serialize_durable!(serializer, event, self.kind()),
            Self::TextStarted(event) => serialize_durable!(serializer, event, self.kind()),
            Self::TextDelta(event) => serialize_live!(serializer, event, self.kind()),
            Self::TextEnded(event) => serialize_durable!(serializer, event, self.kind()),
            Self::UsageUpdated(event) => serialize_live!(serializer, event, self.kind()),
            Self::SessionStatus(event) => serialize_live!(serializer, event, self.kind()),
            Self::SessionTitleChanged(event) => serialize_live!(serializer, event, self.kind()),
            Self::ServerStopping(event) => serialize_live!(serializer, event, self.kind()),
            Self::McpCatalogChanged(event) | Self::McpResourceChanged(event) => {
                serialize_live!(serializer, event, self.kind())
            }
            Self::SkillsChanged(event) => serialize_live!(serializer, event, self.kind()),
            Self::MailAttention(event) => serialize_live!(serializer, event, self.kind()),
            Self::ContinuityAttention(event) => serialize_live!(serializer, event, self.kind()),
            Self::WebSearchUpdated(event) => serialize_live!(serializer, event, self.kind()),
            Self::ToolProgress(event) => serialize_live!(serializer, event, self.kind()),
            Self::ToolInputStarted(event) => serialize_durable!(serializer, event, self.kind()),
            Self::ToolCalled(event) => serialize_durable!(serializer, event, self.kind()),
            Self::ToolSuccess(event) => serialize_durable!(serializer, event, self.kind()),
            Self::ToolFailed(event) => serialize_durable!(serializer, event, self.kind()),

            Self::ExecutionStarted(event) => serialize_durable!(serializer, event, self.kind()),
            Self::ExecutionSucceeded(event) => serialize_durable!(serializer, event, self.kind()),
            Self::ExecutionFailed(event) => serialize_durable!(serializer, event, self.kind()),
            Self::ExecutionInterrupted(event) => {
                serialize_durable!(serializer, event, self.kind())
            }
            Self::CompactionEnded(event) => serialize_durable!(serializer, event, self.kind()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_two_durable_reference_serializes_its_exact_version(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let reference =
            DurableEventRef::version_two(AggregateIdDto::try_new("ses_version_two")?, 1_u64.into());
        let wire = serde_json::to_value(reference)?;
        assert_eq!(
            wire["version"], 2,
            "version-two durable reference regressed to another wire version"
        );
        Ok(())
    }
}
