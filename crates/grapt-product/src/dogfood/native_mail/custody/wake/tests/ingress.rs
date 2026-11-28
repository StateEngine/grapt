//! Authenticated ingress uses the same ordinary host as production.
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub(super) async fn bind_capability(
    application: &Arc<ServerApplication>,
    session: &SessionId,
) -> TestResult {
    let response = call(
        application,
        "PUT",
        &format!("/api/session/{session}/stateengine/capability"),
        r#"{"capability":"fixture-secret"}"#,
    )
    .await?;
    assert!(
        response.starts_with(b"HTTP/1.1 204"),
        "authenticated capability route refused"
    );
    Ok(())
}

pub(super) async fn call(
    application: &Arc<ServerApplication>,
    method: &str,
    path: &str,
    body: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let limits = crate::compose_host::host_limits()?;
    let config = crate::input::ServiceConfig {
        slot: "product".into(),
        instance: "coder".into(),
        environment: "acceptance".into(),
        build: "seat03-fixture".into(),
        version: "0.1.0".into(),
        serve_window_ms: 10000,
        provider_connect_ms: 1000,
        provider_response_head_ms: 1000,
        provider_active_attempts: 1,
        close_grace_ms: None,
        max_inflight_streams: None,
    };
    let composition = crate::compose::ServerComposition::ready(
        application.clone(),
        application.clone(),
        application.hub.clone(),
        None,
    );
    let service = crate::compose_host::product_service(
        &limits,
        &crate::input::Credential {
            username: "fixture".into(),
            secret: "secret".into(),
        },
        grapt_types::service::ProcessId::new(std::process::id()).ok_or("process")?,
        &config,
        Some(composition),
    )?;
    let host = grapt_api::PreparedApiHost::new(
        crate::compose_host::bind_loopback().await?,
        service,
        limits,
        Arc::new(grapt_api::TokioDeadlineDriver::new()),
    )?
    .start();
    let address = host.ready().observed_address();
    let result = async {
        let mut socket = TcpStream::connect(address).await?;
        let request = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Basic Zml4dHVyZTpzZWNyZXQ=\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        socket.write_all(request.as_bytes()).await?;
        let mut response = Vec::new();
        for _ in 0..32 {
            let mut chunk = [0; 2048];
            let count = tokio::time::timeout(std::time::Duration::from_secs(5), socket.read(&mut chunk)).await??;
            if count == 0 { break; }
            response.extend_from_slice(&chunk[..count]);
        }
        Ok::<_, Box<dyn std::error::Error>>(response)
    }.await;
    assert!(
        matches!(
            host.stop().await,
            grapt_api::ApiHostExit::StoppedGracefully { .. }
        ),
        "fixture API host did not join cleanly"
    );
    result
}
