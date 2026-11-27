//! Connection-local attachments over durable product sessions.

use std::path::Path;

use grapt_types::SessionId;
use serde_json::{json, Map, Value};

use super::catalog::{Catalog, ModelRef};
use super::{AcpService, RpcFault};

#[derive(Debug, Clone)]
pub(crate) struct AttachedSession {
    pub(crate) id: SessionId,
    pub(crate) cwd: String,
    pub(crate) catalog: Catalog,
    pub(crate) model: ModelRef,
    pub(crate) mode: String,
}

impl AcpService {
    pub(crate) async fn new_session(&self, params: Value) -> Result<Value, RpcFault> {
        let cwd = cwd(&params)?;
        let mcp = mcp_servers(&params, true)?;
        let catalog = Catalog::load(self.client(), &cwd).await?;
        let response = self
            .client()
            .post_value(
                "api/session",
                &json!({
                    "location": {"directory": cwd},
                    "agent": catalog.default_mode,
                    "model": catalog.default_model.api_value(),
                }),
            )
            .await
            .map_err(remote)?;
        let record = record(&response)?;
        let state = attached(record, cwd, catalog)?;
        let result = json!({
            "sessionId": state.id.as_str(),
            "configOptions": state.catalog.options(&state.model, &state.mode),
        });
        self.register_mcp(&state, &mcp).await?;
        self.attach(state).await?;
        Ok(result)
    }

    pub(crate) async fn load_session(&self, params: Value) -> Result<Value, RpcFault> {
        let session = session_id(&params)?;
        let requested_cwd = cwd(&params)?;
        let mcp = mcp_servers(&params, true)?;
        let response = self
            .client()
            .get_value(&format!("api/session/{session}"))
            .await
            .map_err(remote)?;
        let record = record(&response)?;
        let actual_cwd = session_cwd(record)?;
        if actual_cwd != requested_cwd {
            return Err(RpcFault::invalid_params(
                "session cwd does not match its durable placement",
            ));
        }
        let catalog = Catalog::load(self.client(), &actual_cwd).await?;
        let state = attached(record, actual_cwd, catalog)?;
        let result = json!({"configOptions": state.catalog.options(&state.model, &state.mode)});
        self.register_mcp(&state, &mcp).await?;
        self.attach(state.clone()).await?;
        self.replay(&state).await?;
        Ok(result)
    }

    pub(crate) async fn resume_session(&self, params: Value) -> Result<Value, RpcFault> {
        let session = session_id(&params)?;
        let requested_cwd = cwd(&params)?;
        let mcp = mcp_servers(&params, false)?;
        let response = self
            .client()
            .get_value(&format!("api/session/{session}"))
            .await
            .map_err(remote)?;
        let record = record(&response)?;
        let actual_cwd = session_cwd(record)?;
        if actual_cwd != requested_cwd {
            return Err(RpcFault::invalid_params(
                "session cwd does not match its durable placement",
            ));
        }
        let catalog = Catalog::load(self.client(), &actual_cwd).await?;
        let state = attached(record, actual_cwd, catalog)?;
        let result = json!({"configOptions": state.catalog.options(&state.model, &state.mode)});
        self.register_mcp(&state, &mcp).await?;
        self.attach(state).await?;
        Ok(result)
    }

