//! The sealed transport principal, the verified subject it is minted from, and
//! the credential adapter that is the only route to either.
//!
//! ## What "sealed" has to mean, exactly
//!
//! `grapt-types::authority` records the boundary this crate is on the other
//! side of, and the shape of the fix: a public constructor "under any name,
//! including an `elect`-style trust root" is the bypass, because a caller can
//! assemble the data itself. The same file documents a live instance of the
//! defect at `transport.rs` — a doc comment claiming unforgeability over a type
//! with a public `new`.
//!
//! So the seal here is structural, and it is three separate obligations rather
//! than one:
//!
//! * the mint is crate-private, reachable only from this module's own
//!   verification path;
//! * the inputs to the mint are themselves sealed — a [`VerifiedSubject`] with
//!   no public constructor, not a `String` subject the caller supplies;
//! * the test fixture route is `#[cfg(test)]` rather than a feature or
//!   `doc(hidden)`. Features are additive, so any dependent could turn one on,
//!   and a reachable test hook is a second public route to authority wearing a
//!   different name.
//!
//! A crate that shipped `pub fn from_verified_subject(subject: String)` would
//! have shipped none of this.
//!
//! ## What this lane does NOT ship
//!
//! There is no credential backend here, and there will not be one in this
//! crate: passwords, tokens, OAuth, Basic auth and TLS are permanently excluded
//! from `grapt-api`. `CredentialAdapter::verify` therefore refuses every
//! credential it is given. That is the honest production behaviour of a trust
//! boundary whose verifier is a named later lane, and it is why ruling
//! R4HA-04 says production A can authenticate and refuse but cannot allow.
//!
//! The refusals are still law rather than a stub: a caller that presents an
//! authored [`AuthenticatedClientId`] is refused for *provenance* with its own
//! typed reason, ahead of and distinctly from the generic credential refusal.

use core::fmt;

use grapt_types::{AuthenticatedClientId, SeatRecipientId};

use grapt_auth_basic::BasicAuthVerifier;

use crate::directory::DirectoryGeneration;
use crate::error::{ApiRefusal, Redacted};

/// The largest subject name this boundary admits, in UTF-8 bytes.
///
/// Its own literal, matched to `MAX_SEAT_RECIPIENT_ID_BYTES` because a subject
/// and a recipient are both bounded configuration addresses, and a boundary
/// with two different limits invites the one that is checked second.
pub const MAX_SUBJECT_ID_BYTES: usize = 256;

/// The name of an authentication subject.
///
/// An **address, not authority** — exactly like
/// [`grapt_types::SeatRecipientId`], and publicly constructible for the same
/// reason: configuration has to be able to name one. Holding a `SubjectId`
/// grants nothing. What is sealed is [`VerifiedSubject`], the proof that this
/// boundary verified one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SubjectId(String);

