//! Projects, agents, models and providers — the selection catalogs.
//!
//! Split from `wire.rs` by SUBJECT when that file passed this workspace's
//! module ceiling. Every encode and decode still goes through the real codecs
//! under the real limits; nothing here was rewritten on the way out.

use super::*;

/// The project identity, parsed at the wire edge like every other identity.
pub type ProjectWireId = WireId<grapt_types::ProjectId>;

/// The agent identity.
pub type AgentWireId = WireId<grapt_types::AgentId>;

/// The model identity.
pub type ModelWireId = WireId<grapt_types::ModelId>;

/// An offered model effort, selected as part of the model coordinate.
pub type ModelVariantWireId = WireId<grapt_types::ModelVariantId>;

impl Spelled for ModelVariantWireId {
    fn spelled(&self) -> &str {
        self.as_inner().as_str()
    }
}

/// The provider identity a model is reached through.
pub type ProviderWireId = WireId<grapt_types::ProviderId>;

impl Spelled for ProjectWireId {
    fn spelled(&self) -> &str {
        self.as_inner().as_str()
    }
}

impl Spelled for AgentWireId {
    fn spelled(&self) -> &str {
        self.as_inner().as_str()
    }
}

impl Spelled for ModelWireId {
    fn spelled(&self) -> &str {
        self.as_inner().as_str()
    }
}

impl Spelled for ProviderWireId {
    fn spelled(&self) -> &str {
        self.as_inner().as_str()
    }
}

pub fn project_id(value: &str) -> Result<ProjectWireId, WireError> {
    WireId::parse(value.to_owned()).ok_or_else(|| WireError::Invalid {
        what: "project id",
        value: value.to_owned(),
    })
}

pub fn agent_id(value: &str) -> Result<AgentWireId, WireError> {
    WireId::parse(value.to_owned()).ok_or_else(|| WireError::Invalid {
        what: "agent id",
        value: value.to_owned(),
    })
}

pub fn model_id(value: &str) -> Result<ModelWireId, WireError> {
    WireId::parse(value.to_owned()).ok_or_else(|| WireError::Invalid {
        what: "model id",
        value: value.to_owned(),
    })
}

pub fn provider_id(value: &str) -> Result<ProviderWireId, WireError> {
    WireId::parse(value.to_owned()).ok_or_else(|| WireError::Invalid {
        what: "provider id",
        value: value.to_owned(),
    })
}

/// One row of the project gateway.
///
/// `worktree` is carried because it is not decoration: it is the `directory`
/// the catalog and session-create routes are scoped by, so choosing a project
/// IS choosing that string. A picker that showed only the name would leave the
/// user selecting something the next request could not be addressed with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRow {
    pub id: ProjectWireId,
    pub worktree: String,
    /// The project's own name when it declares one. Absent is rendered as
    /// absent rather than defaulted to the worktree, because a project that
    /// has not been named and one named after its directory are different
    /// facts and the gateway should not merge them.
    pub name: Option<String>,
}

/// Decode a real `ProjectListResponseDto`.
pub fn decode_projects(bytes: &[u8]) -> Result<Vec<ProjectRow>, WireError> {
    use grapt_api::u3a::ProjectListResponseDto;
    let dto: ProjectListResponseDto = decode_page(bytes, limits()?)?;
    Ok(dto
        .as_inner()
        .as_slice()
        .iter()
        .map(|project| ProjectRow {
            id: project.id().get().clone(),
            worktree: project.worktree().get().as_str().to_owned(),
            name: project
                .name()
                .as_present()
                .map(|value| value.as_str().to_owned()),
        })
        .collect())
}

/// One row of the agent roster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRow {
    pub id: AgentWireId,
    pub name: String,
    pub description: Option<String>,
    pub mode: grapt_api::u3a::AgentInfoDtoMode,
    /// The roster hides what the product marks hidden. This is carried rather
    /// than filtered at decode so the FILTER is a rendering decision with a
    /// witness, not an invisible loss of rows at the wire edge.
    pub hidden: bool,
    /// The layer that supplied this definition ("builtin", "user", "project",
    /// "claude-user", "claude-project"), so the roster can say which one
    /// loaded when the same name exists in more than one place.
    pub source: Option<String>,
}

