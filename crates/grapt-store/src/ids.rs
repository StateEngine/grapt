//! The store's named production identity adapter.
//!
//! # Why this module exists
//!
//! A writer owner identity must be unique **over time**, not merely among
//! concurrently live processes. The previous identity was
//! `store-{process id}-{per-process counter}`. Both of its components RESET when
//! a process ends, so once the OS reused a process id the first store in the new
//! process minted the byte-identical string the dead one had used. Because the
//! `election` module's re-election arm decides `Retained` from owner equality
//! and `Retained` leaves the epoch UNCHANGED, that collision handed a new
//! process the dead owner's authority **without advancing the epoch** — the
//! ownership transfer that makes every previous epoch stale simply never ran.
//!
//! Row-11 law confines ambient time and ID creation to NAMED production
//! adapters, and every durable or lifecycle owner consumes an injected
//! authority instead. This module is that named adapter for identity, and it is
//! the only place in the workspace permitted to reach for OS randomness.
//!
//! # Where the uniqueness claim actually rests
//!
//! On the entropy of a single 128-bit draw from the operating system, and on
//! nothing else. In particular it does **not** rest on the wall clock: a clock
//! reading is not a uniqueness argument, because two processes can observe the
//! same millisecond and a skewed or repeated reading would reintroduce exactly
//! the collision this module exists to remove.
//!
//! The neighbouring adapter in [`crate::migration`] reads ambient wall-clock
//! time under the same one-line allow, and its justification is that a wrong or
//! skewed reading can never enter domain state because it only disambiguates a
//! backup FILE NAME. **That justification does not transfer here.** This value
//! is load-bearing: it decides `Retained` versus `Transferred`. The allow below
//! is narrow for the same reason, but it is defended by a different argument.
//!
//! # Fallibility
//!
//! [`grapt_types::IdAuthority::mint`] is infallible by contract and is not
//! widened here. The one fallible step — asking the operating system for
//! entropy — happens ONCE, when the adapter is seeded, and its failure becomes a
//! typed store-open error. There is no panic, `unwrap` or `expect` on this path:
//! a machine that cannot supply randomness must refuse to open a store, not
//! abort the process and not quietly proceed with a weaker identity.

use grapt_types::{IdAuthority, IdToken, Secret};
use std::sync::atomic::{AtomicU64, Ordering};

/// Fresh cryptographic material from the named OS entropy adapter. Identity
/// tokens contain a reused process seed and must never substitute for this.
#[derive(Debug)]
pub struct OsEntropy;

impl OsEntropy {
    pub fn draw_256() -> Result<Secret<[u8; 32]>, getrandom::Error> {
        let mut bytes = [0u8; 32];
        fill_entropy(&mut bytes)?;
        Ok(Secret::new(bytes))
    }
}

fn fill_entropy(bytes: &mut [u8]) -> Result<(), getrandom::Error> {
    // The workspace's sole ambient-randomness call remains in this named
    // adapter; each invocation reads the OS rather than an identity counter.
    #[allow(clippy::disallowed_methods)]
    getrandom::fill(bytes)
}

/// The store could not obtain the entropy its writer identity is built from.
///
/// Carried as a typed error rather than a panic: refusing to open is a truthful
/// outcome, while aborting the process or falling back to a weaker identity are
/// both worse than saying no.
#[derive(Debug, thiserror::Error)]
pub enum InstanceSeedError {
    #[error("could not seed the store's writer identity from OS randomness: {0}")]
    Entropy(#[from] getrandom::Error),
    /// Structurally unreachable — 16 bytes always render as 32 hex characters —
    /// but represented rather than unwrapped, because `IdToken::new` is an
    /// `Option` and the deny-list forbids resolving it by assertion.
    #[error("the seeded writer identity rendered empty")]
    EmptyToken,
}

/// Sixteen bytes of OS randomness, rendered once and reused for this adapter's
/// lifetime.
///
/// One adapter models one process instance. [`crate::EventStore::open`] seeds a
/// fresh adapter per call, so two stores opened in one process still receive
/// different identities and remain two owners — the property F3B1a's residual 10
/// records and that the per-process counter used to provide.
#[derive(Debug)]
pub struct OsRandomIds {
    /// The rendered 128-bit draw. Every token this adapter mints carries it.
    instance: String,
    /// Distinguishes repeat mints from one adapter. This is NOT what makes the
    /// identity unique over time — `instance` is — so a reader is not tempted to
    /// treat a counter as a fencing guarantee again.
    next: AtomicU64,
    /// A pre-validated token, used only as the structurally unreachable fallback
    /// in [`IdAuthority::mint`], which cannot return a `Result`.
    seeded: IdToken,
}

impl OsRandomIds {
    /// Draws 128 bits from the operating system and renders them.
    ///
    /// This is the single ambient-randomness call site in the workspace.
    pub fn seed() -> Result<Self, InstanceSeedError> {
        let mut bytes = [0u8; 16];
        fill_entropy(&mut bytes)?;

        const HEX: [u8; 16] = *b"0123456789abcdef";
        let mut instance = String::with_capacity(32);
        for byte in bytes {
            instance.push(HEX[usize::from(byte >> 4)] as char);
            instance.push(HEX[usize::from(byte & 0x0f)] as char);
        }

        let seeded = IdToken::new(instance.clone()).ok_or(InstanceSeedError::EmptyToken)?;
        Ok(Self {
            instance,
            next: AtomicU64::new(0),
            seeded,
        })
    }

