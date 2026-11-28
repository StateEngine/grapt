//! Resource consent uses the permission owner; process cleanup uses its existing registry.
use super::*;
use grapt_api::u3a::{
    SandboxGrantDto, SandboxGrantListResponseDto, SandboxGrantRevokeResponseDto,
    SandboxGrantScopeDto,
};
use grapt_permission::{
    PermissionRevokeRuleCommand, PermissionRuleWithdrawal, SavedPermissionRule,
    SANDBOX_WRITE_ACTION,
};

fn failure(error: impl std::fmt::Display) -> PermissionApplicationError {
    PermissionApplicationError::InvalidRequest(invalid(error))
}

fn session_record(
    application: &ServerApplication,
    session: &SessionId,
) -> Result<grapt_session::SessionRecord, PermissionApplicationError> {
    let inbox = application.lock_inbox();
    grapt_session::SessionStore::over(inbox.store())
        .record(session)
        .map_err(|error| match error {
            grapt_session::SessionStoreError::Unknown { .. } => session_not_found(session),
            error => failure(error),
        })
}

fn terminals(
    application: &ServerApplication,
) -> std::sync::Arc<super::super::tools::TerminalRegistry> {
    application.executor.0.terminals()
}

pub(crate) fn list(
    application: &ServerApplication,
    session: SessionId,
) -> Result<SandboxGrantListResponseDto, PermissionApplicationError> {
    let record = session_record(application, &session)?;
    let mut data = Vec::new();
    {
        let permissions = application.lock_permissions();
        let inbox = application.lock_inbox();
        for resource in permissions
            .session_rule_resources(&session, SANDBOX_WRITE_ACTION)
            .map_err(failure)?
        {
            data.push(json!({"scope":"session","resource":resource}));
        }
        for resource in PermissionHttpState::saved_rule_resources(
            inbox.store(),
            &record.project,
            SANDBOX_WRITE_ACTION,
        )
        .map_err(failure)?
        {
            data.push(json!({"scope":"project","resource":resource}));
        }
    }
    for grant in terminals(application)
        .running_write_grants(&session)
        .map_err(failure)?
    {
        data.push(
            json!({"scope":"run","resource":grant.resource,"operation":grant.operation.as_str()}),
        );
    }
    if data.len() > grapt_api::u3a::MAX_SANDBOX_GRANTS {
        return Err(failure("Sandbox grant list exceeds its bound"));
    }
    decode(json!({"data":data})).map_err(failure)
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(crate) fn revoke(
    application: &ServerApplication,
    principal: SealedPrincipal,
    session: SessionId,
    request: SandboxGrantDto,
) -> ApplicationFuture<SandboxGrantRevokeResponseDto, PermissionApplicationError> {
    let application = application.clone();
    Box::pin(async move {
        let record = session_record(&application, &session)?;
        let scope = *request.scope().get();
        let resource = request.resource().get().as_str();
        let operation = request
            .operation()
            .as_present()
            .map(|id| id.as_inner().clone());
        if resource.is_empty()
            || resource.len() > 8192
            || resource.contains('\0')
            || (scope == SandboxGrantScopeDto::Run) != operation.is_some()
        {
            return Err(failure(
                "Revoke needs an exact resource and an operation only for run scope",
            ));
        }
        application
            .ensure_session_writer(&session)
            .map_err(failure)?;
        let owner = permission_owner(&principal);
        let withdrawal = (|| -> Result<bool, PermissionApplicationError> {
            let mut permissions = application.lock_permissions();
            let mut inbox = application.lock_inbox();
            let mut more = false;
            match scope {
                SandboxGrantScopeDto::Session => {
                    permissions.revoke_session_rule(&session, SANDBOX_WRITE_ACTION, resource)
                }
                SandboxGrantScopeDto::Project => {
                    let event = application.mint_event().map_err(failure)?;
                    let rule = SavedPermissionRule {
                        project: record.project.clone(),
                        action: SANDBOX_WRITE_ACTION.into(),
                        resource: resource.into(),
                    };
                    let withdrawal = permissions
                        .revoke_saved_rule(
                            inbox.store_mut(),
                            PermissionRevokeRuleCommand {
                                owner: permission_owner(&principal),
                                session: &session,
                                rule: &rule,
                                event: &event,
                                created_ms: application.clock.now().as_millis(),
                            },
                        )
                        .map_err(failure)?;
                    more = withdrawal == PermissionRuleWithdrawal::Pending;
                    if withdrawal != PermissionRuleWithdrawal::Absent {
                        super::super::input_events::publish_id(
                            &inbox,
                            &application.hub,
                            &session,
                            &event,
                        );
                    }
                }
                SandboxGrantScopeDto::Run => {}
            }
            let asks = permissions
                .pending_sandbox_grants(
                    inbox.store(),
                    &owner,
                    &record.project,
                    (scope != SandboxGrantScopeDto::Project).then_some(&session),
                    resource,
                )
                .map_err(failure)?;
            more |= asks.len() == 128;
            for (permission, held_session) in asks {
                let event = application.mint_event().map_err(failure)?;
                if let Some(cancelled) = permissions
                    .cancel_tool(
                        inbox.store_mut(),
                        &owner,
                        &held_session,
                        &permission,
                        &event,
                        application.clock.now().as_millis(),
                    )
                    .map_err(failure)?
                {
                    application.tool_permissions.cancel(cancelled);
                    super::super::input_events::publish_id(
                        &inbox,
                        &application.hub,
                        &held_session,
                        &event,
                    );
                }
            }
            Ok(more)
        })();
        let cleanup = terminals(&application)
            .revoke_write_grant(
                &record.project,
                (scope != SandboxGrantScopeDto::Project).then_some(&session),
                resource,
                operation.as_ref(),
            )
            .await
            .map_err(failure);
        let pending = withdrawal?;
        cleanup?;
        decode(json!({"state":if pending {"pending"} else {"complete"},
            "message":if pending {"Withdrawal is pending; consent may remain active. Continue revocation."}
                else {"Consent withdrawn; affected commands stopped."}})).map_err(failure)
    })
}
