//! The bounded, generation-stamped, server-owned subject-to-recipient mapping.
//!
//! This is the only place a [`SeatRecipientId`] is chosen. A request never
//! selects one — not preferentially, not as a fallback, not at all — and the
//! type system carries that rule rather than a convention:
//! [`SeatPrincipalDirectory::resolve`]
//! takes a [`VerifiedSubject`] and nothing else, so there is no parameter a
//! decoded DTO could occupy.
//!
//! ## Why the generation exists
//!
//! Takeover and revocation are different mechanisms and only one of them has a
//! latent defect if it is deferred:
//!
//! * **takeover** changes which live connection currently represents a
//!   recipient, and touches no durable identity;
//! * **revocation** withdraws the mapping itself — this subject no longer owns
//!   this recipient.
//!
//! A principal is minted once and then lives for as long as its connection,
//! which may be hours. Without a generation stamped into the principal and
//! compared at every authorization, revocation does nothing to a principal
//! already in flight.
//!
//! The comparison is **equality**, not `>=`. This project has already paid for
//! that distinction once: a reclaim needs `>=` because a lost owner must match
//! its own row, while a currency marker needs `==` because the question is
//! "is this still the live one", and `>=` answers "was this ever valid".
//!
//! ## The disclosed cost of a directory-wide generation
//!
//! Revoking ONE subject advances the generation for ALL of them, so every live
//! principal becomes inert and every connection must re-authenticate. That is
//! deliberate. A per-subject generation would be cheaper and would also mean
//! the directory has to be consulted per subject on every authorization to know
//! which generation is current — which is the same lookup, with a wider window
//! in which a half-revoked directory can answer inconsistently. Fail closed and
//! coarse beats fail open and precise.

use std::collections::{BTreeMap, BTreeSet};

use grapt_types::{AuthenticatedClientId, DeterministicAuthorities, SeatRecipientId};

use crate::auth::{AuthenticatedSeatPrincipal, SubjectId, VerifiedSubject};
use crate::error::{ApiRefusal, Redacted};

/// Which revision of the mapping is live.
///
/// `Copy` and totally ordered so a stale principal can be *described* as older,
/// but note that ordering is not what authorizes: [`SeatPrincipalDirectory::is_current`]
/// compares with `==`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DirectoryGeneration(u64);

impl DirectoryGeneration {
    /// The generation a freshly configured directory is live at.
    pub const FIRST: DirectoryGeneration = DirectoryGeneration(1);

    pub fn value(self) -> u64 {
        self.0
    }

    /// The next generation. Saturating rather than wrapping: a wrap would make
    /// an ancient principal current again, which is the exact failure the
    /// generation exists to prevent.
    ///
    /// R1 observation O1 corrects what this note used to claim. Saturation is
    /// strictly better than wrapping, but it is NOT simply "the safe
    /// direction": at the `u64` ceiling [`SeatPrincipalDirectory::revoke`] still
    /// removes the mapping and returns `Ok` while the generation stops
    /// advancing, so principals already minted stay current — the in-flight
    /// failure the module note describes. The `resolve` path remains safe
    /// because the mapping is gone. Reaching it needs 2^64 revocations, so this
    /// is recorded precisely rather than defended against.
    fn next(self) -> Self {
        DirectoryGeneration(self.0.saturating_add(1))
    }
}

/// A checked positive capacity. There is no `Default`: a bound that can be
/// omitted is a bound nobody chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaxPrincipalMappings(usize);

impl MaxPrincipalMappings {
    pub fn try_new(value: usize) -> Result<Self, ApiRefusal> {
        if value == 0 {
            return Err(ApiRefusal::BoundNotPositive {
                bound: "max_principal_mappings",
            });
        }
        Ok(Self(value))
    }

    pub fn value(self) -> usize {
        self.0
    }
}

/// One configured mapping: this subject owns this recipient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatDirectoryEntry {
    subject: SubjectId,
    recipient: SeatRecipientId,
}

impl SeatDirectoryEntry {
    pub fn new(subject: SubjectId, recipient: SeatRecipientId) -> Self {
        Self { subject, recipient }
    }

    pub fn subject(&self) -> &SubjectId {
        &self.subject
    }

    pub fn recipient(&self) -> &SeatRecipientId {
        &self.recipient
    }
}

/// One mapping as the directory holds it, with the client identity issued for
/// that subject.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Mapping {
    recipient: SeatRecipientId,
    client: AuthenticatedClientId,
}

/// The server-owned mapping.
///
/// Immutable in content for a given generation: the only mutation is
/// [`Self::revoke`], and it advances the generation as part of the same call.
#[derive(Debug)]
pub struct SeatPrincipalDirectory {
    mappings: BTreeMap<SubjectId, Mapping>,
    generation: DirectoryGeneration,
    max: MaxPrincipalMappings,
}

