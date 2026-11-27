//! Native pins cross the authenticated helper channel with SCM_RIGHTS.
//! A serialized path alone never reconstructs an approved Linux resource.
use grapt_sandbox::{Policy, WriteTarget};
use nix::sys::socket::{recvmsg, sendmsg, ControlMessage, ControlMessageOwned, MsgFlags};
use std::fs::File;
use std::io::{self, IoSlice, IoSliceMut};
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;

pub(super) async fn send(
    channel: &tokio::net::UnixStream,
    targets: &[WriteTarget],
) -> Result<(), &'static str> {
    for target in targets {
        let file = target
            .duplicate_file()
            .map_err(|_| "sandbox pin duplication failed")?;
        channel
            .async_io(tokio::io::Interest::WRITABLE, || {
                sendmsg::<()>(
                    channel.as_raw_fd(),
                    &[IoSlice::new(b"G")],
                    &[ControlMessage::ScmRights(&[file.as_raw_fd()])],
                    MsgFlags::MSG_NOSIGNAL,
                    None,
                )
                .map_err(io::Error::from)
                .and_then(|count| {
                    if count == 1 {
                        Ok(())
                    } else {
                        Err(io::Error::other("sandbox pin transfer was incomplete"))
                    }
                })
            })
            .await
            .map_err(|_| "sandbox pin transfer failed")?;
    }
    Ok(())
}

struct Descriptors(Vec<RawFd>);
impl Drop for Descriptors {
    fn drop(&mut self) {
        for descriptor in &self.0 {
            let _closed = nix::unistd::close(*descriptor);
        }
    }
}

pub(super) fn receive(
    channel: &std::os::unix::net::UnixStream,
    policy: &Policy,
    paths: &[PathBuf],
) -> Result<Vec<WriteTarget>, String> {
    if paths.len() > 128 {
        return Err("Sandbox write target count exceeds bound".into());
    }
    let mut targets = Vec::with_capacity(paths.len());
    for path in paths {
        let mut marker = [0];
        let mut payload = [IoSliceMut::new(&mut marker)];
        let mut ancillary = nix::cmsg_space!([RawFd; 1]);
        let message = recvmsg::<()>(
            channel.as_raw_fd(),
            &mut payload,
            Some(&mut ancillary),
            MsgFlags::MSG_CMSG_CLOEXEC,
        )
        .map_err(|error| format!("Sandbox pin was not received: {error}"))?;
        let mut descriptors = Descriptors(Vec::new());
        let mut unexpected = false;
        for control in message
            .cmsgs()
            .map_err(|error| format!("Sandbox pin metadata is invalid: {error}"))?
        {
            match control {
                // The fixed ancillary buffer bounds this collection; reject
                // padding-sized extra descriptors too, closing every received pin.
                ControlMessageOwned::ScmRights(fds) => descriptors.0.extend(fds),
                _ => unexpected = true,
            }
        }
        let valid = message.bytes == 1
            && !message
                .flags
                .intersects(MsgFlags::MSG_CTRUNC | MsgFlags::MSG_TRUNC);
        if !valid
            || marker != *b"G"
            || unexpected
            || descriptors.0.len() != 1
            || descriptors.0[0] < 3
        {
            return Err("Sandbox pin handoff is malformed or truncated".into());
        }
        // Safe OwnedFd adoption via an O_PATH duplicate. The received raw FD
        // remains exclusively owned here until duplication and is always closed
        // by the guard; no parent flags or unrelated descriptors are changed.
        let file = nix::fcntl::open(
            format!("/proc/self/fd/{}", descriptors.0[0]).as_str(),
            nix::fcntl::OFlag::O_PATH | nix::fcntl::OFlag::O_CLOEXEC,
            nix::sys::stat::Mode::empty(),
        )
        .map(File::from)
        .map_err(|error| format!("Sandbox received pin is unavailable: {error}"))?;
        targets.push(WriteTarget::from_file(policy, path, file)?);
    }
    Ok(targets)
}