    pub(crate) async fn list_sessions(&self, params: Value) -> Result<Value, RpcFault> {
        object(&params)?;
        let cwd_filter = nullable_string(&params, "cwd")?.map(str::to_owned);
        let cursor = nullable_string(&params, "cursor")?.map(str::to_owned);
        let mut pairs = vec![("order", "desc"), ("limit", "100")];
        if let Some(cwd) = cwd_filter.as_deref() {
            if !Path::new(cwd).is_absolute() {
                return Err(RpcFault::invalid_params("cwd must be absolute"));
            }
            pairs.push(("directory", cwd));
        }
        if let Some(cursor) = cursor.as_deref() {
            pairs.push(("cursor", cursor));
        }
        let response = self
            .client()
            .get_value_query("api/session", &pairs)
            .await
            .map_err(remote)?;
        let rows = response
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| RpcFault::internal("session list response is malformed"))?;
        let sessions = rows
            .iter()
            .map(|row| {
                Ok(json!({
                    "sessionId": required_string(row, "id")?,
                    "cwd": session_cwd(row)?,
                    "title": required_string(row, "title")?,
                }))
            })
            .collect::<Result<Vec<_>, RpcFault>>()?;
        let mut result = json!({"sessions": sessions});
        if let Some(next) = response
            .get("cursor")
            .and_then(|value| value.get("next"))
            .and_then(Value::as_str)
        {
            result["nextCursor"] = json!(next);
        }
        Ok(result)
    }

    pub(crate) async fn delete_session(&self, params: Value) -> Result<Value, RpcFault> {
        let session = session_id(&params)?;
        self.client()
            .delete_empty(&format!("api/session/{session}"))
            .await
            .map_err(remote)?;
        self.sessions.lock().await.remove(&session);
        self.registered_mcp.lock().await.remove(&session);
        Ok(json!({}))
    }

    pub(crate) async fn fork_session(&self, params: Value) -> Result<Value, RpcFault> {
        let session = session_id(&params)?;
        let requested_cwd = cwd(&params)?;
        let mcp = mcp_servers(&params, false)?;
        // The one typed fork path (ApiClient::fork_session), re-encoded in the
        // wire shape the ACP session helpers read: location, id, model, agent.
        let forked = self.client().fork_session(&session).await.map_err(remote)?;
        let record = serde_json::to_value(forked.data().get())
            .map_err(|_| RpcFault::internal("forked session record could not be encoded"))?;
        let record = &record;
        let actual_cwd = session_cwd(record)?;
        if actual_cwd != requested_cwd {
            return Err(RpcFault::invalid_params(
                "fork cwd does not match the durable parent placement",
            ));
        }
        let catalog = Catalog::load(self.client(), &actual_cwd).await?;
        let state = attached(record, actual_cwd, catalog)?;
        let result = json!({
            "sessionId": state.id.as_str(),
            "configOptions": state.catalog.options(&state.model, &state.mode),
        });
        self.register_mcp(&state, &mcp).await?;
        self.attach(state.clone()).await?;
        self.replay(&state).await?;
        Ok(result)
    }

    pub(crate) async fn close_session(&self, params: Value) -> Result<Value, RpcFault> {
        let session = session_id(&params)?;
        let interrupt = self.client().interrupt(&session).await;
        self.sessions.lock().await.remove(&session);
        if let Err(error) = interrupt {
            self.peer()?.diagnostic(format!(
                "ACP close interrupt completed with a remote refusal: {error}"
            ));
        }
        Ok(json!({}))
    }

    pub(crate) async fn set_session_mode(&self, params: Value) -> Result<Value, RpcFault> {
        let session = session_id(&params)?;
        let mode = required_string(&params, "modeId")?.to_owned();
        let current = self.require_session(&session).await?;
        if !current.catalog.has_mode(&mode) {
            return Err(RpcFault::invalid_params("unknown session mode"));
        }
        self.client()
            .post_empty(
                &format!("api/session/{session}/agent"),
                &json!({"agent": mode}),
            )
            .await
            .map_err(remote)?;
        if let Some(state) = self.sessions.lock().await.get_mut(&session) {
            state.mode = mode;
        }
        Ok(json!({}))
    }

    pub(crate) async fn set_config_option(&self, params: Value) -> Result<Value, RpcFault> {
        let session = session_id(&params)?;
        let config = required_string(&params, "configId")?;
        let value = required_string(&params, "value")?;
        let mut current = self.require_session(&session).await?;
        match config {
            "model" => current.model = current.catalog.select_model(value)?,
            "effort" => current.model = current.catalog.select_effort(&current.model, value)?,
            "mode" => {
                return self
                    .set_session_mode(json!({"sessionId": session.as_str(), "modeId": value}))
                    .await
                    .map(|_| {
                        current.mode = value.to_owned();
                        json!({"configOptions": current.catalog.options(&current.model, &current.mode)})
                    });
            }
            _ => {
                return Err(RpcFault::invalid_params(
                    "unknown session configuration option",
                ))
            }
        }
        self.client()
            .post_empty(
                &format!("api/session/{session}/model"),
                &json!({"model": current.model.api_value()}),
            )
            .await
            .map_err(remote)?;
        if let Some(state) = self.sessions.lock().await.get_mut(&session) {
            state.model = current.model.clone();
        }
        Ok(json!({
            "configOptions": current.catalog.options(&current.model, &current.mode),
        }))
    }

    pub(crate) async fn cancel(&self, params: Value) -> Result<(), RpcFault> {
        let session = session_id(&params)?;
        self.require_session(&session).await?;
        self.client().interrupt(&session).await.map_err(remote)
    }

    async fn register_mcp(
        &self,
        session: &AttachedSession,
        servers: &[McpRegistration],
    ) -> Result<(), RpcFault> {
        for server in servers {
            let key = format!("{}:{}", server.name, server.config);
            let reserved = self
                .registered_mcp
                .lock()
                .await
                .entry(session.id.clone())
                .or_default()
                .insert(key.clone());
            if !reserved {
                continue;
            }
            let path = format!("api/mcp/{}", encode_path_segment(&server.name));
            if let Err(error) = self
                .client()
                .put_empty_query(
                    &path,
                    &[("location[directory]", session.cwd.as_str())],
                    &json!({"config": server.config}),
                )
                .await
            {
                if let Some(current) = self.registered_mcp.lock().await.get_mut(&session.id) {
                    current.remove(&key);
                }
                return Err(remote(error));
            }
        }
        Ok(())
    }

    async fn attach(&self, state: AttachedSession) -> Result<(), RpcFault> {
        let commands = state
            .catalog
            .commands
            .iter()
            .chain(state.catalog.skills.iter())
            .filter_map(|value| {
                let name = value.get("name")?.as_str()?;
                if name == "compact" {
                    return None;
                }
                Some(json!({
                    "name": name,
                    "description": value.get("description").and_then(Value::as_str).unwrap_or(""),
                }))
            })
            .collect::<Vec<_>>();
        // Commands are namespaced by the Product fold. A colliding skill
        // cannot replace the built-in either; make its refusal visible.
        let mut commands = commands;
        commands.push(json!({
            "name": "compact",
            "description": if state.catalog.names("compact") {
                "Compact before your next message; configured /compact is unavailable because this name is reserved"
            } else { "Compact the conversation before your next message" },
        }));
        self.sessions
            .lock()
            .await
            .insert(state.id.clone(), state.clone());
        self.peer()?
            .notification(
                "session/update",
                json!({
                    "sessionId": state.id.as_str(),
                    "update": {
                        "sessionUpdate": "available_commands_update",
                        "availableCommands": commands,
                    }
                }),
            )
            .await
            .map_err(|_| RpcFault::internal("ACP command catalog notification failed"))
    }

    pub(crate) async fn require_session(
        &self,
        session: &SessionId,
    ) -> Result<AttachedSession, RpcFault> {
        self.sessions
            .lock()
            .await
            .get(session)
            .cloned()
            .ok_or_else(|| {
                RpcFault::invalid_params_with_data(
                    "session is not attached to this ACP connection",
                    json!({"sessionId": session.as_str()}),
                )
            })
    }
}

