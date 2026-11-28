// SPDX-License-Identifier: MIT OR Apache-2.0
//! An authenticated connection: one request at a time, each answer checked
//! against its request and bounded by the negotiated reply size.
use crate::frame::{decode, encode, read_frame, write_frame};
use crate::handshake::{handshake, HandshakeOptions, Session};
use crate::peer::PeerBinding;
use crate::proof::OwnerCredential;
use crate::wire::{IpcEnvelope, IpcResponse, IpcResult, Operation, Output, Request};
use crate::{Endpoint, IpcError, API_VERSION, MAX_FRAME_BYTES};
use grapt_stateengine_api::IdempotencyKey;
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
    /// Authenticate on a connected stream; see [`handshake`].
    pub async fn open(
        mut stream: S,
        endpoint: &Endpoint,
        credential: &OwnerCredential,
        binding: &PeerBinding,
        client_nonce: [u8; 32],
        options: &HandshakeOptions,
    ) -> Result<Self, IpcError> {
        let session = handshake(
            &mut stream,
            endpoint,
            credential,
            binding,
            client_nonce,
            options,
        )
        .await?;
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

    async fn call(
        &mut self,
        name: &str,
        arguments: Value,
        key: Option<String>,
    ) -> Result<Output, IpcError> {
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
            request: Request::Operation(Operation {
                name: name.to_owned(),
                arguments,
                key,
            }),
        };
        let payload = encode(&envelope)?;
        // Once a request is on the wire, any failure leaves the stream's
        // position unknown, so the connection takes no further request.
        self.broken = true;
        write_frame(&mut self.stream, &payload, MAX_FRAME_BYTES).await?;
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
            IpcResult::Error(error) => Err(IpcError::Refused(Box::new(error))),
        }
    }
}
