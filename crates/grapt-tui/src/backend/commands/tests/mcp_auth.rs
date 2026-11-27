use super::*;
use crate::backend::CommandId;

pub(super) fn handles(operation: Operation) -> bool {
    matches!(
        operation,
        Operation::McpAuth
            | Operation::McpConfiguration
            | Operation::McpReconnect
            | Operation::McpApproval
    )
}

pub(super) fn response(
    peer: &mut Peer,
    request: &WireRequest,
) -> Result<WireResponse, TransportError> {
    if request.operation() != Operation::McpAuth {
        return configuration_response(peer, request);
    }
    if let Some(socket) = &mut peer.auth_socket {
        return socket.call(request);
    }
    let directory = request
        .query()
        .iter()
        .find(|(key, _)| key == "location[directory]")
        .map(|(_, value)| value.as_str());
    if peer.nested_session && directory.is_some_and(|value| value != "/work/project-a/nested") {
        return Ok(WireResponse {
            status: 400,
            body: grapt_client::wire::encode_invalid_request(
                "MCP catalog location differs from its Session",
            )
            .map_err(|_| TransportError::NotConnected)?,
        });
    }
    assert_eq!(
        request.path(),
        "/api/mcp/%64%6F%63%73%2F%73%65%72%76%65%72/auth"
    );
    let body: serde_json::Value =
        serde_json::from_slice(request.body().ok_or(TransportError::NotConnected)?)
            .map_err(|_| TransportError::NotConnected)?;
    peer.auth.push(body.clone());
    peer.auth_sessions.push(
        request
            .query()
            .iter()
            .find(|(name, _)| name == "session")
            .map(|(_, value)| value.clone()),
    );
    if let Some(status) = peer.next_auth_status.take() {
        let body = if status == 400 {
            grapt_client::wire::encode_invalid_request(
                "MCP authentication failed: unknown sign-in attempt",
            )
            .map_err(|_| TransportError::NotConnected)?
        } else {
            Vec::new()
        };
        return Ok(WireResponse { status, body });
    }
    let response = match body["action"].as_str() {
        Some("login") => {
            serde_json::json!({"state":"awaiting_browser","login_id":"mcl_browser","authorization_url":"https://example.invalid/authorize?state=browser-only"})
        }
        Some("status") => {
            assert_eq!(body["login_id"], "mcl_browser");
            serde_json::json!({"state":"signed_in","login_id":"mcl_browser","authorization_url":null})
        }
        Some("logout") => {
            serde_json::json!({"state":"signed_out","login_id":null,"authorization_url":null})
        }
        _ => return Err(TransportError::NotConnected),
    };
    Ok(WireResponse {
        status: 200,
        body: serde_json::to_vec(&response).map_err(|_| TransportError::NotConnected)?,
    })
}

#[test]
fn sign_in_uses_the_current_session_config_and_does_not_reuse_another_sessions_attempt(
) -> TestResult {
    let mut client = prepared(true)?;
    let session = client.selected().ok_or("saved session")?.clone();
    client.peer_mut().nested_session = true;
    client.resume_session(&session)?;
    assert_eq!(
        client.project().ok_or("project")?.worktree,
        "/work/project-a"
    );
    let first = client
        .selected()
        .ok_or("first session")?
        .spelled()
        .to_owned();
    for command in ["/mcp", "/mcps"] {
        run(&mut client, command)?;
        let query = client.peer_mut().mcp_queries.last().ok_or("MCP query")?;
        assert_eq!(query.as_slice(), &[("session".to_owned(), first.clone())]);
    }
    assert!(run(&mut client, "/mcp login docs/server")?.consume);
    let second = client.create_session()?.spelled().to_owned();
    assert_ne!(first, second);
    assert!(run(&mut client, "/mcp login docs/server")?.consume);
    assert!(run(&mut client, "/mcp login docs/server")?.consume);
    let peer = client.peer_mut();
    assert_eq!(
        peer.auth_sessions,
        [Some(first), Some(second.clone()), Some(second)]
    );
    let actions: Vec<_> = peer
        .auth
        .iter()
        .filter_map(|body| body["action"].as_str())
        .collect();
    assert_eq!(actions, ["login", "login", "status"]);
    Ok(())
}

