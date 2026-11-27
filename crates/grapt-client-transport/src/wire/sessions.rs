//! Sessions: identity, info, listing, creation, prompting and interruption.
//!
//! Split from `wire.rs` by SUBJECT when that file passed this workspace's
//! module ceiling. Every encode and decode still goes through the real codecs
//! under the real limits; nothing here was rewritten on the way out.

use super::*;

/// Parse an event id at the wire edge, once, through `grapt_types::EventId`.
pub fn event_id(value: &str) -> Result<EventWireId, WireError> {
    WireId::parse(value.to_owned()).ok_or_else(|| WireError::Invalid {
        what: "event id",
        value: value.to_owned(),
    })
}

/// Parse an execution id at the wire edge, once.
///
/// `ExecutionId` is an OPAQUE identity — the domain gives it no prefix law,
/// only non-emptiness — so this parse is genuinely weaker than the session one.
/// It is still a parse rather than a `String`, because the point of the newtype
/// is that an execution id cannot be passed where a session id is expected; the
/// compiler enforces that even where the value law is thin.
pub fn execution_id(value: &str) -> Result<ExecutionWireId, WireError> {
    WireId::parse(value.to_owned()).ok_or_else(|| WireError::Invalid {
        what: "execution id",
        value: value.to_owned(),
    })
}

// ---------------------------------------------------------------------------
// Session list
// ---------------------------------------------------------------------------

/// Build one real `SessionInfoDto`.
///
/// Every REQUIRED field of the contract is supplied. That is the point: the
/// invented shape had two keys, and the real one has seven required fields, so
/// a double that can build this is a double that speaks the contract.
pub fn session_info(
    id: &SessionWireId,
    title: &str,
    directory: &str,
    parent: Option<&SessionWireId>,
) -> Result<grapt_api::u3a::SessionInfoDto, WireError> {
    use grapt_api::u3a::{
        LocationRefDto, MoneyUSDDto, SessionInfoDto, SessionInfoDtoTime, TokenUsageInfoDto,
        TokenUsageInfoDtoCache,
    };
    let project = WireId::parse("prj_double".to_owned()).ok_or_else(|| WireError::Invalid {
        what: "project id",
        value: "prj_double".to_owned(),
    })?;
    let cache =
        TokenUsageInfoDtoCache::new(Required::new(finite(0.0)?), Required::new(finite(0.0)?));
    let tokens = TokenUsageInfoDto::new(
        Required::new(finite(0.0)?),
        Required::new(finite(0.0)?),
        Required::new(finite(0.0)?),
        Required::new(cache),
    );
    let time = SessionInfoDtoTime::new(
        Optional::present(finite(1_700_000_000_000.0)?),
        Required::new(finite(1_700_000_000_000.0)?),
        Optional::absent(),
    );
    let location = LocationRefDto::new(Required::new(dto_string(directory)?), Optional::absent());
    Ok(SessionInfoDto::new(
        Required::new(id.clone()),
        // `parentID`. A subagent's session sets it; a session the user started
        // leaves it absent.
        match parent {
            Some(parent) => Optional::present(parent.clone()),
            None => Optional::absent(),
        },
        Optional::absent(),
        Required::new(project),
        Optional::absent(),
        Optional::absent(),
        Optional::absent(),
        Optional::present(MoneyUSDDto::new(finite(0.0)?)),
        Optional::present(grapt_api::u3a::SessionInfoDtoCostStatus::KnownNonbillable),
        Optional::present(tokens),
        Required::new(time),
        Required::new(dto_string(title)?),
        Required::new(location),
        Optional::absent(),
        Optional::absent(),
        Optional::absent(),
        Optional::absent(),
        Optional::absent(),
        Optional::absent(),
    ))
}

/// Encode a real `SessionsResponseDto`.
pub fn encode_sessions(rows: Vec<grapt_api::u3a::SessionInfoDto>) -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::{SessionsResponseDto, SessionsResponseDtoCursor};
    let data = BoundedVec::try_new(rows).map_err(|reason| WireError::Invalid {
        what: "session page",
        value: reason.to_owned(),
    })?;
    let cursor =
        SessionsResponseDtoCursor::new(OptionalNullable::absent(), OptionalNullable::absent());
    let dto = SessionsResponseDto::new(Required::new(data), Required::new(cursor));
    Ok(encode_page(&dto, limits()?)?)
}