impl SubjectId {
    /// Bounded and non-empty. An empty subject would collapse every
    /// unauthenticated request onto one directory key.
    pub fn try_new(raw: impl Into<String>) -> Result<Self, ApiRefusal> {
        let raw = raw.into();
        if raw.is_empty() || raw.len() > MAX_SUBJECT_ID_BYTES {
            // Its OWN bound, named. R1 finding H3: this used to report the seat
            // recipient bound, which is a different type with a different limit.
            return Err(ApiRefusal::SubjectMalformed {
                max: MAX_SUBJECT_ID_BYTES,
                observed: Redacted::new(raw),
            });
        }
        Ok(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SubjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The version of the authentication policy that verified a subject.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AuthenticationPolicyVersion(String);

impl AuthenticationPolicyVersion {
    pub fn try_new(raw: impl Into<String>) -> Result<Self, ApiRefusal> {
        let raw = raw.into();
        if raw.is_empty() {
            // Carries no observation: the only defect possible here is
            // emptiness, so there is nothing to redact and nothing to name
            // beyond the rule itself.
            return Err(ApiRefusal::PolicyVersionMalformed);
        }
        Ok(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Proof that THIS boundary verified a subject.
///
/// No public constructor, no `FromStr`, no `From<SubjectId>`, no `Default`. The
/// only route is `CredentialAdapter::verify`, which is crate-private, plus
/// the `#[cfg(test)]` fixture route below.
///
/// Not `Clone`: a verification is one verification. Cloning would let one
/// successful verify be spent as two, which is the same defect the non-clone
/// lease closes one layer up.
#[derive(PartialEq, Eq)]
pub struct VerifiedSubject {
    subject: SubjectId,
    policy: AuthenticationPolicyVersion,
}

impl VerifiedSubject {
    /// The subject this boundary verified.
    pub fn subject(&self) -> &SubjectId {
        &self.subject
    }

    /// The authentication policy version that verified it.
    pub fn policy(&self) -> &AuthenticationPolicyVersion {
        &self.policy
    }

    /// The crate-private mint. See the module note: this being private, and
    /// this being the only route, is the whole seal.
    ///
    /// The `dead_code` allowance is scoped to `not(test)` on purpose, following
    /// `grapt-types::authority`'s handling of the identical situation. In a
    /// test build the seam must be genuinely exercised or the warning fires for
    /// real; in a plain library build it has no in-crate caller, because this
    /// lane ships no credential backend and the verifier that will call it is a
    /// named later lane. A blanket `allow` would hide a genuinely unused item in
    /// both configurations, which is what the no-broad-suppression law forbids.
    ///
    /// That this is unreachable in a production build is not a gap — it is the
    /// literal content of ruling R4HA-04: production A authenticates and
    /// refuses, and cannot allow.
    #[cfg_attr(not(test), allow(dead_code))]
    fn mint(subject: SubjectId, policy: AuthenticationPolicyVersion) -> Self {
        Self { subject, policy }
    }
}

/// Redacted, for the reason `LivePermissionDecision` is: diagnostics may name a
/// verification without exporting the subject it verified.
impl fmt::Debug for VerifiedSubject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VerifiedSubject(<verified, process-local>)")
    }
}

/// What was verified, retained inside the principal as audit truth.
///
/// The *subject* and the *policy version*, never the credential. A receipt that
/// carried the credential would put it everywhere the principal goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubjectReceipt {
    subject: SubjectId,
    policy: AuthenticationPolicyVersion,
}

impl SubjectReceipt {
    pub fn subject(&self) -> &SubjectId {
        &self.subject
    }

    pub fn policy(&self) -> &AuthenticationPolicyVersion {
        &self.policy
    }
}

/// The server-owned, unforgeable-by-DTO context for one authenticated seat
/// connection.
///
/// This is the value R3C2T names as its downstream requirement, and it is what
/// every authorization in this crate consumes. It carries, all private and
/// readable only through the accessors below:
///
/// 1. the subject receipt — what was verified;
/// 2. the [`AuthenticatedClientId`] this boundary issued, which is load-bearing
///    **only** by being inside this sealed value;
/// 3. the [`SeatRecipientId`], server-derived and never request-derived;
/// 4. the [`DirectoryGeneration`] it was minted under.
///
/// Field 4 is the one that is easy to leave out and expensive to add later. A
/// principal is minted once, at authentication, and then lives for the duration
/// of a connection that may be very long. Without the generation, revoking a
/// mapping does nothing to a principal already in flight: the revoked subject
/// keeps full authority until it happens to disconnect, and no type notices.
///
/// Not `Clone`, not `Default`, no public constructor, no `From` from anything,
/// and no serialization route — this crate takes no serde dependency at all, so
/// there is no derive that could grow one by accident.
#[derive(PartialEq, Eq)]
pub struct AuthenticatedSeatPrincipal {
    receipt: SubjectReceipt,
    client: AuthenticatedClientId,
    recipient: SeatRecipientId,
    generation: DirectoryGeneration,
}

impl AuthenticatedSeatPrincipal {
    /// The crate-private mint.
    ///
    /// Reachable only from [`crate::directory::SeatPrincipalDirectory::resolve`],
    /// which is the only code that holds both a [`VerifiedSubject`] and the
    /// server-owned mapping. Taking the subject **by value** is deliberate: one
    /// verification mints one principal.
    pub(crate) fn mint(
        subject: VerifiedSubject,
        client: AuthenticatedClientId,
        recipient: SeatRecipientId,
        generation: DirectoryGeneration,
    ) -> Self {
        Self {
            receipt: SubjectReceipt {
                subject: subject.subject.clone(),
                policy: subject.policy.clone(),
            },
            client,
            recipient,
            generation,
        }
    }

    /// What was verified.
    pub fn receipt(&self) -> &SubjectReceipt {
        &self.receipt
    }

    /// The client identity this boundary issued.
    pub fn client(&self) -> &AuthenticatedClientId {
        &self.client
    }

    /// The server-derived recipient. Never read from a request.
    pub fn recipient(&self) -> &SeatRecipientId {
        &self.recipient
    }

    /// The directory generation this principal was minted under.
    pub fn generation(&self) -> DirectoryGeneration {
        self.generation
    }
}

impl fmt::Debug for AuthenticatedSeatPrincipal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuthenticatedSeatPrincipal(<sealed, process-local>)")
    }
}

/// What a caller presents at the boundary.
///
/// Note the second field. `AuthenticatedClientId::new` is public, so a caller
/// *can* author a syntactically perfect client id and attach it. Modelling that
/// explicitly is the point: the refusal is then a decision this boundary makes
/// about provenance, rather than something that happens not to be read.
///
/// # This type holds the raw secret, and neither prints nor compares it
///
/// R1 finding H1, and it was the blocking one. This type derived `Debug`, so
/// `format!("{:?}", credential)` printed `material` verbatim — while the very
/// same bytes were being carefully redacted one step later into
/// `ApiRefusal::CredentialRejected`. **The crate redacted the copy and printed
/// the original.** Every other secret-bearing type here — `VerifiedSubject`,
/// [`AuthenticatedSeatPrincipal`], `RecipientLease` — already hand-wrote a
/// redacting `Debug`; the one type holding the actual credential was the one
/// that derived it.
///
/// The root cause is worth naming so it is not repeated: the workspace lints
/// `missing_debug_implementations = "warn"`, which makes `derive(Debug)` the
/// path of least resistance on every public type. The fix is a redacting impl,
/// never removing `Debug`.
///
/// `PartialEq`/`Eq` are gone for the H2 reason applied to the type that holds
/// the original rather than the copy: a derived equality over `material` is an
/// offline guess oracle against the raw credential.
#[derive(Clone)]
pub struct PresentedCredential {
    material: Vec<u8>,
    claimed_client: Option<AuthenticatedClientId>,
}

/// Redacted, like every other secret-bearing type in this crate.
///
/// The claimed client id is an inert, publicly constructible identity rather
/// than secret material, so naming its presence is safe and useful for
/// diagnosis — but its value is redacted too, because it is caller-authored
/// text arriving at a trust boundary and this type is not the place to decide
/// it is harmless.
impl fmt::Debug for PresentedCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PresentedCredential")
            .field("material", &"<redacted>")
            .field(
                "claimed_client",
                match &self.claimed_client {
                    Some(_) => &"Some(<redacted>)",
                    None => &"None",
                },
            )
            .finish()
    }
}

