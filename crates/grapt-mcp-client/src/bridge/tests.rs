use super::*;

#[tokio::test]
async fn cancelling_a_partial_pipe_read_preserves_its_consumed_bytes(
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut source, destination) = tokio::io::duplex(4096);
    let (reader, _writer) = tokio::io::split(destination);
    let mut reader = BufReader::new(reader);
    let mut partial = Vec::new();
    source.write_all(b"{\"jsonrpc\":\"2.0\",").await?;
    assert!(tokio::time::timeout(
        Duration::from_millis(10),
        read_frame(&mut reader, &mut partial, 4096)
    )
    .await
    .is_err());
    assert_eq!(partial, b"{\"jsonrpc\":\"2.0\",");
    source.write_all(b"\"method\":\"ping\"}\n").await?;
    let frame = tokio::time::timeout(
        Duration::from_secs(1),
        read_frame(&mut reader, &mut partial, 4096),
    )
    .await??
    .ok_or("missing frame")?;
    assert_eq!(frame, b"{\"jsonrpc\":\"2.0\",\"method\":\"ping\"}");
    assert!(partial.is_empty());
    Ok(())
}

#[tokio::test]
async fn expired_original_deadline_is_not_restarted_for_a_response_reader() {
    assert!(matches!(
        crate::HumanWait::default().deadline(Duration::ZERO),
        Err(HttpError::TimedOut)
    ));
}
