use super::{HttpClient, HttpError, HttpReply, Incoming};
use std::error::Error;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type TestResult = Result<(), Box<dyn Error>>;
type FixtureError = Box<dyn Error + Send + Sync>;
type FixtureTask = tokio::task::JoinHandle<Result<Vec<String>, FixtureError>>;
const BUDGET: Duration = Duration::from_secs(3);

fn budgets() -> crate::TransportBudgets {
    crate::TransportBudgets {
        startup: BUDGET,
        catalog: BUDGET,
        execution: BUDGET,
    }
}

async fn request(
    stream: &mut tokio::net::TcpStream,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let count = stream.read(&mut chunk).await?;
        if count == 0 {
            return Err("fixture connection ended before its request".into());
        }
        if bytes.len() + count > 8192 {
            return Err("fixture request exceeded its bound".into());
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let head = std::str::from_utf8(&bytes[..end])?;
            let length = head
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

async fn fixture(replies: Vec<String>) -> Result<(String, FixtureTask), Box<dyn Error>> {
    assert!(replies.len() <= 8, "fixture exchange bound");
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/mcp", listener.local_addr()?);
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for reply in replies {
            let (mut stream, _) = listener.accept().await?;
            requests.push(request(&mut stream).await?);
            stream.write_all(reply.as_bytes()).await?;
        }
        Ok(requests)
    });
    Ok((endpoint, task))
}

fn messages(reply: HttpReply) -> Result<Incoming, Box<dyn Error>> {
    match reply {
        HttpReply::Messages(messages) => Ok(*messages),
        HttpReply::Accepted => Err("fixture expected messages".into()),
    }
}

async fn line(reader: &mut crate::RemoteRead) -> Result<String, Box<dyn Error>> {
    tokio::time::timeout(BUDGET, async {
        let mut line = Vec::new();
        for _ in 0..8192 {
            let mut byte = [0u8; 1];
            reader.read_exact(&mut byte).await?;
            if byte[0] == b'\n' {
                return Ok::<_, Box<dyn Error>>(String::from_utf8(line)?);
            }
            line.push(byte[0]);
        }
        Err("fixture output exceeded its line bound".into())
    })
    .await?
}

#[tokio::test]
async fn initialize_metadata_is_reused_by_the_initialized_notification() -> TestResult {
    let initialize = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"fixture"}}}"#;
    let (url, server) = fixture(vec![
        response(
            "200 OK",
            "Content-Type: application/json\r\nMcp-Session-Id: private-session\r\n",
            initialize,
        ),
        response("202 Accepted", "", ""),
    ])
    .await?;
    let client = HttpClient::new(&url, 4096)?;
    let mut incoming = messages(
        client
            .post(
                br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
                BUDGET,
            )
            .await?,
    )?;
    let frame = incoming.next(BUDGET).await?.ok_or("no initialize result")?;
    assert_eq!(frame, initialize.as_bytes());
    assert!(incoming.next(BUDGET).await?.is_none());
    assert!(matches!(
        client
            .post(
                br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                BUDGET
            )
            .await?,
        HttpReply::Accepted
    ));
    let requests = server.await?.map_err(|error| error as Box<dyn Error>)?;
    let notification = requests[1].to_ascii_lowercase();
    assert!(notification.contains("mcp-session-id: private-session"));
    assert!(notification.contains("mcp-protocol-version: 2025-06-18"));
    assert!(notification.contains("accept: application/json, text/event-stream"));
    Ok(())
}

#[tokio::test]
async fn incomplete_sse_resumes_with_get_and_never_replays_the_post() -> TestResult {
    let first = "id: one\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\nid: two\ndata: {\"unfinished\":\n";
    let second = "id: two\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}\n\n";
    let (url, server) = fixture(vec![
        response("200 OK", "Content-Type: text/event-stream\r\n", first),
        response("200 OK", "Content-Type: text/event-stream\r\n", second),
    ])
    .await?;
    let client = HttpClient::new(&url, 4096)?;
    let mut stream = messages(
        client
            .post(
                br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{}}"#,
                BUDGET,
            )
            .await?,
    )?;
    assert!(stream.next(BUDGET).await?.is_some());
    assert!(matches!(
        stream.next(BUDGET).await,
        Err(HttpError::Sse(crate::SseDecodeError::Truncated))
    ));
    assert_eq!(stream.last_event_id(), Some("one"));
    let mut resumed = client
        .resume(stream.last_event_id().ok_or("cursor missing")?, BUDGET)
        .await?;
    assert!(resumed.next(BUDGET).await?.is_some());
    let requests = server.await?.map_err(|error| error as Box<dyn Error>)?;
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("POST /mcp "));
    assert!(requests[1].starts_with("GET /mcp "));
    assert!(requests[1]
        .to_ascii_lowercase()
        .contains("last-event-id: one"));
    assert!(!requests[1].contains("tools/call"));
    Ok(())
}

