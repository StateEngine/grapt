//! The MCP protocol domain: bounded JSON-RPC framing, a negotiated initialize
//! handshake, and validated tool discovery and invocation.
//!
//! This crate is deliberately pure. It owns no transport, no child process, no
//! socket, no clock and no permission authority. It turns bytes into validated
//! protocol facts and validated protocol facts into bytes, and it refuses
//! anything outside its declared bounds. Everything that can block, spawn,
//! wait, or decide what a user is allowed to do lives above it, with the owners
//! that already hold those authorities.
//!
//! That split is what keeps the transport honest. A server is remote code: it
//! can answer late, answer twice, answer with a different id, claim a protocol
//! revision this client has never seen, advertise ten thousand tools with
//! megabyte schemas, or return a megabyte of content from a one-line call.
//! Every one of those is a refusal here, decided against a
//! declared bound, before any of it can reach a model prompt or a tool
//! registry.
//!
//! Absence is preserved. A server that reports no tools is a server with no
//! tools; a server whose reply could not be understood is a refusal. Neither
//! becomes an empty catalog by default.

pub mod content;
pub mod discovery;
pub mod elicitation;
pub mod incoming;
mod reply;
pub mod session;

pub use reply::{decode_reply, ServerErrorData};
use serde_json::{json, Map, Value};
use thiserror::Error;

/// The protocol revision this client OFFERS in `initialize`.
///
/// It is the newest revision whose message shapes this crate actually
/// implements, not the newest revision that exists. Offering a revision we do
/// not implement would be a claim we cannot honour, and it buys nothing: the
/// specification requires a server that does not support the offered revision
/// to answer with one it does support, so an older offer never locks us out of
/// a newer server. It only tells the truth about what we speak.
pub const PROTOCOL_VERSION: &str = "2025-11-25";

/// The revisions this client will ACCEPT a server answering with, newest first.
///
/// Negotiation is a set, not an equality. A server names the revision it will
/// speak, and the client proceeds if it understands that revision — refusing
/// anything else. Demanding that a server answer with our own offer is not a
/// stricter reading of the protocol, it is a different protocol: the widely
/// deployed revisions below are exactly the ones a real configured server
/// answers with, and an exact-match handshake refuses all but one of them
/// before a single tool can be discovered.
///
/// The set is closed and declared on purpose. The refusal this crate cares
/// about — never guessing at message shapes it has never seen — is preserved
/// by the bound of this list, not by the size of it.
///
/// # What this list is evidenced by, and what it is not
///
/// The claim is deliberately narrow, because copying another client's accepted
/// list would prove what THAT client supports, not what this one does.
///
/// No per-revision specification is pinned in this tree, so this list is NOT
/// asserted from the specification text. It is asserted from the pinned oracle
/// dependency — `@modelcontextprotocol/sdk` 1.29.0, which
/// The oracle's `client.ts` hands the whole handshake to — and from
/// what that dependency does across these revisions for the four operations
/// this client actually implements: `initialize`,
/// `notifications/initialized`, `tools/list` and `tools/call`.
///
/// Two facts from that pinned dependency carry the claim. It applies ONE
/// schema set to every negotiated revision, with no version-conditional branch
/// for any of those four operations. And its single piece of
/// version-conditional behaviour anywhere is the `MCP-Protocol-Version` header
/// its SSE and streamable-HTTP transports attach after negotiation.
///
/// That header belongs to a REMOTE transport, and this client composes only
/// stdio. So the one behaviour a negotiated revision changes in the pinned
/// oracle is one this client cannot currently reach — which is why matching
/// its acceptance is sound here and would not be sound unexamined.
///
/// The obligation that comes with a remote transport is therefore named rather
/// than discovered later: whoever composes one MUST echo
/// [`ServerIdentity::negotiated_protocol`] in that header. The value is
/// recorded on the identity for exactly that reason.
///
/// Fields a newer revision adds and an older one never sends — `tools/call`'s
/// `structuredContent` is the live example — are read as ABSENT rather than
/// empty, so an older server is described truthfully instead of being reported
/// as having answered with nothing.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
    "2024-10-07",
];