/// Decode a real `AgentListResponseDto`.
pub fn decode_agents(bytes: &[u8]) -> Result<Vec<AgentRow>, WireError> {
    use grapt_api::u3a::AgentListResponseDto;
    let dto: AgentListResponseDto = decode_page(bytes, limits()?)?;
    Ok(dto
        .data()
        .get()
        .as_slice()
        .iter()
        .map(|agent| AgentRow {
            id: agent.id().get().clone(),
            name: agent.name().get().as_str().to_owned(),
            description: agent
                .description()
                .as_present()
                .map(|value| value.as_str().to_owned()),
            mode: *agent.mode().get(),
            hidden: *agent.hidden().get(),
            source: agent
                .source()
                .as_present()
                .map(|value| value.as_str().to_owned()),
        })
        .collect())
}

/// One row of the model picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRow {
    pub id: ModelWireId,
    pub provider_id: ProviderWireId,
    pub name: String,
    /// A model the location has disabled is still LISTED, marked, and
    /// unselectable. Dropping it would answer "why can I not find it" with an
    /// empty list, which is the frozen picker's behaviour inverted.
    pub enabled: bool,
    /// The offered effort selected together with this model.
    pub variant: Option<ModelVariantWireId>,
    /// Effort inherited when no session variant is selected; never a write.
    pub default_effort: Option<ModelVariantWireId>,
    /// Catalog capacity; zero in the retained DTO means unreported.
    pub context_window: Option<u64>,
    /// Reported capabilities. `None` is unknown — the catalog did not report
    /// the field — and renders as such, never as a denial.
    pub capabilities: ModelCapabilities,
}

/// What the catalog reports about a model's request surface. The retained DTO
/// spells unknown as false/empty; the settings metadata names those unknowns,
/// and this row keeps the two facts apart.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub tools: Option<bool>,
    pub input_text: Option<bool>,
    pub output_text: Option<bool>,
}

fn model_row(model: &grapt_api::u3a::ModelInfoDto) -> Result<ModelRow, WireError> {
    Ok(ModelRow {
        id: model.id().get().clone(),
        provider_id: model.provider_id().get().clone(),
        name: model.name().get().as_str().to_owned(),
        enabled: *model.enabled().get(),
        variant: None,
        default_effort: super::status::default_effort(model)?,
        context_window: super::status::context_window(model)?,
        capabilities: super::status::capabilities(model)?,
    })
}

/// Model references carry identities, with the catalog name left unknown.
pub(super) fn model_ref_row(model: &grapt_api::u3a::ModelRefDto) -> ModelRow {
    ModelRow {
        id: model.id().get().clone(),
        provider_id: model.provider_id().get().clone(),
        name: model.id().get().spelled().to_owned(),
        enabled: true,
        context_window: None,
        default_effort: None,
        capabilities: ModelCapabilities::default(),
        variant: model
            .variant()
            .as_present()
            .filter(|level| level.as_inner().as_str() != "default")
            .cloned(),
    }
}

/// Decode a real `ModelListResponseDto`.
pub fn decode_models(bytes: &[u8]) -> Result<Vec<ModelRow>, WireError> {
    use grapt_api::u3a::ModelListResponseDto;
    let dto: ModelListResponseDto = decode_page(bytes, limits()?)?;
    let mut rows = Vec::new();
    for model in dto.data().get().as_slice() {
        let base = model_row(model)?;
        rows.push(base.clone());
        for effort in model.variants().get().as_slice() {
            let mut choice = base.clone();
            choice.variant = Some(effort.id().get().clone());
            rows.push(choice);
        }
    }
    Ok(rows)
}

/// Decode a real `ModelDefaultResponseDto`.
///
/// The `data` field is NULLABLE in the frozen record: a location with no
/// resolvable default answers `null`, and that is a fact the picker renders
/// rather than an error. Collapsing it to an error would make an ordinary
/// unconfigured location look like a broken product.
pub fn decode_model_default(bytes: &[u8]) -> Result<Option<ModelRow>, WireError> {
    use grapt_api::u3a::ModelDefaultResponseDto;
    let dto: ModelDefaultResponseDto = decode_page(bytes, limits()?)?;
    dto.data().as_value().map(model_row).transpose()
}