#[tokio::test]
async fn same_origin_307_preserves_post_but_cross_origin_and_302_are_rejected() -> TestResult {
    let result = r#"{"jsonrpc":"2.0","id":1,"result":{}}"#;
    let (url, server) = fixture(vec![
        response("307 Temporary Redirect", "Location: /second\r\n", ""),
        response(
            "200 OK",
            "Content-Type: application/json; charset=utf-8\r\n",
            result,
        ),
    ])
    .await?;
    let client = HttpClient::new(&url, 4096)?;
    let body = br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
    let mut incoming = messages(client.post(body, BUDGET).await?)?;
    assert_eq!(
        incoming.next(BUDGET).await?.as_deref(),
        Some(result.as_bytes())
    );
    let requests = server.await?.map_err(|error| error as Box<dyn Error>)?;
    assert!(requests[1].starts_with("POST /second "));
    assert!(requests[1].ends_with(std::str::from_utf8(body)?));
    for (status, location) in [
        ("307 Temporary Redirect", "http://127.0.0.1:1/trap"),
        ("302 Found", "/second"),
    ] {
        let (url, server) = fixture(vec![response(
            status,
            &format!("Location: {location}\r\n"),
            "",
        )])
        .await?;
        assert!(matches!(
            HttpClient::new(&url, 4096)?.post(body, BUDGET).await,
            Err(HttpError::Redirect)
        ));
        server.await?.map_err(|error| error as Box<dyn Error>)?;
    }
    Ok(())
}

#[tokio::test]
async fn json_body_and_session_headers_remain_bounded() -> TestResult {
    let (url, server) = fixture(vec![response(
        "200 OK",
        "Content-Type: application/json\r\n",
        &"x".repeat(65),
    )])
    .await?;
    let client = HttpClient::new(&url, 64)?;
    let mut incoming = messages(
        client
            .post(br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#, BUDGET)
            .await?,
    )?;
    assert!(matches!(
        incoming.next(BUDGET).await,
        Err(HttpError::Oversized)
    ));
    server.await?.map_err(|error| error as Box<dyn Error>)?;
    let (url, server) = fixture(vec![response(
        "200 OK",
        &format!(
            "Content-Type: application/json\r\nMcp-Session-Id: {}\r\n",
            "x".repeat(1025)
        ),
        "{}",
    )])
    .await?;
    assert!(matches!(
        HttpClient::new(&url, 4096)?
            .post(br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#, BUDGET)
            .await,
        Err(HttpError::Headers)
    ));
    server.await?.map_err(|error| error as Box<dyn Error>)?;
    Ok(())
}

#[tokio::test]
async fn dead_stream_deadline_covers_comment_dribbles() -> TestResult {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/mcp", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        request(&mut stream).await?;
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 100\r\n\r\n: heartbeat\n\n").await?;
        tokio::time::sleep(Duration::from_millis(250)).await;
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });
    let mut incoming = messages(
        HttpClient::new(&url, 4096)?
            .post(br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#, BUDGET)
            .await?,
    )?;
    assert!(matches!(
        incoming.next(Duration::from_millis(20)).await,
        Err(HttpError::TimedOut)
    ));
    assert!(incoming.next(BUDGET).await?.is_none());
    server.await?.map_err(|error| error as Box<dyn Error>)?;
    Ok(())
}

#[test]
fn endpoint_validation_and_debug_do_not_expose_url_secrets() -> TestResult {
    for endpoint in [
        "http://example.com/mcp",
        "https://user:password@example.com/mcp",
        "https://example.com/mcp#fragment",
        "file:///tmp/mcp",
    ] {
        assert!(matches!(
            HttpClient::new(endpoint, 4096),
            Err(HttpError::Endpoint)
        ));
    }
    let client = HttpClient::new("https://example.com/mcp?token=private-query", 4096)?;
    assert!(!format!("{client:?}").contains("private-query"));
    Ok(())
}

#[tokio::test]
async fn initialization_resumed_after_priming_retains_the_negotiated_version() -> TestResult {
    let result = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","serverInfo":{"name":"fixture"}}}"#;
    let (url, server) = fixture(vec![
        response(
            "200 OK",
            "Content-Type: text/event-stream\r\nMcp-Session-Id: session\r\n",
            "id: primed\ndata:\n\n",
        ),
        response(
            "200 OK",
            "Content-Type: text/event-stream\r\n",
            &format!("id: done\ndata: {result}\n\n"),
        ),
        response("202 Accepted", "", ""),
    ])
    .await?;
    let client = HttpClient::new(&url, 4096)?;
    let mut incoming = messages(
        client
            .post(br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#, BUDGET)
            .await?,
    )?;
    assert!(incoming.next(BUDGET).await?.is_none());
    let mut resumed = client
        .resume(incoming.last_event_id().ok_or("no priming cursor")?, BUDGET)
        .await?;
    assert_eq!(
        resumed.next(BUDGET).await?.as_deref(),
        Some(result.as_bytes())
    );
    client
        .post(
            br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            BUDGET,
        )
        .await?;
    let requests = server.await?.map_err(|error| error as Box<dyn Error>)?;
    assert!(requests[2]
        .to_ascii_lowercase()
        .contains("mcp-protocol-version: 2025-03-26"));
    Ok(())
}

#[tokio::test]
async fn expired_or_rotated_sessions_do_not_replay_requests() -> TestResult {
    let result = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"fixture"}}}"#;
    for (status, headers) in [
        ("404 Not Found", ""),
        ("202 Accepted", "Mcp-Session-Id: changed\r\n"),
    ] {
        let (url, server) = fixture(vec![
            response(
                "200 OK",
                "Content-Type: application/json\r\nMcp-Session-Id: original\r\n",
                result,
            ),
            response(status, headers, ""),
        ])
        .await?;
        let client = HttpClient::new(&url, 4096)?;
        let mut incoming = messages(
            client
                .post(br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#, BUDGET)
                .await?,
        )?;
        incoming.next(BUDGET).await?;
        let reply = client
            .post(
                br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                BUDGET,
            )
            .await;
        if status.starts_with("404") {
            assert!(matches!(reply, Err(HttpError::SessionExpired)));
        } else {
            assert!(matches!(reply, Err(HttpError::Headers)));
        }
        assert_eq!(
            server
                .await?
                .map_err(|error| error as Box<dyn Error>)?
                .len(),
            2
        );
    }
    Ok(())
}

