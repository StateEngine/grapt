//! Cancellation-safe idle intake uses the channel's existing frame accumulator.
use super::{ChannelError, McpChannel};
use grapt_mcp::McpProtocolError;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite};
impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> McpChannel<R, W> {
    pub fn poll_idle(&mut self, cx: &mut Context<'_>) -> Result<(), ChannelError> {
        if self.write_in_progress {
            return Err(ChannelError::Disconnected("interrupted frame write".into()));
        }
        match self.poll_idle_reply(cx) {
            Poll::Pending => return Ok(()),
            Poll::Ready(result) => result?,
        }
        // A bounded batch keeps a flooding server from monopolizing the executor.
        for _ in 0..16 {
            let frame = {
                let future = self.read_frame();
                tokio::pin!(future);
                match future.poll(cx) {
                    Poll::Pending => return Ok(()),
                    Poll::Ready(frame) => frame?,
                }
            };
            if let Some(notification) = grapt_mcp::incoming::notification(self.bounds, &frame)
                .map_err(ChannelError::Protocol)?
            {
                if let Some(signal) = &self.catalog_signal {
                    signal.accept(&notification);
                }
                continue;
            }
            if let Some(request) = grapt_mcp::incoming::server_request(self.bounds, &frame)
                .map_err(ChannelError::Protocol)?
            {
                let mut response = if request.method() == "ping" {
                    request.result(self.bounds, serde_json::json!({}))
                } else if request.method() == "roots/list" {
                    self.roots_response(&request)
                } else {
                    request.error(
                        self.bounds,
                        -32601,
                        "Server request requires an active operation",
                    )
                }
                .map_err(ChannelError::Protocol)?;
                response.push(b'\n');
                self.idle_reply = Some((response, 0));
                match self.poll_idle_reply(cx) {
                    Poll::Pending => return Ok(()),
                    Poll::Ready(result) => result?,
                }
                continue;
            }
            // Cancellation leaves the outstanding request for the next owner
            // to classify; consume its late reply before a new exchange starts.
            let value: serde_json::Value = serde_json::from_slice(&frame).map_err(|_| {
                ChannelError::Protocol(McpProtocolError::Malformed("idle frame".into()))
            })?;
            let id = value["id"].as_u64().ok_or_else(|| {
                ChannelError::Protocol(McpProtocolError::Malformed("idle reply identity".into()))
            })?;
            if value["jsonrpc"] != "2.0"
                || value.get("result").is_some() == value.get("error").is_some()
            {
                return Err(ChannelError::Protocol(McpProtocolError::Malformed(
                    "abandoned reply".into(),
                )));
            }
            if self.outstanding == Some(id) {
                self.outstanding = None;
            } else if !self.forget_abandoned(id) {
                return Err(ChannelError::Protocol(McpProtocolError::Malformed(
                    "uncorrelated idle reply".into(),
                )));
            }
        }
        Ok(())
    }
    pub(super) fn poll_idle_reply(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), ChannelError>> {
        if self.idle_reply.is_none() {
            let changed = match self
                .roots
                .as_ref()
                .map(|roots| roots.take_changed())
                .transpose()
            {
                Ok(changed) => changed.unwrap_or(false),
                Err(error) => {
                    return Poll::Ready(Err(ChannelError::Protocol(McpProtocolError::Malformed(
                        error,
                    ))))
                }
            };
            if changed {
                let frame = serde_json::json!({"jsonrpc":"2.0","method":"notifications/roots/list_changed"}).to_string();
                self.idle_reply = Some((format!("{frame}\n").into_bytes(), 0));
            }
        }
        if let Some((frame, offset)) = &mut self.idle_reply {
            while *offset < frame.len() {
                match Pin::new(&mut self.writer).poll_write(cx, &frame[*offset..]) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Ok(0)) => {
                        return Poll::Ready(Err(ChannelError::Disconnected(
                            "idle reply write ended".into(),
                        )))
                    }
                    Poll::Ready(Ok(written)) => *offset += written,
                    Poll::Ready(Err(error)) => {
                        return Poll::Ready(Err(ChannelError::Disconnected(error.to_string())))
                    }
                }
            }
            match Pin::new(&mut self.writer).poll_flush(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => {
                    return Poll::Ready(Err(ChannelError::Disconnected(error.to_string())))
                }
                Poll::Ready(Ok(())) => self.idle_reply = None,
            }
        }
        Poll::Ready(Ok(()))
    }
}
