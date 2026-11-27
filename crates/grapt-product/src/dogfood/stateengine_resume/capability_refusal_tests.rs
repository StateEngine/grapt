use super::*;
use grapt_types::InstructionKey;
use std::collections::BTreeMap;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Clone, Copy, Debug)]
enum RefusalWire {
    Rpc(i64),
    TextEnvelope,
    StructuredEnvelope,
}

async fn refuse(
    stream: tokio::io::DuplexStream,
    wire: RefusalWire,
    code: &'static str,
) -> Result<(), String> {
    let (read, mut write) = tokio::io::split(stream);
    let mut read = BufReader::new(read);
    for _ in 0..5 {
        let mut line = String::new();
        if read.read_line(&mut line).await.map_err(|e| e.to_string())? == 0 {
            return Ok(());
        }
        let request: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        let called = request["method"] == "tools/call";
        let result = match request["method"].as_str() {
            Some("notifications/initialized") => continue,
            Some("initialize") => json!({"protocolVersion":grapt_mcp::PROTOCOL_VERSION,
                "capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}),
            Some("tools/list") => json!({"tools":[{"name":"stateengine_resume",
                "inputSchema":{"type":"object","properties":{"since":{"type":"string"},
                    "byte_budget":{"type":"integer"}}}}]}),
            Some("tools/call") => {
                let body = json!({"schema":"se.error/v1","code":code,
                    "detail":if code == "capability-invalid" { json!({"subject":"fixture session", "reason":"revoked", "next_step":"launch a new session"}) } else { json!({"op":"resume"}) },
                    "retry":{"kind":"never"},"class":"unauthorized",
                    "exit":1,"message":"fixture session refused","remedy":{"text":"launch a new session","command":null}});
                let mut result =
                    json!({"isError":true,"content":[{"type":"text","text":body.to_string()}]});
                if matches!(wire, RefusalWire::StructuredEnvelope) {
                    result["structuredContent"] = body;
                }
                result
            }
            _ => return Err("unexpected fixture method".into()),
        };
        let frame = if let (true, RefusalWire::Rpc(rpc_code)) = (called, wire) {
            json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":rpc_code,
                "message":format!("{code}: Session credential MCP session identity: expected a sec1 session capability")}})
        } else {
            json!({"jsonrpc":"2.0","id":request["id"],"result":result})
        };
        write
            .write_all((frame.to_string() + "\n").as_bytes())
            .await
            .map_err(|e| e.to_string())?;
    }
    Err("fixture call bound exceeded".into())
}

async fn refused_refresh(
    wire: RefusalWire,
    code: &'static str,
) -> Result<ContinuityUnavailable, Box<dyn std::error::Error>> {
    let session: SessionId = "ses_capability_fixture".parse()?;
    let mut store = grapt_store::EventStore::open_in_memory()?;
    InstructionService::put_entry(
        &mut store,
        &session,
        &InstructionKey::new("stateengine-identity").ok_or("key")?,
        r#"{"scope":"project","project_id":"fixture","agent_id":"agt_fixture"}"#,
        1,
    )?;
    let inbox = Arc::new(Mutex::new(Inbox::new(store)?));
    let custody = Arc::new(Mutex::new(SessionCapabilities::default()));
    custody
        .lock()
        .map_err(|_| "custody")?
        .put(session.clone(), "SCC_CAPABILITY")?;
    let registry = Arc::new(crate::dogfood::mcp::registry(Arc::new(
        grapt_store::SystemClock,
    ))?);
    let (client, peer) = tokio::io::duplex(8192);
    let peer = tokio::spawn(refuse(peer, wire, code));
    let directory = "/capability-fixture";
    let declaration = crate::dogfood::mcp::Declaration::Local {
        command: vec!["fixture".into()],
        cwd: directory.into(),
        environment: BTreeMap::new(),
        timeouts: crate::dogfood::mcp::McpTimeouts::default(),
    };
    registry
        .install_live_for_test(
            directory,
            "stateengine",
            declaration.clone(),
            client,
            "fixture",
        )
        .await;
    let declarations = BTreeMap::from([("stateengine".to_owned(), declaration)]);
    let published = Arc::new(
        registry
            .with_status(directory, &declarations, |entries| {
                crate::dogfood::mcp::tool_definitions(entries, &[])
            })
            .await,
    );
    let mut resume = ResumeContext::new(
        (
            session,
            AgentId::new("fixture").ok_or("agent")?,
            directory.into(),
        ),
        (inbox, custody.clone(), registry.clone()),
        published,
        Arc::new(grapt_store::OsRandomIds::seed()?),
    );
    let result = resume.refresh().await;
    assert_eq!(resume.take_input(), Err(ContinuityUnavailable::Incomplete));
    assert!(custody.lock().map_err(|_| "custody")?.resumes.is_empty());
    drop(resume);
    drop(registry);
    tokio::time::timeout(Duration::from_secs(2), peer).await???;
    result
        .err()
        .ok_or_else(|| "a refused capability admitted continuity".into())
}

#[tokio::test]
async fn native_rpc_capability_refusal_requires_rebinding() -> Result<(), Box<dyn std::error::Error>>
{
    assert_eq!(
        refused_refresh(RefusalWire::Rpc(-32001), "capability-invalid").await?,
        ContinuityUnavailable::Capability
    );
    assert_eq!(
        refused_refresh(RefusalWire::Rpc(-32001), "owner-required").await?,
        ContinuityUnavailable::Transport
    );
    assert_eq!(
        refused_refresh(RefusalWire::Rpc(-32001), "session-required").await?,
        ContinuityUnavailable::Capability
    );
    assert_eq!(
        refused_refresh(RefusalWire::Rpc(-32000), "capability-invalid").await?,
        ContinuityUnavailable::Transport
    );
    Ok(())
}

#[tokio::test]
async fn native_tool_capability_refusal_requires_rebinding(
) -> Result<(), Box<dyn std::error::Error>> {
    for wire in [RefusalWire::TextEnvelope, RefusalWire::StructuredEnvelope] {
        assert_eq!(
            refused_refresh(wire, "capability-invalid").await?,
            ContinuityUnavailable::Capability,
            "{wire:?}"
        );
        assert_eq!(
            refused_refresh(wire, "owner-required").await?,
            ContinuityUnavailable::Transport,
            "{wire:?}"
        );
    }
    Ok(())
}
