//! The transcript: durable messages, tools, paging and the fold they feed.
//!
//! Split from `wire.rs` by SUBJECT when that file passed this workspace's
//! module ceiling. Every encode and decode still goes through the real codecs
//! under the real limits; nothing here was rewritten on the way out.

use super::*;
mod compaction;
pub use compaction::{Compaction, CompactionPhase};

mod switched;
mod tool_time;
pub use switched::model_switched_message;
mod footer;
pub use footer::AssistantFooter;

/// A cursor offers another page; only following it establishes completeness.
fn cursor_next(cursor: &grapt_api::u3a::SessionMessagesResponseDtoCursor) -> Option<String> {
    cursor
        .next()
        .as_value()
        .map(|value| value.as_str().to_owned())
}

// ---------------------------------------------------------------------------
// Message list - the durable re-list
// ---------------------------------------------------------------------------

/// One renderable item of an assistant message's ordered content.
///
/// The contract's `SessionMessageAssistantDtoContentItem` also carries `Tool`,
/// which this client does not render yet. It is COUNTED rather than skipped
/// silently, so a transcript missing it cannot claim to be complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageItem {
    Text(String),
    Reasoning(String),
    /// A tool call, IN ITS PLACE in the content sequence.
    ///
    /// It is a member of this enum rather than a parallel list because the
    /// contract carries ONE ordered content array. Decoding tools into a
    /// separate vector and rendering them after the text reordered the
    /// transcript: `[text, tool, text]` came out as `[text, text, tool]`, so a
    /// reader saw an answer that referred to work appearing below it.
    Tool(DurableTool),
}

/// How an assistant message finished, as the contract records it durably.
///
/// This is the DURABLE terminal. The live stream carries
/// `session.execution.*` events; the re-list carries this field. A client that
/// read only the live ones would lose terminal state on every reconnect, which
/// is exactly the kind of hole reconciliation exists to close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableFinish {
    Stop,
    Error,
    /// `tool-calls` — the step ended by CALLING A TOOL, so the turn continues.
    ///
    /// Kept apart from `Other` because it is not a terminal at all. The
    /// producer emits it for `AssertedContinue` (`dogfood/mod.rs:635`), and
    /// folding it in with the rest made a mid-turn record settle the execution
    /// as SUCCEEDED — observed on a real screen with an edit permission
    /// pending, `tool edit running` visible, and the footer reading `durable
    /// succeeded`.
    ToolCalls,
    /// `unknown` — the producer has stated NO terminal for this message.
    ///
    /// Absence of information, not success. The producer emits it whenever a
    /// message has no step terminal yet (`dogfood/mod.rs`, the `None` arm), so
    /// EVERY in-flight assistant message carries it — and folding it in with
    /// the rest settled a turn that had not finished.
    Unknown,
    /// Every other closed member of the contract's finish enum.
    Other,
}

/// One durable structured error, carried verbatim.
///
/// `type` is a FREE-FORM `DtoString` on the contract, not a closed enum, so it
/// is kept as the string it is rather than folded into a vocabulary this client
/// would have to invent. Interpretation happens at one place in the projection;
/// this type only transports what the record said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableError {
    pub kind: String,
    pub message: String,
}

/// One durable tool call, as the re-list carries it.
///
/// History owns final state; live progress cannot settle or reopen a tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableTool {
    /// Reported run-to-completion time; absent means unknown.
    pub duration_ms: Option<u64>,
    /// The call's own identity. Reconnect deduplication is by this id, so a
    /// re-listed page that repeats a call updates it instead of appending a
    /// second row for the same invocation.
    pub id: ToolCallWireId,
    pub name: String,
    pub state: DurableToolState,
    pub progress: Vec<grapt_api::u3a::ToolProgressEntry>,
}

/// Where a tool call had got to when the record was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DurableToolState {
    /// Arguments still arriving; `input` is a partial string, not an object.
    Streaming {
        input: String,
    },
    Running {
        input: String,
    },
    Completed {
        input: String,
        output: String,
    },
    Error {
        input: String,
        error: DurableError,
        output: Option<String>,
    },
}

