use super::super::*;
use serde_json::json;
use std::error::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type FixtureError = Box<dyn Error + Send + Sync>;
type FixtureTask = tokio::task::JoinHandle<Result<Vec<String>, FixtureError>>;
type TestResult = Result<(), Box<dyn Error>>;
const BUDGET: Duration = Duration::from_secs(5);

async fn request(stream: &mut tokio::net::TcpStream) -> Result<String, FixtureError> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let count = stream.read(&mut chunk).await?;
        if count == 0 || bytes.len() + count > 8192 {
            return Err("fixture request missing or oversized".into());
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let length = std::str::from_utf8(&bytes[..end])?
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>())
                })
                .transpose()?
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                return Ok(String::from_utf8(bytes)?);
            }
        }
    }
}

fn response(status: &str, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n{body}",
        body.len()
    )
}

#[expect(clippy::too_many_lines, reason = "legacy")]
async fn fixture(disconnect_call: bool) -> Result<(String, FixtureTask), Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/mcp", listener.local_addr()?);
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        let expected = if disconnect_call { 5 } else { 4 };
        let mut step = 0;
        let mut lifecycle_get = false;
        for _ in 0..expected + 1 {
            let (mut stream, _) = listener.accept().await?;
            let request = request(&mut stream).await?;
            let lower = request.to_ascii_lowercase();
            if request.starts_with("GET ") && !lower.contains("last-event-id:") {
                assert!(!lifecycle_get, "duplicate lifecycle subscription");
                assert!(lower.contains("mcp-session-id: remote-fixture"));
                lifecycle_get = true;
                stream
                    .write_all(response("405 Method Not Allowed", "", "").as_bytes())
                    .await?;
                requests.push(request);
                if step == expected {
                    break;
                }
                continue;
            }
            let (_, body) = request
                .split_once("\r\n\r\n")
                .ok_or("fixture request lacks headers")?;
            let message: Value = if body.is_empty() {
                Value::Null
            } else {
                serde_json::from_str(body)?
            };
            let id = message["id"].clone();
            let reply = match step {
                0 => {
                    assert_eq!(message["method"], "initialize");
                    response("200 OK", "Content-Type: application/json\r\nMcp-Session-Id: remote-fixture\r\n", &json!({
                        "jsonrpc":"2.0", "id":id, "result": {
                            "protocolVersion":"2025-06-18", "serverInfo":{"name":"remote-fixture"},
                            "capabilities":{"tools":{}}
                        }
                    }).to_string())
                }
                1 => {
                    assert_eq!(message["method"], "notifications/initialized");
                    response("202 Accepted", "", "")
                }
                2 => {
                    assert_eq!(message["method"], "tools/list");
                    response(
                        "200 OK",
                        "Content-Type: application/json\r\n",
                        &json!({
                            "jsonrpc":"2.0", "id":id, "result":{"tools":[{
                                "name":"echo", "inputSchema":{"type":"object"}
                            }]}
                        })
                        .to_string(),
                    )
                }
                3 => {
                    assert_eq!(message["method"], "tools/call");
                    if disconnect_call {
                        response(
                            "200 OK",
                            "Content-Type: text/event-stream\r\n",
                            "id: primed\ndata:\n\nid: incomplete\ndata: {",
                        )
                    } else {
                        response("200 OK", "Content-Type: application/json\r\n", &json!({
                            "jsonrpc":"2.0", "id":id, "result":{"content":[{"type":"text","text":"remote result"}]}
                        }).to_string())
                    }
                }
                _ => {
                    assert!(
                        request.starts_with("GET "),
                        "a tool call was replayed instead of resumed"
                    );
                    assert!(request
                        .to_ascii_lowercase()
                        .contains("last-event-id: primed"));
                    response("405 Method Not Allowed", "", "")
                }
            };
            stream.write_all(reply.as_bytes()).await?;
            requests.push(request);
            step += 1;
            if step == expected && lifecycle_get {
                break;
            }
        }
        assert_eq!(step, expected, "protocol exchange did not complete");
        assert!(
            lifecycle_get,
            "initialized session did not open its notification reader"
        );
        Ok(requests)
    });
    Ok((url, task))
}

