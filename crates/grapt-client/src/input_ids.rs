//! The identity a prompt carries before the server has answered for it.
//!
//! A reply can be lost after the server admitted the prompt (a dry run:
//! a socket read timed out, the client kept the draft and showed an error,
//! and resubmitting would have run the task twice). Sending the prompt with
//! an id the CLIENT chose makes that ambiguity recoverable: the client can
//! look for its own id in the session, and a resubmission under the same id
//! is an exact retry the server reconciles rather than a second task (the
//! oracle's TUI mints its prompt id the same way, before sending).
//!
//! THIS IS THE CLIENT'S ONLY SOURCE OF RANDOMNESS (named in `clippy.toml`).
//! The id must be unique within the session, because a collision would make
//! the server treat someone else's input as this one's retry; it need not
//! sort, because every listing orders by durable sequence, never by id.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

use grapt_types::{IdAuthority, IdToken};

/// Mints `<32 hex>x<ordinal>`, the shape the server's own ids take: an
/// instance drawn once from the OS-seeded keys std gives every
/// [`RandomState`], then a counter.
#[derive(Debug)]
pub struct ClientInputIds {
    instance: String,
    next: AtomicU64,
    seeded: IdToken,
}

impl ClientInputIds {
    /// `None` only if the instance rendered empty, which a hex rendering of
    /// two words cannot.
    pub fn seed() -> Option<Self> {
        let mut instance = String::with_capacity(32);
        for half in 0..2u64 {
            let mut hasher = RandomState::new().build_hasher();
            hasher.write_u64(half);
            instance.push_str(&format!("{:016x}", hasher.finish()));
        }
        let seeded = IdToken::new(instance.clone())?;
        Some(Self {
            instance,
            next: AtomicU64::new(0),
            seeded,
        })
    }
}

impl IdAuthority for ClientInputIds {
    fn mint(&self) -> IdToken {
        let ordinal = self.next.fetch_add(1, Ordering::Relaxed);
        IdToken::new(format!("{}x{ordinal}", self.instance)).unwrap_or_else(|| self.seeded.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_seeds_and_two_mints_never_share_an_id() {
        let (Some(one), Some(two)) = (ClientInputIds::seed(), ClientInputIds::seed()) else {
            panic!("seeding failed");
        };
        let ids = [one.mint(), one.mint(), two.mint()];
        assert_ne!(ids[0].as_str(), ids[1].as_str());
        assert_ne!(ids[0].as_str(), ids[2].as_str());
        assert!(ids[0].as_str().ends_with("x0"), "{}", ids[0].as_str());
        assert_eq!(ids[0].as_str().len(), "x0".len() + 32);
    }
}