/// One row of the session picker, read from the real DTO.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub id: SessionWireId,
    pub title: String,
    /// The session this one was spawned BY, when it was spawned by one.
    ///
    /// `Session.Info` carries `parentID` as an optional, and a subagent's own
    /// session sets it. Dropping it made a child indistinguishable from a
    /// session the user started: the list showed both flat, so a subagent's
    /// private transcript sat among the user's own conversations under a title
    /// the subagent chose.
    ///
    /// ABSENT MEANS ROOT. It does not mean unread - the field is only ever
    /// populated from a decoded row, so `None` here is the contract's own
    /// answer that this session has no parent.
    pub parent: Option<SessionWireId>,
    pub detail: SessionDetail,
}

/// Reported subagent-row metadata; every absent field stays unknown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionDetail {
    /// The agent id's spelling until a roster supplies its name.
    pub agent: Option<String>,
    /// `provider/model#variant` exactly as this session reports it.
    pub model: Option<String>,
    /// Reported prompt input plus cache reads, the frozen switcher's `↓` count.
    pub context_tokens: Option<u64>,
    pub created_ms: Option<u64>,
    pub updated_ms: Option<u64>,
    pub directory: Option<String>,
    pub metadata_status: Option<grapt_api::u3a::SessionInfoDtoMetadataStatus>,
}

/// An exact JavaScript-safe non-negative integer, or unknown; never a saturating cast.
fn exact(value: &grapt_api::u3a::FiniteF64) -> Option<u64> {
    let number = serde_json::to_value(value).ok()?.as_f64()?;
    (number >= 0.0 && number.fract() == 0.0 && number <= grapt_api::u3a::SafeTokenCount::MAX as f64)
        .then_some(number as u64)
}

fn detail(info: &grapt_api::u3a::SessionInfoDto) -> SessionDetail {
    SessionDetail {
        agent: info
            .agent()
            .as_present()
            .map(|agent| agent.spelled().to_owned()),
        model: info.model().as_present().map(|model| {
            let variant = model
                .variant()
                .as_present()
                .map_or(String::new(), |level| format!("#{}", level.spelled()));
            format!(
                "{}/{}{variant}",
                model.provider_id().get().spelled(),
                model.id().get().spelled()
            )
        }),
        context_tokens: info.tokens().as_present().and_then(|tokens| {
            exact(tokens.input().get())?
                .checked_add(exact(tokens.cache().get().read().get())?)
                .filter(|sum| *sum <= grapt_api::u3a::SafeTokenCount::MAX)
        }),
        created_ms: info.time().get().created().as_present().and_then(exact),
        updated_ms: exact(info.time().get().updated().get()),
        directory: Some(info.location().get().directory().get().as_str().to_owned()),
        metadata_status: info.metadata_status().as_present().copied(),
    }
}

/// Decode a real `SessionsResponseDto`.
pub fn decode_sessions(bytes: &[u8]) -> Result<Vec<SessionRow>, WireError> {
    decode_sessions_page(bytes).map(|(rows, _)| rows)
}
pub fn decode_sessions_page(bytes: &[u8]) -> Result<(Vec<SessionRow>, bool), WireError> {
    use grapt_api::u3a::SessionsResponseDto;
    let dto: SessionsResponseDto = decode_page(bytes, limits()?)?;
    let more = dto.cursor().get().next().as_value().is_some();
    Ok((
        dto.data()
            .get()
            .as_slice()
            .iter()
            .map(|info| SessionRow {
                id: info.id().get().clone(),
                title: info.title().get().as_str().to_owned(),
                parent: info.parent_id().as_present().cloned(),
                detail: detail(info),
            })
            .collect(),
        more,
    ))
}

/// Encode a real `SessionResponseDto`.
pub fn encode_session(info: grapt_api::u3a::SessionInfoDto) -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::SessionResponseDto;
    let dto = SessionResponseDto::new(Required::new(info));
    Ok(encode_page(&dto, limits()?)?)
}

/// Encode a complete session lookup, retaining its selected model coordinate.
pub fn encode_session_with_model(
    info: grapt_api::u3a::SessionInfoDto,
    model: Option<grapt_api::u3a::ModelRefDto>,
) -> Result<Vec<u8>, WireError> {
    let encoded = encode_session(info)?;
    let mut value: serde_json::Value =
        serde_json::from_slice(&encoded).map_err(|error| WireError::Invalid {
            what: "session encoding",
            value: error.to_string(),
        })?;
    if let Some(model) = model {
        value["data"]["model"] =
            serde_json::to_value(model).map_err(|error| WireError::Invalid {
                what: "model encoding",
                value: error.to_string(),
            })?;
    }
    let bytes = value.to_string().into_bytes();
    let _validated: grapt_api::u3a::SessionResponseDto = decode_page(&bytes, limits()?)?;
    Ok(bytes)
}

