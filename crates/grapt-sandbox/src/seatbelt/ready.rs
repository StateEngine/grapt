//! Seatbelt startup readiness parser.
//!
//! `sandbox-exec` on modern macOS prints deprecation warnings to stderr even
//! when containment succeeds. Readiness therefore keys on the helper's explicit
//! `READY` marker: a deprecation warning followed by `READY` succeeds (with the
//! deprecation noise stripped from caller stderr), while EOF or timeout without
//! `READY` still fails closed.
use super::refused;
use crate::SpawnFailure;
use nix::poll::{poll, PollFd, PollFlags};
use std::io::Read;
use std::os::fd::AsFd;

pub(super) const READY: &[u8] = b"GRAPT_SEATBELT_READY\n";

fn strip_deprecation_warnings(prefix: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(prefix);
    let mut kept = String::new();
    for line in text.split_inclusive('\n') {
        let lower = line.to_ascii_lowercase();
        if lower.contains("sandbox-exec") && lower.contains("deprecated") {
            continue;
        }
        kept.push_str(line);
    }
    kept.into_bytes()
}

pub(super) fn wait_ready(
    pipe: &mut std::io::PipeReader,
    timeout_ms: u32,
) -> Result<Vec<u8>, SpawnFailure> {
    let mut bytes = Vec::new();
    let interval = u16::try_from((timeout_ms.min(5000) / 32).max(1))
        .map_err(|_| refused("Seatbelt startup bound is invalid", false))?;
    for _ in 0..32 {
        let mut descriptors = [PollFd::new(pipe.as_fd(), PollFlags::POLLIN)];
        match poll(&mut descriptors, interval) {
            Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
            Ok(_) => (),
            Err(error) => {
                return Err(refused(
                    format!("Seatbelt startup observation failed: {error}"),
                    true,
                ))
            }
        }
        let mut chunk = [0; 128];
        let count = pipe
            .read(&mut chunk)
            .map_err(|error| refused(format!("Seatbelt startup read failed: {error}"), true))?;
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(position) = bytes
            .windows(READY.len())
            .position(|window| window == READY)
        {
            bytes.drain(position..position + READY.len());
            return Ok(strip_deprecation_warnings(&bytes));
        }
        if count == 0 {
            return Err(refused(
                format!(
                    "sandbox-exec did not establish containment: {}",
                    String::from_utf8_lossy(&bytes).trim()
                ),
                false,
            ));
        }
    }
    Err(refused(
        "Seatbelt startup timed out or exceeded 4096 bytes",
        true,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn deprecation_warning_before_ready_succeeds_and_is_stripped(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (mut reader, mut writer) = std::io::pipe()?;
        writer.write_all(
            b"sandbox-exec: -p is deprecated\nwarning: keep this diagnostic\nGRAPT_SEATBELT_READY\n",
        )?;
        drop(writer);
        let prefix = wait_ready(&mut reader, 1000)?;
        assert_eq!(prefix, b"warning: keep this diagnostic\n");
        Ok(())
    }

    #[test]
    fn deprecation_error_without_ready_still_refuses_closed(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (mut reader, mut writer) = std::io::pipe()?;
        writer.write_all(b"sandbox-exec is deprecated\n")?;
        drop(writer);
        let failure = wait_ready(&mut reader, 1000)
            .err()
            .ok_or("EOF without READY must refuse")?;
        assert!(!failure.effects_possible);
        assert!(failure.reason.contains("deprecated"));
        Ok(())
    }
}