#[tokio::test]
async fn byte_bridge_serializes_notification_acceptance_before_discovery() -> TestResult {
    let initialize = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"fixture"}}}"#;
    let catalog = r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[]}}"#;
    let (url, server) = fixture(vec![
        response(
            "200 OK",
            "Content-Type: application/json\r\nMcp-Session-Id: bridge\r\n",
            initialize,
        ),
        response("202 Accepted", "", ""),
        response("200 OK", "Content-Type: application/json\r\n", catalog),
    ])
    .await?;
    let mut connection = crate::connect(HttpClient::new(&url, 4096)?, budgets())?;
    connection
        .writer
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n")
        .await?;
    assert_eq!(line(&mut connection.reader).await?, initialize);
    connection.writer.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n").await?;
    assert_eq!(line(&mut connection.reader).await?, catalog);
    let requests = tokio::time::timeout(BUDGET, server)
        .await??
        .map_err(|error| error as Box<dyn Error>)?;
    assert!(requests[1].contains("notifications/initialized"));
    assert!(requests[2].contains("tools/list"));
    Ok(())
}

#[tokio::test]
async fn byte_bridge_handles_a_server_ping_while_its_post_stream_is_open() -> TestResult {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/mcp", listener.local_addr()?);
    let terminal = r#"{"jsonrpc":"2.0","id":2,"result":{}}"#;
    let server = tokio::spawn(async move {
        let (mut call, _) = listener.accept().await?;
        request(&mut call).await?;
        let ping = "data: {\"jsonrpc\":\"2.0\",\"id\":\"server-ping\",\"method\":\"ping\"}\n\n";
        let final_event = format!("data: {terminal}\n\n");
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n",
            ping.len() + final_event.len()
        );
        call.write_all(headers.as_bytes()).await?;
        call.write_all(ping.as_bytes()).await?;
        let (mut reply, _) = listener.accept().await?;
        let answer = request(&mut reply).await?;
        assert!(answer.contains("server-ping"));
        assert!(answer.contains("\"result\""));
        reply
            .write_all(response("202 Accepted", "", "").as_bytes())
            .await?;
        call.write_all(final_event.as_bytes()).await?;
        Ok::<_, FixtureError>(())
    });
    let mut connection = crate::connect(HttpClient::new(&url, 4096)?, budgets())?;
    connection
        .writer
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n")
        .await?;
    assert!(line(&mut connection.reader).await?.contains("server-ping"));
    connection
        .writer
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"server-ping\",\"result\":{}}\n")
        .await?;
    assert_eq!(line(&mut connection.reader).await?, terminal);
    tokio::time::timeout(BUDGET, server)
        .await??
        .map_err(|error| error as Box<dyn Error>)?;
    Ok(())
}