pub(crate) fn session_id(params: &Value) -> Result<SessionId, RpcFault> {
    required_string(params, "sessionId")?
        .parse()
        .map_err(|_| RpcFault::invalid_params("sessionId is malformed"))
}

fn attached(record: &Value, cwd: String, catalog: Catalog) -> Result<AttachedSession, RpcFault> {
    let id = required_string(record, "id")?
        .parse()
        .map_err(|_| RpcFault::internal("ordinary API returned a malformed session id"))?;
    let model = record
        .get("model")
        .filter(|value| !value.is_null())
        .map(|value| ModelRef {
            provider: value
                .get("providerID")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            id: value
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            variant: value
                .get("variant")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
        .filter(|model| !model.provider.is_empty() && !model.id.is_empty())
        .unwrap_or_else(|| catalog.default_model.clone());
    let mode = record
        .get("agent")
        .and_then(Value::as_str)
        .unwrap_or(&catalog.default_mode)
        .to_owned();
    Ok(AttachedSession {
        id,
        cwd,
        catalog,
        model,
        mode,
    })
}

fn cwd(params: &Value) -> Result<String, RpcFault> {
    let value = required_string(params, "cwd")?;
    if !Path::new(value).is_absolute() {
        return Err(RpcFault::invalid_params("cwd must be absolute"));
    }
    Ok(value.to_owned())
}

#[derive(Debug, Clone)]
struct McpRegistration {
    name: String,
    config: Value,
}

fn mcp_servers(params: &Value, required: bool) -> Result<Vec<McpRegistration>, RpcFault> {
    let servers = match params.get("mcpServers") {
        Some(Value::Array(servers)) => servers,
        None if !required => return Ok(Vec::new()),
        _ => return Err(RpcFault::invalid_params("mcpServers must be an array")),
    };
    if servers.len() > 256 {
        return Err(RpcFault::invalid_params(
            "mcpServers exceeds its 256-server bound",
        ));
    }
    servers.iter().map(mcp_server).collect()
}

fn mcp_server(value: &Value) -> Result<McpRegistration, RpcFault> {
    let fields = value
        .as_object()
        .ok_or_else(|| RpcFault::invalid_params("each MCP server must be an object"))?;
    let name = bounded_string(fields, "name", 1_024)?.to_owned();
    let config = match fields.get("type") {
        None => {
            exact_keys(fields, &["name", "command", "args", "env"])?;
            let command = bounded_string(fields, "command", 32_768)?;
            let args = string_pairs(fields, "args", false, 128)?;
            let env = name_value_map(fields, "env")?;
            let mut argv = Vec::with_capacity(args.len().saturating_add(1));
            argv.push(Value::String(command.to_owned()));
            argv.extend(args.into_iter().map(Value::String));
            json!({"type":"local", "command":argv, "environment":env})
        }
        Some(Value::String(kind)) if kind == "http" => {
            exact_keys(fields, &["type", "name", "url", "headers"])?;
            let url = bounded_string(fields, "url", 4_096)?;
            let parsed = reqwest::Url::parse(url)
                .map_err(|_| RpcFault::invalid_params("MCP HTTP URL is invalid"))?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(RpcFault::invalid_params(
                    "MCP HTTP URL must use HTTP or HTTPS",
                ));
            }
            let headers = name_value_map(fields, "headers")?;
            json!({"type":"remote", "url":parsed.to_string(), "headers":headers, "oauth":false})
        }
        Some(Value::String(kind)) if matches!(kind.as_str(), "sse" | "acp") => {
            return Err(RpcFault::invalid_params(format!(
                "MCP transport {kind} is not supported"
            )))
        }
        _ => return Err(RpcFault::invalid_params("MCP server type is invalid")),
    };
    Ok(McpRegistration { name, config })
}

fn exact_keys(fields: &Map<String, Value>, allowed: &[&str]) -> Result<(), RpcFault> {
    if fields.keys().all(|key| allowed.contains(&key.as_str())) {
        Ok(())
    } else {
        Err(RpcFault::invalid_params(
            "MCP server contains an unknown field",
        ))
    }
}

fn bounded_string<'a>(
    fields: &'a Map<String, Value>,
    key: &'static str,
    maximum: usize,
) -> Result<&'a str, RpcFault> {
    fields
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= maximum)
        .ok_or_else(|| {
            RpcFault::invalid_params(format!(
                "MCP {key} must be a non-empty string within {maximum} bytes"
            ))
        })
}

