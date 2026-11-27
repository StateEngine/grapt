//! The frozen wire vocabulary this client is entitled to expect.
//!
//! Every name in this module was READ from the frozen compatibility record at
//! `fixtures/rust-reconstruction/compat/openapi/openapi.json` (101 paths, 120
//! operations) and from the `V2Event` union it declares. Nothing here is
//! invented and nothing is a Rust-side convenience: a name the terminal cannot
//! observe on the wire has no business in a client whose whole contract is that
//! it renders only what the protocol carries.
//!
//! ## Why the client owns a vocabulary at all
//!
//! It would be shorter to reach into `grapt_api::u3a` for the event enum and
//! render that. It would also be wrong. The U3a `DogfoodEvent` closure is
//! today's TWELVE-member implementation of a FIFTEEN-member frozen union:
//! `session.created`, `session.execution.started` and `session.idle` are
//! declared frozen and have no emitter yet. A client shaped by the current
//! implementation would silently acquire a hole exactly where the contract
//! promises a value, and the hole would be invisible until the emitter landed
//! and the renderer ignored it.
//!
//! The vocabulary retains the frozen members and the existing tool lifecycle
//! extension: version-1 openings and version-2 terminals refresh durable rows.
//! These hints never render their payloads as tool results.
//! So the compatibility vocabulary below preserves the FROZEN one. [`EventMember::is_served_today`]
//! records the gap explicitly, as a fact the client can report, rather than as
//! an absence nobody wrote down.
//!
//! ## The durable/live asymmetry
//!
//! In the frozen union NINE journey members REQUIRE a `durable` object of
//! `{aggregateID, seq, version: 1}`: `session.created`,
//! `session.reasoning.started`, `session.reasoning.ended`,
//! `session.text.started`, `session.text.ended`, `session.execution.started`
//! and the three execution terminals. SIX declare no such property and, being
//! `additionalProperties: false`, must not carry one: `server.connected`,
//! `session.reasoning.delta`, `session.text.delta`, `session.usage.updated`,
//! `session.status` and `session.idle`.
//!
//! That asymmetry is the wire-visible form of durable-versus-live separation,
//! and it is the whole reason this client can converge after a dropped stream
//! rather than merely hope it did. A transport that stamped `durable` onto a
//! live frame would make every frame look re-derivable and would defeat the
//! reconciliation in the client's durable cursor; a transport that dropped it from a
//! member that requires one would make durable truth unorderable. Both
//! directions are refused, in one place, by [`EventEnvelope::decode`].
//!
//! The membership is spelled out MEMBER BY MEMBER rather than derived from
//! terminality. The two sets are not the same set — the terminals are three of
//! the nine — and deriving one from the other is exactly how
//! `session.execution.started` once reached a harness transcript without the
//! `durable` block the frozen record requires while every law reported green.

use thiserror::Error;

mod operations;
pub use operations::{Method, Operation};

/// The journey-relevant members of the frozen `V2Event` union.
///
/// The union is larger than this; these are the members the essential journey
/// observes. An unknown `type` is not an error — it is a member outside this
/// client's vocabulary — but it is never counted as one of the checkpoints
/// either, and it never becomes renderable content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EventMember {
    ServerConnected,
    SessionCreated,
    ReasoningStarted,
    ReasoningDelta,
    ReasoningEnded,
    TextStarted,
    TextDelta,
    TextEnded,
    UsageUpdated,
    ToolProgress,
    ToolInputStarted,
    ToolCalled,
    ToolSuccess,
    ToolFailed,
    ExecutionStarted,
    ExecutionSucceeded,
    ExecutionFailed,
    ExecutionInterrupted,
    SessionIdle,
    SessionStatus,
    MailAttention,
    ContinuityAttention,
}