/// Encode a real `SessionAgentSelectDto`, validated by the real decoder.
pub fn encode_agent_select(agent: &AgentWireId) -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::SessionAgentSelectDto;
    let bytes = serde_json::json!({ "agent": agent.spelled() })
        .to_string()
        .into_bytes();
    let _validated: SessionAgentSelectDto = decode_request(&bytes, limits()?)?;
    Ok(bytes)
}

/// Decode a real `SessionAgentSelectDto` and return the chosen agent.
pub fn decode_agent_select(bytes: &[u8]) -> Result<AgentWireId, WireError> {
    use grapt_api::u3a::SessionAgentSelectDto;
    let dto: SessionAgentSelectDto = decode_request(bytes, limits()?)?;
    Ok(dto.agent().get().clone())
}

/// Encode a real `SessionModelSelectDto`, validated by the real decoder.
///
/// The body carries a `Model.Ref` — id AND provider — because a model id is
/// only unique within its provider. Sending the id alone would compile, would
/// often work, and would silently switch to the wrong model the first time two
/// providers published the same name.
pub fn encode_model_select(
    model: &ModelWireId,
    provider: &ProviderWireId,
) -> Result<Vec<u8>, WireError> {
    encode_model_choice(model, provider, None)
}

/// A model and its offered effort are one atomic selection.
pub fn encode_model_choice(
    model: &ModelWireId,
    provider: &ProviderWireId,
    variant: Option<&WireId<grapt_types::ModelVariantId>>,
) -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::SessionModelSelectDto;
    let mut value = serde_json::json!({
        "model": { "id": model.spelled(), "providerID": provider.spelled() }
    });
    if let Some(variant) = variant {
        value["model"]["variant"] = serde_json::json!(variant.as_inner().as_str());
    }
    let bytes = value.to_string().into_bytes();
    let _validated: SessionModelSelectDto = decode_request(&bytes, limits()?)?;
    Ok(bytes)
}

/// Decode a real `SessionModelSelectDto` and return the chosen model reference.
pub fn decode_model_select(bytes: &[u8]) -> Result<(ModelWireId, ProviderWireId), WireError> {
    use grapt_api::u3a::SessionModelSelectDto;
    let dto: SessionModelSelectDto = decode_request(bytes, limits()?)?;
    let model = dto.model().get();
    Ok((model.id().get().clone(), model.provider_id().get().clone()))
}

/// Read the optional effort without reducing a model reference to its id.
pub fn decode_model_variant(
    bytes: &[u8],
) -> Result<Option<WireId<grapt_types::ModelVariantId>>, WireError> {
    let dto: grapt_api::u3a::SessionModelSelectDto = decode_request(bytes, limits()?)?;
    Ok(dto.model().get().variant().as_present().cloned())
}

/// The three catalog responses all carry a `Location.Info`, so it is built
/// once here rather than spelled at three call sites.
pub(super) fn location_value(project: &ProjectRow) -> serde_json::Value {
    serde_json::json!({
        "directory": project.worktree,
        "project": { "id": project.id.spelled(), "directory": project.worktree },
    })
}

/// Encode a real `ProjectListResponseDto`, validated by the real decoder.
///
/// Like [`encode_prompt`], the DTO cannot be CONSTRUCTED from outside
/// `grapt-api` — `ProjectDto` requires a `ProjectTimeDto` whose fields are
/// that crate's internal bounded numbers — so the honest form is to build the
/// body and prove it decodes as the real DTO before it leaves. A key that
/// drifted from the frozen record fails here rather than at a client that
/// would have to interpret a 400.
pub fn encode_projects(rows: &[ProjectRow]) -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::ProjectListResponseDto;
    let data: Vec<serde_json::Value> = rows
        .iter()
        .map(|project| {
            let mut value = serde_json::json!({
                "id": project.id.spelled(),
                "worktree": project.worktree,
                "time": { "created": 0, "updated": 0 },
                "sandboxes": [],
            });
            if let (Some(name), Some(object)) = (project.name.as_ref(), value.as_object_mut()) {
                object.insert("name".to_owned(), serde_json::Value::from(name.as_str()));
            }
            value
        })
        .collect();
    let bytes = serde_json::Value::from(data).to_string().into_bytes();
    let _validated: ProjectListResponseDto = decode_page(&bytes, limits()?)?;
    Ok(bytes)
}