/// Decode a real `SessionResponseDto` and return its identity and title.
pub fn decode_session(bytes: &[u8]) -> Result<SessionRow, WireError> {
    use grapt_api::u3a::SessionResponseDto;
    let dto: SessionResponseDto = decode_page(bytes, limits()?)?;
    let info = dto.data().get();
    Ok(SessionRow {
        id: info.id().get().clone(),
        title: info.title().get().as_str().to_owned(),
        parent: info.parent_id().as_present().cloned(),
        detail: detail(info),
    })
}

/// Read a session's complete model coordinate without losing its effort.
pub fn decode_session_model(bytes: &[u8]) -> Result<Option<ModelRow>, WireError> {
    let dto: grapt_api::u3a::SessionResponseDto = decode_page(bytes, limits()?)?;
    Ok(dto
        .data()
        .get()
        .model()
        .as_present()
        .map(super::catalogs::model_ref_row))
}

/// Saved selections for an explicitly requested launch session.
#[derive(Debug)]
pub struct SessionAttachment {
    pub id: SessionWireId,
    pub project: ProjectRow,
    pub agent: Option<AgentWireId>,
    pub model: Option<ModelRow>,
}

pub fn decode_session_attachment(bytes: &[u8]) -> Result<SessionAttachment, WireError> {
    let dto: grapt_api::u3a::SessionResponseDto = decode_page(bytes, limits()?)?;
    let info = dto.data().get();
    let directory = info.location().get().directory().get().as_str();
    if directory.is_empty() || directory.len() > 8_192 || directory.chars().any(char::is_control) {
        return Err(WireError::Invalid {
            what: "session directory",
            value: directory.to_owned(),
        });
    }
    Ok(SessionAttachment {
        id: info.id().get().clone(),
        project: ProjectRow {
            id: info.project_id().get().clone(),
            worktree: directory.to_owned(),
            name: None,
        },
        agent: info.agent().as_present().cloned(),
        model: info
            .model()
            .as_present()
            .map(super::catalogs::model_ref_row),
    })
}

// ---------------------------------------------------------------------------
// Prompt and interrupt
// ---------------------------------------------------------------------------

/// Encode a real `PromptRequestDto`, validated by the real decoder.
///
/// ## Why requests are validated rather than constructed
///
/// `PromptRequestDto`'s `text` field is `Required<PromptText>`, and
/// `PromptText` is NOT exported from `grapt_api::u3a` — it lives in that
/// crate's internal prelude. So an outside crate can DECODE a prompt request
/// but cannot CONSTRUCT one. That is a property of the exported surface, not a
/// shortcut taken here.
///
/// The honest response is to build the body and then prove it is a valid
/// request by round-tripping it through the REAL decoder before it leaves.
/// A key that drifted from the contract fails here, at the client, rather than
/// at a server that would answer 400 — which is the same guarantee
/// construction would have given.
pub fn encode_prompt(text: &str) -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::PromptRequestDto;
    let bytes = serde_json::json!({ "text": text }).to_string().into_bytes();
    let _validated: PromptRequestDto = decode_request(&bytes, limits()?)?;
    Ok(bytes)
}

/// Startup provenance is public presentation metadata, never permission authority.
pub fn encode_seat_startup(
    text: &str,
    id: &WireId<grapt_types::MessageId>,
) -> Result<Vec<u8>, WireError> {
    encode_submission(text, id, &[], super::SubmissionOrigin::SeatStartup)
}

pub fn encode_submission(
    text: &str,
    id: &WireId<grapt_types::MessageId>,
    files: &[grapt_api::u3a::PromptInputFileAttachmentDto],
    origin: super::SubmissionOrigin,
) -> Result<Vec<u8>, WireError> {
    let delivery = if origin == super::SubmissionOrigin::Composer {
        "steer"
    } else {
        "queue"
    };
    let mut value = serde_json::json!({"id":id,"text":text,"delivery":delivery});
    if origin.startup() {
        value["metadata"] = serde_json::json!({"origin":"seat-startup"});
    }
    if !files.is_empty() {
        value["files"] = serde_json::to_value(files).map_err(|error| WireError::Codec {
            reason: error.to_string(),
        })?;
    }
    let bytes = serde_json::to_vec(&value).map_err(|error| WireError::Codec {
        reason: error.to_string(),
    })?;
    if files.is_empty() {
        let _: grapt_api::u3a::PromptRequestDto = decode_request(&bytes, limits()?)?;
    } else {
        grapt_api::u3a::decode_image_prompt(&bytes)?;
    }
    Ok(bytes)
}