fn string_pairs(
    fields: &Map<String, Value>,
    key: &'static str,
    optional: bool,
    maximum: usize,
) -> Result<Vec<String>, RpcFault> {
    let Some(value) = fields.get(key) else {
        return if optional {
            Ok(Vec::new())
        } else {
            Err(RpcFault::invalid_params(format!("MCP {key} is required")))
        };
    };
    let rows = value
        .as_array()
        .filter(|rows| rows.len() <= maximum)
        .ok_or_else(|| RpcFault::invalid_params(format!("MCP {key} must be a bounded array")))?;
    rows.iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| value.len() <= 32_768)
                .map(str::to_owned)
                .ok_or_else(|| RpcFault::invalid_params(format!("MCP {key} entry is invalid")))
        })
        .collect()
}

fn name_value_map(
    fields: &Map<String, Value>,
    key: &'static str,
) -> Result<Map<String, Value>, RpcFault> {
    let rows = fields
        .get(key)
        .and_then(Value::as_array)
        .filter(|rows| rows.len() <= 256)
        .ok_or_else(|| RpcFault::invalid_params(format!("MCP {key} must be a bounded array")))?;
    let mut result = Map::new();
    for row in rows {
        let pair = row
            .as_object()
            .ok_or_else(|| RpcFault::invalid_params(format!("MCP {key} entry is invalid")))?;
        exact_keys(pair, &["name", "value"])?;
        let name = bounded_string(pair, "name", 1_024)?;
        let value = bounded_string(pair, "value", 32_768)?;
        result.insert(name.to_owned(), Value::String(value.to_owned()));
    }
    Ok(result)
}