/// Encode a real `AgentListResponseDto`, validated by the real decoder.
pub fn encode_agents(location: &ProjectRow, rows: &[AgentRow]) -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::AgentListResponseDto;
    let data: Vec<serde_json::Value> = rows
        .iter()
        .map(|agent| {
            let mut value = serde_json::json!({
                "id": agent.id.spelled(),
                "name": agent.name,
                "request": { "settings": {}, "headers": {}, "body": {} },
                "mode": agent.mode,
                "hidden": agent.hidden,
                "permissions": [],
            });
            if let (Some(text), Some(object)) = (agent.description.as_ref(), value.as_object_mut())
            {
                object.insert(
                    "description".to_owned(),
                    serde_json::Value::from(text.as_str()),
                );
            }
            if let (Some(text), Some(object)) = (agent.source.as_ref(), value.as_object_mut()) {
                object.insert("source".to_owned(), serde_json::Value::from(text.as_str()));
            }
            value
        })
        .collect();
    let bytes = serde_json::json!({ "location": location_value(location), "data": data })
        .to_string()
        .into_bytes();
    let _validated: AgentListResponseDto = decode_page(&bytes, limits()?)?;
    Ok(bytes)
}

/// One model, in the frozen `Model.Info` shape.
fn model_value(model: &ModelRow) -> serde_json::Value {
    let capability = |reported: Option<bool>| reported.unwrap_or(false);
    let modalities = |reported: Option<bool>| {
        if reported == Some(true) {
            serde_json::json!(["text"])
        } else {
            serde_json::json!([])
        }
    };
    let mut value = serde_json::json!({
        "id": model.id.spelled(),
        "modelID": model.id.spelled(),
        "providerID": model.provider_id.spelled(),
        "name": model.name,
        "capabilities": {
            "tools": capability(model.capabilities.tools),
            "input": modalities(model.capabilities.input_text),
            "output": modalities(model.capabilities.output_text)
        },
        "variants": [],
        "time": { "released": 0.0 },
        "cost": [],
        "status": "active",
        "enabled": model.enabled,
        "limit": { "context": model.context_window.unwrap_or(0), "output": 0 },
    });
    let mut unknown = Vec::new();
    if model.capabilities.tools.is_none() {
        unknown.push("tools");
    }
    if model.capabilities.input_text.is_none() {
        unknown.push("input");
    }
    if model.capabilities.output_text.is_none() {
        unknown.push("output");
    }
    if !unknown.is_empty() {
        value["settings"] = serde_json::json!({"metadata":{"unknown":unknown}});
    }
    value
}

/// Encode a real `ModelListResponseDto`, validated by the real decoder.
pub fn encode_models(location: &ProjectRow, rows: &[ModelRow]) -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::ModelListResponseDto;
    let data: Vec<serde_json::Value> = rows.iter().map(model_value).collect();
    let bytes = serde_json::json!({ "location": location_value(location), "data": data })
        .to_string()
        .into_bytes();
    let _validated: ModelListResponseDto = decode_page(&bytes, limits()?)?;
    Ok(bytes)
}

/// Encode a real `ModelDefaultResponseDto`, validated by the real decoder.
///
/// `None` encodes as an explicit JSON `null` rather than an absent key,
/// because the frozen field is `nullable` and not `optional`. Omitting it
/// would be a different wire fact and the decoder refuses it — which is the
/// point of validating rather than trusting.
pub fn encode_model_default(
    location: &ProjectRow,
    row: Option<&ModelRow>,
) -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::ModelDefaultResponseDto;
    let data = row.map_or(serde_json::Value::Null, model_value);
    let bytes = serde_json::json!({ "location": location_value(location), "data": data })
        .to_string()
        .into_bytes();
    let _validated: ModelDefaultResponseDto = decode_page(&bytes, limits()?)?;
    Ok(bytes)
}

