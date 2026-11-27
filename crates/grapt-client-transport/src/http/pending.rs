//! Incremental acquisition for recovery reads. One poll performs bounded,
//! nonblocking I/O; the existing encoder and head parser remain authoritative.

use super::*;

const IO_PER_POLL: usize = 8;

/// One MCP acquisition owns both allowances across every observed form.
struct McpAcquisition {
    observed_at: tokio::time::Instant,
    active_left: Duration,
    human_left: Duration,
    human_wait: bool,
}

impl McpAcquisition {
    fn account(&mut self) {
        let now = tokio::time::Instant::now();
        let elapsed = now.saturating_duration_since(self.observed_at);
        self.observed_at = now;
        let remaining = if self.human_wait {
            &mut self.human_left
        } else {
            &mut self.active_left
        };
        *remaining = remaining.saturating_sub(elapsed);
    }

    fn expired(&mut self) -> bool {
        self.account();
        self.active_left.is_zero() || self.human_left.is_zero()
    }
}

pub(super) struct PendingHttp {
    pub(super) stream: TcpStream,
    request: Vec<u8>,
    written: usize,
    head: Vec<u8>,
    response: Option<WireResponse>,
    length: Option<usize>,
    bound: usize,
    subscription: bool,
    reads: usize,
    deadline: tokio::time::Instant,
    mcp_acquisition: Option<McpAcquisition>,
}

impl std::fmt::Debug for PendingHttp {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Encoded request bytes contain the existing Basic header.
        formatter
            .debug_struct("PendingHttp")
            .field("written", &self.written)
            .field("reads", &self.reads)
            .field("subscription", &self.subscription)
            .finish_non_exhaustive()
    }
}

impl PendingHttp {
    pub(super) fn new(
        stream: TcpStream,
        request: Vec<u8>,
        bound: usize,
        subscription: bool,
    ) -> Result<Self, TransportError> {
        stream
            .set_nonblocking(true)
            .map_err(|error| TransportError::from_io("nonblocking socket", error))?;
        Ok(Self {
            stream,
            request,
            written: 0,
            head: Vec::new(),
            response: None,
            length: None,
            bound,
            subscription,
            reads: 0,
            deadline: tokio::time::Instant::now() + READ_TIMEOUT,
            mcp_acquisition: None,
        })
    }

    pub(super) fn with_timeout(mut self, timeout: Duration) -> Self {
        self.deadline = tokio::time::Instant::now() + timeout;
        self
    }

    pub(super) fn with_mcp_startup(mut self, active_allowance: Duration) -> Self {
        self.mcp_acquisition = Some(McpAcquisition {
            observed_at: tokio::time::Instant::now(),
            active_left: active_allowance,
            human_left: Duration::from_secs(600),
            human_wait: false,
        });
        self
    }

    pub(super) fn human_wait(&mut self, waiting: bool) {
        if let Some(acquisition) = &mut self.mcp_acquisition {
            // Charge the previous observation first; toggling or repeated
            // forms never renew either allowance, even after it expires.
            acquisition.account();
            acquisition.human_wait = waiting;
        }
    }

    pub(super) fn poll(&mut self) -> Result<Option<WireResponse>, TransportError> {
        let expired = match &mut self.mcp_acquisition {
            Some(acquisition) => acquisition.expired(),
            None => tokio::time::Instant::now() >= self.deadline,
        };
        if expired {
            return Err(TransportError::Unavailable {
                reason: "response acquisition allowance elapsed".into(),
            });
        }
        for _ in 0..IO_PER_POLL {
            if self.written < self.request.len() {
                match self.stream.write(&self.request[self.written..]) {
                    Ok(0) => {
                        return Err(TransportError::Unavailable {
                            reason: "request socket closed".into(),
                        })
                    }
                    Ok(count) => self.written += count,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        return Ok(None)
                    }
                    Err(error) => return Err(TransportError::from_io("write request", error)),
                }
                continue;
            }
            if let Some(response) = &mut self.response {
                if self.subscription
                    || self
                        .length
                        .is_some_and(|length| response.body.len() >= length)
                {
                    if let Some(length) = self.length.filter(|_| !self.subscription) {
                        response.body.truncate(length);
                    }
                    return Ok(self.response.take());
                }
            }
            if self.reads >= MAX_READS {
                return Err(TransportError::Unavailable {
                    reason: "response acquisition read bound reached".into(),
                });
            }
            let mut chunk = [0; 8_192];
            let count = match self.stream.read(&mut chunk) {
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
                Err(error) => return Err(TransportError::from_io("read response", error)),
            };
            self.reads += 1;
            if count == 0 {
                return match self.response.take() {
                    Some(response) if self.length.is_none() => Ok(Some(response)),
                    _ => Err(TransportError::Unavailable {
                        reason: "response socket closed before completion".into(),
                    }),
                };
            }
            if let Some(response) = &mut self.response {
                if response.body.len().saturating_add(count) > self.bound {
                    return Err(TransportError::ResponseTooLarge { bound: self.bound });
                }
                response.body.extend_from_slice(&chunk[..count]);
            } else {
                if self.head.len().saturating_add(count) > MAX_HEAD_BYTES + chunk.len() {
                    return Err(HttpError::HeadTooLarge.into());
                }
                self.head.extend_from_slice(&chunk[..count]);
                if let Some(at) = find_terminator(&self.head) {
                    if at > MAX_HEAD_BYTES {
                        return Err(HttpError::HeadTooLarge.into());
                    }
                    let head = parse_head(&self.head, at)?;
                    if (!self.subscription
                        && head
                            .content_length
                            .is_some_and(|length| length > self.bound))
                        || head.body_prefix.len() > self.bound
                    {
                        return Err(TransportError::ResponseTooLarge { bound: self.bound });
                    }
                    self.length = head.content_length;
                    self.response = Some(WireResponse {
                        status: head.status,
                        body: head.body_prefix,
                    });
                    self.head.clear();
                } else if self.head.len() > MAX_HEAD_BYTES {
                    return Err(HttpError::HeadTooLarge.into());
                }
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
#[path = "pending_tests.rs"]
mod tests;