fn declaration(url: String) -> Declaration {
    Declaration::Remote {
        url,
        headers: BTreeMap::new(),
        oauth: false,
        timeouts: McpTimeouts {
            startup: BUDGET,
            catalog: BUDGET,
            execution: BUDGET,
        },
    }
}

async fn approval(registry: &McpRegistry, url: String) -> Result<LaunchIdentity, String> {
    registry
        .with_status(
            "/remote-fixture",
            &BTreeMap::from([("remote".to_owned(), declaration(url))]),
            |entries| match entries.get("remote") {
                Some(ServerEntry::Live(live)) => Ok(live.launch().clone()),
                Some(entry) => Err(format!("fixture did not connect: {}", entry.detail())),
                None => Err("fixture server missing".to_owned()),
            },
        )
        .await
}

#[tokio::test]
async fn registered_remote_server_discovers_and_executes_through_the_existing_session_owner(
) -> TestResult {
    let (url, server) = fixture(false).await?;
    let registry = registry(Arc::new(grapt_store::SystemClock))?;
    let approved = approval(&registry, url).await?;
    let tool = McpToolName::new("echo").ok_or("fixture tool name")?;
    let result = registry
        .call(
            "/remote-fixture",
            "remote",
            &tool,
            &approved,
            "call-once",
            json!({"text":"hello"}),
            BUDGET,
        )
        .await
        .map_err(|error| format!("{error:?}"))?;
    assert!(result.text().contains("remote result"));
    let repeated = registry
        .call(
            "/remote-fixture",
            "remote",
            &tool,
            &approved,
            "call-once",
            json!({"text":"hello"}),
            BUDGET,
        )
        .await;
    assert!(matches!(repeated, Err(McpCallRefusal::Session(_))));
    let requests = tokio::time::timeout(BUDGET, server)
        .await??
        .map_err(|error| error as Box<dyn Error>)?;
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.contains("\"tools/call\""))
            .count(),
        1
    );
    let discovery = requests
        .iter()
        .find(|request| request.contains("\"tools/list\""))
        .ok_or("tool discovery missing")?;
    assert!(discovery
        .to_ascii_lowercase()
        .contains("mcp-protocol-version: 2025-06-18"));
    Ok(())
}

#[tokio::test]
async fn an_ambiguous_remote_call_is_never_replayed_or_lent_to_another_call() -> TestResult {
    let (url, server) = fixture(true).await?;
    let registry = registry(Arc::new(grapt_store::SystemClock))?;
    let approved = approval(&registry, url).await?;
    let tool = McpToolName::new("echo").ok_or("fixture tool name")?;
    let first = registry
        .call(
            "/remote-fixture",
            "remote",
            &tool,
            &approved,
            "ambiguous",
            json!({}),
            BUDGET,
        )
        .await;
    assert!(matches!(first, Err(McpCallRefusal::Disconnected(_))));
    let second = registry
        .call(
            "/remote-fixture",
            "remote",
            &tool,
            &approved,
            "ambiguous",
            json!({}),
            BUDGET,
        )
        .await;
    assert!(matches!(
        second,
        Err(McpCallRefusal::NotConnected(_)) | Err(McpCallRefusal::Session(_))
    ));
    let requests = tokio::time::timeout(BUDGET, server)
        .await??
        .map_err(|error| error as Box<dyn Error>)?;
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.contains("\"tools/call\""))
            .count(),
        1
    );
    Ok(())
}

#[test]
fn remote_declaration_debug_does_not_expose_query_credentials() {
    let declaration = declaration("https://example.com/mcp?token=private-query".to_owned());
    assert!(!format!("{declaration:?}").contains("private-query"));
}