impl PresentedCredential {
    /// A credential with no claimed identity.
    pub fn new(material: impl Into<String>) -> Self {
        Self {
            material: material.into().into_bytes(),
            claimed_client: None,
        }
    }

    /// A credential that also claims a client identity the caller authored.
    pub fn claiming(material: impl Into<String>, client: AuthenticatedClientId) -> Self {
        Self {
            material: material.into().into_bytes(),
            claimed_client: Some(client),
        }
    }

    /// A raw HTTP credential, including a value HTTP permitted but UTF-8 does
    /// not. The verifier owns its bytes so malformed input remains observable
    /// as a refusal without being normalized into plausible text.
    pub(crate) fn new_raw(material: impl Into<Vec<u8>>) -> Self {
        Self {
            material: material.into(),
            claimed_client: None,
        }
    }
}

/// The crate-private verification path.
///
/// Crate-private in its entirety. A public adapter would be a public route to
/// [`VerifiedSubject`], and therefore to the principal, no matter how the
/// verification inside it were written.
#[derive(Debug)]
pub(crate) struct CredentialAdapter {
    policy: AuthenticationPolicyVersion,
    backend: CredentialBackend,
}

#[derive(Debug)]
enum CredentialBackend {
    Refuse,
    InjectedBasic(BasicAuthVerifier),
}

impl CredentialAdapter {
    pub(crate) fn new(policy: AuthenticationPolicyVersion) -> Self {
        Self {
            policy,
            backend: CredentialBackend::Refuse,
        }
    }

    pub(crate) fn injected_basic(
        policy: AuthenticationPolicyVersion,
        verifier: BasicAuthVerifier,
    ) -> Self {
        Self {
            policy,
            backend: CredentialBackend::InjectedBasic(verifier),
        }
    }

