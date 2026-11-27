use super::*;
use tokio::io::AsyncReadExt;
#[derive(Debug)]
struct FixedClock;
impl grapt_types::ClockAuthority for FixedClock {
    fn now(&self) -> grapt_types::Timestamp {
        grapt_types::Timestamp::from_millis(1)
    }
}
fn params_at(bytes: usize) -> Result<Value, Box<dyn std::error::Error>> {
    let mut params = json!({"sessionId":"ses_frame", "update":{
        "sessionUpdate":"tool_call_update", "toolCallId":"call_frame", "status":"completed",
        "rawInput":{"value":"i".repeat(131_072)},
        "content":[{"type":"content","content":{"type":"text","text":""}}],
        "rawOutput":{"metadata":{}}
    }});
    let base =
        serde_json::to_vec(&json!({"jsonrpc":"2.0","method":"session/update","params":params}))?
            .len()
            + 1;
    let padding = bytes
        .checked_sub(base)
        .ok_or("frame smaller than its envelope")?;
    params["update"]["content"][0]["content"]["text"] = json!("x".repeat(padding));
    assert_eq!(
        serde_json::to_vec(&json!({"jsonrpc":"2.0","method":"session/update","params":params}))?
            .len()
            + 1,
        bytes
    );
    Ok(params)
}
async fn send(
    params: Value,
    maximum: usize,
) -> Result<(Result<(), AcpConnectionError>, Vec<u8>), Box<dyn std::error::Error>> {
    let (outbound, inbound) = mpsc::channel(1);
    let peer = Peer {
        output: outbound,
        pending: Arc::new(Mutex::new(BTreeMap::new())),
        next_id: Arc::new(AtomicU64::new(0)),
        frame_bytes: maximum,
    };
    let (output, mut reader) = tokio::io::duplex(maximum.saturating_add(1));
    let writer = tokio::spawn(write_output(
        inbound,
        output,
        tokio::io::sink(),
        maximum,
        super::super::timing::AcpTiming::bounded(Arc::new(FixedClock)),
    ));
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        peer.notification("session/update", params),
    )
    .await?;
    drop(peer);
    let settled = tokio::time::timeout(Duration::from_secs(2), writer).await?;
    assert!(settled.is_ok(), "writer task panicked");
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), reader.read_to_end(&mut bytes)).await??;
    Ok((result, bytes))
}
#[tokio::test]
async fn combined_tool_frames_obey_the_real_peer_writer_boundary(
) -> Result<(), Box<dyn std::error::Error>> {
    for size in [FRAME_BYTES - 1, FRAME_BYTES, FRAME_BYTES + 1] {
        let (result, bytes) = send(params_at(size)?, FRAME_BYTES).await?;
        if size <= FRAME_BYTES {
            result?;
            assert_eq!(bytes.len(), size);
            assert_eq!(bytes.last(), Some(&b'\n'));
        } else {
            assert!(
                matches!(result,Err(AcpConnectionError::OutputFrameTooLarge{maximum}) if maximum==FRAME_BYTES)
            );
            assert!(bytes.is_empty());
        }
    }
    Ok(())
}
#[tokio::test]
async fn truncated_snapshot_and_marker_fit_the_actual_peer_writer(
) -> Result<(), Box<dyn std::error::Error>> {
    for maximum in [200_000, FRAME_BYTES] {
        let mut params = params_at(FRAME_BYTES + 1)?;
        let original = params["update"]["content"][0]["content"]["text"]
            .as_str()
            .ok_or("missing text")?
            .to_owned();
        let update = params.get_mut("update").ok_or("missing update")?.take();
        params["update"] = super::super::tool_output::bounded("ses_frame", update, maximum)
            .map_err(|fault| fault.message)?;
        let (result, bytes) = send(params, maximum).await?;
        result?;
        assert!(bytes.len() <= maximum);
        assert_eq!(bytes.last(), Some(&b'\n'));
        let delivered: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(delivered["params"]["update"]["status"], "completed");
        assert_eq!(
            delivered["params"]["update"]["rawInput"]["value"],
            "i".repeat(131_072)
        );
        let text = delivered["params"]["update"]["content"][0]["content"]["text"]
            .as_str()
            .ok_or("missing final text")?;
        let (prefix, marker) = text
            .rsplit_once("\n[output truncated:")
            .ok_or("missing marker")?;
        assert!(original.starts_with(prefix));
        assert_eq!(
            marker,
            format!(
                " kept {} of {} bytes; full output is in the session history]",
                prefix.len(),
                original.len()
            )
        );
    }
    Ok(())
}
