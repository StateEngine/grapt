// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: bounded serial startup with cumulative human-pause accounting.
// (active_time_timeout), commit bb64b894fd67158be0f874872e6d7bfe717e7368.

//! Startup owns both phase budgets; the channel owns the nested human waits.
use super::{
    channel::ServerRequestHandler, McpChannel, McpSession, StartFailure, StartPhase, StartedServer,
};
use grapt_mcp::session::Unusable;
#[cfg(test)]
use grapt_mcp::McpBounds;
pub(crate) use grapt_mcp_host::startup::start_with;
#[cfg(test)]
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};

pub(crate) fn registry_with_forms(
    auth_root: Option<&std::path::Path>,
    inbox: std::sync::Arc<std::sync::Mutex<grapt_inbox::Inbox>>,
    ids: std::sync::Arc<grapt_store::OsRandomIds>,
    clock: std::sync::Arc<grapt_store::SystemClock>,
    hub: std::sync::Arc<grapt_api::DogfoodEventHub>,
) -> Result<super::McpRegistry, String> {
    let mut registry = super::registry(clock.clone())?;
    registry.catalog_emitter = Some(super::notifications::Emitter::new(
        ids.clone(),
        clock.clone(),
        hub.clone(),
    ));
    registry.auth = auth_root
        .map(|root| super::auth::AuthManager::new(root, inbox.clone(), clock.clone()))
        .transpose()
        .map_err(|error| error.to_string())?;
    registry.forms = Some(super::super::elicitation::FormProducer {
        inbox,
        ids,
        clock,
        hub,
        // Startup is location-wide and has no Session hook owner.
        hooks: None,
    });
    Ok(registry)
}

impl super::McpRegistry {
    pub(crate) async fn start_channel<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
        &self,
        channel: &mut McpChannel<R, W>,
        session: &mut McpSession,
        timeouts: super::McpTimeouts,
        directory: &str,
        server: &str,
    ) -> Result<StartedServer, StartFailure> {
        let mut forms = self
            .forms
            .as_ref()
            .map(|producer| producer.connect_startup(directory, server))
            .transpose()
            .map_err(|detail| StartFailure {
                reason: Unusable::Failed(detail),
                phase: StartPhase::Handshake,
            })?;
        let result = start_with(
            channel,
            session,
            self.bounds,
            timeouts.startup,
            timeouts.catalog,
            &self.client_version,
            forms
                .as_mut()
                .map(|forms| forms as &mut dyn ServerRequestHandler),
        )
        .await;
        if let Some(forms) = &mut forms {
            forms.suspend().map_err(|detail| StartFailure {
                reason: Unusable::Failed(detail),
                phase: result
                    .as_ref()
                    .err()
                    .map_or(StartPhase::Discovery, |failure| failure.phase),
            })?;
        }
        result
    }
}

#[cfg(test)]
pub(crate) async fn start<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    channel: &mut McpChannel<R, W>,
    session: &mut McpSession,
    bounds: McpBounds,
    deadline: Duration,
    catalog_deadline: Duration,
    client_version: &str,
) -> Result<StartedServer, StartFailure> {
    start_with(
        channel,
        session,
        bounds,
        deadline,
        catalog_deadline,
        client_version,
        None,
    )
    .await
}

#[cfg(test)]
pub(crate) struct RefuseStartupElicitation;

#[cfg(test)]
impl ServerRequestHandler for RefuseStartupElicitation {
    fn answer<'a>(
        &'a mut self,
        _: &'a grapt_mcp::incoming::ServerRequest,
        _: Option<&'a grapt_form::global::CallDispatch>,
    ) -> grapt_runtime::BoxFuture<'a, Result<serde_json::Value, String>> {
        Box::pin(async { Err("fixture has no startup answer".into()) })
    }
}