/// The input id a real `PromptRequestDto` carries, if any.
pub fn decode_prompt_id(bytes: &[u8]) -> Result<Option<String>, WireError> {
    use grapt_api::u3a::PromptRequestDto;
    let dto: PromptRequestDto = decode_request(bytes, limits()?)?;
    Ok(dto
        .id()
        .as_value()
        .map(|id| id.as_inner().as_str().to_owned()))
}

/// Decode a real `PromptRequestDto` and return its text.
pub fn decode_prompt(bytes: &[u8]) -> Result<String, WireError> {
    use grapt_api::u3a::PromptRequestDto;
    let dto: PromptRequestDto = decode_request(bytes, limits()?)?;
    Ok(dto.text().get().as_str().to_owned())
}

/// Encode a real `InterruptRequestDto`, validated by the real decoder.
pub fn encode_interrupt() -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::InterruptRequestDto;
    let bytes = b"{}".to_vec();
    let _validated: InterruptRequestDto = decode_request(&bytes, limits()?)?;
    Ok(bytes)
}

/// A stop fenced to one execution, so it cannot reach a later one.
pub fn encode_interrupt_execution(execution: &ExecutionWireId) -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::InterruptRequestDto;
    let body = serde_json::json!({"executionID": execution.spelled()});
    let bytes = serde_json::to_vec(&body).map_err(|error| WireError::Codec {
        reason: error.to_string(),
    })?;
    let _validated: InterruptRequestDto = decode_request(&bytes, limits()?)?;
    Ok(bytes)
}

/// Esc may resume admitted human input after the interrupted owner settles.
pub fn encode_interrupt_and_resume(
    execution: Option<&ExecutionWireId>,
) -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::InterruptRequestDto;
    let mut body = serde_json::json!({"resumePending":true});
    if let Some(execution) = execution {
        body["executionID"] = serde_json::json!(execution.spelled());
    }
    let bytes = serde_json::to_vec(&body).map_err(|error| WireError::Codec {
        reason: error.to_string(),
    })?;
    let _validated: InterruptRequestDto = decode_request(&bytes, limits()?)?;
    Ok(bytes)
}

/// Encode a fork without inventing unvalidated keys at the client call site.
pub fn encode_fork(before: Option<&WireId<grapt_types::MessageId>>) -> Result<Vec<u8>, WireError> {
    let value = match before {
        Some(before) => serde_json::json!({"beforeMessageID":before}),
        None => serde_json::json!({}),
    };
    let bytes = serde_json::to_vec(&value).map_err(|error| WireError::Codec {
        reason: error.to_string(),
    })?;
    let _: grapt_api::u3a::SessionForkRequestDto = decode_request(&bytes, limits()?)?;
    Ok(bytes)
}

/// Encode a real `CompactRequestDto`, validated by the real decoder. The
/// server mints the compaction id, so the body names none.
pub fn encode_compact() -> Result<Vec<u8>, WireError> {
    encode_compact_with_instructions(None)
}

pub fn encode_compact_with_instructions(instructions: Option<&str>) -> Result<Vec<u8>, WireError> {
    let mut body = serde_json::json!({});
    if let Some(instructions) = instructions {
        body["instructions"] = serde_json::json!(instructions);
    }
    let bytes = serde_json::to_vec(&body).map_err(|error| WireError::Codec {
        reason: error.to_string(),
    })?;
    decode_compact(&bytes)?;
    Ok(bytes)
}

/// Decode a real `CompactRequestDto`.
pub fn decode_compact(bytes: &[u8]) -> Result<(), WireError> {
    use grapt_api::u3a::CompactRequestDto;
    let _validated: CompactRequestDto = decode_request(bytes, limits()?)?;
    Ok(())
}

/// Decode a real `CompactResponseDto`: the compaction the server admitted.
pub fn decode_compacted(bytes: &[u8]) -> Result<(), WireError> {
    use grapt_api::u3a::CompactResponseDto;
    let _admitted: CompactResponseDto = decode_page(bytes, limits()?)?;
    Ok(())
}

/// Encode a real `SessionReconcileRequestDto`, validated by the real decoder.
pub fn encode_reconcile() -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::SessionReconcileRequestDto;
    let bytes = b"{}".to_vec();
    let _validated: SessionReconcileRequestDto = decode_request(&bytes, limits()?)?;
    Ok(bytes)
}

/// Decode a real `SessionReconcileResponseDto`: how many lost runs it settled.
pub fn decode_reconciled(bytes: &[u8]) -> Result<u64, WireError> {
    use grapt_api::u3a::SessionReconcileResponseDto;
    let reconciled: SessionReconcileResponseDto = decode_page(bytes, limits()?)?;
    serde_json::to_value(reconciled.data().get().reconciled().get())
        .map_err(|error| WireError::Codec {
            reason: error.to_string(),
        })?
        .as_u64()
        .ok_or_else(|| WireError::Invalid {
            what: "reconciled count",
            value: "count is not an unsigned integer".into(),
        })
}