/// One durable message, as the re-list carries it.
#[derive(Debug, Clone, PartialEq)]
pub enum DurableMessage {
    Compaction {
        info: Compaction,
        text: String,
    },
    User {
        text: String,
    },
    SeatStartup {
        text: String,
    },
    /// A product-recorded synthetic input, never a human editor draft.
    Automated {
        text: String,
    },
    /// A `system` row, which carries the PRODUCT'S OWN explanation of what
    /// happened — including why an execution failed.
    ///
    /// Rendered rather than counted. Omitting it cost a user the sentence
    /// "Execution failed: The execution stopped before completing. Inspect the
    /// Session before retrying." at exactly the moment they needed it, which is
    /// the actionable error this client owes.
    System {
        text: String,
    },
    /// A System row explicitly tagged as an execution failure by the service.
    ExecutionFailure {
        text: String,
    },
    /// A known-valid member of the frozen union whose PRESENTATION this client
    /// does not implement yet.
    ///
    /// It keeps its identity — the union tag — so the transcript can say what
    /// is there rather than leave a hole. This is NOT corrupt history: the row
    /// decoded, and the client simply cannot draw it. Those are different
    /// facts, and conflating them once disabled live streaming outright.
    Unsupported {
        kind: &'static str,
    },
    Assistant {
        items: Vec<MessageItem>,
        finish: Option<DurableFinish>,
        /// The durable structured error, when the record carries one.
        ///
        /// Decoded rather than dropped because it is the ONLY place durable
        /// truth can say an execution was interrupted: `finish` carries the
        /// PROVIDER finish vocabulary, which has no interrupted member and is
        /// not going to grow one, since an interrupt is not a provider finish.
        /// A client that read only `finish` reported every interrupted turn as
        /// a failure the moment it reconciled.
        error: Option<DurableError>,
        /// Token accounting, when the durable record carries it.
        ///
        /// `tokens` is OPTIONAL on the frozen assistant message, and an
        /// interrupted turn really does have none. Absent stays absent: a
        /// rebuilt transcript that showed `in 0 out 0` for a turn nobody
        /// measured would be inventing an observation.
        usage: Option<(u64, u64)>,
        cache: Box<crate::cache::CacheUsage>,
        footer: Option<Box<AssistantFooter>>,
    },
}

/// Build one real user message.
pub fn user_message(
    id: &str,
    text: &str,
) -> Result<grapt_api::u3a::SessionMessageInfoDto, WireError> {
    use grapt_api::u3a::{
        SessionMessageInfoDto, SessionMessageUserDto, SessionMessageUserDtoTime,
        SessionMessageUserDtoType,
    };
    let message_id = WireId::parse(id.to_owned()).ok_or_else(|| WireError::Invalid {
        what: "message id",
        value: id.to_owned(),
    })?;
    let time = SessionMessageUserDtoTime::new(Required::new(finite(1_700_000_000_000.0)?));
    Ok(SessionMessageInfoDto::User(Box::new(
        SessionMessageUserDto::new(
            Required::new(SessionMessageUserDtoType::User),
            Required::new(message_id),
            Optional::absent(),
            Required::new(time),
            Required::new(dto_string(text)?),
            Optional::absent(),
            Optional::absent(),
        ),
    )))
}

/// Read one tool call into the shape the transcript renders.
///
/// The arguments are kept as the JSON the record carried rather than being
/// reformatted: they are what the user was asked to approve, and a client that
/// paraphrased them would be showing something other than the thing that ran.
fn decode_tool(
    tool: &grapt_api::u3a::SessionMessageAssistantToolDto,
) -> Result<DurableTool, WireError> {
    use grapt_api::u3a::SessionMessageAssistantToolDtoState as S;

    // Use the DTO's serializer, as the wire and `form_fields` do.
    // Serialization failure must not become `{}`: that would falsely claim
    // the user approved a call without arguments.
    let open = |value: &grapt_api::u3a::ToolInputObject| -> Result<String, WireError> {
        serde_json::to_string(value).map_err(|_| WireError::Invalid {
            what: "tool arguments",
            value: "unserialisable".to_owned(),
        })
    };
    let content = |items: &[grapt_api::u3a::ToolContentDto]| {
        let mut out = String::new();
        for item in items {
            if !out.is_empty() {
                out.push('\n');
            }
            match item {
                grapt_api::u3a::ToolContentDto::TextContent(text) => {
                    out.push_str(text.text().get().as_str())
                }
                grapt_api::u3a::ToolContentDto::FileContent(file) => {
                    let safe = |text: &str| {
                        text.chars()
                            .take(96)
                            .filter(|c| c.is_alphanumeric() || " ._-+/()".contains(*c))
                            .collect::<String>()
                    };
                    out.push_str("attachment ");
                    out.push_str(&safe(file.mime().get().as_str()));
                    if let Some(name) = file.name().as_value() {
                        let name = name.as_str().rsplit(['/', '\\']).next().unwrap_or_default();
                        out.push(' ');
                        out.push_str(&safe(name));
                    }
                }
            }
        }
        out
    };

    let state = match tool.state().get() {
        S::Streaming(streaming) => DurableToolState::Streaming {
            input: streaming.input().get().as_str().to_owned(),
        },
        S::Running(running) => DurableToolState::Running {
            input: open(running.input().get())?,
        },
        S::Completed(completed) => DurableToolState::Completed {
            input: open(completed.input().get())?,
            output: content(completed.content().get().as_slice()),
        },
        S::Error(error) => DurableToolState::Error {
            input: open(error.input().get())?,
            error: DurableError {
                kind: error.error().get().kind().get().as_str().to_owned(),
                message: error.error().get().message().get().as_str().to_owned(),
            },
            output: error
                .content()
                .as_present()
                .map(|items| content(items.as_slice())),
        },
    };
    let metadata = match tool.state().get() {
        S::Streaming(_) => None,
        S::Running(value) => Some(value.metadata().get()),
        S::Completed(value) => value.metadata().as_present(),
        S::Error(value) => value.metadata().as_present(),
    };
    let progress = metadata
        .map(serde_json::to_value)
        .transpose()
        .map_err(|_| WireError::Invalid {
            what: "tool progress",
            value: "unserialisable".to_owned(),
        })?
        .and_then(|mut value| value.get_mut("toolCalls").map(serde_json::Value::take))
        .map(|calls| {
            serde_json::from_value::<grapt_api::u3a::ToolProgressMetadata>(
                serde_json::json!({"toolCalls": calls}),
            )
        })
        .transpose()
        .map_err(|_| WireError::Invalid {
            what: "tool progress",
            value: "malformed or oversized".to_owned(),
        })?
        .map(|value| value.calls().to_vec())
        .unwrap_or_default();
    Ok(DurableTool {
        duration_ms: tool_time::decode(tool.time().get())?,
        id: tool.id().get().clone(),
        name: tool.name().get().as_str().to_owned(),
        state,
        progress,
    })
}

