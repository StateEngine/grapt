use super::*;
use crate::oauth::OAuthMetadata;
use grapt_types::Secret;
use serde_json::json;

fn flow(port: u16) -> Result<AuthorizationFlow, Box<dyn std::error::Error>> {
    let metadata = json!({"issuer":"https://auth.example/tenant", "authorization_endpoint":"https://auth.example/authorize", "token_endpoint":"https://auth.example/token", "code_challenge_methods_supported":["S256"], "authorization_response_iss_parameter_supported":true});
    Ok(AuthorizationFlow::new(
        OAuthMetadata::parse(
            &serde_json::to_vec(&metadata)?,
            "https://auth.example/tenant",
        )?,
        "https://mcp.example/mcp",
        "se-code-fixture",
        port,
        Secret::new([3; 32]),
        Secret::new([9; 32]),
        None,
    )?)
}

async fn send(port: u16, target: &str, host: &str) -> Result<String, Box<dyn std::error::Error>> {
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await?;
        stream
            .write_all(
                format!("GET {target} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        Ok(String::from_utf8(response)?)
    })
    .await?
}

#[tokio::test]
async fn invalid_host_state_or_issuer_does_not_consume_a_valid_callback(
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = CallbackListener::bind().await?;
    let port = listener.port();
    let flow = flow(port)?;
    let authorization = url::Url::parse(flow.authorization_url().expose())?;
    let state = authorization
        .query_pairs()
        .find(|(key, _)| key == "state")
        .ok_or("fixture state missing")?
        .1
        .to_string();
    let path = flow.redirect_uri().path().to_owned();
    let target =
        format!("{path}?state={state}&iss=https%3A%2F%2Fauth.example%2Ftenant&code=PRIVATE_CODE");
    let wait = tokio::spawn(listener.wait(flow, Duration::from_secs(15)));
    let host = format!("127.0.0.1:{port}");
    for (target, host) in [
        (target.clone(), "attacker.example".into()),
        (target.replace(&state, "wrong-state"), host.clone()),
        (
            target.replace("auth.example", "other.example"),
            host.clone(),
        ),
    ] {
        let response = send(port, &target, &host).await?;
        assert!(response.starts_with("HTTP/1.1 400"));
        assert!(!response.contains("PRIVATE_CODE"));
        assert!(!response.contains(&state));
    }
    let response = send(port, &target, &host).await?;
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(!response.contains("PRIVATE_CODE"));
    assert!(response.contains("Cache-Control: no-store"));
    let exchange = tokio::time::timeout(Duration::from_secs(3), wait).await???;
    assert!(exchange.form.expose().contains("code=PRIVATE_CODE"));
    assert!(!format!("{exchange:?}").contains("PRIVATE_CODE"));
    Ok(())
}

#[tokio::test]
async fn a_stalled_browser_socket_cannot_extend_the_callback_deadline(
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = CallbackListener::bind().await?;
    let port = listener.port();
    let flow = flow(port)?;
    let socket = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await?;
    let outcome = tokio::time::timeout(
        Duration::from_secs(1),
        listener.wait(flow, Duration::from_millis(30)),
    )
    .await?;
    assert!(matches!(outcome, Err(ListenerError::Exhausted)));
    drop(socket);
    let listener = CallbackListener::bind().await?;
    let wrong = flow_for_other_port(listener.port())?;
    assert!(matches!(
        listener.wait(wrong, Duration::from_secs(1)).await,
        Err(ListenerError::Configuration)
    ));
    Ok(())
}
fn flow_for_other_port(port: u16) -> Result<AuthorizationFlow, Box<dyn std::error::Error>> {
    flow(if port == 65535 { 65534 } else { port + 1 })
}
