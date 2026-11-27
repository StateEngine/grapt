//! Shared fixtures for the MCP witnesses.
//!
//! A conformant server that refuses discovery until initialized and never
//! answers a notification, so a client which skips either does not get a
//! lenient pass.

use std::collections::BTreeMap;
use std::time::Duration;

use grapt_mcp::session::{McpSession, SessionBounds};
use grapt_mcp::{accept_tools, DiscoveredTool, McpBounds, McpToolName, PROTOCOL_VERSION};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::super::publication::ConnectedCatalog;
use super::super::*;

pub(crate) fn bounds() -> McpBounds {
    match McpBounds::conservative() {
        Ok(bounds) => bounds,
        Err(_) => panic!("the conservative bounds must be constructible"),
    }
}

pub(crate) fn session() -> McpSession {
    match SessionBounds::conservative() {
        Ok(session_bounds) => McpSession::declared(session_bounds),
        Err(_) => panic!("the conservative session bounds must be constructible"),
    }
}

/// A scripted server: it answers each request it reads with the next line
/// of `script`, so a test can make it answer late, wrongly, or not at all.
pub(crate) fn scripted(
    script: Vec<String>,
) -> (tokio::io::DuplexStream, tokio::task::JoinHandle<()>) {
    let (ours, theirs) = tokio::io::duplex(1024 * 64);
    let handle = tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(theirs);
        let mut lines = BufReader::new(read);
        for reply in script {
            let mut request = String::new();
            match lines.read_line(&mut request).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            if reply.is_empty() {
                // An empty script line means "go silent": the server read
                // the request and never answers.
                tokio::time::sleep(Duration::from_secs(30)).await;
                return;
            }
            if write.write_all(reply.as_bytes()).await.is_err() {
                return;
            }
            if write.write_all(b"\n").await.is_err() {
                return;
            }
            let _ = write.flush().await;
        }
    });
    (ours, handle)
}

/// A server that behaves the way the specification says a server may.
///
/// Two differences from [`scripted`] carry the whole point, and both are
/// server behaviour rather than test convenience:
///
/// - It does not answer a notification. A notification has no id and no
///   reply, so a fixture that answers one lets a client which never sends
///   it pass anyway, with the next scripted reply silently sliding into
///   its place.
/// - It refuses `tools/list` until `notifications/initialized` has
///   arrived, which a conformant server is entitled to do and real ones
///   do. That is what turns a missing notification from something a
///   lenient fixture forgives into the startup failure a user would see.
///
/// Every frame it reads is recorded, so a test can assert what was
/// actually sent and in what order rather than inferring it from the fact
/// that startup happened to succeed.
pub(crate) fn conformant(
    handshake_reply: String,
) -> (
    tokio::io::DuplexStream,
    std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorder = std::sync::Arc::clone(&received);
    let (ours, theirs) = tokio::io::duplex(1024 * 64);
    let handle = tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(theirs);
        let mut lines = BufReader::new(read);
        let mut initialized = false;
        loop {
            let mut line = String::new();
            match lines.read_line(&mut line).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let frame: Value = match serde_json::from_str(line.trim_end()) {
                Ok(frame) => frame,
                Err(_) => return,
            };
            if let Ok(mut log) = recorder.lock() {
                log.push(frame.clone());
            }
            let method = frame.get("method").and_then(Value::as_str).unwrap_or("");
            let id = frame.get("id").and_then(Value::as_u64);
            let Some(id) = id else {
                // A notification. Nothing is written back, deliberately.
                if method == "notifications/initialized" {
                    initialized = true;
                }
                continue;
            };
            let reply = match method {
                "initialize" => {
                    let mut result: Value = match serde_json::from_str(&handshake_reply) {
                        Ok(result) => result,
                        Err(_) => return,
                    };
                    result["id"] = json!(id);
                    result
                }
                "tools/list" if !initialized => json!({
                    "jsonrpc":"2.0","id":id,
                    "error":{"code":-32002,
                             "message":"received request before initialization was complete"}}),
                "tools/list" => json!({"jsonrpc":"2.0","id":id,"result":{"tools":[
                    {"name":"fixture_read","description":"reads",
                     "inputSchema":{"type":"object","properties":{}}}]}}),
                "tools/call" => {
                    let arguments = frame
                        .get("params")
                        .and_then(|params| params.get("arguments"))
                        .cloned()
                        .unwrap_or(json!({}));
                    // "silent" makes the server read and never answer, so
                    // a deadline can be provoked on demand.
                    if arguments.get("silent") == Some(&json!(true)) {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        return;
                    }
                    let failed = arguments.get("fail") == Some(&json!(true));
                    let called = frame
                        .get("params")
                        .and_then(|params| params.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    let text = format!("{called} ran with {arguments}");
                    json!({"jsonrpc":"2.0","id":id,"result":{
                        "content":[{"type":"text","text":text}],
                        "isError": failed}})
                }
                _ => json!({"jsonrpc":"2.0","id":id,
                            "error":{"code":-32601,"message":"method not found"}}),
            };
            if write.write_all(reply.to_string().as_bytes()).await.is_err() {
                return;
            }
            if write.write_all(b"\n").await.is_err() {
                return;
            }
            let _ = write.flush().await;
        }
    });
    (ours, received, handle)
}