/// Build one tool content item from the shape the transcript holds.
///
/// The inverse of `decode_tool`, so a witness can encode MIXED content —
/// `[text, tool, text]` — through the real codec and prove the order survives.
/// Without it the only tool a test could build was a message containing nothing
/// else, which is exactly the shape that hid the reordering defect.
fn tool_content_item(
    tool: &DurableTool,
) -> Result<grapt_api::u3a::SessionMessageAssistantDtoContentItem, WireError> {
    use grapt_api::u3a::{
        SessionMessageAssistantDtoContentItem, SessionMessageAssistantToolDto,
        SessionMessageAssistantToolDtoType,
    };
    let state = tool_state_dto(&tool.state)?;
    Ok(SessionMessageAssistantDtoContentItem::Tool(Box::new(
        SessionMessageAssistantToolDto::new(
            Required::new(SessionMessageAssistantToolDtoType::Tool),
            Required::new(tool.id.clone()),
            Required::new(dto_string(&tool.name)?),
            Optional::absent(),
            Optional::absent(),
            Optional::absent(),
            Required::new(state),
            Required::new(tool_time::encode(tool.duration_ms)?),
        ),
    )))
}

fn tool_state_dto(
    state: &DurableToolState,
) -> Result<grapt_api::u3a::SessionMessageAssistantToolDtoState, WireError> {
    use grapt_api::u3a::{
        OpenObject, SessionMessageAssistantToolDtoState, SessionMessageToolStateCompletedDto,
        SessionMessageToolStateCompletedDtoStatus, SessionMessageToolStateErrorDto,
        SessionMessageToolStateErrorDtoStatus, SessionMessageToolStateRunningDto,
        SessionMessageToolStateRunningDtoStatus, SessionMessageToolStateStreamingDto,
        SessionMessageToolStateStreamingDtoStatus, SessionStructuredErrorDto, ToolContentDto,
        ToolTextContentDto, ToolTextContentDtoType,
    };
    let object = |text: &str| -> Result<grapt_api::u3a::ToolInputObject, WireError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|_| WireError::Invalid {
                what: "tool input",
                value: text.to_owned(),
            })?;
        serde_json::from_value(value).map_err(|_| WireError::Invalid {
            what: "tool input",
            value: text.to_owned(),
        })
    };
    let empty = || -> Result<OpenObject, WireError> {
        serde_json::from_slice(b"{}").map_err(|error| WireError::Invalid {
            what: "open object",
            value: error.to_string(),
        })
    };
    let text_content = |text: &str| -> Result<BoundedVec<ToolContentDto, 256>, WireError> {
        BoundedVec::try_new(vec![ToolContentDto::TextContent(Box::new(
            ToolTextContentDto::new(
                Required::new(ToolTextContentDtoType::Text),
                Required::new(dto_string(text)?),
            ),
        ))])
        .map_err(|reason| WireError::Invalid {
            what: "tool content",
            value: reason.to_owned(),
        })
    };
    Ok(match state {
        DurableToolState::Streaming { input } => SessionMessageAssistantToolDtoState::Streaming(
            Box::new(SessionMessageToolStateStreamingDto::new(
                Required::new(SessionMessageToolStateStreamingDtoStatus::Streaming),
                Required::new(dto_string(input)?),
            )),
        ),
        DurableToolState::Running { input } => SessionMessageAssistantToolDtoState::Running(
            Box::new(SessionMessageToolStateRunningDto::new(
                Required::new(SessionMessageToolStateRunningDtoStatus::Running),
                Required::new(object(input)?),
                Required::new(empty()?),
            )),
        ),
        DurableToolState::Completed { input, output } => {
            SessionMessageAssistantToolDtoState::Completed(Box::new(
                SessionMessageToolStateCompletedDto::new(
                    Required::new(SessionMessageToolStateCompletedDtoStatus::Completed),
                    Required::new(object(input)?),
                    Required::new(text_content(output)?),
                    Optional::absent(),
                ),
            ))
        }
        DurableToolState::Error {
            input,
            error,
            output,
        } => SessionMessageAssistantToolDtoState::Error(Box::new(
            SessionMessageToolStateErrorDto::new(
                Required::new(SessionMessageToolStateErrorDtoStatus::Error),
                Required::new(object(input)?),
                Required::new(SessionStructuredErrorDto::new(
                    Required::new(dto_string(&error.kind)?),
                    Required::new(dto_string(&error.message)?),
                )),
                match output {
                    Some(output) => Optional::present(text_content(output)?),
                    None => Optional::absent(),
                },
                Optional::absent(),
            ),
        )),
    })
}

