//! Versioned, correlation-demultiplexed JSONL transport for one bridge process.
//!
//! Frames are read with a bounded streaming reader that refuses an oversized or
//! unterminated line before its buffer can grow past the frame bound. Replies
//! are delivered only to the exchange that is still waiting for them; an
//! unknown, late, or duplicate reply is transport corruption.

use std::collections::BTreeMap;
use std::sync::mpsc::{SyncSender, TrySendError};

use super::owned::PluginProcess;
use super::*;
use crate::protocol::{HostRequest, Response};

type Reply = Result<Response, PluginError>;

pub(super) struct Channel {
    generation: PluginGenerationId,
    input: Mutex<BridgeInput>,
    state: Mutex<ChannelState>,
    frame_bytes: usize,
    pending_limit: usize,
}

struct ChannelState {
    next: u64,
    pending: BTreeMap<u64, SyncSender<Reply>>,
    closed: Option<String>,
}

impl Channel {
    pub(super) fn new(
        input: BridgeInput,
        generation: PluginGenerationId,
        limits: BridgeLimits,
    ) -> Self {
        Self {
            generation,
            input: Mutex::new(input),
            state: Mutex::new(ChannelState {
                next: 1,
                pending: BTreeMap::new(),
                closed: None,
            }),
            frame_bytes: limits.frame_bytes,
            pending_limit: limits.pending_responses,
        }
    }

    /// Send one request and wait for its own reply. A timeout leaves the
    /// correlation state untrustworthy; the caller must invalidate the owner.
    pub(super) fn exchange(
        &self,
        operation: &'static str,
        payload: Value,
        timeout: Duration,
        stage: &'static str,
    ) -> Result<Value, PluginError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let correlation = {
            let mut state = self.state.lock().map_err(|_| PluginError::Poisoned)?;
            if let Some(reason) = &state.closed {
                return Err(PluginError::Protocol(reason.clone()));
            }
            if state.pending.len() >= self.pending_limit {
                return Err(PluginError::Protocol(format!(
                    "plugin bridge pending-response bound of {} is exhausted",
                    self.pending_limit
                )));
            }
            let correlation = state.next;
            state.next = correlation.checked_add(1).ok_or_else(|| {
                PluginError::Protocol("correlation identity overflowed".to_owned())
            })?;
            state.pending.insert(correlation, sender);
            correlation
        };
        let request = Request {
            version: PROTOCOL_VERSION,
            generation: self.generation.value(),
            correlation,
            operation,
            payload,
        };
        let written = serde_json::to_vec(&request)
            .map_err(|error| PluginError::Protocol(error.to_string()))
            .and_then(|bytes| self.write_frame(bytes, "bridge request"));
        if let Err(error) = written {
            self.forget(correlation);
            return Err(error);
        }
        match receiver.recv_timeout(timeout) {
            Ok(Ok(response)) => {
                if response.outcome != "ok" {
                    return Err(PluginError::Invocation(
                        response
                            .error
                            .unwrap_or_else(|| "plugin returned no error detail".to_owned()),
                    ));
                }
                Ok(response.payload)
            }
            Ok(Err(error)) => Err(error),
            Err(RecvTimeoutError::Timeout) => {
                self.forget(correlation);
                Err(PluginError::Timeout(stage))
            }
            Err(RecvTimeoutError::Disconnected) => Err(PluginError::Protocol(
                self.closed_reason()
                    .unwrap_or_else(|| "plugin bridge exited before replying".to_owned()),
            )),
        }
    }

    fn forget(&self, correlation: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.pending.remove(&correlation);
        }
    }

    pub(super) fn closed_reason(&self) -> Option<String> {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.closed.clone())
    }

    /// Close the channel and settle every waiting exchange with `reason`.
    pub(super) fn close(&self, reason: &str) {
        let pending = match self.state.lock() {
            Ok(mut state) => {
                if state.closed.is_none() {
                    state.closed = Some(reason.to_owned());
                }
                std::mem::take(&mut state.pending)
            }
            Err(_) => return,
        };
        for (_, waiter) in pending {
            let _ = waiter.try_send(Err(PluginError::Protocol(reason.to_owned())));
        }
    }

    /// Deliver a reply to the one exchange waiting for it.
    fn deliver(&self, response: Response) -> Result<(), String> {
        if response.version != PROTOCOL_VERSION || response.generation != self.generation.value() {
            return Err("bridge reply identity does not match its generation".to_owned());
        }
        let waiter = self
            .state
            .lock()
            .map_err(|_| "plugin bridge channel is poisoned".to_owned())?
            .pending
            .remove(&response.correlation);
        let Some(waiter) = waiter else {
            return Err(format!(
                "bridge reply correlation {} is unknown, late, or duplicated",
                response.correlation
            ));
        };
        let _ = waiter.try_send(Ok(response));
        Ok(())
    }

    pub(super) fn write_host_response(
        &self,
        correlation: u64,
        outcome: Result<Value, String>,
    ) -> Result<(), PluginError> {
        let payload = match outcome {
            Ok(payload) => HostResponsePayload {
                outcome: "ok",
                payload,
                error: None,
            },
            Err(error) => HostResponsePayload {
                outcome: "error",
                payload: Value::Null,
                error: Some(error.chars().take(4096).collect()),
            },
        };
        let response = HostResponse {
            version: PROTOCOL_VERSION,
            generation: self.generation.value(),
            correlation,
            operation: "host-response",
            payload,
        };
        let bytes = serde_json::to_vec(&response)
            .map_err(|error| PluginError::Protocol(error.to_string()))?;
        if bytes.len().saturating_add(1) > self.frame_bytes {
            return self.write_host_response(
                correlation,
                Err("plugin host response exceeds the frame bound".to_owned()),
            );
        }
        self.write_frame(bytes, "plugin host response")
    }

    fn write_frame(&self, mut bytes: Vec<u8>, label: &str) -> Result<(), PluginError> {
        if bytes.len().saturating_add(1) > self.frame_bytes {
            return Err(PluginError::Protocol(format!(
                "{label} exceeds the frame bound"
            )));
        }
        bytes.push(b'\n');
        self.input
            .lock()
            .map_err(|_| PluginError::Poisoned)?
            .write(&bytes)
    }
}