impl EventMember {
    /// Every frozen journey member.
    pub const ALL: &'static [EventMember] = &[
        EventMember::ServerConnected,
        EventMember::SessionCreated,
        EventMember::ReasoningStarted,
        EventMember::ReasoningDelta,
        EventMember::ReasoningEnded,
        EventMember::TextStarted,
        EventMember::TextDelta,
        EventMember::TextEnded,
        EventMember::UsageUpdated,
        EventMember::ToolProgress,
        EventMember::ToolInputStarted,
        EventMember::ToolCalled,
        EventMember::ToolSuccess,
        EventMember::ToolFailed,
        EventMember::ExecutionStarted,
        EventMember::ExecutionSucceeded,
        EventMember::ExecutionFailed,
        EventMember::ExecutionInterrupted,
        EventMember::SessionIdle,
        EventMember::SessionStatus,
        EventMember::MailAttention,
        EventMember::ContinuityAttention,
    ];

    /// The frozen `type` discriminator.
    pub fn as_str(self) -> &'static str {
        match self {
            EventMember::ServerConnected => "server.connected",
            EventMember::SessionCreated => "session.created",
            EventMember::ReasoningStarted => "session.reasoning.started",
            EventMember::ReasoningDelta => "session.reasoning.delta",
            EventMember::ReasoningEnded => "session.reasoning.ended",
            EventMember::TextStarted => "session.text.started",
            EventMember::TextDelta => "session.text.delta",
            EventMember::TextEnded => "session.text.ended",
            EventMember::UsageUpdated => "session.usage.updated",
            EventMember::ToolProgress => "session.tool.progress",
            EventMember::ToolInputStarted => "session.tool.input.started",
            EventMember::ToolCalled => "session.tool.called",
            EventMember::ToolSuccess => "session.tool.success",
            EventMember::ToolFailed => "session.tool.failed",
            EventMember::ExecutionStarted => "session.execution.started",
            EventMember::ExecutionSucceeded => "session.execution.succeeded",
            EventMember::ExecutionFailed => "session.execution.failed",
            EventMember::ExecutionInterrupted => "session.execution.interrupted",
            EventMember::SessionIdle => "session.idle",
            EventMember::SessionStatus => "session.status",
            EventMember::MailAttention => "session.mail.attention",
            EventMember::ContinuityAttention => "session.continuity.attention",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|kind| kind.as_str() == raw)
    }

    /// Existing tool events are hints to re-read bounded durable tool rows.
    pub fn is_tool_hint(self) -> bool {
        matches!(
            self,
            Self::ToolProgress
                | Self::ToolInputStarted
                | Self::ToolCalled
                | Self::ToolSuccess
                | Self::ToolFailed
        )
    }

    /// The three terminal outcomes of one execution.
    ///
    /// `session.idle` is NOT a terminal outcome. It is a session-level
    /// quiescence signal the frozen union declares WITHOUT a `durable` block,
    /// and rendering it as a terminal is precisely the mistake that would show
    /// a settled run that never settled.
    pub fn is_execution_terminal(self) -> bool {
        matches!(
            self,
            EventMember::ExecutionSucceeded
                | EventMember::ExecutionFailed
                | EventMember::ExecutionInterrupted
        )
    }

    /// Whether the frozen record REQUIRES a `durable` block on this member.
    ///
    /// The match is EXHAUSTIVE with no wildcard on purpose. Adding a member
    /// must not silently inherit an answer; it must fail to compile until
    /// somebody has read the frozen record for that member.
    pub fn requires_durable(self) -> bool {
        match self {
            EventMember::SessionCreated
            | EventMember::ToolInputStarted
            | EventMember::ToolCalled
            | EventMember::ToolSuccess
            | EventMember::ToolFailed
            | EventMember::ReasoningStarted
            | EventMember::ReasoningEnded
            | EventMember::TextStarted
            | EventMember::TextEnded
            | EventMember::ExecutionStarted
            | EventMember::ExecutionSucceeded
            | EventMember::ExecutionFailed
            | EventMember::ExecutionInterrupted => true,
            EventMember::ServerConnected
            | EventMember::ReasoningDelta
            | EventMember::TextDelta
            | EventMember::UsageUpdated
            | EventMember::ToolProgress
            | EventMember::SessionIdle
            | EventMember::SessionStatus
            | EventMember::MailAttention
            | EventMember::ContinuityAttention => false,
        }
    }

    /// Whether the frozen record requires a `created` timestamp.
    ///
    /// `server.connected` is the single journey member whose `required` list
    /// is `[id, type, data]` with no `created`. It is a cheap exact
    /// discriminator that a hand-rolled transport tends to get wrong in one
    /// direction or the other.
    pub fn requires_created(self) -> bool {
        !matches!(self, EventMember::ServerConnected)
    }

    /// The four `session.execution.*` members, the only ones the frozen record
    /// gives an `executionID`.
    ///
    /// Attribution reads this. A start or terminal carrying no execution id is
    /// MISSING EVIDENCE, and a projection that counted such frames globally
    /// could not tell one execution settling twice from two executions
    /// settling once each.
    pub fn carries_execution_id(self) -> bool {
        matches!(self, EventMember::ExecutionStarted) || self.is_execution_terminal()
    }

    /// Whether `grapt-api`'s U3a closure emits this member TODAY.
    ///
    /// This is a measured fact about the current engine, not a property of the
    /// contract, and it is recorded so the client can say "the contract
    /// promises this and nothing emits it yet" instead of rendering a silent
    /// hole. Measured at base `151edd66` against the eleven-member
    /// `dogfood_event_closure!` in `crates/grapt-api/src/u3a/events.rs`, and
    /// re-measured at base `c755c828` when the SESSION-QUIESCENCE emitter
    /// admitted `session.status` to the
    /// twelve-member closure.
    ///
    /// When another emitter lands, this function changes and
    /// `unserved_members_are_exactly_the_measured_three` fails until somebody
    /// re-measures. That failure is the point.
    pub fn is_served_today(self) -> bool {
        match self {
            EventMember::SessionCreated
            | EventMember::ExecutionStarted
            | EventMember::SessionIdle => false,
            EventMember::ServerConnected
            | EventMember::ReasoningStarted
            | EventMember::ReasoningDelta
            | EventMember::ReasoningEnded
            | EventMember::TextStarted
            | EventMember::TextDelta
            | EventMember::TextEnded
            | EventMember::UsageUpdated
            | EventMember::ToolProgress
            | EventMember::ToolInputStarted
            | EventMember::ToolCalled
            | EventMember::ToolSuccess
            | EventMember::ToolFailed
            | EventMember::ExecutionSucceeded
            | EventMember::ExecutionFailed
            | EventMember::ExecutionInterrupted
            | EventMember::SessionStatus
            | EventMember::MailAttention
            | EventMember::ContinuityAttention => true,
        }
    }
}

