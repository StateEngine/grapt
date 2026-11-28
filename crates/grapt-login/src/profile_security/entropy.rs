//! The credential sealer's named OS-entropy adapter. It supplies sealing keys
//! and nonces only; nothing durable or ordered is derived from it.
use std::io;

pub(super) fn fill(bytes: &mut [u8]) -> io::Result<()> {
    #[allow(
        clippy::disallowed_methods,
        reason = "named adapter: credential sealing keys and nonces"
    )]
    getrandom::fill(bytes)
        .map_err(|error| io::Error::other(format!("OS randomness is unavailable: {error}")))
}
