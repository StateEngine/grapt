//! ACP 1.2.1 JSON-RPC adapter over the ordinary versioned product API.
//!
//! This module owns connection-local protocol state only. Durable sessions,
//! events, permissions, forms, configuration, and execution remain owned by
//! the existing product and are reached through [`ApiClient`].

mod backfill;
mod catalog;
mod content;
mod diagnostics;
mod events;
mod replay;
mod session;
mod timing;
mod tool_output;
mod transport;
mod wire;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Mutex, OnceCell};

use crate::{ApiClient, CliLimits};
pub use transport::AcpConnectionError;
pub(crate) use transport::{run_connection, Peer};
pub use wire::{FrameRead, FrameReader, Inbound, RpcFault, RpcId};

pub const ACP_PROTOCOL_VERSION: u64 = 1;

#[derive(Debug)]
pub(crate) struct AcpService {
    client: ApiClient,
    timing: Arc<timing::AcpTiming>,
    initialized: AtomicBool,
    client_capabilities: OnceLock<ClientCapabilities>,
    sessions: Mutex<BTreeMap<grapt_types::SessionId, session::AttachedSession>>,
    registered_mcp: Mutex<BTreeMap<grapt_types::SessionId, BTreeSet<String>>>,
    peer: OnceCell<Peer>,
}

#[derive(Debug, Clone, Copy)]
struct ClientCapabilities {
    write_text_file: bool,
}

impl AcpService {
    pub(crate) fn new(client: ApiClient) -> Arc<Self> {
        Arc::new(Self {
            client,
            timing: timing::AcpTiming::bounded(Arc::new(timing::DiagnosticClock::new())),
            initialized: AtomicBool::new(false),
            client_capabilities: OnceLock::new(),
            sessions: Mutex::new(BTreeMap::new()),
            registered_mcp: Mutex::new(BTreeMap::new()),
            peer: OnceCell::new(),
        })
    }

    pub(crate) fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }

    pub(crate) async fn handle_request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, RpcFault> {
        match method {
            "initialize" => self.initialize(params),
            "authenticate" => self.authenticate(params),
            _ if !self.is_initialized() => Err(RpcFault::invalid_request(
                "initialize must be the first ACP request",
            )),
            "session/new" => self.new_session(params).await,
            "session/load" => self.load_session(params).await,
            "session/list" => self.list_sessions(params).await,
            "session/delete" => self.delete_session(params).await,
            "session/fork" => self.fork_session(params).await,
            "session/resume" => self.resume_session(params).await,
            "session/close" => self.close_session(params).await,
            "session/set_mode" => self.set_session_mode(params).await,
            "session/set_config_option" => self.set_config_option(params).await,
            "session/prompt" => self.prompt(params).await,
            _ => Err(RpcFault::method_not_found(method)),
        }
    }

    pub(crate) async fn handle_notification(
        &self,
        method: &str,
        params: Value,
    ) -> Result<(), RpcFault> {
        if !self.is_initialized() {
            return Err(RpcFault::invalid_request(
                "initialize must precede ACP notifications",
            ));
        }
        match method {
            "session/cancel" => self.cancel(params).await,
            _ => Err(RpcFault::method_not_found(method)),
        }
    }

    fn initialize(&self, params: Value) -> Result<Value, RpcFault> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct InitializeParams {
            protocol_version: u64,
            #[serde(default)]
            client_capabilities: Value,
            #[serde(default)]
            client_info: Option<Value>,
        }

        let request: InitializeParams = serde_json::from_value(params)
            .map_err(|_| RpcFault::invalid_params("invalid initialize parameters"))?;
        if request.protocol_version != ACP_PROTOCOL_VERSION {
            return Err(RpcFault::invalid_params("unsupported protocol version"));
        }
        if !request.client_capabilities.is_object() {
            return Err(RpcFault::invalid_params(
                "clientCapabilities must be an object",
            ));
        }
        if request
            .client_info
            .as_ref()
            .is_some_and(|value| !value.is_object())
        {
            return Err(RpcFault::invalid_params("clientInfo must be an object"));
        }
        let write_text_file = request
            .client_capabilities
            .get("fs")
            .and_then(|value| value.get("writeTextFile"))
            .and_then(Value::as_bool)
            == Some(true);
        if self
            .initialized
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(RpcFault::invalid_request(
                "initialize may be requested exactly once",
            ));
        }
        self.client_capabilities
            .set(ClientCapabilities { write_text_file })
            .map_err(|_| RpcFault::internal("client capabilities were already bound"))?;

        Ok(json!({
            "protocolVersion": ACP_PROTOCOL_VERSION,
            "agentCapabilities": {
                "loadSession": true,
                "mcpCapabilities": {"http": true, "sse": false},
                "promptCapabilities": {"embeddedContext": true, "image": true},
                "sessionCapabilities": {
                    "close": {},
                    "delete": {},
                    "fork": {},
                    "list": {},
                    "resume": {}
                }
            },
            "authMethods": [],
            "agentInfo": {"name": "Grapt", "version": env!("CARGO_PKG_VERSION")}
        }))
    }

    fn authenticate(&self, params: Value) -> Result<Value, RpcFault> {
        if !self.is_initialized() {
            return Err(RpcFault::invalid_request(
                "initialize must precede authentication",
            ));
        }
        let method = params
            .as_object()
            .and_then(|value| value.get("methodId"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        Err(RpcFault::invalid_params_with_data(
            "no interactive authentication method is advertised",
            json!({"methodId": method}),
        ))
    }

    pub(crate) fn client(&self) -> &ApiClient {
        &self.client
    }

    pub(crate) fn bind_peer(&self, peer: Peer) -> Result<(), AcpConnectionError> {
        self.peer.set(peer).map_err(|_| AcpConnectionError::Task)
    }

    pub(crate) fn peer(&self) -> Result<&Peer, RpcFault> {
        self.peer
            .get()
            .ok_or_else(|| RpcFault::internal("ACP transport is not bound"))
    }

    pub(crate) fn client_writes_files(&self) -> bool {
        self.client_capabilities
            .get()
            .is_some_and(|capabilities| capabilities.write_text_file)
    }
}

pub async fn serve_acp<R, W, E>(
    client: ApiClient,
    input: R,
    output: W,
    diagnostics: E,
    limits: CliLimits,
) -> Result<(), AcpConnectionError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
    E: AsyncWrite + Unpin + Send + 'static,
{
    run_connection(AcpService::new(client), input, output, diagnostics, limits).await
}