/// Bounds on everything a remote server can inflate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpBounds {
    max_frame_bytes: usize,
    max_tools: usize,
    max_schema_bytes: usize,
    max_tool_name_bytes: usize,
    max_description_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum McpBoundsError {
    #[error("every MCP bound must be nonzero")]
    Zero,
}

impl McpBounds {
    pub fn new(
        max_frame_bytes: usize,
        max_tools: usize,
        max_schema_bytes: usize,
        max_tool_name_bytes: usize,
        max_description_bytes: usize,
    ) -> Result<Self, McpBoundsError> {
        let bounds = [
            max_frame_bytes,
            max_tools,
            max_schema_bytes,
            max_tool_name_bytes,
            max_description_bytes,
        ];
        if bounds.contains(&0) {
            return Err(McpBoundsError::Zero);
        }
        Ok(Self {
            max_frame_bytes,
            max_tools,
            max_schema_bytes,
            max_tool_name_bytes,
            max_description_bytes,
        })
    }

    /// The bounds a configured server starts under. They are deliberately
    /// small: a tool catalog is prompt content, and prompt content that grows
    /// without a ceiling is an unbounded input to every later Step.
    pub fn conservative() -> Result<Self, McpBoundsError> {
        Self::new(1024 * 1024, 128, 32 * 1024, 128, 4 * 1024)
    }

    pub fn max_frame_bytes(self) -> usize {
        self.max_frame_bytes
    }
    pub fn max_tools(self) -> usize {
        self.max_tools
    }
    pub fn max_schema_bytes(self) -> usize {
        self.max_schema_bytes
    }
}

/// A discovered tool's identity.
///
/// Kept in this crate rather than promoted into the shared identity crate: the
/// only thing that currently mints one is MCP discovery, and an identity type
/// with one producer belongs with that producer until a second one exists.
///
/// The admitted shape is deliberately narrow. A discovered name is rendered
/// into a model prompt and matched against permission decisions, so a name
/// carrying whitespace, control bytes, quotes or path separators would be a
/// name that reads as one thing in a prompt and another in a receipt.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct McpToolName(String);

