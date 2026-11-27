//! Product-owned permission application seam over the shared durable store.

use std::sync::MutexGuard;
pub(super) mod grants;

use grapt_api::u3a::{
    ApplicationFuture, NoContent, PermissionApplicationError, PermissionCreateRequestDto,
    PermissionCreateResponseDto, PermissionGlobalListResponseDto, PermissionListResponseDto,
    PermissionLocationQueryDto, PermissionReplyDto, PermissionReplyRequestDto,
    PermissionResponseDto, WireId,
};
use grapt_api::SealedPrincipal;
use grapt_permission::{
    DecisionIds, PermissionCreateEffect, PermissionHttpError, PermissionHttpLifecycle,
    PermissionHttpReply, PermissionHttpRequest, PermissionHttpSource, PermissionHttpState,
    PermissionOwner, PermissionReplyCommand,
};
use grapt_types::{
    AgentId, CasOutcome, ClockAuthority, DecisionNonce, IdAuthority, PermissionId, RequestId,
    SessionId,
};
use serde_json::{json, Value};

use super::{decode, invalid, ready, static_dto, DogfoodApplication};

pub(super) fn saved(
    application: &DogfoodApplication,
    project: Option<grapt_types::ProjectId>,
) -> Result<grapt_api::u3a::PermissionSavedListResponseDto, grapt_api::u3a::InvalidRequestErrorDto>
{
    use base64::Engine as _;
    let inbox = application.lock_inbox();
    let rules =
        PermissionHttpState::saved_rules(inbox.store(), project.as_ref()).map_err(invalid)?;
    let data = rules.into_iter().map(|rule| {
        // Domain identity is the unique replayable (project, action, resource)
        // tuple. JSON array framing prevents concatenation collisions; the
        // public alias remains stable across store replay and service restart.
        let tuple = serde_json::to_vec(&(rule.project.as_str(), &rule.action, &rule.resource)).map_err(invalid)?;
        let digest = ring::digest::digest(&ring::digest::SHA256, &tuple);
        let id = format!("psv_{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest.as_ref()));
        Ok(json!({"id":id, "projectID":rule.project.as_str(), "action":rule.action, "resource":rule.resource}))
    }).collect::<Result<Vec<_>, grapt_api::u3a::InvalidRequestErrorDto>>()?;
    decode(json!({"data":data})).map_err(invalid)
}

pub(super) fn list(
    application: &DogfoodApplication,
    principal: SealedPrincipal,
    query: PermissionLocationQueryDto,
) -> ApplicationFuture<PermissionGlobalListResponseDto, PermissionApplicationError> {
    let application = application.clone();
    Box::pin(async move {
        let location = super::locations::resolve(&application.catalog_snapshot(), &query)
            .await
            .map_err(|error| PermissionApplicationError::InvalidRequest(invalid(error)))?;
        // The frozen global request list reads pending/suspended requests from
        // the store, irrespective of their session directory. Keep the Rust
        // authenticated owner filter; selected location is the response envelope.
        let response = list_for(&application, &principal, None)?;
        decode(json!({"location":location.wire(), "data":response.data().get(), "hasMore":response.has_more().get()}))
            .map_err(|error| PermissionApplicationError::InvalidRequest(invalid(error)))
    })
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn create(
    application: &DogfoodApplication,
    principal: SealedPrincipal,
    session_id: WireId<SessionId>,
    request: PermissionCreateRequestDto,
) -> ApplicationFuture<PermissionCreateResponseDto, PermissionApplicationError> {
    let session = session_id.into_inner();
    let record = match application.get_record(&session) {
        Ok(record) => record,
        Err(_) => return ready(Err(session_not_found(&session))),
    };
    let value = match serde_json::to_value(request) {
        Ok(value) => value,
        Err(error) => {
            return ready(Err(PermissionApplicationError::InvalidRequest(invalid(
                error,
            ))))
        }
    };
    let id = match value.get("id").and_then(Value::as_str) {
        Some(raw) => match raw.parse() {
            Ok(id) => id,
            Err(_) => {
                return ready(Err(PermissionApplicationError::InvalidRequest(invalid(
                    "permission id is invalid",
                ))))
            }
        },
        None => match application.mint_permission() {
            Ok(id) => id,
            Err(error) => {
                return ready(Err(PermissionApplicationError::InvalidRequest(invalid(
                    error,
                ))))
            }
        },
    };
    let Some(action) = value.get("action").and_then(Value::as_str) else {
        return ready(Err(PermissionApplicationError::InvalidRequest(invalid(
            "permission action is absent",
        ))));
    };
    let agent = match permission_agent(value.get("agent"), record.agent.as_deref()) {
        Ok(agent) => agent,
        Err(error) => {
            return ready(Err(PermissionApplicationError::InvalidRequest(invalid(
                error,
            ))))
        }
    };
    let policy = match super::permission_preset::effective(
        std::path::Path::new(&application.catalog_snapshot().directory),
        &record,
        |id| application.get_record(id),
    ) {
        Ok(policy) => policy,
        Err(error) => {
            return ready(Err(PermissionApplicationError::InvalidRequest(invalid(
                error,
            ))))
        }
    };
    let resources = match json_strings(value.get("resources")) {
        Ok(resources) => resources,
        Err(error) => {
            return ready(Err(PermissionApplicationError::InvalidRequest(invalid(
                error,
            ))))
        }
    };
    let checks = resources
        .iter()
        .map(|resource| grapt_permission::PermissionConsequence {
            action: action.to_owned(),
            resource: resource.clone(),
        })
        .collect::<Vec<_>>();
    let layers = policy.layers(
        configured_agent_allows(&agent, action).then_some(action),
        &checks,
    );
    let save = match value.get("save") {
        Some(value) => match json_strings(Some(value)) {
            Ok(save) => Some(save),
            Err(error) => {
                return ready(Err(PermissionApplicationError::InvalidRequest(invalid(
                    error,
                ))))
            }
        },
        None => None,
    };
    let source = match permission_source(value.get("source")) {
        Ok(source) => source,
        Err(error) => {
            return ready(Err(PermissionApplicationError::InvalidRequest(invalid(
                error,
            ))))
        }
    };
    let event = match application.mint_event() {
        Ok(event) => event,
        Err(error) => {
            return ready(Err(PermissionApplicationError::InvalidRequest(invalid(
                error,
            ))))
        }
    };
    let owner = permission_owner(&principal);
    let created_ms = application.clock.now().as_millis();
    let result = {
        let mut permissions = application.lock_permissions();
        let mut inbox = application.lock_inbox();
        let result = permissions.create(
            inbox.store_mut(),
            &owner,
            &event,
            &id,
            &session,
            &agent,
            &layers,
            &application.catalog_snapshot().project,
            action,
            &resources,
            save.as_deref(),
            value.get("metadata"),
            source.as_ref(),
            created_ms,
        );
        if matches!(result, Ok(PermissionCreateEffect::Ask)) {
            super::input_events::publish_id(&inbox, &application.hub, &session, &event);
        }
        result
    };
    ready(match result {
        Ok(effect) => {
            let effect = match effect {
                PermissionCreateEffect::Ask => "ask",
                PermissionCreateEffect::Allow => "allow",
                PermissionCreateEffect::Deny => "deny",
            };
            decode(json!({"data": {"id": id.as_str(), "effect": effect}}))
                .map_err(|error| PermissionApplicationError::InvalidRequest(invalid(error)))
        }
        Err(error) => Err(map_permission_error(error, Some(&id))),
    })
}

pub(super) fn session_list(
    application: &DogfoodApplication,
    principal: SealedPrincipal,
    session_id: WireId<SessionId>,
) -> ApplicationFuture<PermissionListResponseDto, PermissionApplicationError> {
    let session = session_id.into_inner();
    ready(list_for(application, &principal, Some(&session)))
}

pub(super) fn get(
    application: &DogfoodApplication,
    principal: SealedPrincipal,
    session_id: WireId<SessionId>,
    request_id: WireId<PermissionId>,
) -> ApplicationFuture<PermissionResponseDto, PermissionApplicationError> {
    let session = session_id.into_inner();
    if application.get_record(&session).is_err() {
        return ready(Err(session_not_found(&session)));
    }
    let id = request_id.into_inner();
    let owner = permission_owner(&principal);
    let result = {
        let permissions = application.lock_permissions();
        let inbox = application.lock_inbox();
        permissions.get(inbox.store(), &owner, &session, &id)
    };
    ready(match result {
        Ok(row) => decode(json!({"data": permission_json(&row)}))
            .map_err(|error| PermissionApplicationError::InvalidRequest(invalid(error))),
        Err(error) => Err(map_permission_error(error, Some(&id))),
    })
}

pub(super) fn reply(
    application: &DogfoodApplication,
    principal: SealedPrincipal,
    session_id: WireId<SessionId>,
    request_id: WireId<PermissionId>,
    request: PermissionReplyRequestDto,
) -> ApplicationFuture<NoContent, PermissionApplicationError> {
    let session = session_id.into_inner();
    let record = match application.get_record(&session) {
        Ok(record) => record,
        Err(_) => return ready(Err(session_not_found(&session))),
    };
    let id = request_id.into_inner();
    let reply = match request.reply().get() {
        PermissionReplyDto::Once => PermissionHttpReply::Once,
        PermissionReplyDto::Session => PermissionHttpReply::Session,
        PermissionReplyDto::Always => PermissionHttpReply::Always,
        PermissionReplyDto::Reject => PermissionHttpReply::Reject,
    };
    let owner = permission_owner(&principal);
    let created_ms = application.clock.now().as_millis();
    // The retained client supplies the permission ID in the route and only
    // the choice in the body. Internal CAS identities belong to this adapter.
    let identities = match (
        request.request_id().as_present(),
        request.nonce().as_present(),
    ) {
        (None, None) => application.mint_decision_ids(),
        (Some(request), Some(nonce)) => application.mint_event().map(|event| DecisionIds {
            event,
            request: request.as_inner().clone(),
            nonce: nonce.as_inner().clone(),
        }),
        _ => Err("permission decision identifiers must both be present or both be absent".into()),
    };
    let decision = match identities {
        Ok(decision) => decision,
        Err(error) => {
            return ready(Err(PermissionApplicationError::InvalidRequest(invalid(
                error,
            ))))
        }
    };
    let decision_event = decision.event.clone();
    let result = {
        let mut permissions = application.lock_permissions();
        let mut inbox = application.lock_inbox();
        application
            .hub
            .with_permission_approval(&principal, &session, &id, |attached| {
                if attached == Some(false) {
                    if let Some(committed) = permissions.cancel_tool_because(
                        inbox.store_mut(),
                        &owner,
                        &session,
                        &id,
                        &decision_event,
                        created_ms,
                        grapt_permission::CancelReason::ApprovalUnavailable,
                    )? {
                        application.tool_permissions.cancel(committed);
                        return Err(PermissionHttpError::ApprovalUnavailable);
                    }
                    // A durable decision that won before disconnect keeps its
                    // ordinary exact-retry semantics; cancellation cannot undo it.
                }
                permissions.reply_tools(
                    inbox.store_mut(),
                    &application.tool_permissions,
                    PermissionReplyCommand {
                        owner,
                        session: &session,
                        id: &id,
                        reply,
                        project: Some(&record.project),
                        created_ms,
                        decision,
                    },
                    || {
                        application
                            .mint_decision_ids()
                            .map_err(PermissionHttpError::Identity)
                    },
                )
            })
            .unwrap_or_else(|error| Err(PermissionHttpError::Replay(error.to_string())))
    };
    ready(match result {
        Ok(effect) => match effect.outcome() {
            CasOutcome::Applied => {
                let inbox = application.lock_inbox();
                super::input_events::publish_id(
                    &inbox,
                    &application.hub,
                    &session,
                    &decision_event,
                );
                Ok(NoContent)
            }
            CasOutcome::AlreadyTerminal => Err(permission_already_settled(&id)),
            CasOutcome::NotLive => Err(permission_not_found(&id)),
        },
        Err(error) => Err(map_permission_error(error, Some(&id))),
    })
}

impl DogfoodApplication {
    fn mint_permission(&self) -> Result<PermissionId, String> {
        format!("per_{}", self.ids.mint().as_str())
            .parse()
            .map_err(|_| "permission identity was invalid".to_owned())
    }

    fn mint_decision_ids(&self) -> Result<DecisionIds, String> {
        let event = self.mint_event()?;
        let request = RequestId::new(format!("req_{}", self.ids.mint().as_str()))
            .ok_or_else(|| "permission reply request identity was invalid".to_owned())?;
        let nonce = DecisionNonce::new(format!("nonce_{}", self.ids.mint().as_str()))
            .ok_or_else(|| "permission reply nonce was invalid".to_owned())?;
        Ok(DecisionIds {
            event,
            request,
            nonce,
        })
    }

    fn lock_permissions(&self) -> MutexGuard<'_, PermissionHttpState> {
        match self.permissions.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

fn list_for(
    application: &DogfoodApplication,
    principal: &SealedPrincipal,
    session: Option<&SessionId>,
) -> Result<PermissionListResponseDto, PermissionApplicationError> {
    let owner = permission_owner(principal);
    let page = polling_page(application, &owner, session)?;
    decode(json!({
        "data": page.requests.iter().map(permission_json).collect::<Vec<_>>(),
        "hasMore": page.has_more,
    }))
    .map_err(|error| PermissionApplicationError::InvalidRequest(invalid(error)))
}

/// One session's pending requests read from the store alone (the snapshot's
/// lease), never from the published generation and never under the writer's
/// permission lock, typed by the same decode the permission list route uses.
pub(super) fn pinned_rows(
    store: &grapt_store::EventStore,
    principal: &SealedPrincipal,
    session: &SessionId,
) -> Result<PermissionListResponseDto, String> {
    let owner = permission_owner(principal);
    let page = PermissionHttpState::list_from(store, &owner, Some(session))
        .map_err(|error| error.to_string())?;
    if page.has_more {
        return Err(format!(
            "session {session} has more pending permissions than one snapshot carries"
        ));
    }
    decode(json!({
        "data": page.requests.iter().map(permission_json).collect::<Vec<_>>(),
        "hasMore": false,
    }))
}

fn polling_page(
    application: &DogfoodApplication,
    owner: &PermissionOwner<'_>,
    session: Option<&SessionId>,
) -> Result<grapt_permission::PermissionHttpPage, PermissionApplicationError> {
    if let Some(result) = application.poll_committed_permissions(owner, session) {
        return result.map_err(|error| match error {
            super::permission_view::PollError::MissingSession(session) => {
                session_not_found(&session)
            }
            super::permission_view::PollError::Permission(error) => {
                map_permission_error(error, None)
            }
        });
    }
    if let Some(session) = session {
        if application.get_record(session).is_err() {
            return Err(session_not_found(session));
        }
    }
    let result = {
        let permissions = application.lock_permissions();
        let inbox = application.lock_inbox();
        permissions.list(inbox.store(), owner, session)
    };
    result.map_err(|error| map_permission_error(error, None))
}

#[cfg(test)]
#[path = "permission_poll_tests.rs"]
mod poll_tests;

fn permission_agent(
    requested: Option<&Value>,
    session_agent: Option<&str>,
) -> Result<AgentId, String> {
    let raw = requested
        .and_then(Value::as_str)
        .or(session_agent)
        .unwrap_or("default");
    AgentId::new(raw.to_owned()).ok_or_else(|| "permission agent is invalid".to_owned())
}

/// Dogfood's immutable configured-policy witness: only the test-kit fixture
/// agent receives the configured `write` rule, which keeps requested/session
/// AgentId authority observable in tests. Shipped builds compose no fixture
/// agent; real user rules come from the shared permission-preset resolver.
fn configured_agent_allows(agent: &AgentId, action: &str) -> bool {
    super::selection::FIXTURE_AGENT_IDS.contains(&agent.as_str()) && action == "write"
}

fn permission_owner(principal: &SealedPrincipal) -> PermissionOwner<'_> {
    PermissionOwner {
        subject: principal.principal().receipt().subject().as_str(),
        client: principal.principal().client(),
    }
}

fn json_strings(value: Option<&Value>) -> Result<Vec<String>, String> {
    let Some(values) = value.and_then(Value::as_array) else {
        return Err("permission resources must be an array".to_owned());
    };
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "permission resource must be a string".to_owned())
        })
        .collect()
}