/// Which model a session's subagents run as, as the wire answers it.
///
/// `Some(row)` is an explicit selection. `None` is the contract's NULL, which
/// means subagents INHERIT the agent's model — a real answer, not a missing
/// one. A client that rendered both as blank would be reporting "nobody said"
/// where the product said "the default".
pub fn decode_subagent_model(bytes: &[u8]) -> Result<Option<ModelRow>, WireError> {
    use grapt_api::u3a::SessionSubagentModelResponseDto;
    let dto: SessionSubagentModelResponseDto = decode_page(bytes, limits()?)?;
    Ok(dto.data().as_value().map(super::catalogs::model_ref_row))
}

/// Build one real assistant message carrying ordered content.
#[expect(clippy::too_many_lines, reason = "legacy")]
pub fn assistant_message(
    id: &str,
    items: &[MessageItem],
    finish: Option<DurableFinish>,
    usage: Option<(u64, u64)>,
    error: Option<DurableError>,
) -> Result<grapt_api::u3a::SessionMessageInfoDto, WireError> {
    use grapt_api::u3a::{
        ModelRefDto, SessionMessageAssistantDto, SessionMessageAssistantDtoContentItem,
        SessionMessageAssistantDtoTime, SessionMessageAssistantDtoType,
        SessionMessageAssistantReasoningDto, SessionMessageAssistantReasoningDtoTime,
        SessionMessageAssistantReasoningDtoType, SessionMessageAssistantTextDto,
        SessionMessageAssistantTextDtoType, SessionMessageInfoDto, TokenUsageInfoDto,
        TokenUsageInfoDtoCache,
    };
    // Durable token accounting, when the producer measured any. `None` stays
    // absent rather than becoming a zero block: an interrupted turn has no
    // usage, and a record that said `in 0 out 0` would be reporting a
    // measurement that was never taken.
    let tokens = match usage {
        Some((input, output)) => Optional::present(TokenUsageInfoDto::new(
            Required::new(finite(input as f64)?),
            Required::new(finite(output as f64)?),
            Required::new(finite(0.0)?),
            Required::new(TokenUsageInfoDtoCache::new(
                Required::new(finite(0.0)?),
                Required::new(finite(0.0)?),
            )),
        )),
        None => Optional::absent(),
    };
    let message_id = WireId::parse(id.to_owned()).ok_or_else(|| WireError::Invalid {
        what: "message id",
        value: id.to_owned(),
    })?;
    let agent = WireId::parse("agent_double".to_owned()).ok_or_else(|| WireError::Invalid {
        what: "agent id",
        value: "agent_double".to_owned(),
    })?;
    let model_id = WireId::parse("m1".to_owned()).ok_or_else(|| WireError::Invalid {
        what: "model id",
        value: "m1".to_owned(),
    })?;
    let provider_id = WireId::parse("p1".to_owned()).ok_or_else(|| WireError::Invalid {
        what: "provider id",
        value: "p1".to_owned(),
    })?;
    let model = ModelRefDto::new(
        Required::new(model_id),
        Required::new(provider_id),
        Optional::absent(),
    );
    let mut content = Vec::new();
    for item in items {
        if let MessageItem::Tool(tool) = item {
            content.push(tool_content_item(tool)?);
            continue;
        }
        content.push(match item {
            MessageItem::Text(text) => SessionMessageAssistantDtoContentItem::Text(Box::new(
                SessionMessageAssistantTextDto::new(
                    Required::new(SessionMessageAssistantTextDtoType::Text),
                    Required::new(dto_string(text)?),
                    Optional::absent(),
                ),
            )),
            // Handled above; this arm cannot be reached.
            MessageItem::Tool(_) => continue,
            MessageItem::Reasoning(text) => SessionMessageAssistantDtoContentItem::Reasoning(
                Box::new(SessionMessageAssistantReasoningDto::new(
                    Required::new(SessionMessageAssistantReasoningDtoType::Reasoning),
                    Required::new(dto_string(text)?),
                    Optional::absent(),
                    Optional::present(SessionMessageAssistantReasoningDtoTime::new(
                        Required::new(finite(1_700_000_000_000.0)?),
                        Optional::absent(),
                    )),
                )),
            ),
        });
    }
    let content = BoundedVec::try_new(content).map_err(|reason| WireError::Invalid {
        what: "assistant content",
        value: reason.to_owned(),
    })?;
    let time = SessionMessageAssistantDtoTime::new(
        Required::new(finite(1_700_000_000_000.0)?),
        Optional::absent(),
    );
    let finish = match finish {
        None => Optional::absent(),
        Some(DurableFinish::Stop) => {
            Optional::present(grapt_api::u3a::SessionMessageAssistantDtoFinish::Stop)
        }
        Some(DurableFinish::Error) => {
            Optional::present(grapt_api::u3a::SessionMessageAssistantDtoFinish::Error)
        }
        Some(DurableFinish::ToolCalls) => {
            Optional::present(grapt_api::u3a::SessionMessageAssistantDtoFinish::ToolCalls)
        }
        Some(DurableFinish::Unknown) => {
            Optional::present(grapt_api::u3a::SessionMessageAssistantDtoFinish::Unknown)
        }
        Some(DurableFinish::Other) => {
            Optional::present(grapt_api::u3a::SessionMessageAssistantDtoFinish::Unknown)
        }
    };
    let error = match error {
        Some(error) => Optional::present(grapt_api::u3a::SessionStructuredErrorDto::new(
            Required::new(dto_string(&error.kind)?),
            Required::new(dto_string(&error.message)?),
        )),
        None => Optional::absent(),
    };
    Ok(SessionMessageInfoDto::Assistant(Box::new(
        SessionMessageAssistantDto::new(
            Required::new(SessionMessageAssistantDtoType::Assistant),
            Required::new(message_id),
            Optional::absent(),
            Required::new(time),
            Required::new(agent),
            Required::new(model),
            Required::new(content),
            Optional::absent(),
            finish,
            Optional::absent(),
            tokens,
            // `observedUsage` is the provider's own partial accounting. This
            // encoder reports only what the durable record measured, so the
            // observation stays absent rather than echoing `tokens` back as if
            // the provider had confirmed it.
            Optional::absent(),
            error,
            // No retry: a message this encoder builds was produced once.
            Optional::absent(),
        ),
    )))
}