impl McpToolName {
    pub fn new(raw: &str) -> Option<Self> {
        let usable = !raw.is_empty()
            && raw
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
        usable.then(|| Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A JSON-RPC correlation id this client minted.
///
/// Ids are issued in order by [`RequestSequence`] and are never reused within
/// one connection, so a late reply to a retired request cannot be mistaken for
/// the answer to a live one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(u64);

impl RequestId {
    pub fn value(self) -> u64 {
        self.0
    }
}

/// The monotonic id source for one connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RequestSequence(u64);

impl RequestSequence {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint the next id, or refuse at the ceiling. Wrapping would reissue a
    /// retired id, which is exactly the correlation defect ids exist to remove.
    ///
    /// Named `mint` rather than `next` so it is never mistaken for an iterator
    /// step: exhausting this sequence is a refusal, not an end of iteration.
    pub fn mint(&mut self) -> Result<RequestId, McpProtocolError> {
        let next = self
            .0
            .checked_add(1)
            .ok_or(McpProtocolError::RequestIdExhausted)?;
        self.0 = next;
        Ok(RequestId(next))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum McpProtocolError {
    #[error("this connection exhausted its request id space")]
    RequestIdExhausted,
    #[error("an MCP frame of {offered} bytes exceeds the {limit}-byte bound")]
    FrameTooLarge { limit: usize, offered: usize },
    #[error("an MCP frame was not valid JSON: {0}")]
    Malformed(String),
    #[error("an MCP frame was not a JSON-RPC object")]
    NotAnObject,
    #[error("an MCP frame declared jsonrpc {0:?}; only \"2.0\" is spoken")]
    WrongVersion(String),
    #[error("an MCP reply carried no usable correlation id")]
    UnusableId,
    #[error("an MCP reply for request {replied} does not answer request {expected}")]
    CorrelationMismatch { expected: u64, replied: u64 },
    #[error("an MCP reply carried neither a result nor an error")]
    NeitherResultNorError,
    #[error("an MCP reply carried both a result and an error")]
    BothResultAndError,
    #[error("the server refused: {message} (code {code})")]
    ServerError {
        code: i64,
        message: String,
        data: Option<ServerErrorData>,
    },
    #[error("the server requires an external interaction (code -32042); manual retry required")]
    UrlElicitationRequired(elicitation::UrlRequirements),
    #[error("the server answered protocol version {offered:?}; this client speaks {supported:?}")]
    ProtocolVersionUnsupported {
        offered: String,
        supported: &'static [&'static str],
    },
    #[error("the initialize result carried no server identity")]
    MissingServerIdentity,
    #[error("initialize instructions must be a string of at most 65536 bytes")]
    InvalidServerInstructions,
    #[error("the tool listing was not an array")]
    ToolsNotAnArray,
    #[error("tools/list nextCursor must be a nonempty string of at most 1024 bytes")]
    InvalidToolsCursor,
    #[error("tools/list repeated a pagination cursor")]
    RepeatedToolsCursor,
    #[error("tools/list exceeded its {limit}-page discovery bound")]
    TooManyToolPages { limit: usize },
    #[error("tools/list catalog exceeded its {limit}-byte discovery bound")]
    ToolCatalogTooLarge { limit: usize },
    #[error("tools/list discovery has not completed")]
    ToolCatalogIncomplete,
    #[error("the server advertised {offered} tools, above the bound {limit}")]
    TooManyTools { limit: usize, offered: usize },
    #[error("tool {name:?} has a {offered}-byte schema, above the bound {limit}")]
    SchemaTooLarge {
        name: String,
        limit: usize,
        offered: usize,
    },
    #[error("a tool declaration is missing its {0}")]
    ToolMissing(&'static str),
    #[error("tool name {0:?} is not a usable tool identity")]
    ToolNameInvalid(String),
    #[error("the server advertised tool {0:?} twice")]
    ToolRepeated(String),
    #[error("tool {name:?} has a {offered}-byte description, above the bound {limit}")]
    DescriptionTooLarge {
        name: String,
        limit: usize,
        offered: usize,
    },
    #[error("tool {0:?} has an input schema that is not a JSON Schema object")]
    SchemaNotAnObject(String),
    #[error("the arguments for tool {0:?} are not a JSON object")]
    CallArgumentsNotAnObject(String),
    #[error("the tool call result carried no content")]
    CallMissingContent,
    #[error("the tool call result's content was not an array")]
    CallContentNotAnArray,
    #[error("a tool call content block declared no type")]
    CallBlockUntyped,
    #[error("a text content block carried no text")]
    CallTextBlockEmpty,
    #[error("a tool call returned {offered} bytes of content, above the bound {limit}")]
    CallResultTooLarge { limit: usize, offered: usize },
    #[error("the tool call result's isError was not a boolean")]
    CallErrorFlagNotABool,
    #[error("tools/call metadata must be an object")]
    CallMetadataNotAnObject,
    #[error("tools/call with private metadata exceeds the frame bound")]
    PrivateCallFrameTooLarge,
}

/// One request this client is waiting on: the id and the bytes to send.
#[derive(Clone, PartialEq, Eq)]
pub struct OutboundRequest {
    id: RequestId,
    frame: Vec<u8>,
}

impl std::fmt::Debug for OutboundRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutboundRequest")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl OutboundRequest {
    pub fn id(&self) -> RequestId {
        self.id
    }
    pub fn frame(&self) -> &[u8] {
        &self.frame
    }
}

fn encode(
    sequence: &mut RequestSequence,
    bounds: McpBounds,
    method: &str,
    params: Value,
) -> Result<OutboundRequest, McpProtocolError> {
    let id = sequence.mint()?;
    let frame = json!({
        "jsonrpc": "2.0",
        "id": id.value(),
        "method": method,
        "params": params,
    })
    .to_string()
    .into_bytes();
    if frame.len() > bounds.max_frame_bytes {
        return Err(McpProtocolError::FrameTooLarge {
            limit: bounds.max_frame_bytes,
            offered: frame.len(),
        });
    }
    Ok(OutboundRequest { id, frame })
}

/// The `initialize` request that opens a connection.
pub fn initialize_request(
    sequence: &mut RequestSequence,
    bounds: McpBounds,
    client_name: &str,
    client_version: &str,
) -> Result<OutboundRequest, McpProtocolError> {
    initialize_request_with_elicitation(sequence, bounds, client_name, client_version, true)
}

/// Advertise elicitation only when the composition supplies a live consumer.
pub fn initialize_request_with_elicitation(
    sequence: &mut RequestSequence,
    bounds: McpBounds,
    client_name: &str,
    client_version: &str,
    elicitation: bool,
) -> Result<OutboundRequest, McpProtocolError> {
    let mut capabilities = json!({"tools": {}, "roots": {"listChanged": true}});
    if elicitation {
        capabilities["elicitation"] = json!({"form": {}, "url": {}});
    }
    encode(
        sequence,
        bounds,
        "initialize",
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": capabilities,
            "clientInfo": {"name": client_name, "version": client_version},
        }),
    )
}

/// One notification this client sends and expects no answer to.
///
/// It is a distinct type from [`OutboundRequest`] because it carries no id and
/// therefore has nothing to correlate a reply against. Handing it to a caller
/// that waits for a reply would hang until the startup deadline, so the type
/// system refuses that instead of the deadline discovering it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundNotification {
    frame: Vec<u8>,
}

impl OutboundNotification {
    pub fn frame(&self) -> &[u8] {
        &self.frame
    }
}

/// The `notifications/initialized` notification that COMPLETES the handshake.
///
/// The specification makes this the client's half of a two-step handshake: the
/// server answers `initialize`, and the client confirms it is ready before
/// issuing any other request. It is not decoration. A conformant server is
/// entitled to refuse `tools/list` until it has been sent, so omitting it
/// leaves a handshake that a compliant server may legitimately reject and a
/// lenient one silently forgives — the worst kind of gap, because the lenient
/// servers hide it during development and the strict ones surface it in front
/// of a user.
pub fn initialized_notification(
    bounds: McpBounds,
) -> Result<OutboundNotification, McpProtocolError> {
    let frame = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
    })
    .to_string()
    .into_bytes();
    if frame.len() > bounds.max_frame_bytes {
        return Err(McpProtocolError::FrameTooLarge {
            limit: bounds.max_frame_bytes,
            offered: frame.len(),
        });
    }
    Ok(OutboundNotification { frame })
}