/// Encode a real `ProjectCurrentDto`, validated by the real decoder.
pub fn encode_project_current(project: &ProjectRow) -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::ProjectCurrentDto;
    let bytes = serde_json::json!({
        "id": project.id.spelled(),
        "directory": project.worktree,
    })
    .to_string()
    .into_bytes();
    let _validated: ProjectCurrentDto = decode_page(&bytes, limits()?)?;
    Ok(bytes)
}

/// Decode a real `ProjectCurrentDto`.
pub fn decode_project_current(bytes: &[u8]) -> Result<(ProjectWireId, String), WireError> {
    use grapt_api::u3a::ProjectCurrentDto;
    let dto: ProjectCurrentDto = decode_page(bytes, limits()?)?;
    Ok((
        dto.id().get().clone(),
        dto.directory().get().as_str().to_owned(),
    ))
}

// ---------------------------------------------------------------------------
// MCP
// ---------------------------------------------------------------------------
//
// The configured MCP servers and their connection truth, from
// `v2.mcp.list`. Read-only: the connect and disconnect routes are separate
// operations this client does not name, so there is no encoder for them here.
// A toggle encoder with no route to send it through would be an affordance
// that exists in this crate and nowhere else.

/// One configured MCP server's connection status, the frozen `Mcp.Status`
/// union's six members as this client carries them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpServerStatus {
    Connected,
    Pending,
    Disabled,
    Failed { error: String },
    NeedsAuth,
    NeedsRegistration { error: String },
}

impl McpServerStatus {
    /// The status word exactly as the frozen frontend's sidebar spells it:
    /// one word per member, so the pane reads as the oracle read.
    pub fn word(&self) -> &'static str {
        match self {
            McpServerStatus::Connected => "Connected",
            McpServerStatus::Pending => "Pending",
            McpServerStatus::Disabled => "Disabled",
            McpServerStatus::Failed { .. } => "Failed",
            McpServerStatus::NeedsAuth => "Needs auth",
            McpServerStatus::NeedsRegistration { .. } => "Needs client ID",
        }
    }

    /// The error body, on the members that carry one.
    pub fn error(&self) -> Option<&str> {
        match self {
            McpServerStatus::Failed { error } | McpServerStatus::NeedsRegistration { error } => {
                Some(error)
            }
            McpServerStatus::Connected
            | McpServerStatus::Pending
            | McpServerStatus::Disabled
            | McpServerStatus::NeedsAuth => None,
        }
    }
}

/// One row of the MCP server list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerRow {
    pub name: String,
    pub status: McpServerStatus,
    pub integration_id: Option<String>,
    /// None for the retained status-only response; Some(empty) is a read empty catalog.
    pub tools: Option<Vec<McpToolRow>>,
}

/// Decode a real `McpListResponseDto`.
pub(super) fn mcp_server_row(server: &grapt_api::u3a::McpServerDto) -> McpServerRow {
    use grapt_api::u3a::McpStatusDto;
    let status = match server.status().get() {
        McpStatusDto::Connected(_) => McpServerStatus::Connected,
        McpStatusDto::Pending(_) => McpServerStatus::Pending,
        McpStatusDto::Disabled(_) => McpServerStatus::Disabled,
        McpStatusDto::Failed(body) => McpServerStatus::Failed {
            error: body.error().get().as_str().to_owned(),
        },
        McpStatusDto::NeedsAuth(_) => McpServerStatus::NeedsAuth,
        McpStatusDto::NeedsRegistration(body) => McpServerStatus::NeedsRegistration {
            error: body.error().get().as_str().to_owned(),
        },
    };
    McpServerRow {
        tools: None,
        name: server.name().get().as_str().to_owned(),
        status,
        integration_id: server
            .integration_id()
            .as_present()
            .map(|value| value.as_str().to_owned()),
    }
}

/// One inspected MCP tool; a known omission is shown instead of implying availability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolRow {
    pub name: String,
    pub omission: Option<String>,
}

pub fn decode_mcp_servers(bytes: &[u8]) -> Result<Vec<McpServerRow>, WireError> {
    let dto: grapt_api::u3a::McpListResponseDto = decode_page(bytes, limits()?)?;
    Ok(dto
        .data()
        .get()
        .as_slice()
        .iter()
        .map(mcp_server_row)
        .collect())
}