/// Read one newline-terminated frame without letting `line` exceed `limit`.
pub(super) fn read_frame<R: BufRead>(
    reader: &mut R,
    limit: usize,
    line: &mut Vec<u8>,
) -> Result<bool, String> {
    line.clear();
    loop {
        let available = reader.fill_buf().map_err(|error| error.to_string())?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(false)
            } else {
                Err("plugin bridge closed its output inside a frame".to_owned())
            };
        }
        match available.iter().position(|byte| *byte == b'\n') {
            Some(index) => {
                if line.len().saturating_add(index).saturating_add(1) > limit {
                    return Err("bridge frame exceeds the frame bound".to_owned());
                }
                line.extend_from_slice(&available[..index]);
                reader.consume(index + 1);
                return Ok(true);
            }
            None => {
                let length = available.len();
                if line.len().saturating_add(length) >= limit {
                    return Err(
                        "bridge frame exceeds the frame bound before its terminator".to_owned()
                    );
                }
                line.extend_from_slice(available);
                reader.consume(length);
            }
        }
    }
}

pub(super) struct HostQueues {
    general: SyncSender<HostRequest>,
    events: SyncSender<HostRequest>,
}

impl HostQueues {
    pub(super) fn start(process: &Arc<PluginProcess>, workers: usize) -> Result<Self, PluginError> {
        let (general, general_requests) = mpsc::sync_channel(workers);
        let (events, event_requests) = mpsc::sync_channel(workers);
        let general_requests = Arc::new(Mutex::new(general_requests));
        for worker in 0..workers {
            let requests = Arc::clone(&general_requests);
            let process = Arc::downgrade(process);
            thread::Builder::new()
                .name(format!("plugin-host-{worker}"))
                .spawn(move || loop {
                    let request = match requests.lock() {
                        Ok(requests) => requests.recv(),
                        Err(_) => return,
                    };
                    let Ok(request) = request else {
                        return;
                    };
                    let Some(process) = process.upgrade() else {
                        return;
                    };
                    process.serve_host_request(request);
                })
                .map_err(|error| PluginError::Spawn(error.to_string()))?;
        }
        let process = Arc::downgrade(process);
        thread::Builder::new()
            .name("plugin-host-events".to_owned())
            .spawn(move || {
                while let Ok(request) = event_requests.recv() {
                    let Some(process) = process.upgrade() else {
                        return;
                    };
                    process.serve_host_request(request);
                }
            })
            .map_err(|error| PluginError::Spawn(error.to_string()))?;
        Ok(Self { general, events })
    }

    fn submit(&self, request: HostRequest) -> Result<(), HostRequest> {
        let waits_for_event = request.operation == HostOperation::EventSubscribe
            && request.payload.get("action").and_then(Value::as_str) == Some("next");
        let queue = if waits_for_event {
            &self.events
        } else {
            &self.general
        };
        queue.try_send(request).map_err(|error| match error {
            TrySendError::Full(request) | TrySendError::Disconnected(request) => request,
        })
    }
}

/// Own one bridge output stream until it closes or corrupts the transport.
pub(super) fn read_bridge_output<R: Read>(output: R, process: Arc<PluginProcess>) {
    let mut reader = BufReader::new(output);
    let mut line = Vec::new();
    loop {
        let frame = match read_frame(&mut reader, process.frame_bytes(), &mut line) {
            Ok(false) => {
                process.settle_stream_end(None);
                return;
            }
            Ok(true) => serde_json::from_slice::<BridgeFrame>(&line)
                .map_err(|error| format!("bridge frame is malformed: {error}")),
            Err(error) => {
                process.settle_stream_end(Some(error));
                return;
            }
        };
        let violation = match frame {
            Ok(BridgeFrame::Response(response)) => process.channel().deliver(response).err(),
            Ok(BridgeFrame::HostRequest(request)) => process.accept_host_request(request).err(),
            Err(error) => Some(error),
        };
        if let Some(reason) = violation {
            process.corrupt(reason);
            return;
        }
    }
}

impl PluginProcess {
    fn accept_host_request(&self, request: HostRequest) -> Result<(), String> {
        if request.kind != HostFrameKind::HostRequest
            || request.version != PROTOCOL_VERSION
            || request.generation != self.generation().value()
        {
            return Err("plugin host request identity does not match the generation".to_owned());
        }
        let refused = match self.host_queues() {
            Some(queues) => queues.submit(request).err(),
            None => Some(request),
        };
        if let Some(request) = refused {
            self.channel()
                .write_host_response(
                    request.correlation,
                    Err("plugin host request queue bound is exhausted".to_owned()),
                )
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}