/// The `tools/list` request.
pub fn tools_list_request(
    sequence: &mut RequestSequence,
    bounds: McpBounds,
) -> Result<OutboundRequest, McpProtocolError> {
    tools_list_page_request(sequence, bounds, None)
}

pub fn tools_list_page_request(
    sequence: &mut RequestSequence,
    bounds: McpBounds,
    cursor: Option<&str>,
) -> Result<OutboundRequest, McpProtocolError> {
    let params = match cursor {
        Some(cursor) if cursor.is_empty() || cursor.len() > 1024 => {
            return Err(McpProtocolError::InvalidToolsCursor);
        }
        Some(cursor) => json!({"cursor": cursor}),
        None => json!({}),
    };
    encode(sequence, bounds, "tools/list", params)
}

/// What a server said it is. Identity is required: an anonymous server cannot
/// be attributed in a receipt or told apart from another after a restart.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(clippy::struct_excessive_bools, reason = "legacy")]
pub struct ServerIdentity {
    name: String,
    instructions: Option<String>,
    version: Option<String>,
    negotiated_protocol: &'static str,
    advertises_resources: bool,
    subscribes_resources: bool,
    advertises_prompts: bool,
    advertises_tools: bool,
}

impl ServerIdentity {
    /// Optional server-provided usage guidance, never a permission grant.
    pub fn instructions(&self) -> Option<&str> {
        self.instructions.as_deref()
    }

