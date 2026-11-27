//! The deterministic clock and identity seams.
//!
//! Law H2: every durable or lifecycle owner is constructed with injected
//! authorities, so ambient time and ambient identity creation stay confined
//! to named production adapters and replay stays deterministic.
//!
//! This module has no crate-internal dependency at all: it imports only
//! `core::fmt`. That is what makes it a third owner rather than a section of
//! either sibling — it is cross-domain infrastructure and is not about
//! permission. Nothing here may grow a dependency on the audit or live
//! vocabulary.

use core::fmt;

/// Milliseconds since the Unix epoch, as produced by a [`ClockAuthority`].
///
/// Held here rather than in a leaf module because durable append and lifecycle
/// ownership are the only places time is allowed to enter the domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Timestamp(i64);

impl Timestamp {
    pub fn from_millis(millis: i64) -> Self {
        Self(millis)
    }

    pub fn as_millis(self) -> i64 {
        self.0
    }
}

/// The injected clock.
///
/// Law H2: durable and lifecycle owners consume this; ambient
/// `SystemTime::now()` calls exist only inside named production adapters, so
/// replay and golden facts stay deterministic.
pub trait ClockAuthority: fmt::Debug {
    fn now(&self) -> Timestamp;
}

/// A freshly minted opaque identity suffix.
///
/// **This is an inert output value, not authority.** `new` is public only
/// because [`IdAuthority`] is implemented outside this crate: an injected
/// production or deterministic-test adapter must be able to construct what it
/// returns. Holding one grants nothing.
///
/// The property that matters is the absence of a path from a caller-created
/// token to durable or live authority: no domain identity, receipt, grant,
/// lease or settlement constructor in this crate accepts an `IdToken`. Every
/// identity type is built from its own validated input, and every authority
/// type is sealed behind a consuming proof. `id_token_is_inert` asserts this
/// and the row-11 gate covers the real law, which is ambient time/ID creation
/// outside named adapters.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IdToken(String);

impl IdToken {
    pub fn new(raw: impl Into<String>) -> Option<Self> {
        let raw = raw.into();
        if raw.is_empty() {
            return None;
        }
        Some(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The injected identity source.
///
/// Law H2: the same determinism rule as [`ClockAuthority`]. Ambient UUID
/// creation is confined to named production adapters.
pub trait IdAuthority: fmt::Debug {
    fn mint(&self) -> IdToken;
}

/// The authority pair every durable or lifecycle owner must be constructed
/// with.
///
/// There is no `Default` and no zero-argument constructor: a durable owner
/// that forgot to take its authorities does not compile (ownership probe #11).
#[derive(Debug)]
pub struct DeterministicAuthorities<'a> {
    clock: &'a dyn ClockAuthority,
    ids: &'a dyn IdAuthority,
}

impl<'a> DeterministicAuthorities<'a> {
    pub fn new(clock: &'a dyn ClockAuthority, ids: &'a dyn IdAuthority) -> Self {
        Self { clock, ids }
    }

    pub fn now(&self) -> Timestamp {
        self.clock.now()
    }

    pub fn mint(&self) -> IdToken {
        self.ids.mint()
    }
}

#[cfg(test)]
pub(crate) use fakes::{CountingIds, FixedClock, FixedIds};

/// The deterministic fakes this crate's own tests inject.
///
/// They live here, beside `IdToken`, rather than beside the tests that use
/// them: `FixedIds` and `CountingIds` construct an `IdToken` by touching its
/// private tuple field, bypassing the validating `IdToken::new` so the fake
/// cannot panic and cannot trip the workspace `unwrap_used`/`expect_used`
/// denials. Only this module and its descendants may do that, so custody
/// follows the owned invariant rather than the filename. The alternative —
/// an unvalidated `from_raw` constructor, or a `pub(crate)` field — would be a
/// second construction channel for the exact type whose doc argues that no
/// such route matters.
#[cfg(test)]
mod fakes {
    use super::{ClockAuthority, IdAuthority, IdToken, Timestamp};
    use core::cell::Cell;

    #[derive(Debug)]
    pub(crate) struct FixedClock(pub(crate) i64);

    impl ClockAuthority for FixedClock {
        fn now(&self) -> Timestamp {
            Timestamp::from_millis(self.0)
        }
    }

    #[derive(Debug)]
    pub(crate) struct FixedIds;

    impl IdAuthority for FixedIds {
        fn mint(&self) -> IdToken {
            // Constructed directly rather than through the validating
            // constructor so this cannot panic and cannot trip the workspace
            // `unwrap_used`/`expect_used` denials.
            IdToken("id-test".to_owned())
        }
    }

    /// An identity source that counts, so a test can witness that two mints
    /// differ and that neither is ambient.
    #[derive(Debug, Default)]
    pub(crate) struct CountingIds(Cell<u64>);

    impl IdAuthority for CountingIds {
        fn mint(&self) -> IdToken {
            let next = self.0.get() + 1;
            self.0.set(next);
            // The counter starts at one, so this token is never empty.
            IdToken(format!("id-{next}"))
        }
    }
}
