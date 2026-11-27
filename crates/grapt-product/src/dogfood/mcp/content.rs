//! Catalog and operator reads share the held, approved MCP connection.
use super::{
    ChannelError, HeldIo, LiveIo, McpRegistry, McpTimeouts, ServerEntry, MAX_QUEUED_CALLERS,
};
use grapt_mcp::{
    content::{self, CatalogKind, ContentCatalog},
    McpBounds, McpProtocolError,
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

pub(crate) struct ContentServer {
    pub name: String,
    io: Arc<HeldIo<LiveIo>>,
    bounds: McpBounds,
    timeouts: McpTimeouts,
    pub prompts: bool,
    pub resources: bool,
    pub subscribes: bool,
    emitter: Option<Arc<super::notifications::Emitter>>,
    key: String,
}

impl McpRegistry {
    pub(crate) async fn content_servers(
        &self,
        key: &str,
        directory: &str,
        declared: &BTreeMap<String, super::Declaration>,
    ) -> Vec<(String, Result<ContentServer, String>)> {
        self.with_status_at(key, directory, declared, |entries| {
            entries
                .iter()
                .filter_map(|(name, entry)| {
                    let server = match entry {
                        ServerEntry::Disabled => return None,
                        ServerEntry::Live(live)
                            if live.observed_unusable().is_none() && !live.io.is_retired() =>
                        {
                            Ok(ContentServer {
                                name: name.clone(),
                                emitter: self.catalog_emitter.clone(),
                                key: key.into(),
                                io: Arc::clone(&live.io),
                                bounds: self.bounds,
                                timeouts: live.timeouts,
                                prompts: live.identity().advertises_prompts(),
                                resources: live.identity().advertises_resources(),
                                subscribes: live.identity().subscribes_resources(),
                            })
                        }
                        _ => Err(format!(
                            "MCP {} is {}: {}",
                            name.escape_debug(),
                            entry.status_label(),
                            entry.detail()
                        )),
                    };
                    Some((name.clone(), server))
                })
                .collect()
        })
        .await
    }
}

impl ContentServer {
    pub async fn subscribe(
        &self,
        session: &grapt_types::SessionId,
        uri: &str,
    ) -> Result<super::subscriptions::Interest, String> {
        if !self.subscribes {
            return Err("Server does not support resource subscriptions".into());
        }
        let _ticket = self
            .io
            .enqueue(MAX_QUEUED_CALLERS)
            .ok_or("MCP read queue is full")?;
        let due = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut io = tokio::time::timeout_at(due, self.io.io().lock())
            .await
            .map_err(|_| "MCP subscription waited past its deadline")?;
        self.usable()?;
        let interest = self.io.resources.reserve(session, uri)?;
        if !self.io.resources.subscribed(uri) {
            let request =
                content::subscription_request(io.channel.sequence(), self.bounds, uri, true)
                    .map_err(|error| error.to_string())?;
            self.io.resources.confirm(uri);
            let result = tokio::select! {
                _ = self.io.retired() => return Err("MCP connection changed during subscription; retry".into()),
                result = io.channel.exchange(request, due.saturating_duration_since(tokio::time::Instant::now())) => result,
            };
            let result = match result {
                Ok(result) => result,
                Err(error @ ChannelError::Protocol(McpProtocolError::ServerError { .. })) => {
                    // A rejected URI leaves the shared connection and other watches intact.
                    self.io.resources.removed(uri);
                    return Err(self.failure(error));
                }
                Err(error) => {
                    self.io.retire();
                    return Err(self.failure(error));
                }
            };
            if !result.is_object() {
                self.io.retire();
                return Err("MCP resource subscription returned an invalid acknowledgement".into());
            }
        }
        self.usable()?;
        Ok(interest)
    }
    pub fn changed(&self) {
        if let Some(emitter) = &self.emitter {
            emitter.resource_changed(&self.key);
        }
    }
    pub fn unpin(&self, session: &grapt_types::SessionId, uri: &str) {
        self.io.resources.release(session, uri);
    }
    pub fn resource_revision(&self, uri: &str) -> u64 {
        self.io.resources.revision(uri)
    }
    pub async fn catalog(&self, kind: CatalogKind) -> Result<Vec<Value>, String> {
        if kind == CatalogKind::Prompts && !self.prompts
            || kind != CatalogKind::Prompts && !self.resources
        {
            return Ok(Vec::new());
        }
        let budget = self.timeouts.catalog.min(Duration::from_secs(10));
        let due = tokio::time::Instant::now() + budget;
        let _ticket = self
            .io
            .enqueue(MAX_QUEUED_CALLERS)
            .ok_or("MCP read queue is full")?;
        let mut io = tokio::time::timeout_at(due, self.io.io().lock())
            .await
            .map_err(|_| "MCP catalog waited past its deadline")?;
        self.usable()?;
        if let Some(signal) = io.channel.catalog_signal() {
            signal.clear(kind);
        }
        let mut catalog = ContentCatalog::new(kind, self.bounds);
        loop {
            let request =
                content::list_request(io.channel.sequence(), self.bounds, kind, catalog.cursor())
                    .map_err(|e| e.to_string())?;
            let result = tokio::select! {
                _ = self.io.retired() => return Err("MCP connection changed during discovery; retry".into()),
                result = io.channel.exchange(request, due.saturating_duration_since(tokio::time::Instant::now())) => result,
            };
            let result = match result {
                // Templates are optional; only an explicit method refusal means absent.
                Err(ChannelError::Protocol(McpProtocolError::ServerError {
                    code: -32601, ..
                })) if kind == CatalogKind::Templates => json!({"resourceTemplates":[]}),
                Err(error) => return Err(self.failure(error)),
                Ok(result) => result,
            };
            if catalog.accept_page(&result).map_err(|e| e.to_string())? {
                self.usable()?;
                return catalog.finish().map_err(|e| e.to_string());
            }
        }
    }

    pub async fn prompt(
        &self,
        name: &str,
        arguments: &BTreeMap<String, String>,
    ) -> Result<Value, String> {
        if !self.prompts {
            return Err("Server did not advertise prompts".into());
        }
        self.read(Some((name, arguments)), None).await
    }
    pub async fn resource(&self, uri: &str) -> Result<Value, String> {
        if !self.resources {
            return Err("Server did not advertise resources".into());
        }
        self.read(None, Some(uri)).await
    }
    async fn read(
        &self,
        prompt: Option<(&str, &BTreeMap<String, String>)>,
        uri: Option<&str>,
    ) -> Result<Value, String> {
        let due = tokio::time::Instant::now() + self.timeouts.execution;
        let _ticket = self
            .io
            .enqueue(MAX_QUEUED_CALLERS)
            .ok_or("MCP read queue is full")?;
        let mut io = tokio::time::timeout_at(due, self.io.io().lock())
            .await
            .map_err(|_| "MCP content read waited past its deadline")?;
        self.usable()?;
        let request = match (prompt, uri) {
            (Some((name, arguments)), None) => {
                content::prompt_request(io.channel.sequence(), self.bounds, name, arguments)
            }
            (None, Some(uri)) => content::resource_request(io.channel.sequence(), self.bounds, uri),
            _ => return Err("MCP content operation is invalid".into()),
        }
        .map_err(|e| e.to_string())?;
        let result = tokio::select! {
            _ = self.io.retired() => return Err("MCP connection changed during the read; content discarded".into()),
            result = io.channel.exchange(request, due.saturating_duration_since(tokio::time::Instant::now())) => result,
        }.map_err(|e| self.failure(e))?;
        self.usable()?;
        Ok(result)
    }
    fn usable(&self) -> Result<(), String> {
        if self.io.is_retired() || self.io.observed_unusable().is_some() {
            Err("MCP connection is no longer usable; reconnect and retry".into())
        } else {
            Ok(())
        }
    }
    fn failure(&self, error: ChannelError) -> String {
        let reason = match &error {
            ChannelError::TimedOut => "MCP read exceeded its deadline".into(),
            ChannelError::Disconnected(_) => "MCP server disconnected during the read".into(),
            ChannelError::Protocol(error) => error.to_string(),
        };
        self.io.observe(error.into_unusable());
        format!("{}: {reason}", self.name.escape_debug())
    }
}

pub(crate) fn prompt_command_name(server: &str, prompt: &str) -> String {
    format!(
        "mcp__{}__{}",
        super::publication::sanitize_name(server),
        super::publication::sanitize_name(prompt)
    )
}
