// SPDX-License-Identifier: MIT OR Apache-2.0
//! The three-step owner handshake: the host's challenge is checked against
//! the verified endpoint, the client answers with its bound owner proof, and
//! the host's hello is checked against what the client asked for.
use crate::frame::{decode, encode, read_frame, write_frame};
use crate::peer::PeerBinding;
use crate::proof::{owner_mac, OwnerCredential, ProofInput};
use crate::wire::{
    Authority, ClientHello, HostEpoch, OwnerProof, Refusal, ServerChallenge, ServerHello, Version,
};
use crate::{Endpoint, IpcError, API_VERSION, HELLO_TIMEOUT, MAX_ENDPOINT_BYTES, MAX_FRAME_BYTES};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};

const MAX_CAPABILITIES: usize = 32;
const MAX_CAPABILITY_BYTES: usize = 64;

/// Whether the client offers capability negotiation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capabilities {
    /// Send no capability list, as a host that predates negotiation requires.
    Omit,
    /// Offer these capabilities, possibly none, and receive host metadata.
    Offer(Vec<String>),
}

/// What the client asks of the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandshakeOptions {
    /// The host generation the client expects, or `None` for any live one.
    pub expected_generation: Option<u64>,
    /// The largest reply the client accepts, at most [`MAX_FRAME_BYTES`].
    pub maximum_reply_bytes: u32,
    pub capabilities: Capabilities,
}

/// A host that accepted the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub version: Version,
    pub epoch: HostEpoch,
    pub generation: u64,
    pub maximum_reply_bytes: u32,
    /// Present only when the client offered capabilities.
    pub package_version: Option<String>,
    pub store_id: Option<String>,
    pub capabilities: Option<Vec<String>>,
}

/// Run the handshake on a verified stream within [`HELLO_TIMEOUT`].
/// `client_nonce` must be fresh random bytes from the caller's entropy source.
pub(crate) async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    endpoint: &Endpoint,
    credential: &OwnerCredential,
    binding: &PeerBinding,
    client_nonce: [u8; 32],
    options: &HandshakeOptions,
) -> Result<Session, IpcError> {
    check_options(options)?;
    tokio::time::timeout(
        HELLO_TIMEOUT,
        exchange(stream, endpoint, credential, binding, client_nonce, options),
    )
    .await
    .map_err(|_| IpcError::Timeout(HELLO_TIMEOUT))?
}

async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    endpoint: &Endpoint,
    credential: &OwnerCredential,
    binding: &PeerBinding,
    client_nonce: [u8; 32],
    options: &HandshakeOptions,
) -> Result<Session, IpcError> {
    let (challenge, raw): (ServerChallenge, Value) = host_frame(stream).await?;
    if challenge.version != API_VERSION || challenge.version != endpoint.file.api {
        return Err(IpcError::ChallengeMismatch("protocol version"));
    }
    if challenge.epoch != endpoint.file.epoch {
        return Err(IpcError::ChallengeMismatch("host epoch"));
    }
    if challenge.image_sha256 != endpoint.file.image_sha256 {
        return Err(IpcError::ChallengeMismatch("host image digest"));
    }
    // The proof covers the epoch exactly as the host sent it.
    let canonical_epoch = raw
        .get("epoch")
        .map(crate::canonical::canonical)
        .ok_or(IpcError::ChallengeMismatch("host epoch"))?;
    let mac = owner_mac(
        credential,
        &ProofInput {
            version: API_VERSION,
            canonical_epoch: &canonical_epoch,
            image_sha256: &challenge.image_sha256,
            server_nonce: &challenge.nonce,
            client_nonce: &client_nonce,
            binding,
        },
    );
    let offered = match &options.capabilities {
        Capabilities::Omit => None,
        Capabilities::Offer(names) => Some(names.clone()),
    };
    let hello = ClientHello {
        version: API_VERSION,
        expected_generation: options.expected_generation,
        maximum_reply_bytes: options.maximum_reply_bytes,
        authority: Authority::OwnerProof(OwnerProof {
            nonce: client_nonce,
            mac,
        }),
        client_capabilities: offered.clone(),
    };
    write_frame(stream, &encode(&hello)?, MAX_ENDPOINT_BYTES).await?;
    let (accepted, _): (ServerHello, Value) = host_frame(stream).await?;
    session(
        accepted,
        &challenge.epoch,
        endpoint,
        options,
        offered.is_some(),
    )
}