impl SeatPrincipalDirectory {
    /// Build the directory from validated configuration.
    ///
    /// Refuses **whole**, with no partial directory left behind, on:
    ///
    /// * more entries than `max` — checked before anything is inserted, so an
    ///   oversized configuration cannot leave a prefix of itself live;
    /// * a duplicate subject — R4HA-03 rules that one subject maps to exactly
    ///   one recipient, so a second entry for the same subject is a
    ///   configuration defect rather than a last-one-wins overwrite.
    ///
    /// Two distinct subjects MAY name the same recipient; that is explicitly
    /// permitted, bounded by the same `max`.
    ///
    /// Each subject is issued exactly one [`AuthenticatedClientId`], here, from
    /// the injected [`grapt_types::IdAuthority`] — never from ambient
    /// randomness, and never per-resolve, because R4HA-03 binds one subject to
    /// one client identity for the life of the mapping.
    pub fn configure(
        entries: Vec<SeatDirectoryEntry>,
        max: MaxPrincipalMappings,
        authorities: &DeterministicAuthorities<'_>,
    ) -> Result<Self, ApiRefusal> {
        if entries.len() > max.value() {
            return Err(ApiRefusal::DirectoryBoundExceeded { max: max.value() });
        }
        let mut seen: BTreeSet<&SubjectId> = BTreeSet::new();
        for entry in &entries {
            if !seen.insert(entry.subject()) {
                return Err(ApiRefusal::DuplicateSubject {
                    observed: Redacted::new(entry.subject().as_str().to_owned()),
                });
            }
        }
        let mut mappings = BTreeMap::new();
        for entry in entries {
            let client = AuthenticatedClientId::new(authorities.mint().as_str())
                .ok_or(ApiRefusal::ClientIdentityUnavailable)?;
            mappings.insert(
                entry.subject,
                Mapping {
                    recipient: entry.recipient,
                    client,
                },
            );
        }
        Ok(Self {
            mappings,
            generation: DirectoryGeneration::FIRST,
            max,
        })
    }

    /// Build the empty directory, taking no authorities.
    ///
    /// Value-identical to `configure(vec![], max, authorities)` for every
    /// `authorities`, and that is the point: with no entries the `for` loop in
    /// [`Self::configure`] never runs, so no [`AuthenticatedClientId`] is minted
    /// and there is nothing for an [`grapt_types::IdAuthority`] to be asked.
    /// Requiring one anyway made the *type signature* — not any semantic need —
    /// the thing standing between a caller with no principals and a composed
    /// host.
    ///
    /// Returns `Self` rather than `Result`, because every refusal
    /// [`Self::configure`] can produce is quantified over entries: zero entries
    /// cannot exceed a bound `MaxPrincipalMappings` already refuses `0` for, a
    /// duplicate subject needs two entries, and `ClientIdentityUnavailable` is
    /// raised at the mint that does not happen.
    ///
    /// It opens no second issuing path: [`Self::configure`] remains the sole
    /// route that mints an [`AuthenticatedClientId`], which is what keeps this
    /// crate the sole issuer of that identity. A constructor admitting
    /// **non-empty** entries without authorities would be that second path, and
    /// is exactly what this one must never become. A directory built here
    /// resolves no subject, so a boundary over it refuses every credential and a
    /// composed host answers `401` to all of them.
    pub fn configure_empty(max: MaxPrincipalMappings) -> Self {
        Self {
            mappings: BTreeMap::new(),
            generation: DirectoryGeneration::FIRST,
            max,
        }
    }

    /// The live generation.
    pub fn generation(&self) -> DirectoryGeneration {
        self.generation
    }

    /// How many mappings the directory admits.
    pub fn max_mappings(&self) -> MaxPrincipalMappings {
        self.max
    }

    /// How many mappings are live.
    pub fn len(&self) -> usize {
        self.mappings.len()
    }

    /// Whether the directory holds no mappings.
    pub fn is_empty(&self) -> bool {
        self.mappings.is_empty()
    }

    /// Resolve a verified subject to its principal.
    ///
    /// Takes the [`VerifiedSubject`] **by value**: one verification mints one
    /// principal. Note what this signature does not accept — no DTO, no
    /// recipient, no client identity, no header map. There is nowhere for a
    /// caller-supplied recipient to enter, which is why "a request never
    /// selects a recipient" is a property of the type rather than a rule the
    /// body has to remember.
    pub fn resolve(
        &self,
        verified: VerifiedSubject,
    ) -> Result<AuthenticatedSeatPrincipal, ApiRefusal> {
        let mapping = self.mappings.get(verified.subject()).ok_or_else(|| {
            ApiRefusal::CredentialRejected {
                observed: Redacted::new(verified.subject().as_str().to_owned()),
            }
        })?;
        let recipient = mapping.recipient.clone();
        let client = mapping.client.clone();
        Ok(AuthenticatedSeatPrincipal::mint(
            verified,
            client,
            recipient,
            self.generation,
        ))
    }