/// What a built tool row should say it was doing.
///
/// The double needs every arm the contract has, or the client's rendering of an
/// errored or completed call would have no witness at all.
#[derive(Debug, Clone)]
pub enum ToolStateDouble {
    Running,
    Completed {
        input: String,
        output: String,
    },
    Error {
        input: String,
        kind: String,
        message: String,
    },
}

pub fn assistant_tool_message(
    id: &str,
    name: &str,
) -> Result<grapt_api::u3a::SessionMessageInfoDto, WireError> {
    assistant_tool_message_in(id, name, "call_000000000001", ToolStateDouble::Running)
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub fn assistant_tool_message_in(
    id: &str,
    name: &str,
    call_id: &str,
    state: ToolStateDouble,
) -> Result<grapt_api::u3a::SessionMessageInfoDto, WireError> {
    use grapt_api::u3a::{
        ModelRefDto, OpenObject, SessionMessageAssistantDto, SessionMessageAssistantDtoContentItem,
        SessionMessageAssistantDtoTime, SessionMessageAssistantDtoType,
        SessionMessageAssistantToolDto, SessionMessageAssistantToolDtoState,
        SessionMessageAssistantToolDtoTime, SessionMessageAssistantToolDtoType,
        SessionMessageInfoDto, SessionMessageToolStateRunningDto,
        SessionMessageToolStateRunningDtoStatus,
    };
    // `OpenObject` has no public constructor, so an empty one is DECODED
    // through its own `Deserialize` — the same route the wire takes. A parallel
    // constructor would be a second way to build a contract value.
    let empty = || -> Result<OpenObject, WireError> {
        serde_json::from_slice(b"{}").map_err(|error| WireError::Invalid {
            what: "open object",
            value: error.to_string(),
        })
    };
    let message_id = WireId::parse(id.to_owned()).ok_or_else(|| WireError::Invalid {
        what: "message id",
        value: id.to_owned(),
    })?;
    let agent = WireId::parse("agent_double".to_owned()).ok_or_else(|| WireError::Invalid {
        what: "agent id",
        value: "agent_double".to_owned(),
    })?;
    let model_id = WireId::parse("m1".to_owned()).ok_or_else(|| WireError::Invalid {
        what: "model id",
        value: "m1".to_owned(),
    })?;
    let provider_id = WireId::parse("p1".to_owned()).ok_or_else(|| WireError::Invalid {
        what: "provider id",
        value: "p1".to_owned(),
    })?;
    let call = WireId::parse(call_id.to_owned()).ok_or_else(|| WireError::Invalid {
        what: "tool call id",
        value: call_id.to_owned(),
    })?;
    let object = |text: &str| -> Result<grapt_api::u3a::ToolInputObject, WireError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|_| WireError::Invalid {
                what: "tool input",
                value: text.to_owned(),
            })?;
        serde_json::from_value(value).map_err(|_| WireError::Invalid {
            what: "tool input",
            value: text.to_owned(),
        })
    };
    let state = match &state {
        ToolStateDouble::Running => SessionMessageAssistantToolDtoState::Running(Box::new(
            SessionMessageToolStateRunningDto::new(
                Required::new(SessionMessageToolStateRunningDtoStatus::Running),
                Required::new(object("{}")?),
                Required::new(empty()?),
            ),
        )),
        ToolStateDouble::Completed { input, output } => {
            use grapt_api::u3a::{
                SessionMessageToolStateCompletedDto, SessionMessageToolStateCompletedDtoStatus,
                ToolContentDto, ToolTextContentDto, ToolTextContentDtoType,
            };
            let content = BoundedVec::try_new(vec![ToolContentDto::TextContent(Box::new(
                ToolTextContentDto::new(
                    Required::new(ToolTextContentDtoType::Text),
                    Required::new(dto_string(output)?),
                ),
            ))])
            .map_err(|reason| WireError::Invalid {
                what: "tool content",
                value: reason.to_owned(),
            })?;
            SessionMessageAssistantToolDtoState::Completed(Box::new(
                SessionMessageToolStateCompletedDto::new(
                    Required::new(SessionMessageToolStateCompletedDtoStatus::Completed),
                    Required::new(object(input)?),
                    Required::new(content),
                    Optional::absent(),
                ),
            ))
        }
        ToolStateDouble::Error {
            input,
            kind,
            message,
        } => {
            use grapt_api::u3a::{
                SessionMessageToolStateErrorDto, SessionMessageToolStateErrorDtoStatus,
                SessionStructuredErrorDto,
            };
            SessionMessageAssistantToolDtoState::Error(Box::new(
                SessionMessageToolStateErrorDto::new(
                    Required::new(SessionMessageToolStateErrorDtoStatus::Error),
                    Required::new(object(input)?),
                    Required::new(SessionStructuredErrorDto::new(
                        Required::new(dto_string(kind)?),
                        Required::new(dto_string(message)?),
                    )),
                    Optional::absent(),
                    Optional::absent(),
                ),
            ))
        }
    };
    let tool = SessionMessageAssistantToolDto::new(
        Required::new(SessionMessageAssistantToolDtoType::Tool),
        Required::new(call),
        Required::new(dto_string(name)?),
        Optional::absent(),
        Optional::absent(),
        Optional::absent(),
        Required::new(state),
        Required::new(SessionMessageAssistantToolDtoTime::new(
            Required::new(finite(1_700_000_000_000.0)?),
            Optional::absent(),
            Optional::absent(),
        )),
    );
    let content = BoundedVec::try_new(vec![SessionMessageAssistantDtoContentItem::Tool(Box::new(
        tool,
    ))])
    .map_err(|reason| WireError::Invalid {
        what: "assistant content",
        value: reason.to_owned(),
    })?;
    Ok(SessionMessageInfoDto::Assistant(Box::new(
        SessionMessageAssistantDto::new(
            Required::new(SessionMessageAssistantDtoType::Assistant),
            Required::new(message_id),
            Optional::absent(),
            Required::new(SessionMessageAssistantDtoTime::new(
                Required::new(finite(1_700_000_000_000.0)?),
                Optional::absent(),
            )),
            Required::new(agent),
            Required::new(ModelRefDto::new(
                Required::new(model_id),
                Required::new(provider_id),
                Optional::absent(),
            )),
            Required::new(content),
            Optional::absent(),
            Optional::absent(),
            Optional::absent(),
            Optional::absent(),
            Optional::absent(),
            Optional::absent(),
            Optional::absent(),
        ),
    )))
}