#[test]
fn an_expired_login_can_restart_while_transient_failures_keep_its_identity() -> TestResult {
    let mut client = prepared(true)?;
    assert!(run(&mut client, "/mcp login docs/server")?.consume);
    client.peer_mut().next_auth_status = Some(503);
    assert!(!run(&mut client, "/mcp login docs/server")?.consume);
    client.peer_mut().next_auth_status = Some(400);
    let expired = run(&mut client, "/mcp login docs/server")?;
    assert!(!expired.consume && expired.notice.contains("start again"));
    assert!(run(&mut client, "/mcp login docs/server")?.consume);
    let actions: Vec<_> = client
        .peer_mut()
        .auth
        .iter()
        .filter_map(|body| body["action"].as_str())
        .collect();
    assert_eq!(actions, ["login", "status", "status", "login"]);
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn a_pending_browser_setup_allows_interrupts_and_finishes_on_a_later_pump() -> TestResult {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::mpsc,
        time::Duration,
    };
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let (release, wait) = mpsc::sync_channel(1);
    let server = std::thread::spawn(move || -> Result<(), String> {
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let mut socket = None;
        for _ in 0..1000 {
            match listener.accept() {
                Ok((accepted, _)) => {
                    socket = Some(accepted);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(error) => return Err(error.to_string()),
            }
        }
        let mut socket = socket.ok_or("operator request did not connect")?;
        socket.set_nonblocking(false).map_err(|e| e.to_string())?;
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| e.to_string())?;
        socket
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| e.to_string())?;
        let mut request = Vec::new();
        for _ in 0..32 {
            let mut bytes = [0; 1024];
            let read = socket.read(&mut bytes).map_err(|e| e.to_string())?;
            if read == 0 {
                return Err("operator request closed".into());
            }
            request.extend_from_slice(&bytes[..read]);
            if request.windows(4).any(|part| part == b"\r\n\r\n") {
                break;
            }
        }
        if !request.windows(4).any(|part| part == b"\r\n\r\n") {
            return Err("request header bound".into());
        }
        let header_end = request
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .ok_or("missing header")?
            + 4;
        let header = std::str::from_utf8(&request[..header_end]).map_err(|e| e.to_string())?;
        let length = header
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .map(|(_, value)| value.trim().parse::<usize>())
            .transpose()
            .map_err(|e| e.to_string())?
            .unwrap_or(0);
        if length > 4096 {
            return Err("operator request body bound".into());
        }
        for _ in 0..8 {
            if request.len() >= header_end + length {
                break;
            }
            let mut bytes = [0; 1024];
            let read = socket.read(&mut bytes).map_err(|e| e.to_string())?;
            if read == 0 {
                return Err("operator request body closed".into());
            }
            request.extend_from_slice(&bytes[..read]);
        }
        if request.len() < header_end + length {
            return Err("operator request body incomplete".into());
        }
        wait.recv_timeout(Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        let body = br#"{"state":"awaiting_browser","login_id":"mcl_browser","authorization_url":"https://example.invalid/authorize"}"#;
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .map_err(|e| e.to_string())?;
        socket.write_all(body).map_err(|e| e.to_string())?;
        Ok(())
    });
    let mut client = prepared(true)?;
    client.peer_mut().auth_socket = Some(grapt_client::HttpConnection::attach(
        address,
        grapt_client::BasicCredential::new("operator", "fixture")?,
        Bounds::PRODUCTION,
    ));
    let mut submissions = crate::backend::first_prompt::Submissions::default();
    let started = submissions.submit(
        &mut client,
        "/mcp login docs/server".into(),
        false,
        CommandId::TEST,
    );
    assert!(started.notice.contains("in progress"));
    assert!(submissions.advance(&mut client).is_none());
    client.interrupt()?;
    assert!(client
        .peer_mut()
        .calls
        .contains(&Operation::SessionInterrupt));
    release.send(())?;
    for _ in 0..1000 {
        if submissions.advance(&mut client).is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let (_, done) = submissions.command.ok_or("operator completion missing")?;
    assert!(done.consume && matches!(done.view, Some(View::Text(_))));
    server.join().map_err(|_| "operator fixture panicked")??;
    Ok(())
}

pub(super) fn configuration_response(
    peer: &mut Peer,
    request: &WireRequest,
) -> Result<WireResponse, TransportError> {
    if request.operation() == Operation::McpApproval {
        return Ok(WireResponse { status: 200, body: serde_json::to_vec(&serde_json::json!({
            "fingerprint":"a".repeat(64), "source":"/work/project-a/grapt.jsonc",
            "directory":"/work/project-a/nested", "description":"python3 server.py", "approved":false
        })).map_err(|_| TransportError::NotConnected)? });
    }
    peer.mcp_queries.push(request.query().to_vec());
    let body = request
        .body()
        .map(serde_json::from_slice)
        .transpose()
        .map_err(|_| TransportError::NotConnected)?
        .unwrap_or(serde_json::Value::Null);
    peer.auth.push(body);
    Ok(WireResponse {
        status: peer.next_auth_status.take().unwrap_or(204),
        body: Vec::new(),
    })
}

#[test]
fn mcp_actions_refresh_after_success_and_do_not_replay_a_failed_change() -> TestResult {
    let mut client = prepared(true)?;
    let session = client.selected().ok_or("session")?.spelled().to_owned();
    for (action, operation) in [
        ("disable", Operation::McpConfiguration),
        ("enable", Operation::McpConfiguration),
        ("reconnect", Operation::McpReconnect),
    ] {
        client.peer_mut().calls.clear();
        client.peer_mut().mcp_queries.clear();
        let done = run(&mut client, &format!("/mcp {action} docs/server"))?;
        assert!(done.consume);
        assert!(!client.mcp_auth_pending());
        assert_eq!(client.peer_mut().calls, [operation, Operation::McpInspect]);
        for query in &client.peer_mut().mcp_queries {
            assert!(query
                .iter()
                .any(|(key, value)| key == "session" && value == &session));
            assert!(!query.iter().any(|(key, _)| key == "location[directory]"));
        }
        let body = client.peer_mut().auth.last().ok_or("change body")?;
        match action {
            "enable" => assert_eq!(
                body,
                &serde_json::json!({"action":"set_enabled","enabled":true})
            ),
            "disable" => assert_eq!(
                body,
                &serde_json::json!({"action":"set_enabled","enabled":false})
            ),
            _ => assert!(body.is_null()),
        }
    }
    client.peer_mut().calls.clear();
    client.peer_mut().next_auth_status = Some(503);
    let done = run(&mut client, "/mcp disable docs/server")?;
    assert!(!done.consume);
    assert!(!client.mcp_auth_pending());
    assert_eq!(client.peer_mut().calls, [Operation::McpConfiguration]);
    assert!(!done.notice.is_empty());
    Ok(())
}

#[test]
fn project_mcp_approval_requires_a_preview_again_after_failure_or_session_switch() -> TestResult {
    let mut client = prepared(true)?;
    client.peer_mut().calls.clear();
    let preview = run(&mut client, "/mcp approve docs/server")?;
    assert!(preview.consume);
    assert_eq!(client.peer_mut().calls, [Operation::McpApproval]);
    assert!(client.peer_mut().auth.is_empty());
    client.peer_mut().calls.clear();
    client.peer_mut().next_auth_status = Some(400);
    let failed = run(&mut client, "/mcp approve docs/server")?;
    assert!(!failed.consume);
    assert_eq!(client.peer_mut().calls, [Operation::McpConfiguration]);
    assert_eq!(
        client.peer_mut().auth.last().ok_or("approval")?,
        &serde_json::json!({"action":"approve_project","fingerprint":"a".repeat(64)})
    );
    client.peer_mut().calls.clear();
    run(&mut client, "/mcp approve docs/server")?;
    assert_eq!(client.peer_mut().calls, [Operation::McpApproval]);
    client.create_session()?;
    client.peer_mut().calls.clear();
    run(&mut client, "/mcp approve docs/server")?;
    assert_eq!(client.peer_mut().calls, [Operation::McpApproval]);
    client.peer_mut().calls.clear();
    assert!(run(&mut client, "/mcp approve docs/server")?.consume);
    assert_eq!(
        client.peer_mut().calls,
        [Operation::McpConfiguration, Operation::McpInspect]
    );
    client.peer_mut().calls.clear();
    assert!(run(&mut client, "/mcp revoke docs/server")?.consume);
    assert_eq!(
        client.peer_mut().auth.last().ok_or("revoke")?,
        &serde_json::json!({"action":"revoke_project_approval"})
    );
    Ok(())
}