/// Encode a real `SessionCreateRequestDto`, validated by the real decoder.
///
/// The request carries the whole selection — agent, model reference and the
/// project's directory as the `Location.Ref` — because those are the fields the
/// frozen record declares and because a session created without them is created
/// against whatever the server defaults to. The user would then be chatting to
/// an agent and a model they never chose, with a screen that says otherwise.
///
/// This function takes the selection by VALUE rather than accepting `None`s: a
/// caller with an incomplete selection is refused earlier, by
/// `Client::create_session` (in `grapt-client`), where the refusal can name what is
/// missing.
pub fn encode_session_create(
    agent: &AgentWireId,
    model: &ModelWireId,
    provider: &ProviderWireId,
    directory: &str,
) -> Result<Vec<u8>, WireError> {
    encode_session_create_choice(agent, model, provider, directory, None)
}

/// Carry the complete chosen model coordinate on initial session creation.
pub fn encode_session_create_choice(
    agent: &AgentWireId,
    model: &ModelWireId,
    provider: &ProviderWireId,
    directory: &str,
    variant: Option<&WireId<grapt_types::ModelVariantId>>,
) -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::SessionCreateRequestDto;
    let mut value = serde_json::json!({
        "agent": agent.spelled(),
        "model": { "id": model.spelled(), "providerID": provider.spelled() },
        "location": { "directory": directory },
    });
    if let Some(variant) = variant {
        value["model"]["variant"] = serde_json::json!(variant.as_inner().as_str());
    }
    let bytes = value.to_string().into_bytes();
    let _validated: SessionCreateRequestDto = decode_request(&bytes, limits()?)?;
    Ok(bytes)
}

/// Decode a real `SessionCreateRequestDto` and return the selection it carries.
///
/// Used by the contract double to prove the client actually SENT the selection
/// rather than merely holding one locally while creating a defaulted session.
pub fn decode_session_create(bytes: &[u8]) -> Result<SessionCreateSelection, WireError> {
    use grapt_api::u3a::SessionCreateRequestDto;
    let dto: SessionCreateRequestDto = decode_request(bytes, limits()?)?;
    Ok(SessionCreateSelection {
        agent: dto.agent().as_value().cloned(),
        variant: dto
            .model()
            .as_value()
            .and_then(|model| model.variant().as_present().cloned()),
        model: dto
            .model()
            .as_value()
            .map(|model| (model.id().get().clone(), model.provider_id().get().clone())),
        directory: dto
            .location()
            .as_value()
            .map(|location| location.directory().get().as_str().to_owned()),
    })
}

/// The selection a session-create request carries.
///
/// Every field is optional because every field is optional ON THE WIRE. This
/// client always sends all three, and that is exactly why the type keeps them
/// separable: a reader that collapsed them into "the selection" could not tell
/// a request that omitted the model from one that sent it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCreateSelection {
    pub agent: Option<AgentWireId>,
    pub model: Option<(ModelWireId, ProviderWireId)>,
    pub variant: Option<WireId<grapt_types::ModelVariantId>>,
    pub directory: Option<String>,
}

/// Encode the frozen rename request and validate it through the real codec.
pub fn encode_rename(title: &str) -> Result<Vec<u8>, WireError> {
    let bytes = serde_json::json!({"title":title}).to_string().into_bytes();
    decode_rename(&bytes)?;
    Ok(bytes)
}

pub fn decode_rename(bytes: &[u8]) -> Result<String, WireError> {
    let request: grapt_api::u3a::SessionRenameRequestDto = decode_request(bytes, limits()?)?;
    Ok(request.title().get().as_str().to_owned())
}

/// Validate the transient request against the same DTO the server accepts.
pub fn encode_side_question(prompt: &str) -> Result<Vec<u8>, WireError> {
    let bytes = serde_json::to_vec(&serde_json::json!({"prompt":prompt})).map_err(|error| {
        WireError::Codec {
            reason: error.to_string(),
        }
    })?;
    let _: grapt_api::u3a::SideQuestionRequestDto = decode_request(&bytes, limits()?)?;
    Ok(bytes)
}

pub fn decode_side_answer(bytes: &[u8]) -> Result<String, WireError> {
    let value: grapt_api::u3a::SideQuestionResponseDto = decode_page(bytes, limits()?)?;
    Ok(value.text().get().as_str().to_owned())
}