/// The frozen durable envelope version.
pub const DURABLE_VERSION: u64 = 1;

/// One decoded frame, validated against the frozen envelope law.
///
/// ## Every identity here is TYPED, and that is load-bearing
///
/// The previous revision carried `id`, `session_id` and `execution_id` as
/// `String`, admitted a session id on `starts_with("ses")`, and even asserted
/// in a positive test that `sesABC` is a valid session. Three separate things
/// were wrong with that and only one of them is the prefix:
///
/// 1. `^ses` is the frozen wire pattern, but `grapt_types::SessionId` — the
///    ratified identity — is `ses_` plus a non-empty valid suffix. The frozen
///    pattern is the WEAKER constraint, so honouring it admits identities the
///    domain cannot represent, which is not compatibility. See
///    [`crate::wire::session_id`] for the full reasoning.
/// 2. A prefix test is not a parse. It says nothing about the rest of the
///    value, so `ses_` with an empty suffix and `ses_a-b` both passed.
/// 3. A `String` identity can be passed where a different identity is
///    expected. `execution_id` and `session_id` were the same type, so
///    nothing but care stopped one reaching a site that wanted the other.
///
/// All three are closed by making the field type the domain newtype and
/// parsing ONCE, here, at the edge. Downstream — the cursor's watermarks, the
/// projection's ownership check, the client's view map — cannot re-derive the
/// question, because they never see a string to re-derive it from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventEnvelope {
    pub id: crate::wire::EventWireId,
    pub kind_raw: String,
    /// `None` when the frame carries a `type` outside this vocabulary.
    pub member: Option<EventMember>,
    pub session_id: Option<crate::wire::SessionWireId>,
    pub execution_id: Option<crate::wire::ExecutionWireId>,
    /// `Some` exactly when the frame carried a valid `durable` block.
    pub durable: Option<DurableRef>,
    /// The server-stamped `created` field, when the member carries one.
    ///
    /// `None` only for the members the frozen record forbids it on (today:
    /// `server.connected`). It is SERVER time, not client time: comparing two
    /// envelopes' values orders facts the way the server stamped them, which
    /// is what a quiescence carry-forward needs and a client clock would
    /// never be allowed to give it.
    pub created: Option<u64>,
    pub data: serde_json::Value,
}