/// Encode a real `McpListResponseDto`, validated by the real decoder.
pub fn encode_mcp_servers(
    location: &ProjectRow,
    rows: &[McpServerRow],
) -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::McpListResponseDto;
    let data: Vec<serde_json::Value> = rows
        .iter()
        .map(|server| {
            let status = match &server.status {
                McpServerStatus::Connected => serde_json::json!({ "status": "connected" }),
                McpServerStatus::Pending => serde_json::json!({ "status": "pending" }),
                McpServerStatus::Disabled => serde_json::json!({ "status": "disabled" }),
                McpServerStatus::Failed { error } => serde_json::json!({
                    "status": "failed",
                    "error": error,
                }),
                McpServerStatus::NeedsAuth => serde_json::json!({ "status": "needs_auth" }),
                McpServerStatus::NeedsRegistration { error } => serde_json::json!({
                    "status": "needs_client_registration",
                    "error": error,
                }),
            };
            let mut value = serde_json::json!({
                "name": server.name,
                "status": status,
            });
            if let (Some(id), Some(object)) =
                (server.integration_id.as_ref(), value.as_object_mut())
            {
                object.insert(
                    "integrationID".to_owned(),
                    serde_json::Value::from(id.as_str()),
                );
            }
            value
        })
        .collect();
    let bytes = serde_json::json!({ "location": location_value(location), "data": data })
        .to_string()
        .into_bytes();
    let _validated: McpListResponseDto = decode_page(&bytes, limits()?)?;
    Ok(bytes)
}

/// Encode a frozen `InvalidRequestError` body.
///
/// The 400 the frozen route answers a refused MCP config with. The message is
/// the whole body apart from the tag: the product's own message already names
/// the faulting file, and the pane renders it as the refusal line.
pub fn encode_invalid_request(message: &str) -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::InvalidRequestErrorDto;
    let bytes = serde_json::json!({
        "_tag": "InvalidRequestError",
        "message": message,
    })
    .to_string()
    .into_bytes();
    let _validated: InvalidRequestErrorDto = decode_page(&bytes, limits()?)?;
    Ok(bytes)
}

/// Decode a frozen `InvalidRequestError` body to its message.
///
/// The single reader for a 400 body at the client's one ingress. A body that
/// is not the frozen error shape refuses here rather than arriving as a bare
/// status the pane could not show.
pub fn decode_invalid_request_message(bytes: &[u8]) -> Result<String, WireError> {
    use grapt_api::u3a::InvalidRequestErrorDto;
    let dto: InvalidRequestErrorDto = decode_page(bytes, limits()?)?;
    Ok(dto.message().get().as_str().to_owned())
}

// ---------------------------------------------------------------------------
// Forms
// ---------------------------------------------------------------------------
//
// A form is a structured question the product asks mid-run, and the frozen
// record gives it eight routes: list (globally and per session), get, state,
// reply, cancel and create. The product at the accepted base serves TWO of
// them -- `v2.form.request.list` and `v2.session.form.list` -- so this client
// can RENDER a pending form and cannot settle one.
//
// That asymmetry is carried here rather than hidden. The rows below are what
// the served routes actually return, and there is no reply encoder, because a
// reply encoder with no route to send it through would be a settlement path
// that exists in this crate and nowhere else -- which is precisely the shape
// that turns "the form rendered" into a claim that the form can be answered.

/// The form identity.
pub type FormWireId = WireId<grapt_types::FormId>;

impl Spelled for FormWireId {
    fn spelled(&self) -> &str {
        self.as_inner().as_str()
    }
}

pub fn form_id(value: &str) -> Result<FormWireId, WireError> {
    WireId::parse(value.to_owned()).ok_or_else(|| WireError::Invalid {
        what: "form id",
        value: value.to_owned(),
    })
}

