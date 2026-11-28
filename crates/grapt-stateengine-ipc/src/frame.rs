// SPDX-License-Identifier: MIT OR Apache-2.0
//! One frame: a `u32` big-endian length, then that many bytes of UTF-8 JSON.
//! A zero or oversized length refuses before any payload buffer exists.
use crate::IpcError;
use serde::{de::DeserializeOwned, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use zeroize::Zeroizing;

/// Read one frame of at most `limit` bytes.
pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
    limit: u32,
) -> Result<Zeroizing<Vec<u8>>, IpcError> {
    let mut prefix = [0; 4];
    reader.read_exact(&mut prefix).await?;
    let announced = u32::from_be_bytes(prefix);
    if announced == 0 {
        return Err(IpcError::EmptyFrame);
    }
    if announced > limit {
        return Err(IpcError::FrameTooLarge { announced, limit });
    }
    let length =
        usize::try_from(announced).map_err(|_| IpcError::FrameTooLarge { announced, limit })?;
    let mut payload = Zeroizing::new(vec![0; length]);
    reader.read_exact(&mut payload).await?;
    Ok(payload)
}

/// Write one frame of at most `limit` bytes.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    payload: &[u8],
    limit: u32,
) -> Result<(), IpcError> {
    let length = u32::try_from(payload.len())
        .ok()
        .filter(|length| *length != 0 && *length <= limit)
        .ok_or(IpcError::OutgoingFrame {
            length: payload.len(),
            limit,
        })?;
    writer.write_all(&length.to_be_bytes()).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;
    Ok(())
}

/// A frame's value, after the depth and repeated-key checks.
pub(crate) fn decode<T: DeserializeOwned>(payload: &[u8]) -> Result<T, IpcError> {
    crate::strict::check(payload)?;
    serde_json::from_slice(payload).map_err(|error| IpcError::Malformed(error.to_string()))
}

pub(crate) fn encode<T: Serialize>(value: &T) -> Result<Zeroizing<Vec<u8>>, IpcError> {
    serde_json::to_vec(value)
        .map(Zeroizing::new)
        .map_err(|error| IpcError::Malformed(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn zero_and_oversized_lengths_refuse_before_the_payload() -> Result<(), IpcError> {
        let mut zero: &[u8] = &[0, 0, 0, 0];
        assert!(matches!(
            read_frame(&mut zero, 16).await,
            Err(IpcError::EmptyFrame)
        ));
        // No payload follows: an allocation-first reader would fail on I/O instead.
        let mut huge: &[u8] = &u32::MAX.to_be_bytes();
        assert!(matches!(
            read_frame(&mut huge, crate::MAX_FRAME_BYTES).await,
            Err(IpcError::FrameTooLarge {
                announced: u32::MAX,
                ..
            })
        ));
        let mut written = Vec::new();
        write_frame(&mut written, b"{}", 16).await?;
        assert_eq!(written, [0, 0, 0, 2, b'{', b'}']);
        assert!(write_frame(&mut written, &[b' '; 17], 16).await.is_err());
        let mut framed: &[u8] = &written;
        assert_eq!(*read_frame(&mut framed, 16).await?, *b"{}");
        Ok(())
    }
}
