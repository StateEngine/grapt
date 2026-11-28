//! Windows Git compatibility preflight. Detection is read-only; only a native
//! user-consent route may create the missing file, outside process containment.
//! Git probes reuse the bounded discovery adapter.
//! Copyright 2025 OpenAI. Apache-2.0; see licenses/openai-codex/
use super::*;
use std::path::PathBuf;

pub(crate) const WARNING: &str = "Windows sandbox Git worktree config warning: extensions.worktreeConfig=true needs a config.worktree file, but it is missing. The sandbox will not create it and Git may fail with Permission denied. Create an empty config.worktree outside the sandbox yourself (find its path with git rev-parse --git-path config.worktree), or preview /sandbox worktree-config and explicitly consent with /sandbox worktree-config create confirm. The deny policy is unchanged.";

/// Ask Git, rather than parsing config syntax, includes, booleans or worktree
/// pointers ourselves. No hooks, shell source, network or mutating Git command.
async fn missing(root: &Path) -> Result<Option<PathBuf>, String> {
    if locations::repository_root(root)?.is_none() {
        return Ok(None);
    }
    let (status, value) = locations::git(
        root,
        &["config", "--bool", "--get", "extensions.worktreeConfig"],
    )
    .await?;
    if status == 1 || (status == 0 && value.trim() == "false") {
        return Ok(None);
    }
    if status != 0 || value.trim() != "true" {
        return Err("Git worktreeConfig setting could not be read".into());
    }
    let (status, raw) = locations::git(
        root,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "config.worktree",
        ],
    )
    .await?;
    let raw = raw.trim_end_matches(['\r', '\n']);
    if status != 0 || raw.is_empty() || raw.len() > 4096 || raw.chars().any(char::is_control) {
        return Err("Git config.worktree path is invalid".into());
    }
    let path = PathBuf::from(raw);
    if !path.is_absolute() || path.file_name() != Some(std::ffi::OsStr::new("config.worktree")) {
        return Err("Git config.worktree path is not absolute".into());
    }
    let parent = path
        .parent()
        .ok_or("Git config.worktree has no directory")?;
    let parent = std::fs::canonicalize(parent)
        .map_err(|_| "Git config.worktree directory is unavailable")?;
    let path = parent.join("config.worktree");
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Some(path)),
        Ok(metadata) if metadata.is_file() && !metadata.is_symlink() => Ok(None),
        Ok(_) => Err("Git config.worktree is not a regular file".into()),
        Err(_) => Err("Git config.worktree could not be checked".into()),
    }
}

pub(crate) fn record(
    inbox: &Mutex<Inbox>,
    clock: &dyn ClockAuthority,
    ids: &dyn IdAuthority,
    hub: &DogfoodEventHub,
    session: &SessionId,
    text: Option<&str>,
) -> Result<(), String> {
    let mut held = inbox
        .lock()
        .map_err(|_| "sandbox notice store is unavailable")?;
    let authorities = DeterministicAuthorities::new(clock, ids);
    let notice = grapt_session::notice::record_sandbox_worktree_config(
        held.store_mut(),
        &authorities,
        session,
        text,
    )?;
    drop(held);
    if notice.is_some() {
        hub.reconcile_because(
            grapt_api::ReconcileReason::UnpublishedAdvisoryNotice,
            &format!("session {session}: sandbox worktree config notice changed"),
        );
    }
    Ok(())
}

pub(crate) async fn observe(application: &ServerApplication, session: &SessionId, root: &Path) {
    let result = inspect(root, Path::new(&application.catalog_snapshot().directory)).await;
    let warning = match result {
        Ok(Some(_)) => Some(WARNING.to_owned()),
        Ok(None) => None,
        Err(error) => Some(format!(
            "Windows sandbox Git worktree config check unavailable: {error}"
        )),
    };
    if let Err(error) = application.ensure_session_writer(session).and_then(|()| {
        record(
            &application.inbox,
            &*application.clock,
            &*application.ids,
            &application.hub,
            session,
            warning.as_deref(),
        )
    }) {
        crate::diagnostics::report(format!(
            "session {session}: sandbox worktree config warning could not be recorded: {error}"
        ));
    }
}

