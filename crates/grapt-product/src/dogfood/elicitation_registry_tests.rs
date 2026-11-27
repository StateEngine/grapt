//! The configured-call registry and durable form producer on one duplex connection.
use super::{elicitation::FormProducer, mcp};
use grapt_form::{catalog::FormState, global_catalog::pending_global_forms, FormService};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

type TestResult = Result<(), Box<dyn std::error::Error>>;

async fn server(stream: tokio::io::DuplexStream) -> Result<Option<Value>, String> {
    let (read, mut write) = tokio::io::split(stream);
    let mut read = BufReader::new(read);
    for expected in [
        "initialize",
        "notifications/initialized",
        "tools/list",
        "tools/call",
    ] {
        let mut line = String::new();
        tokio::io::AsyncReadExt::take(&mut read, 8192)
            .read_line(&mut line)
            .await
            .map_err(|error| error.to_string())?;
        let message: Value = serde_json::from_str(&line).map_err(|error| error.to_string())?;
        assert_eq!(message["method"], expected);
        let result = match expected {
            "initialize" => {
                json!({"protocolVersion":grapt_mcp::PROTOCOL_VERSION,"capabilities":{"tools":{}},"serverInfo":{"name":"greeter","version":"1"}})
            }
            "notifications/initialized" => continue,
            "tools/list" => json!({"tools":[{"name":"greet","inputSchema":{"type":"object"}}]}),
            "tools/call" => {
                let elicitation = json!({"jsonrpc":"2.0","id":"name-request","method":"elicitation/create","params":{"message":"Name?","requestedSchema":{"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}}});
                write
                    .write_all((elicitation.to_string() + "\n").as_bytes())
                    .await
                    .map_err(|error| error.to_string())?;
                line.clear();
                let count = tokio::io::AsyncReadExt::take(&mut read, 8192)
                    .read_line(&mut line)
                    .await
                    .map_err(|error| error.to_string())?;
                if count == 0 {
                    return Ok(None);
                }
                let reply: Value =
                    serde_json::from_str(&line).map_err(|error| error.to_string())?;
                assert_eq!(reply["id"], "name-request");
                let terminal = json!({"jsonrpc":"2.0","id":message["id"],"result":{"content":[{"type":"text","text":"greeted"}]}});
                write
                    .write_all((terminal.to_string() + "\n").as_bytes())
                    .await
                    .map_err(|error| error.to_string())?;
                return Ok(Some(reply));
            }
            _ => return Err("unexpected fixture phase".into()),
        };
        let reply = json!({"jsonrpc":"2.0","id":message["id"],"result":result});
        write
            .write_all((reply.to_string() + "\n").as_bytes())
            .await
            .map_err(|error| error.to_string())?;
    }
    Err("fixture never reached its tool call".into())
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn registry_call_consumes_the_form_reply_once_and_retirement_cannot_resume_it() -> TestResult
{
    for retire in [false, true] {
        let mut store = grapt_store::EventStore::open_in_memory()?;
        FormService::initialize(&mut store)?;
        let producer = FormProducer {
            hooks: None,
            inbox: Arc::new(Mutex::new(grapt_inbox::Inbox::new(store)?)),
            ids: Arc::new(grapt_store::OsRandomIds::seed()?),
            clock: Arc::new(grapt_store::SystemClock),
            hub: super::event_hub()?,
        };
        let registry = Arc::new(mcp::registry(std::sync::Arc::new(
            grapt_store::SystemClock,
        ))?);
        let (client, peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(server(peer));
        let directory = "/elicitation-registry";
        let declaration = mcp::Declaration::Local {
            command: vec!["greeter".into()],
            cwd: directory.into(),
            environment: BTreeMap::new(),
            timeouts: mcp::McpTimeouts::default(),
        };
        let incarnation = registry
            .install_live_for_test(directory, "greeter", declaration.clone(), client, "greeter")
            .await;
        let approved = mcp::LaunchIdentity {
            declaration: std::sync::Arc::new(declaration),
            program: "greeter".into(),
            cwd: directory.into(),
            arguments: Vec::new(),
            incarnation,
        };
        let mut forms = producer.connect(directory, "greeter")?;
        let running_registry = Arc::clone(&registry);
        let tool = grapt_mcp::McpToolName::new("greet").ok_or("tool")?;
        let call = tokio::spawn(async move {
            running_registry
                .call_with(
                    directory,
                    "greeter",
                    &tool,
                    &approved,
                    "call-one",
                    json!({}),
                    Duration::from_secs(2),
                    Some(&mut forms),
                    None,
                    None,
                )
                .await
        });
        let row = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let rows = {
                    let inbox = producer.inbox.lock().map_err(|_| "inbox poisoned")?;
                    pending_global_forms(inbox.store()).map_err(|error| error.to_string())?
                };
                if let Some(row) = rows.into_iter().next() {
                    return Ok::<_, String>(row);
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await??;
        if retire {
            registry
                .with_status(directory, &BTreeMap::new(), |entries| {
                    assert!(entries.is_empty())
                })
                .await;
            assert!(matches!(
                tokio::time::timeout(Duration::from_secs(1), call).await??,
                Err(mcp::McpCallRefusal::ConnectionReplaced { .. })
            ));
            {
                let mut inbox = producer.inbox.lock().map_err(|_| "inbox poisoned")?;
                assert_eq!(
                    pending_global_forms(inbox.store())?[0].state,
                    FormState::Suspended(grapt_types::FormSuspensionReason::Shutdown)
                );
                FormService::answer_global_on(
                    inbox.store_mut(),
                    &"evt_late_answer".parse()?,
                    &row.id,
                    &row.binding,
                    &json!({"name":"late"}),
                    2,
                )?;
            }
            assert!(tokio::time::timeout(Duration::from_secs(1), server)
                .await???
                .is_none());
        } else {
            {
                let mut inbox = producer.inbox.lock().map_err(|_| "inbox poisoned")?;
                FormService::answer_global_on(
                    inbox.store_mut(),
                    &"evt_live_answer".parse()?,
                    &row.id,
                    &row.binding,
                    &json!({"name":"Ada"}),
                    2,
                )?;
            }
            let outcome = call.await?.map_err(|error| format!("{error:?}"))?;
            assert_eq!(outcome.text(), "greeted");
            let response = server.await??.ok_or("server received no answer")?;
            assert_eq!(
                response["result"],
                json!({"action":"accept","content":{"name":"Ada"}})
            );
        }
    }
    Ok(())
}