fn permission_source(value: Option<&Value>) -> Result<Option<PermissionHttpSource>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| "permission source type is absent".to_owned())?;
    if kind != "tool" {
        return Err(format!(
            "permission source {kind:?} is outside the closed vocabulary"
        ));
    }
    let message_id = value
        .get("messageID")
        .and_then(Value::as_str)
        .ok_or_else(|| "permission source messageID is absent".to_owned())?;
    let call_id = value
        .get("callID")
        .and_then(Value::as_str)
        .ok_or_else(|| "permission source callID is absent".to_owned())?;
    Ok(Some(PermissionHttpSource::Tool {
        message_id: message_id.to_owned(),
        call_id: call_id.to_owned(),
    }))
}

pub(super) fn permission_json(row: &PermissionHttpRequest) -> Value {
    let mut value = json!({
        "id": row.id.as_str(),
        "sessionID": row.session.as_str(),
        "action": row.action,
        "resources": row.resources,
    });
    if let Some(save) = &row.save {
        value["save"] = json!(save);
    }
    if let Some(metadata) = &row.metadata {
        value["metadata"] = metadata.clone();
    }
    if let Some(source) = &row.source {
        value["source"] = match source {
            PermissionHttpSource::Tool {
                message_id,
                call_id,
            } => {
                json!({"type": "tool", "messageID": message_id, "callID": call_id})
            }
        };
    }
    match row.lifecycle {
        PermissionHttpLifecycle::Pending => value["status"] = json!("pending"),
        PermissionHttpLifecycle::Suspended(reason) => {
            value["status"] = json!("suspended");
            value["suspendedReason"] = json!(match reason {
                grapt_types::PermissionSuspensionReason::Shutdown => "shutdown",
                grapt_types::PermissionSuspensionReason::ProcessLoss => "process-loss",
            });
        }
    }
    include_preview(&mut value);
    value
}