/// The `{aggregateID, seq, version}` block, validated.
///
/// `aggregate_id` is a `SessionWireId` rather than a string, because every
/// durable member of the frozen union is session-aggregated: the aggregate a
/// `session.text.started` belongs to IS its session. Typing it is what lets the
/// cursor key its watermarks on the same value the projection keys its views
/// on, and be certain by construction that the two agree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableRef {
    pub aggregate_id: crate::wire::SessionWireId,
    pub seq: u64,
}

impl EventEnvelope {
    /// Decode one `data:` payload and enforce the frozen envelope law.
    ///
    /// The law is enforced HERE, once, rather than at each render site,
    /// because an envelope defect a single site forgets to check is an
    /// envelope defect the client renders as truth.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn decode(payload: &str) -> Result<Self, ContractError> {
        let value: serde_json::Value =
            serde_json::from_str(payload).map_err(|error| ContractError::NotJson {
                reason: error.to_string(),
            })?;
        let object = value.as_object().ok_or(ContractError::NotObject)?;

        let id = object
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or(ContractError::FieldMissing { field: "id" })?;
        let id = crate::wire::event_id(id)
            .map_err(|_| ContractError::EventIdInvalid { id: id.to_owned() })?;
        let kind_raw = object
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or(ContractError::FieldMissing { field: "type" })?;
        let data = object
            .get("data")
            .ok_or(ContractError::FieldMissing { field: "data" })?;

        let member = EventMember::parse(kind_raw);
        let durable_block = object.get("durable");
        let has_created = object.contains_key("created");
        // The frozen record stamps `created` as a finite number, and the
        // engine serializes it as an f64 in milliseconds; a u64-only read
        // would accept the frame and then silently drop its stamp.
        let created = object.get("created").and_then(|value| {
            value.as_u64().or_else(|| {
                // 2^64, exactly representable as f64: above it no u64 exists.
                const U64_MAX_PLUS_ONE: f64 = 18_446_744_073_709_551_616.0;
                value
                    .as_f64()
                    .filter(|f| {
                        f.is_finite() && f.fract() == 0.0 && (0.0..U64_MAX_PLUS_ONE).contains(f)
                    })
                    .map(|f| f as u64)
            })
        });

        if let Some(member) = member {
            Self::check_envelope(member, durable_block.is_some(), has_created)?;
        }
        if member == Some(EventMember::ToolProgress) {
            serde_json::from_value::<grapt_api::u3a::ToolProgressData>(data.clone())
                .map_err(|_| ContractError::ToolProgressMalformed)?;
        }
        if member == Some(EventMember::SessionStatus) {
            // The frozen record requires BOTH fields: a status edge with no
            // session cannot be attributed, and a fold that applied it to
            // whichever view was open would report one session's quiescence
            // for another.
            let table = data
                .as_object()
                .ok_or(ContractError::SessionStatusMalformed)?;
            // The identity is REQUIRED and TYPED, not merely present: the presence check
            // admitted `null` and numbers, which decoded to `session_id: None` and let a
            // projection fold mutate whichever view it was applied to. The same wire
            // helper the generic path below uses refuses them here, where the member
            // names them.
            let raw_session = table
                .get("sessionID")
                .and_then(serde_json::Value::as_str)
                .ok_or(ContractError::SessionStatusMalformed)?;
            crate::wire::session_id(raw_session)
                .map_err(|_| ContractError::SessionStatusMalformed)?;
            let status = table
                .get("status")
                .cloned()
                .ok_or(ContractError::SessionStatusMalformed)?;
            serde_json::from_value::<grapt_api::u3a::SessionStatusInfo>(status)
                .map_err(|_| ContractError::SessionStatusMalformed)?;
        }

        if member == Some(EventMember::MailAttention) {
            let table = data
                .as_object()
                .filter(|v| v.len() == 2 || (v.len() == 3 && v.contains_key("staleSince")))
                .ok_or(ContractError::MailAttentionMalformed)?;
            if table
                .get("staleSince")
                .is_some_and(|value| value.as_f64().is_none_or(|since| !since.is_finite()))
            {
                return Err(ContractError::MailAttentionMalformed);
            }
            let session = table
                .get("sessionID")
                .and_then(serde_json::Value::as_str)
                .ok_or(ContractError::MailAttentionMalformed)?;
            crate::wire::session_id(session).map_err(|_| ContractError::MailAttentionMalformed)?;
            let status = table
                .get("status")
                .cloned()
                .ok_or(ContractError::MailAttentionMalformed)?;
            serde_json::from_value::<grapt_api::u3a::MailAttentionInfo>(status)
                .map_err(|_| ContractError::MailAttentionMalformed)?;
        }

