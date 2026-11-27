// Copyright 2025 OpenAI. Licensed under the Apache License, Version 2.0.
// Modified by Grapt.
//! Re-admission of an evicted, unsent MCP route from current Session config.
use super::*;

pub(super) async fn readmit(
    registry: &crate::dogfood::mcp::McpRegistry,
    location: &str,
    route: &crate::dogfood::mcp::McpRoute,
    forms: &crate::dogfood::elicitation::FormProducer,
    session: &SessionId,
    service: Option<&Path>,
    budget: Duration,
) -> Result<Duration, crate::dogfood::mcp::McpCallRefusal> {
    use crate::dogfood::{connections::McpSnapshot, mcp::McpCallRefusal as E, session_config};
    let changed = || E::ConnectionReplaced {
        approved: crate::dogfood::mcp::describe_launch(&route.launch),
        current: "current Session MCP configuration is unavailable or changed".into(),
    };
    let started = tokio::time::Instant::now();
    let service = service.ok_or_else(changed)?;
    let placement = {
        let inbox = forms.inbox.lock().map_err(|_| changed())?;
        session_config::validate_registry_key(&inbox, session, location).map_err(|_| changed())?;
        grapt_session::SessionStore::new(inbox.store().connection())
            .record(session)
            .map_err(|_| changed())?
            .placement
    };
    let roots_revision = registry.roots_revision().map_err(|_| changed())?;
    let sources = session_config::sources(
        &forms.inbox,
        session,
        service,
        Path::new(placement.directory()),
    )
    .map_err(|_| changed())?;
    let snapshot = McpSnapshot::read(&sources, placement.directory()).map_err(|_| changed())?;
    snapshot
        .prepare_roots(registry, location, roots_revision)
        .await
        .map_err(|_| changed())?;
    // Admission owns the active deadline and the bounded startup human wait.
    // A second wall-clock timeout here would cancel that admitted human pause.
    let human_paused = registry
        .readmit_missing(
            location,
            placement.directory(),
            &route.server,
            &route.launch,
            &snapshot.declared,
            budget.saturating_sub(started.elapsed()),
        )
        .await?;
    let inbox = forms.inbox.lock().map_err(|_| changed())?;
    session_config::validate_registry_key(&inbox, session, location).map_err(|_| changed())?;
    Ok(human_paused)
}
