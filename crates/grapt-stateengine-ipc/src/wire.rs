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
#[serde(
    tag = "encoding",
    content = "units",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum NativePath {
    /// 1 to 32768 bytes, absolute, without NUL.
    UnixBytes(Vec<u8>),
    /// At most 32768 UTF-16 units, a Windows absolute path, without NUL.
    WindowsUtf16(Vec<u16>),
}

const MAX_PATH_UNITS: usize = 32_768;

impl NativePath {
    /// Why the path breaks its encoding's rules, if it does.
    pub fn check(&self) -> Result<(), &'static str> {
        match self {
            Self::UnixBytes(units) => {
                if units.is_empty() || units.len() > MAX_PATH_UNITS {
                    Err("a Unix path is empty or longer than 32768 bytes")
                } else if units.first() != Some(&b'/') || units.contains(&0) {
                    Err("a Unix path is not absolute or holds NUL")
                } else {
                    Ok(())
                }
            }
            Self::WindowsUtf16(units) => {
                if units.len() > MAX_PATH_UNITS || units.contains(&0) {
                    Err("a Windows path is longer than 32768 units or holds NUL")
                } else if !windows_absolute(units) {
                    Err("a Windows path is not absolute")
                } else {
                    Ok(())
                }
            }
        }
    }
}

/// `\\?\...`, `\\.\...`, `\\server\share`, or a drive with `\` or `/`.
fn windows_absolute(units: &[u16]) -> bool {
    let unit = |index: usize| {
        units
            .get(index)
            .copied()
            .and_then(|unit| u8::try_from(unit).ok())
    };
    let backslash = Some(b'\\');
    let drive = unit(0).is_some_and(|letter| letter.is_ascii_alphabetic())
        && unit(1) == Some(b':')
        && matches!(unit(2), Some(b'\\' | b'/'));
    let device = unit(0) == backslash
        && unit(1) == backslash
        && matches!(unit(2), Some(b'?' | b'.'))
        && unit(3) == backslash;
    let share = unit(0) == backslash
        && unit(1) == backslash
        && unit(2).is_some_and(|first| first != b'\\')
        && units.iter().skip(3).any(|unit| *unit == u16::from(b'\\'));
    drive || device || share
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

impl ProcessInstance {
    /// Why the process identity is inconsistent, if it is: a Windows start
    /// goes with a Windows path, a Linux or macOS start with a Unix one.
    pub fn check(&self) -> Result<(), &'static str> {
        self.executable.check()?;
        match (&self.started, &self.executable) {
            (StartIdentity::Windows { .. }, NativePath::WindowsUtf16(_))
            | (StartIdentity::Linux { .. }, NativePath::UnixBytes(_)) => Ok(()),
            (StartIdentity::Macos { start_micros, .. }, NativePath::UnixBytes(_)) => {
                if *start_micros < 1_000_000 {
                    Ok(())
                } else {
                    Err("a macOS start has 1000000 or more microseconds")
                }
            }
            _ => Err("the start identity and executable path are for different platforms"),
        }
    }
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
    Error(Box<ErrorEnvelope>),
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
    /// The per-code payload.
    pub detail: Value,
    pub retry: Retry,
    pub class: String,
    pub exit: i32,
    pub message: String,
    pub remedy: Remedy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<Remedy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub far: Option<bool>,
}

/// Whether and when the same request may be tried again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Retry {
    Never,
    AfterReread,
    /// After this many milliseconds.
    After(u64),
    WhenRuntimeReady,
}

/// The step an error names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Remedy {
    pub text: String,
    pub command: Option<Command>,
}

/// A command that takes the step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    /// A StateEngine command line.
    Se { argv: Vec<String> },
    /// A tool call with its arguments.
    Tool { name: String, args: Value },
    /// A step the user takes by hand.
    Manual,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_paths_and_typed_remedies_decode() -> Result<(), serde_json::Error> {
        let path: NativePath =
            serde_json::from_str(r#"{"encoding":"windows_utf16","units":[67,58,92]}"#)?;
        assert_eq!(path, NativePath::WindowsUtf16(vec![67, 58, 92]));
        assert_eq!(path.check(), Ok(()));
        let relative = NativePath::WindowsUtf16("se.exe".encode_utf16().collect());
        assert!(relative.check().is_err());
        assert!(NativePath::UnixBytes(b"se".to_vec()).check().is_err());
        let error: ErrorEnvelope = serde_json::from_str(
            r#"{"schema":"se.error/v1","code":"usage","detail":{},"retry":{"kind":"after","value":250},
                "class":"usage","exit":64,"message":"fixture","remedy":{"text":"fixture",
                "command":{"kind":"se","argv":["se","status"]}},"far":true}"#,
        )?;
        assert_eq!(error.retry, Retry::After(250));
        assert_eq!(
            error.remedy.command,
            Some(Command::Se {
                argv: vec!["se".into(), "status".into()]
            })
        );
        Ok(())
    }
}