/// HTTP, live SSE and capture replay expose the same stored display context.
/// Legacy metadata was arbitrary: a malformed display value must not make an
/// otherwise valid stored permission impossible to answer.
pub(super) fn include_preview(value: &mut Value) {
    if let Some(preview) = value.pointer("/metadata/request/approvalPreview").cloned() {
        value["preview"] = if serde_json::from_value::<grapt_api::u3a::PermissionPreviewDto>(
            preview.clone(),
        )
        .is_ok()
            && serde_json::to_vec(&preview)
                .is_ok_and(|bytes| bytes.len() <= grapt_api::u3a::MAX_PERMISSION_PREVIEW_BYTES)
        {
            preview
        } else {
            json!({"type":"unavailable", "reason":"Stored approval preview is invalid; inspect the requested resources."})
        };
    }
}

fn map_permission_error(
    error: PermissionHttpError,
    request: Option<&PermissionId>,
) -> PermissionApplicationError {
    match error {
        PermissionHttpError::ApprovalUnavailable => PermissionApplicationError::InvalidRequest(
            invalid("approval unavailable (headless): no matching approver is attached"),
        ),
        PermissionHttpError::NotFound(id)
        | PermissionHttpError::WrongPrincipal(id)
        | PermissionHttpError::WrongClient(id) => permission_not_found(&id),
        PermissionHttpError::NotLive { id, reason } => permission_not_live(&id, reason),
        PermissionHttpError::Domain(grapt_permission::PermissionError::InvalidAction {
            action,
        }) => PermissionApplicationError::InvalidRequest(invalid(format!(
            "invalid permission action {action:?}"
        ))),
        other => match grapt_api::u3a::ApplicationUnavailable::try_new("permission") {
            Ok(unavailable) => PermissionApplicationError::Unavailable(unavailable),
            Err(_) => match request {
                Some(id) => permission_not_found(id),
                None => PermissionApplicationError::InvalidRequest(invalid(other.to_string())),
            },
        },
    }
}

