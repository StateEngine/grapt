//! Real MCP call notifications reach only their bounded live display owner.
use super::super::*;
use crate::dogfood::elicitation::FormProducer;
use grapt_api::u3a::ToolProgressData;
use grapt_types::{MessageId, SessionId, ToolCallId};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn active_call_progress_preserves_metadata_and_is_correlated_throttled_and_live_only(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut store = grapt_store::EventStore::open_in_memory()?;
    grapt_form::FormService::initialize(&mut store)?;
    let producer = FormProducer {
        inbox: Arc::new(Mutex::new(grapt_inbox::Inbox::new(store)?)),
        ids: Arc::new(grapt_store::OsRandomIds::seed()?),
        clock: Arc::new(grapt_store::SystemClock),
        hub: crate::dogfood::event_hub()?,
        hooks: None,
    };
    let events_before = producer
        .inbox
        .lock()
        .map_err(|_| "inbox")?
        .store()
        .connection()
        .query_row("SELECT COUNT(*) FROM event", [], |row| row.get::<_, i64>(0))?;
    let feed = producer.hub.subscribe_host()?;
    let session: SessionId = "ses_progress".parse()?;
    let message: MessageId = "msg_progress".parse()?;
    let call = ToolCallId::new("call_progress").ok_or("call identity")?;
    let parent = ToolCallId::new("call_progress_parent").ok_or("parent identity")?;
    let mut observer = producer.connect("/progress", "fixture")?;
    observer.observe_progress(&session, &message, &parent, &call, "fixture_echo");
    let other_call = ToolCallId::new("call_progress_b").ok_or("other_call identity")?;
    let mut other = producer.connect("/progress", "fixture")?;
    other.observe_progress(&session, &message, &parent, &other_call, "fixture_echo");
    let (first, second) = tokio::join!(wire(&mut observer, &call), wire(&mut other, &other_call));
    first?;
    second?;
    drop(other);
    drop(observer);
    feed.cancel();
    let mut messages = std::collections::BTreeMap::<ToolCallId, Vec<String>>::new();
    for _ in 0..128 {
        let Some(frame) = feed.next_frame()? else {
            break;
        };
        let text = std::str::from_utf8(&frame)?;
        let Some(data) = text.lines().find_map(|line| line.strip_prefix("data:")) else {
            continue;
        };
        let value: Value = serde_json::from_str(data.trim())?;
        if value["type"] != "session.tool.progress" {
            continue;
        }
        let data: ToolProgressData = serde_json::from_value(value["data"].clone())?;
        assert_eq!(data.call(), &parent);
        let source = data.source_call().ok_or("source call missing")?;
        assert!([&call, &other_call].contains(&source));
        for entry in data.metadata().calls() {
            messages
                .entry(source.clone())
                .or_default()
                .push(entry.message().ok_or("progress message")?.to_owned());
        }
    }
    for source in [&call, &other_call] {
        let messages = messages
            .get(source)
            .ok_or("parallel source progress missing")?;
        // Publication timestamps need not match the earlier throttle samples.
        assert!(messages.len() < 64, "notification burst was not coalesced");
        assert_eq!(messages.first().map(String::as_str), Some("42%"));
        assert_eq!(
            messages
                .iter()
                .rev()
                .find(|message| !message.is_empty())
                .map(String::as_str),
            Some("Staging complete")
        );
        assert_eq!(
            messages.last().map(String::as_str),
            Some(""),
            "settled call retained its live progress"
        );
        assert!(!messages
            .iter()
            .any(|message| matches!(message.as_str(), "FOREIGN" | "DECREASING")));
    }
    assert_eq!(
        producer
            .inbox
            .lock()
            .map_err(|_| "inbox")?
            .store()
            .connection()
            .query_row("SELECT COUNT(*) FROM event", [], |row| row.get::<_, i64>(0))?,
        events_before,
        "advisory progress wrote history"
    );
    Ok(())
}

async fn wire(
    observer: &mut crate::dogfood::elicitation::LiveForms,
    call: &ToolCallId,
) -> Result<(), Box<dyn std::error::Error>> {
    let bounds = McpBounds::conservative()?;
    let mut session = McpSession::declared(SessionBounds::conservative()?);
    let tool = McpToolName::new("echo").ok_or("tool name")?;
    session.start()?;
    session.ready(vec![tool.clone()])?;
    let (client, server) = tokio::io::duplex(65_536);
    let (read, write) = tokio::io::split(client);
    let mut channel = McpChannel::new(read, write, bounds);
    let server = async move {
        let (read, mut write) = tokio::io::split(server);
        let mut line = String::new();
        BufReader::new(read).read_line(&mut line).await?;
        let request: Value = serde_json::from_str(&line)?;
        assert_eq!(request["params"]["_meta"]["fixture"], "kept");
        let token = request["params"]["_meta"]["progressToken"]
            .as_str()
            .ok_or("progress token missing")?;
        let update = |token: &str, progress: u64, message: Option<&str>| {
            let mut params =
                serde_json::json!({"progressToken":token,"progress":progress,"total":200});
            if let Some(message) = message {
                params["message"] = message.into();
            }
            serde_json::json!({"jsonrpc":"2.0","method":"notifications/progress","params":params})
                .to_string()
                + "\n"
        };
        write
            .write_all(update("other-request", 199, Some("FOREIGN")).as_bytes())
            .await?;
        write.write_all(update(token, 84, None).as_bytes()).await?;
        for value in 85..149 {
            write
                .write_all(update(token, value, Some("Burst")).as_bytes())
                .await?;
        }
        tokio::time::sleep(Duration::from_millis(510)).await;
        write
            .write_all(update(token, 84, Some("DECREASING")).as_bytes())
            .await?;
        write
            .write_all(update(token, 150, Some("Staging complete\n")).as_bytes())
            .await?;
        let reply = serde_json::json!({"jsonrpc":"2.0","id":request["id"],"result":{"content":[{"type":"text","text":"ok"}]}}).to_string()+"\n";
        write.write_all(reply.as_bytes()).await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    let mut reusable = false;
    let (result, server) = tokio::join!(
        run_call_with(
            &mut channel,
            &mut session,
            bounds,
            &tool,
            call.as_str(),
            serde_json::json!({}),
            Duration::from_secs(5),
            Some(observer),
            None,
            Some(serde_json::json!({"fixture":"kept"})),
            &mut reusable
        ),
        server
    );
    server?;
    result.map_err(|error| format!("{error:?}"))?;
    assert!(reusable);
    Ok(())
}
