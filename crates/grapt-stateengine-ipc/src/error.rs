// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::wire::ErrorEnvelope;
use std::time::Duration;

/// Why a step of the IPC exchange failed. Each refusal names its own fact:
/// a bound, a schema, a mismatch with the verified endpoint, or the host's
/// own error envelope.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("the StateEngine connection failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("a frame announced zero bytes")]
    EmptyFrame,
    #[error("a frame announced {announced} bytes, over its {limit}-byte bound")]
    FrameTooLarge { announced: u32, limit: u32 },
    #[error("an outgoing frame of {length} bytes is empty or over its {limit}-byte bound")]
    OutgoingFrame { length: usize, limit: u32 },
    #[error("a frame nests deeper than {} levels", crate::MAX_JSON_DEPTH)]
    TooDeep,
    #[error("a frame repeats the key {0:?}")]
    DuplicateKey(String),
    #[error("a frame does not match its schema: {0}")]
    Malformed(String),
    #[error("endpoint.json is unusable: {0}")]
    Endpoint(String),
    #[error("the owner credential is unusable: {0}")]
    Credential(String),
    #[error("the StateEngine binary is unusable: {0}")]
    Binary(String),
    #[error("the StateEngine binary does not match its pinned SHA-256")]
    BinaryMismatch,
    #[error("the host's challenge does not match endpoint.json: {0}")]
    ChallengeMismatch(&'static str),
    #[error("the host's hello does not match the handshake: {0}")]
    HelloMismatch(&'static str),
    #[error("the handshake options are invalid: {0}")]
    Options(String),
    #[error("the handshake did not finish within {0:?}")]
    Timeout(Duration),
    #[error("the host refused: {} ({})", .0.message, .0.code)]
    Refused(Box<ErrorEnvelope>),
    #[error("the response does not answer the request: {0}")]
    ResponseMismatch(&'static str),
    #[error("no request identity is left on this connection; open another")]
    RequestsExhausted,
    #[error("an earlier request on this connection failed mid-exchange; open another")]
    ConnectionBroken,
}
