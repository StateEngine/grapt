use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn seat01_event_observation_is_not_approval_and_opt_in_is_session_scoped(
) -> Result<(), Box<dyn std::error::Error>> {
    for scope in [None, Some("ses_approval_wire".parse::<SessionId>()?)] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint =
            ClientEndpoint::attributed_loopback(&format!("http://{}/", listener.local_addr()?))?;
        let http = production_client(false, Allowance::Request(Duration::from_secs(2)))?;
        let client = ApiClient {
            endpoint,
            client: http.clone(),
            event_client: http,
            basic: Some(Arc::new(super::super::Credential {
                username: "test".into(),
                secret: "test".into(),
            })),
            limits: CliLimits::production(),
            embedded: None,
            streak: Arc::new(std::sync::Mutex::new(Default::default())),
        };
        let peer = async {
            let (mut stream, _) = listener.accept().await?;
            let mut bytes = Vec::new();
            for _ in 0..16 {
                let mut chunk = [0_u8; 1024];
                let count = stream.read(&mut chunk).await?;
                if count == 0 {
                    return Err("incomplete event request".into());
                }
                bytes.extend_from_slice(&chunk[..count]);
                if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await?;
                    return Ok::<_, Box<dyn std::error::Error>>(String::from_utf8(bytes)?);
                }
            }
            Err("over-bound event request".into())
        };
        let (subscription, wire) = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(client.events_with_approver(scope.as_ref()), peer)
        })
        .await?;
        drop(subscription?);
        let wire = wire?.to_ascii_lowercase();
        assert!(wire.starts_with("get /api/event http/1.1\r\n"));
        assert!(
            wire.contains("authorization: basic "),
            "approver attachment bypassed ordinary authentication"
        );
        match scope {
            Some(session) => assert!(
                wire.contains(&format!("x-grapt-approver: {}\r\n", session.as_str())),
                "attachment did not name its selected Session"
            ),
            None => assert!(
                !wire.contains("x-grapt-approver:"),
                "ordinary headless observer became an approver"
            ),
        }
    }
    Ok(())
}