pub fn encode_messages(
    rows: Vec<grapt_api::u3a::SessionMessageInfoDto>,
    next: Option<&str>,
) -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::{SessionMessagesResponseDto, SessionMessagesResponseDtoCursor};
    let data = BoundedVec::try_new(rows).map_err(|reason| WireError::Invalid {
        what: "message page",
        value: reason.to_owned(),
    })?;
    // The cursor is an OFFER to page, not a claim that there is more. The real
    // product offers one on every first page, including a page that carried the
    // whole history, so a producer that only offered one when it had truly
    // withheld something would be teaching a client to read the cursor in a way
    // the product contradicts.
    let next = match next {
        Some(cursor) => OptionalNullable::value(dto_string(cursor)?),
        None => OptionalNullable::absent(),
    };
    let cursor = SessionMessagesResponseDtoCursor::new(OptionalNullable::absent(), next);
    let dto = SessionMessagesResponseDto::new(Required::new(data), Required::new(cursor));
    Ok(encode_page(&dto, limits()?)?)
}

/// A durable row's identity. Every member of the frozen union carries one,
/// and a member added later is a compile error here.
pub fn row_id(row: &grapt_api::u3a::SessionMessageInfoDto) -> MessageWireId {
    use grapt_api::u3a::{SessionMessageCompactionDto as C, SessionMessageInfoDto as Row};
    match row {
        Row::AgentSelected(row) => row.id().get().clone(),
        Row::ModelSelected(row) => row.id().get().clone(),
        Row::User(row) => row.id().get().clone(),
        Row::Synthetic(row) => row.id().get().clone(),
        Row::System(row) => row.id().get().clone(),
        Row::Skill(row) => row.id().get().clone(),
        Row::Shell(row) => row.id().get().clone(),
        Row::Assistant(row) => row.id().get().clone(),
        Row::Compaction(row) => match row.as_ref() {
            C::Running(row) => row.id().get().clone(),
            C::Completed(row) => row.id().get().clone(),
            C::Failed(row) => row.id().get().clone(),
        },
    }
}

