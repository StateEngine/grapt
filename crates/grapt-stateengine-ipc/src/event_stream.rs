// SPDX-License-Identifier: MIT OR Apache-2.0
//! An open `events.subscribe` stream on a connection of its own. Every frame is
//! checked against its request before it is trusted, and a lagged frame or a
//! host error ends the stream; dropping the stream closes its connection, which
//! cancels the subscription.
use crate::events::{frame, EventCursor, StreamFrame, STREAM_SCHEMA};
use crate::frame::{announced_length, decode};
use crate::wire::{IpcResponse, IpcResult};
use crate::{IpcError, API_VERSION};
use tokio::io::{AsyncRead, AsyncReadExt};

const READ_CHUNK_BYTES: usize = 8192;

/// An open subscription.
pub struct EventStream<S> {
    stream: S,
    request_id: u64,
    generation: u64,
    limit: u32,
    resume: Option<EventCursor>,
    /// Bytes read toward the next frame, kept across a cancelled `next`.
    pending: Vec<u8>,
    ended: bool,
}

impl<S> std::fmt::Debug for EventStream<S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EventStream")
            .field("request_id", &self.request_id)
            .field("limit", &self.limit)
            .field("resume", &self.resume)
            .field("ended", &self.ended)
            .finish_non_exhaustive()
    }
}

impl<S: AsyncRead + Unpin> EventStream<S> {
    pub(crate) fn new(
        stream: S,
        request_id: u64,
        generation: u64,
        limit: u32,
        resume: Option<EventCursor>,
    ) -> Self {
        Self {
            stream,
            request_id,
            generation,
            limit,
            resume,
            pending: Vec::new(),
            ended: false,
        }
    }

    /// The next frame. A lagged frame, a host error or any failure ends the
    /// stream; later calls refuse with [`IpcError::StreamEnded`]. Cancel-safe:
    /// a `next` dropped while waiting loses no bytes of the frame it awaited.
    pub async fn next(&mut self) -> Result<StreamFrame, IpcError> {
        if self.ended {
            return Err(IpcError::StreamEnded);
        }
        let frame = self
            .read_frame()
            .await
            .and_then(|bytes| self.accept(&bytes));
        self.ended = !matches!(
            frame,
            Ok(StreamFrame::Event(_) | StreamFrame::Heartbeat { .. })
        );
        frame
    }

    /// The next whole frame's payload. Its only await is a cancel-safe read
    /// whose bytes land in `pending` before anything else happens.
    async fn read_frame(&mut self) -> Result<Vec<u8>, IpcError> {
        loop {
            if let Some(prefix) = self.pending.first_chunk::<4>() {
                let end = announced_length(*prefix, self.limit)?.saturating_add(4);
                if self.pending.len() >= end {
                    let frame = self.pending[4..end].to_vec();
                    self.pending.drain(..end);
                    return Ok(frame);
                }
            }
            let mut chunk = [0; READ_CHUNK_BYTES];
            let read = self.stream.read(&mut chunk).await?;
            if read == 0 {
                return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
            }
            self.pending.extend_from_slice(&chunk[..read]);
        }
    }

    /// A frame checked against its request, advancing the resume cursor.
    fn accept(&mut self, bytes: &[u8]) -> Result<StreamFrame, IpcError> {
        let response: IpcResponse = decode(bytes)?;
        if response.version != API_VERSION {
            return Err(IpcError::ResponseMismatch("protocol version"));
        }
        if response.request_id != self.request_id {
            return Err(IpcError::ResponseMismatch("request id"));
        }
        if response.generation != self.generation {
            return Err(IpcError::ResponseMismatch("host generation"));
        }
        let output = match response.result {
            IpcResult::Output(output) => output,
            IpcResult::Error(error) => return Err(IpcError::Refused(error)),
        };
        if output.schema != STREAM_SCHEMA {
            return Err(IpcError::Malformed(format!(
                "a stream frame has schema {:?}",
                output.schema
            )));
        }
        let frame = frame(output.value).map_err(|error| IpcError::Malformed(error.to_string()))?;
        match &frame {
            StreamFrame::Event(event) => self.resume = Some(event.cursor.clone()),
            StreamFrame::Heartbeat { head_cursor } => self.resume = Some(head_cursor.clone()),
            StreamFrame::Lagged { .. } => {}
        }
        Ok(frame)
    }

    /// Where a new subscription resumes after this one ends: the last
    /// delivered event or heartbeat head, else where this one started.
    pub fn resume_cursor(&self) -> Option<&EventCursor> {
        self.resume.as_ref()
    }
}

#[cfg(test)]
mod tests;
