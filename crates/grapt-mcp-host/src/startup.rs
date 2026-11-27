// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: bounded serial startup with cumulative human-pause accounting.
// (active_time_timeout), commit bb64b894fd67158be0f874872e6d7bfe717e7368.

//! Startup owns both phase budgets; the channel owns the nested human waits.
use crate::channel::{McpChannel, ServerRequestHandler};
use grapt_mcp::{
    accept_initialize, discovery::ToolCatalog, initialize_request_with_elicitation,
    initialized_notification, session::Unusable, tools_list_page_request, McpBounds,
};
use grapt_mcp::{session::McpSession, DiscoveredTool, ServerIdentity};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};

/// Which phase of startup a failure happened in.
///
/// The reason alone cannot say. `Unusable::TimedOut` means a deadline passed
/// and is identical for the handshake and for discovery, so an operator was
/// told "it did not answer within the startup budget" for a server that
/// handshook perfectly well and was slow to list its tools -- pointing them at
/// the wrong setting. The phases became separately configurable, so the
/// presentation has to distinguish them too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartPhase {
    /// `initialize` and the `notifications/initialized` that completes it.
    Handshake,
    /// `tools/list`.
    Discovery,
}

impl StartPhase {
    pub fn budget_name(self) -> &'static str {
        match self {
            Self::Handshake => "startup",
            Self::Discovery => "catalog",
        }
    }

    pub fn operation(self) -> &'static str {
        match self {
            Self::Handshake => "the handshake",
            Self::Discovery => "tool discovery",
        }
    }
}

/// A startup that failed, and where.
#[derive(Debug)]
pub struct StartFailure {
    pub reason: Unusable,
    pub phase: StartPhase,
}

/// What a completed startup yields: who the server said it is, and the bounded
/// catalog it published.
#[derive(Debug)]
pub struct StartedServer {
    pub identity: ServerIdentity,
    pub catalog: Vec<DiscoveredTool>,
}

fn fail(session: &mut McpSession, reason: Unusable, phase: StartPhase) -> StartFailure {
    session.unusable(reason.clone());
    StartFailure { reason, phase }
}

/// Each phase spends active time, not human response time. All startup human
/// waits share a finite ten-minute allowance and a single nested-request cap.
#[expect(clippy::too_many_lines, reason = "legacy")]
pub async fn start_with<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    channel: &mut McpChannel<R, W>,
    session: &mut McpSession,
    bounds: McpBounds,
    deadline: Duration,
    catalog_deadline: Duration,
    client_version: &str,
    mut handler: Option<&mut dyn ServerRequestHandler>,
) -> Result<StartedServer, StartFailure> {
    if session.start().is_err() {
        return Err(StartFailure {
            reason: Unusable::Failed(
                "a start was requested while one was already in progress".into(),
            ),
            phase: StartPhase::Handshake,
        });
    }
    channel.begin_startup(handler.is_some());
    let started = tokio::time::Instant::now();
    let paused = channel.human_paused();
    let request = initialize_request_with_elicitation(
        channel.sequence(),
        bounds,
        "grapt",
        client_version,
        handler.is_some(),
    )
    .map_err(|error| {
        fail(
            session,
            Unusable::Failed(error.to_string()),
            StartPhase::Handshake,
        )
    })?;
    let remaining = deadline.saturating_sub(channel.active_elapsed(started, paused));
    let result = match handler.as_mut() {
        Some(handler) => {
            channel
                .exchange_with(request, remaining, Some(&mut **handler), None)
                .await
        }
        None => channel.exchange(request, remaining).await,
    }
    .map_err(|error| fail(session, error.into_unusable(), StartPhase::Handshake))?;
    channel.startup_stage("initialize-response");
    let identity = accept_initialize(&result).map_err(|error| {
        fail(
            session,
            Unusable::Failed(error.to_string()),
            StartPhase::Handshake,
        )
    })?;
    channel.set_protocol(identity.negotiated_protocol());
    let notification = initialized_notification(bounds).map_err(|error| {
        fail(
            session,
            Unusable::Failed(error.to_string()),
            StartPhase::Handshake,
        )
    })?;
    channel
        .notify(
            notification,
            deadline.saturating_sub(channel.active_elapsed(started, paused)),
        )
        .await
        .map_err(|error| fail(session, error.into_unusable(), StartPhase::Handshake))?;
    channel.startup_stage("initialized-notification");

    // Discovery has its own active-time budget, shared by every catalog page.
    let cataloguing = tokio::time::Instant::now();
    let paused = channel.human_paused();
    // Content-only servers need no tools endpoint. Empty legacy capability
    // handshakes retain the tool probe supported before content discovery.
    let discovered = if !identity.advertises_tools()
        && (identity.advertises_prompts() || identity.advertises_resources())
    {
        Vec::new()
    } else {
        let mut catalog = ToolCatalog::new(bounds);
        loop {
            let request = tools_list_page_request(channel.sequence(), bounds, catalog.cursor())
                .map_err(|error| {
                    fail(
                        session,
                        Unusable::Failed(error.to_string()),
                        StartPhase::Discovery,
                    )
                })?;
            let remaining =
                catalog_deadline.saturating_sub(channel.active_elapsed(cataloguing, paused));
            let result = match handler.as_mut() {
                Some(handler) => {
                    channel
                        .exchange_with(request, remaining, Some(&mut **handler), None)
                        .await
                }
                None => channel.exchange(request, remaining).await,
            }
            .map_err(|error| fail(session, error.into_unusable(), StartPhase::Discovery))?;
            if catalog.accept_page(&result).map_err(|error| {
                fail(
                    session,
                    Unusable::Failed(error.to_string()),
                    StartPhase::Discovery,
                )
            })? {
                break;
            }
        }
        catalog.finish().map_err(|error| {
            fail(
                session,
                Unusable::Failed(error.to_string()),
                StartPhase::Discovery,
            )
        })?
    };
    let names = discovered.iter().map(|tool| tool.name().clone()).collect();
    session.ready(names).map_err(|_| {
        fail(
            session,
            Unusable::Failed("the session left its starting state during startup".into()),
            StartPhase::Discovery,
        )
    })?;
    channel.finish_startup();
    Ok(StartedServer {
        identity,
        catalog: discovered,
    })
}