fn permission_not_found(id: &PermissionId) -> PermissionApplicationError {
    PermissionApplicationError::PermissionNotFound(static_dto(json!({
        "_tag": "PermissionNotFoundError",
        "requestID": id.as_str(),
        "message": "Permission was not found",
    })))
}

fn permission_not_live(
    id: &PermissionId,
    reason: grapt_types::PermissionSuspensionReason,
) -> PermissionApplicationError {
    let reason = match reason {
        grapt_types::PermissionSuspensionReason::Shutdown => "shutdown",
        grapt_types::PermissionSuspensionReason::ProcessLoss => "process-loss",
    };
    PermissionApplicationError::PermissionNotLive(static_dto(json!({
        "_tag": "PermissionNotLiveError",
        "requestID": id.as_str(),
        "reason": reason,
        "message": format!("Permission request is suspended ({reason}): {id}"),
    })))
}

fn permission_already_settled(id: &PermissionId) -> PermissionApplicationError {
    // The frozen error union has one 409 member and its reason vocabulary is
    // suspension-only. Preserve that wire contract instead of inventing a new
    // terminal reason; the message carries the exact already-settled fact.
    PermissionApplicationError::PermissionNotLive(static_dto(json!({
        "_tag": "PermissionNotLiveError",
        "requestID": id.as_str(),
        "reason": "shutdown",
        "message": format!("Permission request already settled: {id}"),
    })))
}

fn session_not_found(session: &SessionId) -> PermissionApplicationError {
    PermissionApplicationError::SessionNotFound(static_dto(json!({
        "_tag": "SessionNotFoundError",
        "sessionID": session.as_str(),
        "message": format!("Session not found: {session}"),
    })))
}