pub(crate) fn handshake_answering(version: &str) -> String {
    json!({"jsonrpc":"2.0","id":1,"result":{
        "protocolVersion":version,
        "serverInfo":{"name":"fixture-server","version":"1.2"}}})
    .to_string()
}

pub(crate) fn catalog_of(names: &[&str]) -> Vec<DiscoveredTool> {
    let rows: Vec<Value> = names
        .iter()
        .map(|name| {
            json!({"name": name, "description": "fixture",
                   "inputSchema": {"type":"object","properties":{}}})
        })
        .collect();
    match accept_tools(bounds(), &json!({"tools": rows})) {
        Ok(catalog) => catalog,
        Err(error) => panic!("the fixture catalog must be admissible: {error}"),
    }
}

pub(crate) fn channel(
    stream: tokio::io::DuplexStream,
) -> McpChannel<
    tokio::io::ReadHalf<tokio::io::DuplexStream>,
    tokio::io::WriteHalf<tokio::io::DuplexStream>,
> {
    let (read, write) = tokio::io::split(stream);
    McpChannel::new(read, write, bounds())
}

pub(crate) fn handshake() -> String {
    json!({"jsonrpc":"2.0","id":1,"result":{
        "protocolVersion":PROTOCOL_VERSION,
        "serverInfo":{"name":"fixture-server","version":"1.2"}}})
    .to_string()
}

pub(crate) fn listing() -> String {
    json!({"jsonrpc":"2.0","id":2,"result":{"tools":[
        {"name":"fixture_read","description":"reads",
         "inputSchema":{"type":"object","properties":{}}}]}})
    .to_string()
}

pub(crate) fn local(command: &str) -> Declaration {
    Declaration::Local {
        command: vec![command.to_owned()],
        cwd: ".".to_owned(),
        environment: BTreeMap::new(),
        timeouts: McpTimeouts::default(),
    }
}

/// A real, healthy child for reuse tests: failed starts deliberately retry.
pub(crate) fn healthy_local(instance: &str) -> Declaration {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/stateengine-native/fixture_server.py");
    assert!(script.is_file(), "native MCP fixture is absent");
    Declaration::Local {
        command: vec!["python3".to_owned(), script.to_string_lossy().into_owned()],
        cwd: ".".to_owned(),
        environment: BTreeMap::from([("MCP_FIXTURE_INSTANCE".to_owned(), instance.to_owned())]),
        timeouts: McpTimeouts {
            startup: Duration::from_secs(5),
            catalog: Duration::from_secs(5),
            execution: Duration::from_secs(5),
        },
    }
}

pub(crate) fn live_launch(entry: &ServerEntry) -> LaunchIdentity {
    let ServerEntry::Live(live) = entry else {
        panic!("fixture must hold a live connection: {}", entry.detail());
    };
    live.launch().clone()
}

