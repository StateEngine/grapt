//! Public MCP frames during startup, without a fabricated call or Session.
use super::channel::{McpChannel, ServerRequestHandler};
use super::startup::start_with;
use grapt_mcp::{
    session::{McpSession, SessionBounds},
    McpBounds,
};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct Consent {
    protocols: Vec<String>,
}
impl ServerRequestHandler for Consent {
    fn protocol(&mut self, revision: &str) {
        self.protocols.push(revision.to_owned());
    }
    fn answer<'a>(
        &'a mut self,
        request: &'a grapt_mcp::incoming::ServerRequest,
        dispatch: Option<&'a grapt_form::global::CallDispatch>,
    ) -> grapt_runtime::BoxFuture<'a, Result<Value, String>> {
        Box::pin(async move {
            assert!(dispatch.is_none());
            tokio::time::sleep(Duration::from_secs(2)).await;
            Ok(if request.params()["mode"] == "url" {
                json!({"action":"decline"})
            } else {
                json!({"action":"accept","content":{"name":"human"}})
            })
        })
    }
}

#[tokio::test(start_paused = true)]
async fn startup_elicitation_preserves_phase_time_and_negotiated_revision(
) -> Result<(), Box<dyn std::error::Error>> {
    let bounds = McpBounds::conservative()?;
    let (client, server) = tokio::io::duplex(8192);
    let (read, write) = tokio::io::split(client);
    let mut channel = McpChannel::new(read, write, bounds);
    let mut session = McpSession::declared(SessionBounds::conservative()?);
    let mut handler = Consent {
        protocols: Vec::new(),
    };
    let server = async move {
        let (read, mut write) = tokio::io::split(server);
        let mut read = BufReader::new(read);
        for step in 0..4 {
            let request = loop {
                let mut line = String::new();
                read.read_line(&mut line).await?;
                let value: Value = serde_json::from_str(&line)?;
                if value["method"] != "notifications/initialized" {
                    break value;
                }
            };
            if step == 0 {
                assert_eq!(
                    request["params"]["capabilities"]["elicitation"]["url"],
                    json!({})
                );
            }
            let params = if step == 0 {
                json!({"mode":"url","message":"Connect manually","url":"https://example.test/connect","elicitationId":"external-id"})
            } else {
                json!({"message":"Choose a name","requestedSchema":{"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}})
            };
            let elicitation = json!({"jsonrpc":"2.0","id":format!("server-{step}"),"method":"elicitation/create","params":params});
            write
                .write_all((elicitation.to_string() + "\n").as_bytes())
                .await?;
            let mut line = String::new();
            read.read_line(&mut line).await?;
            let answer: Value = serde_json::from_str(&line)?;
            assert_eq!(answer["id"], elicitation["id"]);
            if step == 0 {
                assert_eq!(answer["result"], json!({"action":"decline"}));
            }
            let result = match step {
                0 => {
                    json!({"protocolVersion":"2025-06-18","serverInfo":{"name":"ask","version":"1"},"capabilities":{}})
                }
                1 => json!({"tools":[],"nextCursor":"page-two"}),
                _ => json!({"tools":[]}),
            };
            let reply = json!({"jsonrpc":"2.0","id":request["id"],"result":result});
            write
                .write_all((reply.to_string() + "\n").as_bytes())
                .await?;
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    let client = async {
        start_with(
            &mut channel,
            &mut session,
            bounds,
            Duration::from_secs(1),
            Duration::from_secs(1),
            "test",
            Some(&mut handler),
        )
        .await
        .map_err(|error| format!("{error:?}"))?;
        // A handler minted for a later operation must inherit the connection's
        // revision rather than reverting to the client's newest offer.
        let request = grapt_mcp::tools_list_request(channel.sequence(), bounds)?;
        channel
            .exchange_with(request, Duration::from_secs(1), Some(&mut handler), None)
            .await
            .map_err(|error| format!("{error:?}"))?;
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    let (client, server) = tokio::join!(client, server);
    client?;
    server?;
    assert_eq!(
        handler.protocols,
        [
            grapt_mcp::PROTOCOL_VERSION,
            "2025-06-18",
            "2025-06-18",
            "2025-06-18"
        ]
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn recovery_startup_accepts_a_minute_later_without_spending_active_time(
) -> Result<(), Box<dyn std::error::Error>> {
    struct HumanReply;
    impl ServerRequestHandler for HumanReply {
        fn answer<'a>(
            &'a mut self,
            request: &'a grapt_mcp::incoming::ServerRequest,
            dispatch: Option<&'a grapt_form::global::CallDispatch>,
        ) -> grapt_runtime::BoxFuture<'a, Result<Value, String>> {
            Box::pin(async move {
                assert!(dispatch.is_none());
                let url = grapt_mcp::elicitation::UrlElicitation::parse(request.params())?;
                // A real human-scale delay, not a reduced startup or round
                // allowance. Virtual time keeps the author regression light.
                tokio::time::sleep(Duration::from_secs(60)).await;
                url.response(&json!({"decision":"accept"}))
            })
        }
    }
    let bounds = McpBounds::conservative()?;
    let timeouts = super::McpTimeouts {
        execution: super::retry::ROUND_BUDGET,
        ..Default::default()
    };
    let pause = grapt_mcp_client::HumanWait::default();
    let lease = std::sync::Arc::new(super::startup_lease::StartupLease::with_human_wait(
        pause.clone(),
    ));
    // A catalog admission shares its cumulative allowance across servers, but
    // a nested call joining the other server must not inherit consent's pause.
    let other_lease = std::sync::Arc::new(super::startup_lease::StartupLease::with_human_wait(
        pause.clone(),
    ));
    let mut registry = super::registry(std::sync::Arc::new(grapt_store::SystemClock))?;
    let declaration = std::sync::Arc::new(super::Declaration::Remote {
        url: "https://example.test/mcp".into(),
        headers: Default::default(),
        oauth: false,
        timeouts,
    });
    let approved = super::LaunchIdentity {
        declaration: std::sync::Arc::clone(&declaration),
        program: "remote:https://example.test".into(),
        cwd: "/startup".into(),
        arguments: Vec::new(),
        incarnation: 1,
    };
    // The approved route outlives eviction; another catalog owns its
    // replacement startup when the unsent call reaches re-admission.
    registry.retained_locations = 1;
    registry
        .with_status("/startup", &Default::default(), |_| ())
        .await;
    registry
        .with_status("/elsewhere", &Default::default(), |_| ())
        .await;
    assert!(!registry.retains_location("/startup").await);
    registry
        .with_status("/startup", &Default::default(), |_| ())
        .await;
    let held = super::LocationConnections {
        directory: "/startup".into(),
        declarations: std::collections::BTreeMap::from([
            ("consent".into(), (*declaration).clone()),
            ("other".into(), (*declaration).clone()),
        ]),
        starting: std::collections::BTreeMap::from([
            ("consent".into(), std::sync::Arc::downgrade(&lease)),
            ("other".into(), std::sync::Arc::downgrade(&other_lease)),
        ]),
        ..Default::default()
    };
    let declarations = held.declarations.clone();
    registry
        .entries
        .lock()
        .await
        .insert("/startup".into(), held);
    let outer = pause.deadline(super::retry::ROUND_BUDGET)?;
    let (client, server) = tokio::io::duplex(8192);
    let (read, write) = tokio::io::split(client);
    let mut channel = McpChannel::new(
        Box::new(read) as super::BoxedRead,
        Box::new(write) as super::BoxedWrite,
        bounds,
    )
    .with_startup_lease(lease);
    let mut session = McpSession::declared(SessionBounds::conservative()?);
    let server = async move {
        let (read, mut write) = tokio::io::split(server);
        let mut read = BufReader::new(read);
        let mut line = String::new();
        read.read_line(&mut line).await?;
        let initialize: Value = serde_json::from_str(&line)?;
        tokio::time::sleep(Duration::from_secs(2)).await;
        write.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"startup-consent\",\"method\":\"elicitation/create\",\"params\":{\"mode\":\"url\",\"message\":\"Connect manually\",\"url\":\"https://example.test/connect\",\"elicitationId\":\"external-startup\"}}\n").await?;
        line.clear();
        read.read_line(&mut line).await?;
        let answer: Value = serde_json::from_str(&line)?;
        assert_eq!(answer["result"], json!({"action":"accept"}));
        tokio::time::sleep(Duration::from_secs(2)).await;
        let reply = json!({"jsonrpc":"2.0","id":initialize["id"],"result":{
            "protocolVersion":"2025-11-25","serverInfo":{"name":"consent"},"capabilities":{}}});
        write
            .write_all((reply.to_string() + "\n").as_bytes())
            .await?;
        line.clear();
        read.read_line(&mut line).await?; // notifications/initialized has no reply.
        line.clear();
        read.read_line(&mut line).await?;
        let listing: Value = serde_json::from_str(&line)?;
        let reply = json!({"jsonrpc":"2.0","id":listing["id"],"result":{"tools":[
            {"name":"echo","inputSchema":{"type":"object"}}
        ]}});
        write
            .write_all((reply.to_string() + "\n").as_bytes())
            .await?;
        line.clear();
        read.read_line(&mut line).await?;
        let call: Value = serde_json::from_str(&line)?;
        assert_eq!(call["method"], "tools/call");
        let reply = json!({"jsonrpc":"2.0","id":call["id"],"result":{
            "content":[{"type":"text","text":"after startup"}]
        }});
        write
            .write_all((reply.to_string() + "\n").as_bytes())
            .await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    let mut handler = HumanReply;
    let started = async {
        let started = outer
            .timeout(start_with(
                &mut channel,
                &mut session,
                bounds,
                timeouts.startup,
                timeouts.catalog,
                "human-startup",
                Some(&mut handler),
            ))
            .await?
            .map_err(|failure| format!("{failure:?}"))?;
        let slot = super::ConnectionSlot::admit(&registry.connections, super::MAX_LIVE_CONNECTIONS)
            .ok_or("connection capacity")?;
        let live = super::LiveServer {
            roots: super::roots::State::new(None),
            notifications: None,
            identity: started.identity,
            catalog: started.catalog,
            stderr: None,
            timeouts,
            launch: approved.clone(),
            io: std::sync::Arc::new(super::HeldIo::new(super::LiveIo {
                _slot: slot,
                channel,
                session,
                _child: None,
                _stderr: None,
                _remote: None,
            })),
        };
        {
            let mut locations = registry.entries.lock().await;
            let held = locations.get_mut("/startup").ok_or("startup location")?;
            held.starting.remove("consent");
            held.entries
                .insert("consent".into(), super::ServerEntry::Live(Box::new(live)));
        }
        registry.changed.notify_waiters();
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    let joined = super::nested_startup::NestedStartups::default();
    let unrelated = super::nested_startup::NestedStartups::default();
    let recovered = joined.observe(async {
        let admitted = tokio::time::Instant::now();
        let human_paused = registry
            .readmit_missing(
                "/startup",
                "/startup",
                "consent",
                &approved,
                &declarations,
                timeouts.execution,
            )
            .await
            .map_err(|error| format!("{error:?}"))?;
        let resumed = admitted.checked_add(human_paused).ok_or("pause overflow")?;
        let outcome = registry
            .call(
                "/startup",
                "consent",
                &grapt_mcp::McpToolName::new("echo").ok_or("fixture tool")?,
                &approved,
                "after-readmission",
                json!({}),
                timeouts.execution.saturating_sub(resumed.elapsed()),
            )
            .await
            .map_err(|error| format!("{error:?}"))?;
        assert_eq!(outcome.text(), "after startup");
        Ok::<_, Box<dyn std::error::Error>>(human_paused)
    })?;
    let unjoined = unrelated.observe(async {
        let other = super::admission::recover_queued(
            &registry,
            "/startup",
            "other",
            &approved,
            super::retry::ROUND_BUDGET,
        );
        let check = async {
            tokio::time::sleep(Duration::from_secs(3)).await;
            assert!(pause.is_waiting());
            assert!(joined.is_waiting()?);
            assert!(!unrelated.is_waiting()?);
            Ok::<(), grapt_mcp_client::HttpError>(())
        };
        let (result, checked) = tokio::join!(other, check);
        checked?;
        assert!(matches!(result, Err(super::McpCallRefusal::TimedOut)));
        assert!(joined.is_waiting()?);
        Ok::<(), grapt_mcp_client::HttpError>(())
    })?;
    let (started, served, recovered, unjoined) = tokio::join!(
        started,
        tokio::time::timeout(Duration::from_secs(90), server),
        recovered,
        unjoined,
    );
    served??;
    started?;
    unjoined?;
    drop(other_lease);
    assert!(!joined.is_waiting()?);
    assert!(recovered.map_err(|failure| format!("{failure:?}"))? >= Duration::from_secs(60));
    assert!(pause.paused_elapsed() >= Duration::from_secs(60));
    assert!(pause.active_elapsed()? < timeouts.startup);
    assert!(outer.remaining()? < super::retry::ROUND_BUDGET);
    Ok(())
}
