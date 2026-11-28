// SPDX-License-Identifier: MIT OR Apache-2.0
//! An authenticated connection: one request at a time, each answer checked
//! against its request and bounded by the negotiated reply size.
use crate::activity::{
    Activity, ActivityReceipt, AgentSelector, ExternalRequest, ACTIVITY_CAPABILITY,
};
use crate::event_stream::EventStream;
use crate::events::{SubscribeRequest, EVENTS_CAPABILITY, MIN_BATCH_BYTES};
use crate::frame::{decode, encode, read_frame, write_frame};
use crate::handshake::{handshake, HandshakeOptions, Session};
use crate::peer::PeerBinding;
use crate::server_peer::VerifiedPeer;
use crate::turn::{PublishTurn, PublishTurnReceipt, PUBLISH_TURN_CAPABILITY};
use crate::wire::{CliSelections, IpcEnvelope, IpcResponse, IpcResult, Operation, Output, Request};
use crate::{IpcError, API_VERSION, MAX_FRAME_BYTES};
use grapt_stateengine_api::{IdempotencyKey, ProjectId};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};

/// A connection that finished the handshake.
pub struct Connection<S> {
    stream: S,
    session: Session,
    next_request: u64,
    broken: bool,
}

impl<S> std::fmt::Debug for Connection<S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Connection")
            .field("session", &self.session)
            .field("next_request", &self.next_request)
            .field("broken", &self.broken)
            .finish_non_exhaustive()
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> Connection<S> {
    /// Authenticate to the host behind a verified stream, with the endpoint
    /// and owner credential it was verified against, within
    /// [`HELLO_TIMEOUT`](crate::HELLO_TIMEOUT). `client_nonce` must be fresh
    /// random bytes from the caller's entropy source.
    pub async fn open(
        peer: VerifiedPeer<S>,
        binding: &PeerBinding,
        client_nonce: [u8; 32],
        options: &HandshakeOptions,
    ) -> Result<Self, IpcError> {
        let mut stream = peer.stream;
        let session =
            handshake(&mut stream, &peer.endpoint, binding, client_nonce, options).await?;
        Ok(Self {
            stream,
            session,
            next_request: 1,
            broken: false,
        })
    }

    /// What the host agreed to.
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Run an operation that changes nothing.
    pub async fn query(&mut self, name: &str, arguments: Value) -> Result<Output, IpcError> {
        self.call(name, arguments, None).await
    }

    /// Run an operation that changes state. After an uncertain outcome, retry
    /// with the same key and arguments on a new connection, never a new key.
    pub async fn mutate(
        &mut self,
        name: &str,
        arguments: Value,
        key: &IdempotencyKey,
    ) -> Result<Output, IpcError> {
        if key.0.is_empty() {
            return Err(IpcError::Malformed("an idempotency key is empty".into()));
        }
        self.call(name, arguments, Some(key.0.clone())).await
    }

    /// Record one completed turn of a seat's conversation in `project`. An
    /// owner connection names the seat's agent and session. After an
    /// uncertain outcome, retry with the same turn on a new connection.
    pub async fn publish_turn(
        &mut self,
        project: &ProjectId,
        turn: &PublishTurn,
    ) -> Result<PublishTurnReceipt, IpcError> {
        self.require(PUBLISH_TURN_CAPABILITY)?;
        turn.check().map_err(IpcError::Invalid)?;
        if turn.target_agent_id.is_none() || turn.session_id.is_none() {
            return Err(IpcError::Invalid(
                "an owner connection names the seat's agent and session".into(),
            ));
        }
        let arguments =
            serde_json::to_value(turn).map_err(|error| IpcError::Malformed(error.to_string()))?;
        let operation = Operation {
            name: crate::turn::PUBLISH_TURN_OPERATION.to_owned(),
            arguments,
            key: Some(turn.idempotency_key.0.clone()),
        };
        let selections = CliSelections {
            project: project.clone(),
        };
        let output = self
            .exchange(Request::Operation(operation), Some(selections))
            .await?;
        receipt(output, crate::turn::RECEIPT_SCHEMA)
    }

    /// Report an external seat's turn state, naming its agent and project:
    /// owner authority starts at the registry.
    pub async fn report_activity(
        &mut self,
        agent: &AgentSelector,
        project: &ProjectId,
        state: Activity,
    ) -> Result<ActivityReceipt, IpcError> {
        self.require(ACTIVITY_CAPABILITY)?;
        if agent.is_empty() {
            return Err(IpcError::Invalid(
                "an owner report names the seat's agent".into(),
            ));
        }
        let output = self
            .exchange(
                Request::External(ExternalRequest::SessionActivity {
                    project: Some(project.clone()),
                    agent: Some(agent.clone()),
                    state,
                }),
                None,
            )
            .await?;
        receipt(output, crate::activity::RECEIPT_SCHEMA)
    }

    /// Turn this connection into a push event stream. Its frames are bounded
    /// by the smaller of `max_batch_bytes` and the negotiated reply size.
    pub async fn subscribe(
        mut self,
        request: SubscribeRequest,
    ) -> Result<EventStream<S>, IpcError> {
        self.require(EVENTS_CAPABILITY)?;
        if self.broken {
            return Err(IpcError::ConnectionBroken);
        }
        let limit = request
            .max_batch_bytes
            .min(self.session.maximum_reply_bytes);
        if limit < MIN_BATCH_BYTES {
            return Err(IpcError::Invalid(format!(
                "a subscription's batch bound is {limit} bytes, under {MIN_BATCH_BYTES}"
            )));
        }
        let resume = request.after_cursor.clone();
        let request_id = self.send(Request::Subscribe(request), None).await?;
        Ok(EventStream::new(
            self.stream,
            request_id,
            self.session.generation,
            limit,
            resume,
        ))
    }

    fn require(&self, capability: &'static str) -> Result<(), IpcError> {
        let offered = self
            .session
            .capabilities
            .as_deref()
            .is_some_and(|names| names.iter().any(|name| name == capability));
        if offered {
            Ok(())
        } else {
            Err(IpcError::Unsupported(capability))
        }
    }

    async fn call(
        &mut self,
        name: &str,
        arguments: Value,
        key: Option<String>,
    ) -> Result<Output, IpcError> {
        let operation = Operation {
            name: name.to_owned(),
            arguments,
            key,
        };
        self.exchange(Request::Operation(operation), None).await
    }

    /// Write one request and return its id. Once it is on the wire, any
    /// failure leaves the stream's position unknown, so the connection takes
    /// no further request until an answer is read.
    async fn send(
        &mut self,
        request: Request,
        cli_selections: Option<CliSelections>,
    ) -> Result<u64, IpcError> {
        if self.broken {
            return Err(IpcError::ConnectionBroken);
        }
        let request_id = self.next_request;
        self.next_request = request_id
            .checked_add(1)
            .ok_or(IpcError::RequestsExhausted)?;
        let envelope = IpcEnvelope {
            version: API_VERSION,
            request_id,
            generation: self.session.generation,
            authority: None,
            cli_selections,
            request,
        };
        let payload = encode(&envelope)?;
        // Refused before any byte is written, so the connection stays usable.
        if payload.len() > MAX_FRAME_BYTES as usize {
            return Err(IpcError::OutgoingFrame {
                length: payload.len(),
                limit: MAX_FRAME_BYTES,
            });
        }
        self.broken = true;
        write_frame(&mut self.stream, &payload, MAX_FRAME_BYTES).await?;
        Ok(request_id)
    }

    async fn exchange(
        &mut self,
        request: Request,
        cli_selections: Option<CliSelections>,
    ) -> Result<Output, IpcError> {
        let request_id = self.send(request, cli_selections).await?;
        let reply = read_frame(&mut self.stream, self.session.maximum_reply_bytes).await?;
        let response: IpcResponse = decode(&reply)?;
        if response.version != API_VERSION {
            return Err(IpcError::ResponseMismatch("protocol version"));
        }
        if response.request_id != request_id {
            return Err(IpcError::ResponseMismatch("request id"));
        }
        if response.generation != self.session.generation {
            return Err(IpcError::ResponseMismatch("host generation"));
        }
        self.broken = false;
        match response.result {
            IpcResult::Output(output) => Ok(output),
            IpcResult::Error(error) => Err(IpcError::Refused(error)),
        }
    }
}

/// An output's value under the schema its request expects.
fn receipt<T: DeserializeOwned>(output: Output, schema: &str) -> Result<T, IpcError> {
    if output.schema != schema {
        return Err(IpcError::Malformed(format!(
            "an answer has schema {:?}, not {schema:?}",
            output.schema
        )));
    }
    serde_json::from_value(output.value).map_err(|error| IpcError::Malformed(error.to_string()))
}