/// One field of a pending form, as much of it as a terminal can show.
///
/// `Form.Field` is an OPEN object in the frozen record, validated by shape
/// rather than closed by name: a string field carries a pattern and a
/// placeholder, a multiselect carries options, an external field carries a URL.
/// Rendering all of that faithfully is a form UI; rendering the key, the type
/// and whether it is required is what tells a user WHAT is being asked and
/// whether they can skip it.
///
/// Optional field capabilities retain their canonical JSON values so a frontend
/// can display descriptions and refuse unsupported options or conditions honestly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormField {
    /// Preserve optional field capabilities for honest frontend support checks.
    pub attributes: std::collections::BTreeMap<String, serde_json::Value>,
    pub key: String,
    pub kind: String,
    pub title: Option<String>,
    pub required: bool,
}

/// Who a form belongs to.
///
/// `Form.Info.sessionID` is spelled as a plain string in the frozen record, and
/// that is DELIBERATE rather than loose: the form routes look session-scoped
/// but use a form-specific location middleware with a `global` sentinel for MCP
/// elicitation forms, which are location-scoped and not session-owned. The
/// schema's own comment says to narrow the field once elicitations are
/// attributable.
///
/// So neither obvious option is right. A raw `String` flattens an identity —
/// `tests/typed_identity.rs` refuses it, and refuses it correctly. A
/// `SessionWireId` refuses the sentinel AT THE WIRE EDGE, so a client that
/// parsed it that way would drop every elicitation form and render nothing
/// where the product had asked a question. This enum is the shape the contract
/// actually has: usually a session, sometimes something else, and never a
/// string the client can confuse for either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormOwner {
    /// A real session, parsed through the domain newtype.
    Session(SessionWireId),
    /// The exact canonical MCP elicitation sentinel; unknown owners stay unknown.
    Global,
    /// An owner this client cannot resolve to a session, carried as the product
    /// spelled it. Rendered as what it is; never used to key a session.
    Unattributed(String),
}

impl FormOwner {
    /// The session this form belongs to, when it belongs to one.
    pub fn session(&self) -> Option<&SessionWireId> {
        match self {
            FormOwner::Session(session) => Some(session),
            FormOwner::Global | FormOwner::Unattributed(_) => None,
        }
    }
}

impl Spelled for FormOwner {
    fn spelled(&self) -> &str {
        match self {
            FormOwner::Session(session) => session.spelled(),
            FormOwner::Global => "global",
            FormOwner::Unattributed(owner) => owner,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn project_row() -> ProjectRow {
        ProjectRow {
            id: project_id("prj_agents").unwrap_or_else(|error| panic!("location: {error}")),
            worktree: "/repo".to_owned(),
            name: None,
        }
    }

    #[test]
    fn the_source_layer_is_carried_when_present_and_absent_when_not() -> TestResult {
        let value = serde_json::json!({
            "location": { "directory": "/repo", "project": { "id": "prj_agents", "directory": "/repo" } },
            "data": [
                {
                    "id": "agt_one",
                    "name": "one",
                    "description": "First",
                    "request": { "settings": {}, "headers": {}, "body": {} },
                    "mode": "primary",
                    "hidden": false,
                    "permissions": [],
                    "source": "claude-project",
                },
                {
                    "id": "agt_two",
                    "name": "two",
                    "request": { "settings": {}, "headers": {}, "body": {} },
                    "mode": "primary",
                    "hidden": false,
                    "permissions": [],
                },
            ],
        });
        let rows = decode_agents(&serde_json::to_vec(&value)?)?;
        assert_eq!(rows[0].source.as_deref(), Some("claude-project"));
        assert_eq!(rows[1].source, None);
        Ok(())
    }

    #[test]
    fn encode_agents_round_trips_the_source_layer_through_the_real_dto() -> TestResult {
        let rows = vec![
            AgentRow {
                id: agent_id("agt_one")?,
                name: "one".to_owned(),
                description: Some("First".to_owned()),
                mode: grapt_api::u3a::AgentInfoDtoMode::Subagent,
                hidden: false,
                source: Some("claude-project".to_owned()),
            },
            AgentRow {
                id: agent_id("agt_two")?,
                name: "two".to_owned(),
                description: None,
                mode: grapt_api::u3a::AgentInfoDtoMode::Primary,
                hidden: true,
                source: None,
            },
        ];
        let bytes = encode_agents(&project_row(), &rows)?;
        assert_eq!(decode_agents(&bytes)?, rows);
        Ok(())
    }
}