        if member == Some(EventMember::ContinuityAttention) {
            let table = data
                .as_object()
                .filter(|v| v.len() == 2)
                .ok_or(ContractError::ContinuityAttentionMalformed)?;
            let session = table
                .get("sessionID")
                .and_then(serde_json::Value::as_str)
                .ok_or(ContractError::ContinuityAttentionMalformed)?;
            crate::wire::session_id(session)
                .map_err(|_| ContractError::ContinuityAttentionMalformed)?;
            let status = table
                .get("status")
                .cloned()
                .ok_or(ContractError::ContinuityAttentionMalformed)?;
            serde_json::from_value::<grapt_api::u3a::ContinuityAttentionInfo>(status)
                .map_err(|_| ContractError::ContinuityAttentionMalformed)?;
        }

        if member.is_some_and(EventMember::is_tool_hint) {
            let call = data
                .get("callID")
                .and_then(serde_json::Value::as_str)
                .ok_or(ContractError::ToolHintMalformed)?;
            crate::wire::tool_call_id(call).map_err(|_| ContractError::ToolHintMalformed)?;
            if data
                .get("sessionID")
                .and_then(serde_json::Value::as_str)
                .is_none()
            {
                return Err(ContractError::ToolHintMalformed);
            }
        }

        let durable = match durable_block {
            None => None,
            Some(block) => Some(Self::decode_durable(block, kind_raw)?),
        };

        let table = data.as_object();
        // PARSED, not prefix-tested. A frame naming a session the domain cannot
        // represent is refused here — it never reaches a map key, a watermark
        // or a view. `sesABC` is refused; the previous revision accepted it and
        // asserted so in a positive test.
        let session_id = match table
            .and_then(|data| data.get("sessionID"))
            .and_then(serde_json::Value::as_str)
        {
            None => None,
            Some(raw) => Some(
                crate::wire::session_id(raw)
                    .map_err(|_| ContractError::SessionIdInvalid { id: raw.to_owned() })?,
            ),
        };
        // The execution id is likewise parsed rather than string-filtered. The
        // empty-string filter it replaces was the whole of the old validation,
        // and `ExecutionId::new` refuses empty for the same reason — but now a
        // refusal is VISIBLE as an error rather than silently collapsing to
        // `None`, which is what let a malformed execution id look like an
        // absent one.
        let execution_id = match table
            .and_then(|data| data.get("executionID"))
            .and_then(serde_json::Value::as_str)
        {
            None => None,
            Some(raw) => Some(
                crate::wire::execution_id(raw)
                    .map_err(|_| ContractError::ExecutionIdInvalid { id: raw.to_owned() })?,
            ),
        };

