//! File-changing calls retain their proposals while permission is pending.
use super::*;
use crate::projection::{Mark, ToolPhase};

const EMPTY_ASKS: &[u8] = br#"{"location":{"directory":"/work","project":{"id":"prj_double","directory":"/work"}},"data":[],"hasMore":false}"#;

/// Serves one running tool row and an exact permission list.
#[derive(Debug)]
struct HeldPeer {
    inner: ContractDouble,
    page: Vec<u8>,
    permissions: Vec<u8>,
}
impl crate::transport::Transport for HeldPeer {
    fn call(
        &mut self,
        request: &crate::transport::WireRequest,
    ) -> Result<crate::transport::WireResponse, crate::transport::TransportError> {
        let body = match request.operation() {
            crate::contract::Operation::MessageList => self.page.clone(),
            crate::contract::Operation::PermissionRequestList => self.permissions.clone(),
            _ => return self.inner.call(request),
        };
        Ok(crate::transport::WireResponse { status: 200, body })
    }
}
impl crate::client::Connection for HeldPeer {
    fn subscribed(&self) -> bool {
        true
    }
    fn poll_frame(
        &mut self,
    ) -> Result<crate::transport::FramePoll, crate::transport::TransportError> {
        Ok(crate::transport::FramePoll::Idle)
    }
    fn resubscribe(&mut self) {}
}

fn patch_input() -> serde_json::Value {
    serde_json::json!({"operations":[{
        "op":"update","path":"src/a.rs","before_sha256":"x",
        "old_text":"keep\nold\n","new_text":"keep\nnew\n"
    }]})
}

fn held_call(
    name: &str,
    input: serde_json::Value,
) -> Result<Client<HeldPeer>, Box<dyn std::error::Error>> {
    let tool = crate::wire::DurableTool {
        duration_ms: None,
        id: crate::wire::tool_call_id("call_diff")?,
        name: name.to_owned(),
        state: crate::wire::DurableToolState::Running {
            input: input.to_string(),
        },
        progress: Vec::new(),
    };
    let message = crate::wire::assistant_message(
        "msg_000000000001",
        &[crate::wire::MessageItem::Tool(tool)],
        None,
        None,
        None,
    )?;
    let mut client = Client::new(
        HeldPeer {
            inner: ContractDouble::new(),
            page: crate::wire::encode_messages(vec![message], None)?,
            permissions: EMPTY_ASKS.to_vec(),
        },
        Bounds::PRODUCTION,
    );
    client.select_first_offer()?;
    let session = client.create_session()?;
    client.select(&session)?;
    Ok(client)
}

#[test]
fn permission_held_file_calls_show_proposals_and_keep_their_colours(
) -> Result<(), Box<dyn std::error::Error>> {
    for (name, input) in [
        ("patch", patch_input()),
        (
            "edit",
            serde_json::json!({"path":"src/a.rs","old_text":"keep\nold\n","new_text":"keep\nnew\n"}),
        ),
        (
            "write",
            serde_json::json!({"path":"src/a.rs","content":"new\n"}),
        ),
    ] {
        let mut client = held_call(name, input)?;
        let own = client
            .selected()
            .map(|session| session.spelled().to_owned())
            .unwrap_or_default();
        client.peer_mut().permissions = format!(
        r#"{{"location":{{"directory":"/work","project":{{"id":"prj_double","directory":"/work"}}}},"data":[{{"id":"per_held","sessionID":"{own}","action":"fs.write","resources":["/work/src/a.rs"],"status":"pending","source":{{"type":"tool","messageID":"msg_held","callID":"call_diff"}}}}],"hasMore":false}}"#
    )
    .into_bytes();
        client.refresh_permissions()?;
        let view = client.view().ok_or("selected view absent")?;
        let slice = view
            .slices
            .iter()
            .find(|s| s.tool_id.is_some())
            .ok_or("tool absent")?;
        assert_eq!(slice.tool_phase, Some(ToolPhase::Running));
        assert!(!slice.closed && slice.marks.contains(&Mark::Insert));
        if name != "write" {
            assert!(slice.marks.contains(&Mark::Delete));
        }
    }
    Ok(())
}