pub(crate) async fn before_command(
    root: &Path,
    inbox: &Mutex<Inbox>,
    clock: &(dyn ClockAuthority + Send + Sync),
    ids: &(dyn IdAuthority + Send + Sync),
    hub: &DogfoodEventHub,
    session: &SessionId,
) {
    if !cfg!(windows) {
        return;
    }
    let result = missing(root).await;
    let warning = match result {
        Ok(Some(_)) => Some(WARNING.to_owned()),
        Ok(None) => None,
        Err(error) => Some(format!(
            "Windows sandbox Git worktree config check unavailable: {error}"
        )),
    };
    if let Err(error) = record(inbox, clock, ids, hub, session, warning.as_deref()) {
        crate::diagnostics::report(format!(
            "session {session}: sandbox worktree config warning could not be recorded: {error}"
        ));
    }
}

async fn inspect(root: &Path, service: &Path) -> Result<Option<PathBuf>, String> {
    if !tools::sandbox::windows_enabled(root, service)? {
        return Ok(None);
    }
    missing(root).await
}

fn response(
    path: Option<PathBuf>,
    text: &str,
) -> Result<grapt_api::u3a::SandboxWorktreeConfigResponseDto, SessionLookupApplicationError> {
    let mut data = json!({"missing":path.is_some(), "text":text});
    if let Some(path) = path {
        data["path"] =
            json!(path
                .to_str()
                .ok_or_else(|| SessionLookupApplicationError::InvalidRequest(invalid(
                    "Git path is not UTF-8"
                )))?);
    }
    decode(json!({"data":data}))
        .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))
}

pub(crate) fn get(
    application: ServerApplication,
    session: SessionId,
) -> ApplicationFuture<
    grapt_api::u3a::SandboxWorktreeConfigResponseDto,
    SessionLookupApplicationError,
> {
    Box::pin(async move {
        let record = context::session_record(&application, &session)?;
        let root = Path::new(record.placement.directory());
        let path = inspect(root, Path::new(&application.catalog_snapshot().directory))
            .await
            .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))?;
        observe(&application, &session, root).await;
        let text = if path.is_some() {
            WARNING
        } else {
            "No missing config.worktree warning for this session's current sandbox configuration."
        };
        response(path, text)
    })
}

async fn create_missing(
    root: &Path,
    request: grapt_api::u3a::SandboxWorktreeConfigRequestDto,
) -> Result<(), String> {
    let value = serde_json::to_value(request).map_err(|e| e.to_string())?;
    if value["consent"].as_bool() != Some(true) {
        return Err(
            "Creating config.worktree outside the sandbox requires explicit consent".into(),
        );
    }
    let path = missing(root)
        .await?
        .ok_or("No missing config.worktree to create; preview again")?;
    if path.to_str() != value["path"].as_str() {
        return Err("Git config.worktree path changed; preview again before consenting".into());
    }
    // Never create parent directories, truncate an existing file or follow a
    // pre-existing leaf symlink. No model tool calls this user-consent route.
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .and_then(|file| file.sync_all())
        .map_err(|e| format!("Could not create empty config.worktree outside the sandbox: {e}"))?;
    if missing(root).await?.is_some() {
        return Err(
            "config.worktree is still missing after creation; re-check the repository".into(),
        );
    }
    Ok(())
}

pub(crate) fn create(
    application: ServerApplication,
    session: SessionId,
    request: grapt_api::u3a::SandboxWorktreeConfigRequestDto,
) -> ApplicationFuture<
    grapt_api::u3a::SandboxWorktreeConfigResponseDto,
    SessionLookupApplicationError,
> {
    Box::pin(async move {
        let record = context::session_record(&application, &session)?;
        let root = Path::new(record.placement.directory());
        if !tools::sandbox::windows_enabled(
            root,
            Path::new(&application.catalog_snapshot().directory),
        )
        .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))?
        {
            return Err(SessionLookupApplicationError::InvalidRequest(invalid(
                "Windows sandbox is not enabled for this session",
            )));
        }
        create_missing(root, request)
            .await
            .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))?;
        observe(&application, &session, root).await;
        response(None, "Created empty config.worktree outside the sandbox after explicit consent and re-checked it. The deny policy is unchanged.")
    })
}

#[cfg(test)]
#[path = "sandbox_worktree_config_tests.rs"]
mod tests;