/// The host's next handshake frame, or its refusal.
async fn host_frame<S: AsyncRead + Unpin, T: DeserializeOwned>(
    stream: &mut S,
) -> Result<(T, Value), IpcError> {
    let payload = read_frame(stream, MAX_ENDPOINT_BYTES).await?;
    let raw: Value = decode(&payload)?;
    if raw.get("error").is_some() {
        let refusal: Refusal = from_value(raw)?;
        return Err(IpcError::Refused(Box::new(refusal.error)));
    }
    Ok((from_value(raw.clone())?, raw))
}

fn from_value<T: DeserializeOwned>(value: Value) -> Result<T, IpcError> {
    serde_json::from_value(value).map_err(|error| IpcError::Malformed(error.to_string()))
}

fn session(
    hello: ServerHello,
    epoch: &HostEpoch,
    endpoint: &Endpoint,
    options: &HandshakeOptions,
    offered: bool,
) -> Result<Session, IpcError> {
    if hello.version != API_VERSION {
        return Err(IpcError::HelloMismatch("protocol version"));
    }
    if hello.epoch != *epoch {
        return Err(IpcError::HelloMismatch("host epoch"));
    }
    if options
        .expected_generation
        .is_some_and(|expected| expected != hello.generation)
    {
        return Err(IpcError::HelloMismatch("host generation"));
    }
    if hello.maximum_reply_bytes == 0 || hello.maximum_reply_bytes > options.maximum_reply_bytes {
        return Err(IpcError::HelloMismatch("reply bound"));
    }
    if offered {
        let names = hello
            .capabilities
            .as_deref()
            .ok_or(IpcError::HelloMismatch("capability list"))?;
        check_capabilities(names).map_err(|_| IpcError::HelloMismatch("capability list"))?;
        if hello.package_version.as_deref() != Some(endpoint.file.package_version.as_str()) {
            return Err(IpcError::HelloMismatch("package version"));
        }
    } else if hello.package_version.is_some()
        || hello.store_id.is_some()
        || hello.capabilities.is_some()
    {
        return Err(IpcError::HelloMismatch(
            "capability fields the client did not ask for",
        ));
    }
    Ok(Session {
        version: hello.version,
        epoch: hello.epoch,
        generation: hello.generation,
        maximum_reply_bytes: hello.maximum_reply_bytes,
        package_version: hello.package_version,
        store_id: hello.store_id,
        capabilities: hello.capabilities,
    })
}

fn check_options(options: &HandshakeOptions) -> Result<(), IpcError> {
    if options.maximum_reply_bytes == 0 || options.maximum_reply_bytes > MAX_FRAME_BYTES {
        return Err(IpcError::Options(format!(
            "maximum_reply_bytes must be 1..={MAX_FRAME_BYTES}"
        )));
    }
    match &options.capabilities {
        Capabilities::Omit => Ok(()),
        Capabilities::Offer(names) => check_capabilities(names).map_err(IpcError::Options),
    }
}

/// At most 32 names, each 1 to 64 bytes of `[A-Za-z0-9._-]`, or the reason not.
fn check_capabilities(names: &[String]) -> Result<(), String> {
    if names.len() > MAX_CAPABILITIES {
        return Err(format!("more than {MAX_CAPABILITIES} capability names"));
    }
    match names.iter().find(|name| {
        !(1..=MAX_CAPABILITY_BYTES).contains(&name.len())
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    }) {
        Some(name) => Err(format!("capability name {name:?}")),
        None => Ok(()),
    }
}