    /// Withdraw a subject's mapping and advance the generation.
    ///
    /// The advance is unconditional on success and is what makes every already
    /// minted principal inert — including principals for subjects that were not
    /// revoked. See the module note on why that coarseness is the safe
    /// direction.
    pub fn revoke(&mut self, subject: &SubjectId) -> Result<(), ApiRefusal> {
        if self.mappings.remove(subject).is_none() {
            return Err(ApiRefusal::CredentialRejected {
                observed: Redacted::new(subject.as_str().to_owned()),
            });
        }
        self.generation = self.generation.next();
        Ok(())
    }

    /// Whether a principal was minted under the live generation.
    ///
    /// **Equality.** A principal minted under generation *N* is inert the
    /// instant the directory reaches *N+1*. Written as `==` rather than `>=`
    /// deliberately: `>=` would ask "was this ever valid", and every stale
    /// principal in flight answers yes.
    pub fn is_current(&self, principal: &AuthenticatedSeatPrincipal) -> bool {
        principal.generation() == self.generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use grapt_types::{ClockAuthority, IdAuthority, IdToken, Timestamp};

    use crate::auth::{AuthenticationPolicyVersion, CredentialAdapter, SubjectId};

    /// Authorities that refuse to be used. A double that answered plausibly
    /// would let "the empty configuration asks nothing of them" pass whether or
    /// not it were true; these turn that claim into one the harness enforces.
    #[derive(Debug)]
    struct UnaskedClock;

    impl ClockAuthority for UnaskedClock {
        fn now(&self) -> Timestamp {
            panic!("configuring no entries read a clock")
        }
    }

    #[derive(Debug)]
    struct UnaskedIds;

    impl IdAuthority for UnaskedIds {
        fn mint(&self) -> IdToken {
            panic!("configuring no entries minted a client identity")
        }
    }

    fn bound() -> MaxPrincipalMappings {
        match MaxPrincipalMappings::try_new(4) {
            Ok(max) => max,
            Err(refusal) => panic!("a positive mapping bound was refused: {refusal}"),
        }
    }

    /// Compared field by observable field, because `SeatPrincipalDirectory`
    /// deliberately derives no `PartialEq`. The authorities go only to the
    /// `configure` side: the whole claim is that the other side needs none.
    #[test]
    fn configure_empty_is_value_identical_to_configuring_no_entries() {
        let clock = UnaskedClock;
        let ids = UnaskedIds;
        let authorities = DeterministicAuthorities::new(&clock, &ids);

        let configured = match SeatPrincipalDirectory::configure(Vec::new(), bound(), &authorities)
        {
            Ok(directory) => directory,
            Err(refusal) => panic!("an empty configuration must be admitted: {refusal}"),
        };
        let empty = SeatPrincipalDirectory::configure_empty(bound());

        assert_eq!(
            empty.generation(),
            configured.generation(),
            "both are freshly configured, so both are live at the FIRST generation"
        );
        assert_eq!(
            empty.max_mappings(),
            configured.max_mappings(),
            "the stated bound is carried through unchanged"
        );
        assert_eq!(empty.len(), configured.len());
        assert_eq!(empty.is_empty(), configured.is_empty());

        // Absolutely as well as relatively: an equality between two wrong values
        // is still an equality.
        assert_eq!(empty.len(), 0);
        assert!(empty.is_empty());
    }

    /// The property a composed host renders as `401`: there is no subject to
    /// find, so `resolve` refuses without naming a bound or a reason.
    #[test]
    fn an_empty_directory_resolves_no_subject() {
        let directory = SeatPrincipalDirectory::configure_empty(bound());
        let policy = match AuthenticationPolicyVersion::try_new("auth-policy-v1") {
            Ok(policy) => policy,
            Err(refusal) => panic!("the fixture policy was refused: {refusal}"),
        };
        let adapter = CredentialAdapter::new(policy);

        // Distinct spellings, so a pass cannot come from one unlucky value.
        for name in ["alpha", "beta", "seat-alpha", "unknown"] {
            let subject = match SubjectId::try_new(name) {
                Ok(subject) => subject,
                Err(refusal) => panic!("the fixture subject {name:?} was refused: {refusal}"),
            };
            match directory.resolve(adapter.verify_fixture(subject)) {
                Ok(_) => panic!("an empty directory resolved {name:?}"),
                Err(refusal) => assert!(
                    matches!(refusal, ApiRefusal::CredentialRejected { .. }),
                    "an unresolvable subject is a rejected credential; got {refusal:?}"
                ),
            }
        }
    }
}
