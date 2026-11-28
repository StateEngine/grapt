// SPDX-License-Identifier: MIT OR Apache-2.0
//! Authenticating the host before the owner proof leaves this process: the
//! kernel's view of the connected server (user, process, start time and
//! executable) must match the endpoint, and its executable the pinned image.
#[cfg(target_os = "linux")]
use crate::{Endpoint, IpcError, Principal, Sha256Hex};

/// A connected stream whose server process was checked against its endpoint
/// and the pinned host image. Only [`connect`] makes one, so no unchecked
/// stream ever receives the owner proof.
pub struct VerifiedPeer<S> {
    pub(crate) stream: S,
}

impl<S> std::fmt::Debug for VerifiedPeer<S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("VerifiedPeer(..)")
    }
}

/// Connect to the host's Unix socket and verify the process behind it: the
/// same user as `principal`, the endpoint's process id and start time, and an
/// executable (hashed up to `max_image_bytes`) that is `pinned_image`, which
/// the endpoint must name too. Other platforms have no verified connection
/// yet and so cannot reach a host.
#[cfg(target_os = "linux")]
pub async fn connect(
    endpoint: &Endpoint,
    principal: &Principal,
    pinned_image: &Sha256Hex,
    max_image_bytes: u64,
) -> Result<VerifiedPeer<tokio::net::UnixStream>, IpcError> {
    use crate::wire::StartIdentity;
    let Principal::Unix(uid) = principal else {
        return Err(IpcError::PeerMismatch(
            "a Unix host needs a Unix principal".into(),
        ));
    };
    if !crate::binary::same_digest(&endpoint.file.image_sha256, pinned_image) {
        return Err(IpcError::BinaryMismatch);
    }
    let owner = &endpoint.file.epoch.owner;
    let StartIdentity::Linux { start_ticks } = owner.started else {
        return Err(IpcError::PeerMismatch(
            "the host did not start on Linux".into(),
        ));
    };
    let socket = endpoint
        .unix_socket()
        .ok_or_else(|| IpcError::Endpoint("the host does not listen on a Unix socket".into()))?;
    let stream = tokio::net::UnixStream::connect(socket).await?;
    let credentials = stream.peer_cred()?;
    if credentials.uid() != *uid {
        return Err(IpcError::PeerMismatch(
            "the server runs as another user".into(),
        ));
    }
    let pid = credentials
        .pid()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid == owner.pid)
        .ok_or_else(|| IpcError::PeerMismatch("the server is not the endpoint's process".into()))?;
    let directory = crate::linux_proc::process(pid);
    let started = || crate::linux_proc::start_ticks(&directory);
    if started()? != start_ticks {
        return Err(IpcError::PeerMismatch(
            "the server process started at another time".into(),
        ));
    }
    let hashed = directory.clone();
    let image =
        tokio::task::spawn_blocking(move || crate::linux_proc::image(&hashed, max_image_bytes))
            .await
            .map_err(|_| IpcError::Binary("the server image could not be hashed".into()))??;
    // The same process instance throughout: its start time is checked again.
    if !crate::binary::same_digest(&image, pinned_image) || started()? != start_ticks {
        return Err(IpcError::BinaryMismatch);
    }
    Ok(VerifiedPeer { stream })
}
