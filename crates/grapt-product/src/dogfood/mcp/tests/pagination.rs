//! Pagination crosses the actual transport and never publishes a partial catalog.
use super::super::*;
use super::fixtures::*;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

async fn discover(second: Value) -> (Result<StartedServer, StartFailure>, McpSession) {
    discover_with_delays(second, Duration::ZERO, Duration::from_secs(2)).await
}

async fn discover_with_delays(
    second: Value,
    page_delay: Duration,
    budget: Duration,
) -> (Result<StartedServer, StartFailure>, McpSession) {
    let (ours, theirs) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(theirs);
        let mut lines = BufReader::new(read);
        for index in 0..4 {
            let mut line = String::new();
            assert!(matches!(lines.read_line(&mut line).await, Ok(n) if n > 0));
            let request: Value = match serde_json::from_str(&line) {
                Ok(value) => value,
                Err(error) => panic!("invalid request: {error}"),
            };
            if index == 1 {
                assert_eq!(request["method"], "notifications/initialized");
                continue;
            }
            let result = match index {
                0 => {
                    json!({"protocolVersion":grapt_mcp::PROTOCOL_VERSION,"serverInfo":{"name":"pages"}})
                }
                2 => {
                    assert_eq!(request["method"], "tools/list");
                    assert!(request["params"].get("cursor").is_none());
                    json!({"tools":[{"name":"first","inputSchema":{"type":"object"}}],"nextCursor":"opaque-page-2"})
                }
                _ => {
                    assert_eq!(request["method"], "tools/list");
                    assert_eq!(request["params"]["cursor"], "opaque-page-2");
                    second.clone()
                }
            };
            let reply = format!(
                "{}\n",
                json!({"jsonrpc":"2.0","id":request["id"],"result":result})
            );
            if index >= 2 {
                tokio::time::sleep(page_delay).await;
            }
            if write.write_all(reply.as_bytes()).await.is_err() {
                return;
            }
        }
    });
    let mut channel = channel(ours);
    let mut session = session();
    let result = start(
        &mut channel,
        &mut session,
        bounds(),
        Duration::from_secs(2),
        budget,
        "test",
    )
    .await;
    // A one-page regression must fail promptly, including when it leaves the
    // fixture waiting for the request that never came.
    drop(channel);
    assert!(server.await.is_ok());
    (result, session)
}

#[tokio::test]
async fn discovery_pages_share_one_catalog_deadline() {
    let (result, session) = discover_with_delays(
        json!({"tools":[]}),
        Duration::from_millis(300),
        Duration::from_millis(500),
    )
    .await;
    assert!(matches!(
        result,
        Err(StartFailure {
            reason: Unusable::TimedOut,
            phase: StartPhase::Discovery
        })
    ));
    assert!(!session.state().is_ready());
    assert!(session.catalog().is_empty());
}

#[tokio::test]
async fn discovery_follows_cursor_and_registers_the_second_page_tool() {
    let (result, session) =
        discover(json!({"tools":[{"name":"second","inputSchema":{"type":"object"}}]})).await;
    let started = match result {
        Ok(started) => started,
        Err(error) => panic!("pagination failed: {error:?}"),
    };
    assert_eq!(started.catalog.len(), 2);
    assert_eq!(started.catalog[1].name().as_str(), "second");
    assert_eq!(session.catalog().len(), 2);
    assert!(session.state().is_ready());
}

#[tokio::test]
async fn discovery_cycle_or_malformed_second_page_never_becomes_ready() {
    for second in [
        json!({"tools":[],"nextCursor":"opaque-page-2"}),
        json!({"tools":[],"nextCursor":3}),
        json!({"tools":"broken"}),
    ] {
        let (result, session) = discover(second).await;
        let failure = match result {
            Err(failure) => failure,
            Ok(_) => panic!("invalid catalog was published"),
        };
        assert_eq!(failure.phase, StartPhase::Discovery);
        assert!(!session.state().is_ready());
        assert!(session.catalog().is_empty());
    }
}