    /// Verify a presented credential.
    ///
    /// Two refusals, in this order, and the order is the law:
    ///
    /// 1. a caller-authored client identity is refused for **provenance**. This
    ///    check is first because it must not depend on whether the credential
    ///    would otherwise have verified — a caller must never be able to learn
    ///    that its authored id was "nearly" accepted.
    /// 2. everything else is refused because this crate ships no credential
    ///    backend, permanently and by exclusion.
    ///
    /// The `policy` field is what a real verifier would stamp into the
    /// resulting [`VerifiedSubject`]; the `#[cfg(test)]` route below uses it, so
    /// the sealed shape is exercised rather than merely declared.
    pub(crate) fn verify(
        &self,
        presented: &PresentedCredential,
    ) -> Result<VerifiedSubject, ApiRefusal> {
        if let Some(claimed) = &presented.claimed_client {
            return Err(ApiRefusal::ClientIdentityNotIssuedHere {
                observed: Redacted::new(claimed.as_str().to_owned()),
            });
        }
        match &self.backend {
            CredentialBackend::Refuse => Err(ApiRefusal::CredentialRejected {
                observed: Redacted::new(String::from_utf8_lossy(&presented.material).into_owned()),
            }),
            CredentialBackend::InjectedBasic(verifier) => {
                let verified = verifier.verify(&presented.material);
                match verified {
                    Ok(subject) => SubjectId::try_new(subject)
                        .map(|subject| VerifiedSubject::mint(subject, self.policy.clone())),
                    Err(_) => Err(ApiRefusal::CredentialRejected {
                        observed: Redacted::new(
                            String::from_utf8_lossy(&presented.material).into_owned(),
                        ),
                    }),
                }
            }
        }
    }

    /// Deterministic verification for this crate's own witnesses.
    ///
    /// `#[cfg(test)]` is the point, and it is the same decision
    /// `grapt-types::authority::test_support` documents: this compiles only
    /// into this crate's own library test binary. It is not a feature — features
    /// are additive, so any dependent could turn one on — and not
    /// `doc(hidden)`, which is reachable too. A dependent crate cannot call
    /// this, and neither can this crate's own integration tests, which is why
    /// the sealed-path witnesses live in the library rather than in `tests/`.
    #[cfg(test)]
    pub(crate) fn verify_fixture(&self, subject: SubjectId) -> VerifiedSubject {
        VerifiedSubject::mint(subject, self.policy.clone())
    }
}

/// The composed trust boundary: verify a credential, then resolve the verified
/// subject against the server-owned directory.
///
/// This is the production entry point, and composing the two steps in one owner
/// is what makes the trust flow in the module note a sequence rather than a
/// suggestion. A caller cannot hold the directory and skip the adapter, because
/// [`crate::directory::SeatPrincipalDirectory::resolve`] takes a
/// [`VerifiedSubject`] it has no way to obtain.
#[derive(Debug)]
pub struct SeatTrustBoundary {
    adapter: CredentialAdapter,
    directory: crate::directory::SeatPrincipalDirectory,
}

impl SeatTrustBoundary {
    pub fn new(
        policy: AuthenticationPolicyVersion,
        directory: crate::directory::SeatPrincipalDirectory,
    ) -> Self {
        Self {
            adapter: CredentialAdapter::new(policy),
            directory,
        }
    }

    /// Compose the boundary around exactly one externally owned Basic verifier.
    ///
    /// The verifier may decide whether presented material is valid, but it can
    /// never mint a [`VerifiedSubject`] or bypass this directory: its sole
    /// output is an address consumed by the private adapter below.
    pub fn with_injected_basic_verifier(
        policy: AuthenticationPolicyVersion,
        directory: crate::directory::SeatPrincipalDirectory,
        verifier: BasicAuthVerifier,
    ) -> Self {
        Self {
            adapter: CredentialAdapter::injected_basic(policy, verifier),
            directory,
        }
    }

    /// The server-owned directory, for generation currency checks.
    pub fn directory(&self) -> &crate::directory::SeatPrincipalDirectory {
        &self.directory
    }

    /// Revoke a subject's mapping, advancing the directory generation.
    pub fn revoke(&mut self, subject: &SubjectId) -> Result<(), ApiRefusal> {
        self.directory.revoke(subject)
    }

    /// Verify, then resolve, then mint. Every step can refuse, and a refusal at
    /// any step means no principal exists at all — there is no partially
    /// authenticated state for a later check to be lenient about.
    pub fn authenticate(
        &self,
        presented: &PresentedCredential,
    ) -> Result<AuthenticatedSeatPrincipal, ApiRefusal> {
        let verified = self.adapter.verify(presented)?;
        self.directory.resolve(verified)
    }

    /// The same composition the `#[cfg(test)]` fixture route drives, so the
    /// witnesses exercise `resolve` through the real owner rather than reaching
    /// past it.
    #[cfg(test)]
    pub(crate) fn authenticate_fixture(
        &self,
        subject: SubjectId,
    ) -> Result<AuthenticatedSeatPrincipal, ApiRefusal> {
        let verified = self.adapter.verify_fixture(subject);
        self.directory.resolve(verified)
    }
}