fn encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(b"0123456789ABCDEF"[usize::from(byte >> 4)]));
            encoded.push(char::from(b"0123456789ABCDEF"[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

fn object(value: &Value) -> Result<(), RpcFault> {
    if value.is_object() {
        Ok(())
    } else {
        Err(RpcFault::invalid_params("parameters must be an object"))
    }
}

fn record(value: &Value) -> Result<&Value, RpcFault> {
    value
        .get("data")
        .filter(|value| value.is_object())
        .ok_or_else(|| RpcFault::internal("ordinary session response is malformed"))
}

fn session_cwd(value: &Value) -> Result<String, RpcFault> {
    value
        .get("location")
        .and_then(|location| location.get("directory"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| RpcFault::internal("session placement is malformed"))
}

fn required_string<'a>(value: &'a Value, key: &'static str) -> Result<&'a str, RpcFault> {
    object(value)?;
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| RpcFault::invalid_params(format!("{key} must be a non-empty string")))
}

fn nullable_string<'a>(value: &'a Value, key: &'static str) -> Result<Option<&'a str>, RpcFault> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(RpcFault::invalid_params(format!(
            "{key} must be a string or null"
        ))),
    }
}

fn remote(error: crate::ApiClientError) -> RpcFault {
    RpcFault::internal(format!("ordinary API request failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{encode_path_segment, mcp_servers};
    use serde_json::json;

    #[test]
    fn acp_mcp_servers_translate_both_supported_shapes_and_escape_names() {
        let result = mcp_servers(
            &json!({"mcpServers":[
                {"name":"tools", "command":"runner", "args":["serve"],
                 "env":[{"name":"TOKEN", "value":"private"}]},
                {"type":"http", "name":"docs/api", "url":"https://example.com/mcp",
                 "headers":[{"name":"Authorization", "value":"Bearer private"}]}
            ]}),
            true,
        );
        let Ok(servers) = result else {
            panic!("valid MCP declarations were refused: {result:?}");
        };
        assert_eq!(servers.len(), 2);
        assert_eq!(
            servers[0].config,
            json!({"type":"local", "command":["runner","serve"], "environment":{"TOKEN":"private"}})
        );
        assert_eq!(
            servers[1].config,
            json!({"type":"remote", "url":"https://example.com/mcp", "headers":{"Authorization":"Bearer private"}, "oauth":false})
        );
        assert_eq!(encode_path_segment(&servers[1].name), "docs%2Fapi");
    }

    #[test]
    fn unsupported_or_open_mcp_shapes_are_refused_before_remote_effects() {
        for value in [
            json!({"mcpServers":[{"type":"sse", "name":"legacy", "url":"https://example.com"}]}),
            json!({"mcpServers":[{"type":"http", "name":"open", "url":"https://example.com", "headers":[], "extra":true}]}),
            json!({"mcpServers":[{"name":"local", "command":"runner", "args":[], "env":[], "extra":true}]}),
        ] {
            assert!(mcp_servers(&value, true).is_err());
        }
    }
}
