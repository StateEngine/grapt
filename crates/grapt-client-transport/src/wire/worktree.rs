//! Checkout management crosses the bounded public DTO boundary in both directions.
use super::*;
pub use grapt_api::u3a::WorkspaceWorktreeEffectDto as WorktreeEffect;
pub use grapt_api::u3a::WorkspaceWorktreeRequestDto as WorktreeRequest;

/// Encode checkout coordinates without exposing identity text to client state.
pub fn with_worktree_location(
    request: crate::transport::WireRequest,
    directory: &str,
    session: Option<&SessionWireId>,
) -> crate::transport::WireRequest {
    let request = request.with_query("location[directory]", directory);
    match session {
        Some(session) => request.with_query("session", session.spelled()),
        None => request,
    }
}

/// Render uncertainty while retaining the confirmed creation identity as typed.
pub fn worktree_uncertainty(created: Option<&SessionWireId>) -> String {
    const UNCERTAIN: &str = "The worktree or session-creation effect may have happened. Inspect the checkouts and Sessions before continuing; do not retry. Reopen the client only after inspection.";
    created.map_or_else(
        || UNCERTAIN.into(),
        |id| format!("Session {} was created. {UNCERTAIN}", id.spelled()),
    )
}

/// Render confirmed attachment only after the client installs typed selection.
pub fn worktree_session_selected(id: &SessionWireId) -> Vec<String> {
    vec![
        "WORKTREE SESSION".into(),
        format!(
            "New session {} selected; the previous session was not moved.",
            id.spelled()
        ),
    ]
}

/// A management receipt, before the client validates its source and effect.
#[derive(Debug)]
pub struct WorktreeReply {
    pub location: FormLocation,
    pub project_directory: String,
    pub directory: String,
    pub message: String,
    pub effect: WorktreeEffect,
}

pub fn encode_worktree(request: &WorktreeRequest) -> Result<Vec<u8>, WireError> {
    let bytes = encode_page(request, limits()?)?;
    let _: WorktreeRequest = decode_request(&bytes, limits()?)?;
    Ok(bytes)
}

pub fn decode_worktree(bytes: &[u8]) -> Result<WorktreeReply, WireError> {
    let dto: grapt_api::u3a::WorkspaceWorktreeResponseDto = decode_page(bytes, limits()?)?;
    let location = dto.location().get();
    let data = dto.data().get();
    Ok(WorktreeReply {
        location: FormLocation {
            project: location.project().get().id().get().clone(),
            directory: location.directory().get().as_str().to_owned(),
        },
        project_directory: location
            .project()
            .get()
            .directory()
            .get()
            .as_str()
            .to_owned(),
        directory: data.directory().get().as_str().to_owned(),
        message: data.message().get().as_str().to_owned(),
        effect: *data.effect().get(),
    })
}

impl WorktreeReply {
    pub fn completed_lines(&self, request: &WorktreeRequest) -> Result<Vec<String>, WireError> {
        let mut lines = vec!["WORKTREE".into(), self.message.clone()];
        lines.extend(worktree_request_context(&self.location, request));
        if self.effect == WorktreeEffect::Completed
            && matches!(request, WorktreeRequest::Create { .. })
        {
            // Only completed create returns a new cwd. Other replies retain a
            // safe source cwd, not the requested merge/remove/error target.
            let quoted =
                serde_json::to_string(&self.directory).map_err(|error| WireError::Codec {
                    reason: error.to_string(),
                })?;
            lines.push(format!("Created backend cwd: {quoted}"));
            lines.push(format!(
                "To start a NEW session, use this absolute backend path: /worktree open {quoted}"
            ));
        }
        lines.push("Existing sessions keep their directories.".into());
        Ok(lines)
    }
}

/// Display original backend coordinates without resolving paths on the TUI host.
pub fn worktree_request_context(source: &FormLocation, request: &WorktreeRequest) -> Vec<String> {
    let target = match request {
        WorktreeRequest::Create { destination, .. } => destination.as_str(),
        WorktreeRequest::Merge { directory } | WorktreeRequest::Remove { directory } => {
            directory.as_str()
        }
    };
    vec![
        format!("Source (backend cwd): {}", quoted_path(&source.directory)),
        format!("Requested target: {}", quoted_path(target)),
        "Relative targets are interpreted relative to Source on the backend; the TUI does not resolve them.".into(),
    ]
}

fn quoted_path(path: &str) -> String {
    serde_json::Value::String(path.to_owned()).to_string()
}
