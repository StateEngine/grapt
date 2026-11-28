// SPDX-License-Identifier: MIT OR Apache-2.0
//! The closed wire schemas of IPC version 1. Every type refuses unknown
//! fields; free-form payloads are JSON values that still pass the depth and
//! repeated-key checks.
use crate::Sha256Hex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::num::NonZeroU16;

/// An IPC protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
}

/// A path in the host's native encoding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "encoding", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativePath {
    UnixBytes { units: Vec<u8> },
}

/// When a process started, in its operating system's own terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "os", rename_all = "snake_case", deny_unknown_fields)]
pub enum StartIdentity {
    Linux {
        start_ticks: u64,
    },
    Macos {
        start_seconds: u64,
        start_micros: u32,
    },
    Windows {
        creation_filetime: u64,
    },
}

/// One running host process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessInstance {
    pub machine: String,
    pub boot: String,
    pub pid: u32,
    pub started: StartIdentity,
    pub executable: NativePath,
    pub argv_sha256: Sha256Hex,
}

/// A host process and its generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostEpoch {
    pub owner: ProcessInstance,
    pub generation: u64,
}

/// Where a host listens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Address {
    UnixSocket { path: NativePath },
    WindowsPipe { name: String },
}

/// How a host runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKind {
    Standalone,
    EmbeddedSecode,
}

/// `endpoint.json` as the host writes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointFile {
    pub schema: u32,
    pub address: Address,
    pub epoch: HostEpoch,
    pub generation: u64,
    pub kind: HostKind,
    #[serde(default)]
    pub listener_port: Option<NonZeroU16>,
    pub api: Version,
    pub package_version: String,
    pub image_sha256: Sha256Hex,
}

/// Handshake step 1: the host's challenge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerChallenge {
    pub version: Version,
    pub epoch: HostEpoch,
    pub nonce: [u8; 32],
    pub image_sha256: Sha256Hex,
}

/// A host's refusal in place of a handshake frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Refusal {
    pub error: ErrorEnvelope,
}

/// Handshake step 2: the client's hello.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientHello {
    pub version: Version,
    pub expected_generation: Option<u64>,
    pub maximum_reply_bytes: u32,
    pub authority: Authority,
    /// Omitted entirely for hosts that do not negotiate capabilities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_capabilities: Option<Vec<String>>,
}

/// The authority a client presents. This client presents only owner proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "proof",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Authority {
    OwnerProof(OwnerProof),
}

/// Proof of the owner credential, bound to this challenge and connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerProof {
    pub nonce: [u8; 32],
    pub mac: [u8; 32],
}

/// Handshake step 3: the host's hello.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerHello {
    pub version: Version,
    pub epoch: HostEpoch,
    pub generation: u64,
    pub maximum_reply_bytes: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
}

/// One request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IpcEnvelope {
    pub version: Version,
    pub request_id: u64,
    pub generation: u64,
    /// Always `null`: the request inherits the connection's owner authority.
    pub authority: Option<Value>,
    pub request: Request,
}

/// What a request asks for. This client sends only operations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "input",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Request {
    Operation(Operation),
}

/// One registered operation, with the idempotency key every mutation carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub name: String,
    pub arguments: Value,
    pub key: Option<String>,
}

/// The answer to one request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IpcResponse {
    pub version: Version,
    pub request_id: u64,
    pub generation: u64,
    pub result: IpcResult,
}

/// An operation's output or the host's error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum IpcResult {
    Output(Output),
    Error(ErrorEnvelope),
}

/// An operation's output under its schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    pub schema: String,
    pub value: Value,
    pub window: Value,
}

/// A host's typed error: why, whether a retry can help, and the step that does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorEnvelope {
    pub schema: String,
    pub code: String,
    pub detail: Value,
    pub retry: Value,
    pub class: String,
    pub exit: i32,
    pub message: String,
    pub remedy: Remedy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub far: Option<Value>,
}

/// The step an error names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Remedy {
    pub text: String,
    pub command: Option<String>,
}