    /// The rendered draw, for witnesses that need to compare two adapters.
    pub fn instance(&self) -> &str {
        &self.instance
    }
}

impl IdAuthority for OsRandomIds {
    fn mint(&self) -> IdToken {
        let ordinal = self.next.fetch_add(1, Ordering::Relaxed);
        // Non-empty by construction; the fallback exists only because the
        // trait is infallible and `unwrap`/`expect` are denied.
        // `IdToken` is reused as the suffix of typed `ses_*`, `msg_*`, and
        // `evt_*` identities. Keep its production alphabet inside the typed
        // identity alphabet so a valid authority token cannot become an
        // invalid durable identity at the next ownership boundary.
        IdToken::new(format!("{}x{ordinal}", self.instance)).unwrap_or_else(|| self.seeded.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn cryptographic_draws_are_fresh_and_remain_secret() -> TestResult {
        let first = OsEntropy::draw_256()?;
        let second = OsEntropy::draw_256()?;
        assert_eq!(first.expose().len(), 32);
        assert!(
            first.expose() != second.expose(),
            "independent OS draws repeated"
        );
        assert_eq!(format!("{first:?}"), "Secret(<redacted>)");
        Ok(())
    }

    /// W1, the identity half.
    ///
    /// Two adapters seeded in the SAME process model two process lifetimes at
    /// the same process id. The old identity could not tell them apart, because
    /// every component it had reset when a process ended.
    #[test]
    fn two_independently_seeded_adapters_do_not_share_an_instance_token() -> TestResult {
        let first_life = OsRandomIds::seed()?;
        let second_life = OsRandomIds::seed()?;
        assert_ne!(
            first_life.instance(),
            second_life.instance(),
            "two seeded adapters share an instance token, so a restarted \
             process could reproduce a dead instance's writer identity"
        );
        Ok(())
    }

    /// The seeded draw is the full 128 bits, rendered.
    ///
    /// Pinned because the width IS the uniqueness claim: nothing else in the
    /// identity distinguishes one process lifetime from another.
    #[test]
    fn the_instance_token_carries_a_full_128_bit_draw() -> TestResult {
        let ids = OsRandomIds::seed()?;
        assert_eq!(
            ids.instance().len(),
            32,
            "128 bits render as 32 hex characters"
        );
        assert!(
            ids.instance().chars().all(|c| c.is_ascii_hexdigit()),
            "instance token is hex"
        );
        Ok(())
    }

    /// One adapter still mints fresh tokens, so two stores opened in one
    /// process remain two owners (F3B1a residual 10).
    #[test]
    fn one_adapter_mints_a_fresh_token_each_call() -> TestResult {
        let ids = OsRandomIds::seed()?;
        assert_ne!(
            ids.mint().as_str(),
            ids.mint().as_str(),
            "two stores sharing one adapter would collapse into one owner"
        );
        Ok(())
    }

    #[test]
    fn production_tokens_are_valid_typed_identity_suffixes() -> TestResult {
        let ids = OsRandomIds::seed()?;
        assert!(
            format!("evt_{}", ids.mint().as_str())
                .parse::<grapt_types::EventId>()
                .is_ok(),
            "production IdToken suffix was rejected by EventId"
        );
        assert!(
            format!("msg_{}", ids.mint().as_str())
                .parse::<grapt_types::MessageId>()
                .is_ok(),
            "production IdToken suffix was rejected by MessageId"
        );
        assert!(
            format!("ses_{}", ids.mint().as_str())
                .parse::<grapt_types::SessionId>()
                .is_ok(),
            "production IdToken suffix was rejected by SessionId"
        );
        Ok(())
    }
}
