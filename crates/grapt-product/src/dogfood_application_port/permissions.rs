use grapt_api::u3a::{
    ApplicationFuture, InvalidRequestErrorDto, NoContent, PermissionApplicationError,
    PermissionCreateRequestDto, PermissionCreateResponseDto, PermissionGlobalListResponseDto,
    PermissionListResponseDto, PermissionLocationQueryDto, PermissionReplyRequestDto,
    PermissionResponseDto, PermissionSavedListResponseDto, SandboxGrantDto,
    SandboxGrantListResponseDto, SandboxGrantRevokeResponseDto, WireId,
};
use grapt_api::SealedPrincipal;
use grapt_types::{PermissionId, ProjectId, SessionId};

use crate::dogfood::support::ready;
use crate::dogfood::{permission, DogfoodApplication};

pub(super) fn sandbox_grant_list(
    application: &DogfoodApplication,
    session: WireId<SessionId>,
) -> ApplicationFuture<SandboxGrantListResponseDto, PermissionApplicationError> {
    ready(permission::grants::list(application, session.into_inner()))
}

pub(super) fn sandbox_grant_revoke(
    application: &DogfoodApplication,
    principal: SealedPrincipal,
    session: WireId<SessionId>,
    request: SandboxGrantDto,
) -> ApplicationFuture<SandboxGrantRevokeResponseDto, PermissionApplicationError> {
    permission::grants::revoke(application, principal, session.into_inner(), request)
}

pub(super) fn permission_saved_list(
    application: &DogfoodApplication,
    project: Option<WireId<ProjectId>>,
) -> ApplicationFuture<PermissionSavedListResponseDto, InvalidRequestErrorDto> {
    ready(permission::saved(
        application,
        project.map(WireId::into_inner),
    ))
}

pub(super) fn permission_list(
    application: &DogfoodApplication,
    principal: SealedPrincipal,
    query: PermissionLocationQueryDto,
) -> ApplicationFuture<PermissionGlobalListResponseDto, PermissionApplicationError> {
    permission::list(application, principal, query)
}

pub(super) fn session_permission_create(
    application: &DogfoodApplication,
    principal: SealedPrincipal,
    session_id: WireId<SessionId>,
    request: PermissionCreateRequestDto,
) -> ApplicationFuture<PermissionCreateResponseDto, PermissionApplicationError> {
    permission::create(application, principal, session_id, request)
}

pub(super) fn session_permission_list(
    application: &DogfoodApplication,
    principal: SealedPrincipal,
    session_id: WireId<SessionId>,
) -> ApplicationFuture<PermissionListResponseDto, PermissionApplicationError> {
    permission::session_list(application, principal, session_id)
}

pub(super) fn session_permission_get(
    application: &DogfoodApplication,
    principal: SealedPrincipal,
    session_id: WireId<SessionId>,
    request_id: WireId<PermissionId>,
) -> ApplicationFuture<PermissionResponseDto, PermissionApplicationError> {
    permission::get(application, principal, session_id, request_id)
}

pub(super) fn session_permission_reply(
    application: &DogfoodApplication,
    principal: SealedPrincipal,
    session_id: WireId<SessionId>,
    request_id: WireId<PermissionId>,
    request: PermissionReplyRequestDto,
) -> ApplicationFuture<NoContent, PermissionApplicationError> {
    permission::reply(application, principal, session_id, request_id, request)
}