        Ok(Self {
            id,
            kind_raw: kind_raw.to_owned(),
            member,
            session_id,
            execution_id,
            durable,
            data: data.clone(),
            created,
        })
    }

    /// Both directions of both envelope rules.
    ///
    /// Each rule is checked in BOTH directions deliberately. A one-directional
    /// check would accept a transport that stamps `durable` on every frame,
    /// which is exactly the shape that makes a live frame look re-derivable
    /// and defeats gap reconciliation downstream.
    fn check_envelope(
        member: EventMember,
        has_durable: bool,
        has_created: bool,
    ) -> Result<(), ContractError> {
        match (member.requires_durable(), has_durable) {
            (true, false) => {
                return Err(ContractError::DurableMissing {
                    member: member.as_str(),
                })
            }
            (false, true) => {
                return Err(ContractError::DurableForbidden {
                    member: member.as_str(),
                })
            }
            _ => {}
        }
        match (member.requires_created(), has_created) {
            (true, false) => Err(ContractError::CreatedMissing {
                member: member.as_str(),
            }),
            (false, true) => Err(ContractError::CreatedForbidden {
                member: member.as_str(),
            }),
            _ => Ok(()),
        }
    }

    /// Validate the `durable` block.
    ///
    /// The retained tool terminal extension uses version 2; opening and
    /// all other durable events retain version 1. Unknown versions are refused.
    fn decode_durable(
        block: &serde_json::Value,
        kind_raw: &str,
    ) -> Result<DurableRef, ContractError> {
        let table = block
            .as_object()
            .ok_or_else(|| ContractError::DurableMalformed {
                member: kind_raw.to_owned(),
                reason: "durable is not an object".to_owned(),
            })?;
        let aggregate_id = table
            .get("aggregateID")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        // Parsed through the same authoritative newtype as `data.sessionID`,
        // deliberately. The two must be the same KIND of value or the cursor's
        // watermark and the projection's view could be keyed on identities that
        // merely look alike; an emptiness check could not tell them apart.
        let aggregate_id =
            crate::wire::session_id(aggregate_id).map_err(|_| ContractError::DurableMalformed {
                member: kind_raw.to_owned(),
                reason: format!(
                    "durable.aggregateID is not a valid session identity: {aggregate_id:?}"
                ),
            })?;
        let expected_version = if matches!(
            EventMember::parse(kind_raw),
            Some(EventMember::ToolSuccess | EventMember::ToolFailed)
        ) {
            2
        } else {
            DURABLE_VERSION
        };
        match table.get("version").and_then(serde_json::Value::as_u64) {
            Some(version) if version == expected_version => {}
            other => {
                return Err(ContractError::DurableMalformed {
                    member: kind_raw.to_owned(),
                    reason: format!("durable.version must be {expected_version}, found {other:?}"),
                })
            }
        }
        let seq = table
            .get("seq")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| ContractError::DurableMalformed {
                member: kind_raw.to_owned(),
                reason: "durable.seq is absent or not a non-negative integer".to_owned(),
            })?;
        Ok(DurableRef { aggregate_id, seq })
    }
}

/// Every way the wire can fail this client's frozen expectations.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContractError {
    #[error("tool refresh hint requires a valid session and call identity")]
    ToolHintMalformed,
    #[error("tool progress is malformed or exceeds its bounds")]
    ToolProgressMalformed,
    #[error("session.status carried no session identity or no parsable status payload")]
    SessionStatusMalformed,
    #[error("session mail attention is malformed")]
    MailAttentionMalformed,
    #[error("session continuity attention is malformed")]
    ContinuityAttentionMalformed,
    #[error("{operation} is session-scoped and was given no session id")]
    SessionRequired { operation: &'static str },
    #[error("{operation} is request-scoped and was given no request id")]
    RequestRequired { operation: &'static str },
    #[error("{operation} is form-scoped and was given no form id")]
    FormRequired { operation: &'static str },
    #[error("event payload was not JSON: {reason}")]
    NotJson { reason: String },
    #[error("event payload was not a JSON object")]
    NotObject,
    #[error("event payload omitted the required field `{field}`")]
    FieldMissing { field: &'static str },
    #[error("event id `{id}` is not a valid `grapt_types::EventId`")]
    EventIdInvalid { id: String },
    /// The refusal names the PARSE, not a prefix.
    ///
    /// The wording matters for the same reason the check does: a message that
    /// said "does not carry the `ses` prefix" would send the next reader to
    /// re-check a prefix, when what actually failed is the ratified identity
    /// law — which also refuses an empty suffix and an invalid byte.
    #[error("session id `{id}` is not a valid `grapt_types::SessionId`")]
    SessionIdInvalid { id: String },
    #[error("execution id `{id}` is not a valid `grapt_types::ExecutionId`")]
    ExecutionIdInvalid { id: String },
    #[error("{member} requires a durable block and carried none")]
    DurableMissing { member: &'static str },
    #[error("{member} is a live frame and must carry no durable block")]
    DurableForbidden { member: &'static str },
    #[error("{member} requires a created timestamp and carried none")]
    CreatedMissing { member: &'static str },
    #[error("{member} must carry no created timestamp")]
    CreatedForbidden { member: &'static str },
    #[error("{member} carried a malformed durable block: {reason}")]
    DurableMalformed { member: String, reason: String },
}

#[cfg(test)]
mod tests;

/// The three terminal outcomes the frozen record declares. There is no fourth,
/// and `session.idle` is not one of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Succeeded,
    Failed,
    Interrupted,
}