/// The frozen union tag for a durable message row.
///
/// Spelled out rather than derived, so an added member is a COMPILE error here
/// and gets a deliberate decision about presentation instead of silently
/// becoming another anonymous placeholder.
fn union_tag(row: &grapt_api::u3a::SessionMessageInfoDto) -> &'static str {
    use grapt_api::u3a::SessionMessageInfoDto as Row;
    match row {
        Row::AgentSelected(_) => "agent-switched",
        Row::ModelSelected(_) => "model-switched",
        Row::User(_) => "user",
        Row::Synthetic(_) => "synthetic",
        Row::System(_) => "system",
        Row::Skill(_) => "skill",
        Row::Shell(_) => "shell",
        Row::Assistant(_) => "assistant",
        Row::Compaction(_) => "compaction",
    }
}

/// Decode a real `SessionMessagesResponseDto`.
#[expect(clippy::too_many_lines, reason = "legacy")]
pub fn decode_messages(bytes: &[u8]) -> Result<MessagePage, WireError> {
    use grapt_api::u3a::{
        SessionMessageAssistantDtoContentItem, SessionMessageInfoDto, SessionMessagesResponseDto,
    };
    let dto: SessionMessagesResponseDto = decode_page(bytes, limits()?)?;
    let next = cursor_next(dto.cursor().get());
    let previous = dto
        .cursor()
        .get()
        .previous()
        .as_value()
        .map(|value| value.as_str().to_owned());
    let mut messages = Vec::new();
    let mut unsupported = 0usize;
    let rows = dto.data().get().as_slice().len();
    let mut ids = Vec::new();
    for row in dto.data().get().as_slice() {
        ids.push(row_id(row));
        match row {
            SessionMessageInfoDto::User(user) => {
                let text = user.text().get().as_str().to_owned();
                messages.push(
                    if super::submission::is_seat_startup(user.metadata().as_present())? {
                        DurableMessage::SeatStartup { text }
                    } else {
                        DurableMessage::User { text }
                    },
                );
            }
            SessionMessageInfoDto::Synthetic(prompt) => messages.push(DurableMessage::Automated {
                text: prompt.text().get().as_str().to_owned(),
            }),
            SessionMessageInfoDto::Assistant(assistant) => {
                let mut items = Vec::new();
                for item in assistant.content().get().as_slice() {
                    match item {
                        SessionMessageAssistantDtoContentItem::Text(text) => {
                            items.push(MessageItem::Text(text.text().get().as_str().to_owned()))
                        }
                        SessionMessageAssistantDtoContentItem::Reasoning(reasoning) => items.push(
                            MessageItem::Reasoning(reasoning.text().get().as_str().to_owned()),
                        ),
                        SessionMessageAssistantDtoContentItem::Tool(tool) => {
                            items.push(MessageItem::Tool(decode_tool(tool)?));
                        }
                    }
                }
                let finish = assistant.finish().as_present().map(|value| {
                    use grapt_api::u3a::SessionMessageAssistantDtoFinish as F;
                    match value {
                        F::Stop => DurableFinish::Stop,
                        F::Error => DurableFinish::Error,
                        F::ToolCalls => DurableFinish::ToolCalls,
                        F::Unknown => DurableFinish::Unknown,
                        _ => DurableFinish::Other,
                    }
                });
                // Durable usage. The live `session.usage.updated` frames are
                // not in the re-list, so a rebuilt projection that ignored this
                // reported `usage -` for a turn whose cost the record knew
                // exactly — observed against the real product, which returned
                // 157 input and 10 output tokens for a turn the reopened screen
                // then described as unmeasured.
                let usage = assistant.tokens().as_present().and_then(|tokens| {
                    // `FiniteF64` exposes no accessor, so its value is read back
                    // through its own `Serialize` — the same route the wire
                    // takes, and the same one `form_fields` uses for an open
                    // object. A parallel reader would be a second way to
                    // interpret a contract value.
                    let count = |value: &grapt_api::u3a::FiniteF64| -> Option<u64> {
                        let number = serde_json::to_value(value).ok()?.as_f64()?;
                        // The frozen field is a finite number, not an integer.
                        // A negative or fractional count is a value this client
                        // cannot render as tokens, and rounding one into a
                        // plausible number would be inventing an observation.
                        (number >= 0.0 && number.fract() == 0.0).then_some(number as u64)
                    };
                    Some((count(tokens.input().get())?, count(tokens.output().get())?))
                });
                // The durable structured error, read as the two strings it is.
                // `type` is a free-form `DtoString` on the contract, so it is
                // transported verbatim and interpreted in ONE place, in the
                // projection, rather than folded into a vocabulary here.
                let error = assistant.error().as_present().map(|error| DurableError {
                    kind: error.kind().get().as_str().to_owned(),
                    message: error.message().get().as_str().to_owned(),
                });
                let cache = cache::assistant(assistant)?;
                messages.push(DurableMessage::Assistant {
                    items,
                    finish,
                    usage,
                    cache: Box::new(cache),
                    footer: Some(Box::new(footer::decode(assistant, usage, cache))),
                    error,
                });
            }
            // `system` carries the product's own account of what happened —
            // the failure explanation a user needs most — so it is RENDERED.
            SessionMessageInfoDto::System(system) => {
                let text = system.text().get().as_str().to_owned();
                let metadata =
                    serde_json::to_value(system.metadata().as_present()).map_err(|error| {
                        WireError::Codec {
                            reason: error.to_string(),
                        }
                    })?;
                messages.push(
                    if metadata
                        .get("executionFailure")
                        .and_then(serde_json::Value::as_bool)
                        == Some(true)
                    {
                        DurableMessage::ExecutionFailure { text }
                    } else {
                        DurableMessage::System { text }
                    },
                );
            }
            SessionMessageInfoDto::Compaction(row) => {
                messages.push(compaction::decode(row.as_ref())?);
            }
            // Every other member of the frozen union decodes but has no
            // presentation here yet. It keeps its IDENTITY so the transcript can
            // name what it is holding, and it is counted so the screen can say
            // how much it is not drawing. It is NOT evidence of missing
            // history: the row was read.
            other => {
                unsupported += 1;
                messages.push(DurableMessage::Unsupported {
                    kind: union_tag(other),
                });
            }
        }
    }
    Ok(MessagePage {
        previous,
        next,
        messages,
        rows,
        ids,
        unsupported,
    })
}
