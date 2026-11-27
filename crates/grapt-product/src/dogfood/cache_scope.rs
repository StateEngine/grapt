// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: use immutable history-fork ancestry for body and header
// cache affinity, retaining actual child thread identity. See THIRD_PARTY_NOTICES.md.
//! Stable cache affinity follows retained conversation ancestry. The separate
//! thread headers remain the actual Session; diagnostic scopes are unchanged.

use std::path::Path;

use grapt_provider::PromptCacheKey;
use grapt_types::{ProjectId, ResolvedModelTarget, SessionId};

use crate::input::{read_prompt_cache_scope, PromptCacheScope};

/// The key for `session`'s requests to `target`, under the profile at
/// `service_root`.
pub(super) fn session_cache_key(
    service_root: &Path,
    inbox: &std::sync::Mutex<grapt_inbox::Inbox>,
    session: &SessionId,
    project: &ProjectId,
    target: &ResolvedModelTarget,
) -> Result<PromptCacheKey, String> {
    let own = PromptCacheKey::for_session(session).map_err(|error| error.to_string())?;
    let scope = read_prompt_cache_scope(&service_root.join("service.conf"))
        .map_err(|error| error.to_string())?;
    if scope == PromptCacheScope::Session {
        let inbox = inbox
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let origin = grapt_session::SessionStore::over(inbox.store())
            .history_fork_origin(session)
            .map_err(|e| e.to_string())?;
        // ChatGPT routes prompt caching by session-id, not only the body key.
        // Both affinity values must agree.
        // CodexTurn still supplies this Session's thread/client-request IDs.
        return PromptCacheKey::for_session(&origin).map_err(|e| e.to_string());
    }
    Ok(scoped(scope, own, project, target))
}

fn scoped(
    scope: PromptCacheScope,
    own: PromptCacheKey,
    project: &ProjectId,
    target: &ResolvedModelTarget,
) -> PromptCacheKey {
    let shared = || {
        PromptCacheKey::for_project(
            project.as_str(),
            target.provider().as_str(),
            target.model().as_str(),
        )
    };
    match scope {
        PromptCacheScope::Session => own,
        PromptCacheScope::ProjectKey => shared().with_session_header(&own),
        PromptCacheScope::ProjectKeyAndHeader => shared(),
    }
}

#[cfg(test)]
#[path = "cache_scope_tests.rs"]
mod tests;