#[tokio::test]
async fn byte_bridge_resumes_a_primed_stream_without_replaying_a_call() -> TestResult {
    let terminal = r#"{"jsonrpc":"2.0","id":2,"result":{}}"#;
    let (url, server) = fixture(vec![
        response(
            "200 OK",
            "Content-Type: text/event-stream\r\n",
            "id: primed\ndata:\n\n",
        ),
        response(
            "200 OK",
            "Content-Type: text/event-stream\r\n",
            &format!("id: done\ndata: {terminal}\n\n"),
        ),
    ])
    .await?;
    let mut connection = crate::connect(HttpClient::new(&url, 4096)?, budgets())?;
    connection
        .writer
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\"}\n")
        .await?;
    assert_eq!(line(&mut connection.reader).await?, terminal);
    let requests = tokio::time::timeout(BUDGET, server)
        .await??
        .map_err(|error| error as Box<dyn Error>)?;
    assert!(requests[0].starts_with("POST "));
    assert!(requests[1].starts_with("GET "));
    assert!(requests[1]
        .to_ascii_lowercase()
        .contains("last-event-id: primed"));
    Ok(())
}

#[tokio::test]
async fn retiring_the_connection_cancels_its_response_reader() -> TestResult {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/mcp", listener.local_addr()?);
    let (ready, observed) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        request(&mut stream).await?;
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 1000\r\n\r\n: heartbeat\n\n").await?;
        ready.send(()).map_err(|_| "fixture dropped")?;
        let mut byte = [0u8; 1];
        let end = tokio::time::timeout(BUDGET, stream.read(&mut byte)).await??;
        assert_eq!(end, 0, "retired connection retained its HTTP reader");
        Ok::<_, FixtureError>(())
    });
    let mut connection = crate::connect(HttpClient::new(&url, 4096)?, budgets())?;
    connection
        .writer
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\"}\n")
        .await?;
    tokio::time::timeout(BUDGET, observed).await??;
    drop(connection);
    tokio::time::timeout(BUDGET, server)
        .await??
        .map_err(|error| error as Box<dyn Error>)?;
    Ok(())
}

#[test]
fn configured_headers_are_bounded_sensitive_and_cannot_override_transport_identity() -> TestResult {
    use std::collections::BTreeMap;
    let fields = BTreeMap::from([(
        "Authorization".to_owned(),
        "Bearer fixture-secret".to_owned(),
    )]);
    let headers = super::headers::configured(&fields)?;
    assert!(headers["authorization"].is_sensitive());
    assert!(!format!("{headers:?}").contains("fixture-secret"));
    for name in [
        "Host",
        "Content-Length",
        "Transfer-Encoding",
        "Connection",
        "Accept",
        "Content-Type",
        "Mcp-Session-Id",
        "Mcp-Protocol-Version",
        "Last-Event-Id",
        "Proxy-Authorization",
    ] {
        assert!(
            super::headers::configured(&BTreeMap::from([(name.into(), "override".into())]))
                .is_err()
        );
    }
    assert!(super::headers::configured(&BTreeMap::from([
        ("X-Token".into(), "first".into()),
        ("x-token".into(), "second".into())
    ]))
    .is_err());
    assert!(super::headers::configured(&BTreeMap::from([(
        "Authorization".into(),
        "value\r\ninjected: yes".into()
    )]))
    .is_err());
    assert!(
        super::headers::configured(&BTreeMap::from([("X-Token".into(), "x".repeat(16385))]))
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn configured_bearer_is_sent_only_to_the_bound_origin() -> TestResult {
    tokio::time::timeout(BUDGET, async {
        let other = TcpListener::bind("127.0.0.1:0").await?;
        let location = format!("http://{}/other", other.local_addr()?);
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let redirected = tokio::spawn(async move {
            tokio::select! {
                accepted = other.accept() => {
                    let (mut stream, _) = accepted?;
                    let observed = request(&mut stream).await?;
                    stream.write_all(response("202 Accepted", "", "").as_bytes()).await?;
                    Ok::<_, FixtureError>(Some(observed))
                },
                _ = stopped => Ok(None),
            }
        });
        let (endpoint, task) = fixture(vec![response(
            "307 Temporary Redirect",
            &format!("Location: {location}\r\n"),
            "",
        )])
        .await?;
        let headers = std::collections::BTreeMap::from([(
            "Authorization".into(),
            "Bearer fixture-secret".into(),
        )]);
        let client = HttpClient::new_with_headers(&endpoint, 4096, &headers)?;
        assert!(!format!("{client:?}").contains("fixture-secret"));
        let result = client
            .post(
                br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                BUDGET,
            )
            .await;
        let _ = stop.send(());
        let leaked = redirected.await?.map_err(|error| error.to_string())?;
        assert!(
            leaked.is_none(),
            "another origin received a request with configured credentials"
        );
        assert!(matches!(result, Err(HttpError::Redirect)));
        let requests = task.await?.map_err(|error| error.to_string())?;
        assert_eq!(requests.len(), 1);
        assert!(requests[0]
            .to_ascii_lowercase()
            .contains("authorization: bearer fixture-secret\r\n"));
        Ok::<_, Box<dyn Error>>(())
    })
    .await?
}

mod auth;