    pub fn name(&self) -> &str {
        &self.name
    }
    /// `None` when the server declared no version. Absent is not "0".
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    /// The revision this connection settled on: what the server answered, not
    /// what this client offered. The two differ whenever a server speaks an
    /// older revision, which is the ordinary case, so a receipt that recorded
    /// our offer would misattribute the conversation that actually happened.
    pub fn negotiated_protocol(&self) -> &'static str {
        self.negotiated_protocol
    }

    /// Whether the handshake advertised a `resources` capability.
    ///
    /// This is what lets an empty resource catalog be TRUE rather than merely
    /// convenient: a server that never claimed resources has none to report,
    /// while a server that did claim them and was not asked must not be
    /// reported as offering none.
    pub fn advertises_prompts(&self) -> bool {
        self.advertises_prompts
    }

    pub fn advertises_tools(&self) -> bool {
        self.advertises_tools
    }

    pub fn subscribes_resources(&self) -> bool {
        self.subscribes_resources
    }

    pub fn advertises_resources(&self) -> bool {
        self.advertises_resources
    }
}

/// Validate an `initialize` result.
pub fn accept_initialize(result: &Value) -> Result<ServerIdentity, McpProtocolError> {
    let offered = result
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let negotiated = SUPPORTED_PROTOCOL_VERSIONS
        .iter()
        .find(|supported| **supported == offered)
        .ok_or_else(|| McpProtocolError::ProtocolVersionUnsupported {
            offered: offered.to_owned(),
            supported: SUPPORTED_PROTOCOL_VERSIONS,
        })?;
    let info = result
        .get("serverInfo")
        .ok_or(McpProtocolError::MissingServerIdentity)?;
    let name = info
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(McpProtocolError::MissingServerIdentity)?
        .to_owned();
    let version = info
        .get("version")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let advertises_resources = result
        .get("capabilities")
        .and_then(|capabilities| capabilities.get("resources"))
        .is_some();
    let instructions = result
        .get("instructions")
        .map(|value| {
            value
                .as_str()
                .filter(|text| text.len() <= 65_536)
                .map(str::to_owned)
                .ok_or(McpProtocolError::InvalidServerInstructions)
        })
        .transpose()?;
    Ok(ServerIdentity {
        name,
        instructions,
        version,
        negotiated_protocol: negotiated,
        advertises_resources,
        subscribes_resources: result["capabilities"]["resources"]["subscribe"] == true,
        advertises_prompts: result["capabilities"].get("prompts").is_some(),
        advertises_tools: result["capabilities"].get("tools").is_some(),
    })
}

/// One discovered tool, already inside every bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredTool {
    name: McpToolName,
    description: Option<String>,
    input_schema: Value,
    output_schema: Option<Value>,
}

impl DiscoveredTool {
    pub fn name(&self) -> &McpToolName {
        &self.name
    }
    /// `None` when the server described the tool with nothing. An absent
    /// description is not an empty one.
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
    /// The bounded JSON Schema this tool's arguments are validated against.
    /// This is what may be shown to a model; nothing else from the server is.
    pub fn input_schema(&self) -> &Value {
        &self.input_schema
    }
    /// Absent is distinct from an explicitly malformed output schema.
    pub fn output_schema(&self) -> Option<&Value> {
        self.output_schema.as_ref()
    }
}

/// The `tools/call` request that runs one discovered tool.
///
/// The tool name is an [`McpToolName`], which can only exist by having been
/// validated, so a name that was never discovered cannot be assembled here by
/// string. Arguments are passed through as the model produced them: this crate
/// validates the SHAPE of the frame and its size, and deliberately does not
/// second-guess a schema the server published and the model answered. The
/// authority that decides whether this call may happen at all lives above,
/// with the permission owner.
pub fn tools_call_request(
    sequence: &mut RequestSequence,
    bounds: McpBounds,
    name: &McpToolName,
    arguments: Value,
) -> Result<OutboundRequest, McpProtocolError> {
    tools_call_request_with_metadata(sequence, bounds, name, arguments, None)
}

/// Harness metadata is placed beside arguments, never merged with them.
/// It may carry private capabilities, so frame-size diagnostics are redacted.
pub fn tools_call_request_with_metadata(
    sequence: &mut RequestSequence,
    bounds: McpBounds,
    name: &McpToolName,
    arguments: Value,
    metadata: Option<Value>,
) -> Result<OutboundRequest, McpProtocolError> {
    // Arguments must be a JSON object. A bare scalar or array would be encoded
    // happily and rejected by the server as a protocol error, which is a much
    // worse place to discover it than here.
    if !arguments.is_object() {
        return Err(McpProtocolError::CallArgumentsNotAnObject(
            name.as_str().to_owned(),
        ));
    }
    let mut params = json!({"name": name.as_str(), "arguments": arguments});
    let private = metadata.is_some();
    if let Some(metadata) = metadata {
        if !metadata.is_object() {
            return Err(McpProtocolError::CallMetadataNotAnObject);
        }
        params["_meta"] = metadata;
    }
    encode(sequence, bounds, "tools/call", params).map_err(|error| {
        if private && matches!(error, McpProtocolError::FrameTooLarge { .. }) {
            McpProtocolError::PrivateCallFrameTooLarge
        } else {
            error
        }
    })
}