/// A declaration differing from [`local`] ONLY in the field named, so a
/// witness can show that field participating in connection identity.
pub(crate) fn local_with_cwd(command: &str, cwd: &str) -> Declaration {
    Declaration::Local {
        command: vec![command.to_owned()],
        cwd: cwd.to_owned(),
        environment: BTreeMap::new(),
        timeouts: McpTimeouts::default(),
    }
}

pub(crate) fn local_with_environment(
    command: &str,
    environment: BTreeMap<String, String>,
) -> Declaration {
    Declaration::Local {
        command: vec![command.to_owned()],
        cwd: ".".to_owned(),
        environment,
        timeouts: McpTimeouts::default(),
    }
}

/// The consent a user would have given for a connection installed from
/// `declaration`.
///
/// DERIVED FROM THE DECLARATION, because that is how production builds a
/// launch identity. Tests wrote the cwd by hand and matched the LOCATION,
/// which agreed only while the helper ignored the declaration's own `cwd`. An
/// identity that agrees by coincidence is not evidence that the product does.
pub(crate) fn approval_for(
    declaration: &Declaration,
    program: &str,
    incarnation: u64,
) -> LaunchIdentity {
    let cwd = match declaration {
        Declaration::Local { cwd, .. } => cwd.clone(),
        _ => ".".to_owned(),
    };
    LaunchIdentity {
        declaration: Arc::new(declaration.clone()),
        program: program.to_owned(),
        cwd,
        arguments: match declaration {
            Declaration::Local { command, .. } => command.iter().skip(1).cloned().collect(),
            _ => Vec::new(),
        },
        incarnation,
    }
}

pub(crate) fn declared(name: &str, declaration: Declaration) -> BTreeMap<String, Declaration> {
    let mut map = BTreeMap::new();
    map.insert(name.to_owned(), declaration);
    map
}

pub(crate) fn registry_for_tests() -> McpRegistry {
    McpRegistry::new(
        bounds(),
        session_bounds_for_tests(),
        "0.1.0",
        Arc::new(grapt_store::SystemClock),
    )
}

pub(crate) fn session_bounds_for_tests() -> SessionBounds {
    match SessionBounds::conservative() {
        Ok(bounds) => bounds,
        Err(_) => panic!("the conservative session bounds must be constructible"),
    }
}

pub(crate) fn connected_catalog<'a>(
    server: &'a str,
    catalog: &'a [DiscoveredTool],
) -> ConnectedCatalog<'a> {
    ConnectedCatalog {
        server,
        launch: LaunchIdentity {
            declaration: std::sync::Arc::new(crate::dogfood::mcp::Declaration::Disabled),
            program: format!("{server}-program"),
            cwd: "E:/projects/fixture".to_owned(),
            arguments: Vec::new(),
            incarnation: 1,
        },
        catalog,
    }
}

pub(crate) fn discovered_tool() -> McpToolName {
    match McpToolName::new("fixture_read") {
        Some(name) => name,
        None => panic!("the fixture tool name must be usable"),
    }
}

/// Drive a real startup against the conformant fixture and hand back the
/// live channel and session, so a call runs on a connection that actually
/// handshook and discovered rather than on a hand-built one.
pub(crate) async fn connected() -> (
    McpChannel<
        tokio::io::ReadHalf<tokio::io::DuplexStream>,
        tokio::io::WriteHalf<tokio::io::DuplexStream>,
    >,
    McpSession,
    tokio::task::JoinHandle<()>,
) {
    let (stream, _received, server) = conformant(handshake_answering(PROTOCOL_VERSION));
    let mut channel = channel(stream);
    let mut session = session();
    match start(
        &mut channel,
        &mut session,
        bounds(),
        Duration::from_secs(5),
        Duration::from_secs(5),
        "0.1.0",
    )
    .await
    {
        Ok(_) => {}
        Err(reason) => panic!("the fixture server did not start: {reason:?}"),
    }
    (channel, session, server)
}
