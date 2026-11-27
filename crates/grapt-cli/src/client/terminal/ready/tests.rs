use super::signal;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn notification_consumes_only_its_byte_and_preserves_the_final_receipt() -> Result<(), String>
{
    let mut input = &b"\x1e{\"outcome\":\"served\"}\n"[..];
    signal(&mut input).await.map_err(|error| error.reason)?;
    assert_eq!(input, b"{\"outcome\":\"served\"}\n");
    Ok(())
}

#[tokio::test]
async fn startup_refusal_keeps_its_first_byte_and_eof_is_a_failure() -> Result<(), String> {
    let mut refusal = &b"{\"outcome\":\"refused\"}"[..];
    let failure = signal(&mut refusal).await.err().ok_or("refusal accepted")?;
    assert_eq!(failure.prefix, Some(b'{'));
    assert_eq!(refusal, b"\"outcome\":\"refused\"}");
    let failure = signal(&mut &b""[..]).await.err().ok_or("EOF accepted")?;
    assert!(failure.reason.contains("closed before notification"));
    assert_eq!(failure.prefix, None);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_pending_notification_read_can_be_cancelled_without_losing_the_signal(
) -> Result<(), String> {
    let (mut input, mut writer) = tokio::io::duplex(16);
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(30), signal(&mut input))
            .await
            .is_err()
    );
    writer
        .write_all(b"\x1e{")
        .await
        .map_err(|error| error.to_string())?;
    signal(&mut input).await.map_err(|error| error.reason)?;
    assert_eq!(
        input.read_u8().await.map_err(|error| error.to_string())?,
        b'{'
    );
    Ok(())
}

#[cfg(unix)]
mod process;