/// What a tool call produced.
///
/// `is_error` is the server's own verdict, carried rather than translated. A
/// tool that ran and reported failure is not the same event as a call that
/// could not be made, and folding the two together would leave a model unable
/// to tell "the command exited non-zero" from "the server is gone".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallOutcome {
    text: String,
    is_error: bool,
    structured: Option<Value>,
    content: Vec<Value>,
}

impl ToolCallOutcome {
    /// Text and textual resources, joined in the order the server sent them.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether the SERVER called this an error. This is the tool's verdict on
    /// its own run, never this client's verdict on the tool.
    pub fn is_error(&self) -> bool {
        self.is_error
    }

    /// `structuredContent` when the server supplied it. Absent is not empty:
    /// a server that published no structured result has none, and inventing
    /// `{}` would claim it answered.
    pub fn structured(&self) -> Option<&Value> {
        self.structured.as_ref()
    }

    /// Original bounded content blocks for artifact-aware consumers. Direct
    /// text publication remains the existing text projection above.
    pub fn content(&self) -> &[Value] {
        &self.content
    }
}

/// Validate a `tools/call` result into a bounded outcome.
///
/// Content blocks without a textual projection are named rather than dropped. A
/// tool that returned an image and a client that reported nothing would be
/// indistinguishable from a tool that returned nothing, and the model would
/// continue from an absence that was actually a value it never saw.
pub fn accept_call(bounds: McpBounds, result: &Value) -> Result<ToolCallOutcome, McpProtocolError> {
    measure_call_result(result, bounds.max_frame_bytes())?;
    let blocks = result
        .get("content")
        .ok_or(McpProtocolError::CallMissingContent)?
        .as_array()
        .ok_or(McpProtocolError::CallContentNotAnArray)?;
    let limit = bounds.max_schema_bytes();
    let mut text = String::new();
    for block in blocks {
        let kind = block
            .get("type")
            .and_then(Value::as_str)
            .ok_or(McpProtocolError::CallBlockUntyped)?;
        let rendered = match kind {
            "text" => block
                .get("text")
                .and_then(Value::as_str)
                .ok_or(McpProtocolError::CallTextBlockEmpty)?
                .to_owned(),
            "resource" => block
                .get("resource")
                .and_then(|resource| resource.get("text"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| unsupported_content(kind)),
            "resource_link" => {
                let uri = block
                    .get("uri")
                    .and_then(Value::as_str)
                    .filter(|uri| !uri.is_empty());
                match uri {
                    Some(uri) => {
                        let label = block
                            .get("title")
                            .and_then(Value::as_str)
                            .filter(|title| !title.is_empty())
                            .or_else(|| block.get("name").and_then(Value::as_str));
                        label.map_or_else(|| uri.to_owned(), |label| format!("{label}: {uri}"))
                    }
                    None => unsupported_content(kind),
                }
            }
            // Named, not dropped, and not decoded: this client has nowhere to
            // put the bytes, and saying so is the truthful answer.
            other => unsupported_content(other),
        };
        let offered = text.len() + rendered.len() + usize::from(!text.is_empty());
        if offered > limit {
            return Err(McpProtocolError::CallResultTooLarge { limit, offered });
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&rendered);
    }
    // Absent `isError` means no error was claimed. That is the specification's
    // default and it is a claim about the tool, so it is read rather than
    // guessed at from whether content happens to be present.
    let is_error = match result.get("isError") {
        None => false,
        Some(value) => value
            .as_bool()
            .ok_or(McpProtocolError::CallErrorFlagNotABool)?,
    };
    Ok(ToolCallOutcome {
        text,
        is_error,
        structured: result.get("structuredContent").cloned(),
        content: blocks.clone(),
    })
}

fn unsupported_content(kind: &str) -> String {
    format!("[unsupported MCP content block: {kind}]")
}

// Bound the full structured/binary envelope before cloning it into the
// outcome. Counting through a writer does not allocate a second JSON string.
fn measure_call_result(result: &Value, limit: usize) -> Result<(), McpProtocolError> {
    struct Count {
        bytes: usize,
        limit: usize,
    }
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes = self.bytes.saturating_add(bytes.len());
            if self.bytes > self.limit {
                return Err(std::io::Error::other("MCP result bound exceeded"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count { bytes: 0, limit };
    serde_json::to_writer(&mut count, result).map_err(|_| McpProtocolError::CallResultTooLarge {
        limit,
        offered: count.bytes,
    })
}

/// Validate a `tools/list` result into a bounded catalog.
///
/// A server that lists no tools yields an empty catalog, which is a real
/// answer. A server whose listing cannot be understood yields a refusal, which
/// is a different answer, and the caller must be able to tell them apart.
pub fn accept_tools(
    bounds: McpBounds,
    result: &Value,
) -> Result<Vec<DiscoveredTool>, McpProtocolError> {
    let rows = result
        .get("tools")
        .and_then(Value::as_array)
        .ok_or(McpProtocolError::ToolsNotAnArray)?;
    if rows.len() > bounds.max_tools {
        return Err(McpProtocolError::TooManyTools {
            limit: bounds.max_tools,
            offered: rows.len(),
        });
    }
    let mut admitted: Vec<DiscoveredTool> = Vec::with_capacity(rows.len());
    for row in rows {
        let raw = row
            .get("name")
            .and_then(Value::as_str)
            .ok_or(McpProtocolError::ToolMissing("name"))?;
        if raw.len() > bounds.max_tool_name_bytes {
            return Err(McpProtocolError::ToolNameInvalid(raw.to_owned()));
        }
        let name = McpToolName::new(raw)
            .ok_or_else(|| McpProtocolError::ToolNameInvalid(raw.to_owned()))?;
        if admitted.iter().any(|tool| tool.name == name) {
            return Err(McpProtocolError::ToolRepeated(raw.to_owned()));
        }
        let description = match row.get("description").and_then(Value::as_str) {
            None => None,
            Some("") => None,
            Some(text) => {
                if text.len() > bounds.max_description_bytes {
                    return Err(McpProtocolError::DescriptionTooLarge {
                        name: raw.to_owned(),
                        limit: bounds.max_description_bytes,
                        offered: text.len(),
                    });
                }
                Some(text.to_owned())
            }
        };
        let schema = row
            .get("inputSchema")
            .ok_or(McpProtocolError::ToolMissing("inputSchema"))?;
        let schema_object: &Map<String, Value> = schema
            .as_object()
            .ok_or_else(|| McpProtocolError::SchemaNotAnObject(raw.to_owned()))?;
        // Measured as it will be carried, not as the server happened to format
        // it: a schema is bounded by the bytes it costs in a prompt.
        let measured = Value::Object(schema_object.clone()).to_string();
        if measured.len() > bounds.max_schema_bytes {
            return Err(McpProtocolError::SchemaTooLarge {
                name: raw.to_owned(),
                limit: bounds.max_schema_bytes,
                offered: measured.len(),
            });
        }
        let output_schema = match row.get("outputSchema") {
            None => None,
            Some(output) => {
                if !output.is_object() {
                    return Err(McpProtocolError::SchemaNotAnObject(raw.to_owned()));
                }
                let offered = measured.len().saturating_add(output.to_string().len());
                if offered > bounds.max_schema_bytes {
                    return Err(McpProtocolError::SchemaTooLarge {
                        name: raw.to_owned(),
                        limit: bounds.max_schema_bytes,
                        offered,
                    });
                }
                Some(output.clone())
            }
        };
        admitted.push(DiscoveredTool {
            name,
            description,
            input_schema: schema.clone(),
            output_schema,
        });
    }
    Ok(admitted)
}

#[cfg(test)]
mod tests;
