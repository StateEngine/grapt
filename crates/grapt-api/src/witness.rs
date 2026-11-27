//! The executable owner of the R4H-A acceptance corpus.
//!
//! ## Why these witnesses live in the library rather than in `tests/`
//!
//! Every law worth witnessing here starts from a sealed value. The mint for
//! [`crate::auth::VerifiedSubject`] is crate-private and its fixture route is
//! `#[cfg(test)]` — which, deliberately, compiles into this crate's own library
//! test binary and nowhere else. An integration test under `tests/` is a
//! separate crate that links this one as a dependency, so it cannot reach the
//! fixture route either. That is the seal working, not a gap in it.
//!
//! The alternative would have been a feature flag or a `doc(hidden)`
//! constructor, and `grapt-types::authority` records exactly why both are
//! wrong: features are additive, so any dependent can turn one on, and a
//! reachable test hook is a second public route to authority wearing a
//! different name.
//!
//! So the sealed-path witnesses are here, and `tests/` carries what genuinely
//! only needs the public surface: the router, the decode bound, the two
//! dependency gates, the compile-fail probes, and the corpus validator that
//! binds each scenario in this file to a real `#[test]`.
//!
//! `crates/grapt-api/R4HA_RESIDUALS.md` records this as a disclosed deviation
//! from the preflight's proposed file list.

use std::cell::Cell;

use grapt_types::{
    AuthenticatedClientId, ClockAuthority, DeterministicAuthorities, IdAuthority, IdToken,
    SeatRecipientId, Timestamp,
};

use crate::auth::{
    AuthenticatedSeatPrincipal, AuthenticationPolicyVersion, PresentedCredential,
    SeatTrustBoundary, SubjectId,
};
use crate::authorization::{
    authenticate_and_govern, authorize, govern, AuthorizationDecision, AuthorizationRequest,
    SeatApplication,
};
use crate::directory::{MaxPrincipalMappings, SeatDirectoryEntry, SeatPrincipalDirectory};
use crate::error::{ApiRefusal, Redacted};
use crate::takeover::{MaxTakeoverSlots, RecipientConnectionRegistry};

// ---------------------------------------------------------------------------
// Injected authorities. Law H2: nothing here reads an ambient clock or an
// ambient randomness source, so replay is deterministic and the row-11
// `disallowed_methods` gate has nothing to catch.
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct FixedClock(i64);

impl ClockAuthority for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0)
    }
}

/// Mints a DISTINCT token per call, because R4HA-03 binds one subject to one
/// client identity and a constant double would make that indistinguishable
/// from "every subject shares one identity".
#[derive(Debug)]
struct CountingIds {
    next: Cell<u64>,
}

impl CountingIds {
    fn new() -> Self {
        Self { next: Cell::new(0) }
    }
}

impl IdAuthority for CountingIds {
    fn mint(&self) -> IdToken {
        let ordinal = self.next.get();
        self.next.set(ordinal.saturating_add(1));
        match IdToken::new(format!("cli-{ordinal}")) {
            Some(token) => token,
            // `panic!` rather than a silent fallback: a double that cannot mint
            // has broken the fixture, and a witness must not continue on a
            // value it invented. The workspace denies `unwrap`/`expect`, not
            // `panic!` with a diagnostic.
            None => panic!("the counting id authority produced an empty token"),
        }
    }
}

/// Counts invocations, and nothing else. The whole law this seam carries is
/// how many times the application was reached.
#[derive(Debug, Default)]
struct CountingApplication {
    calls: usize,
}

impl SeatApplication for CountingApplication {
    fn invoke(&mut self, _request: &AuthorizationRequest) {
        self.calls += 1;
    }
}

// ---------------------------------------------------------------------------
// Fixture builders. Each panics with a diagnostic rather than returning a
// Result: an `Err` return from a helper leaves the harness no message to
// attribute a failure to.
// ---------------------------------------------------------------------------

fn subject(raw: &str) -> SubjectId {
    match SubjectId::try_new(raw) {
        Ok(value) => value,
        Err(refusal) => panic!("fixture subject {raw:?} was refused: {refusal}"),
    }
}

fn recipient(raw: &str) -> SeatRecipientId {
    match SeatRecipientId::try_new(raw) {
        Ok(value) => value,
        Err(error) => panic!("fixture recipient {raw:?} was refused: {error:?}"),
    }
}

fn policy() -> AuthenticationPolicyVersion {
    match AuthenticationPolicyVersion::try_new("auth-policy-v1") {
        Ok(value) => value,
        Err(refusal) => panic!("fixture policy was refused: {refusal}"),
    }
}

fn max_mappings(value: usize) -> MaxPrincipalMappings {
    match MaxPrincipalMappings::try_new(value) {
        Ok(bound) => bound,
        Err(refusal) => panic!("fixture bound {value} was refused: {refusal}"),
    }
}

fn directory_of(
    entries: Vec<SeatDirectoryEntry>,
    max: usize,
    ids: &CountingIds,
) -> SeatPrincipalDirectory {
    let clock = FixedClock(1_700_000_000_000);
    let authorities = DeterministicAuthorities::new(&clock, ids);
    match SeatPrincipalDirectory::configure(entries, max_mappings(max), &authorities) {
        Ok(directory) => directory,
        Err(refusal) => panic!("fixture directory was refused: {refusal}"),
    }
}

/// One subject `alpha` owning recipient `seat-alpha`, plus a second subject
/// `beta` owning `seat-beta`, which is what the DTO-spoof rows need.
fn two_seat_boundary(ids: &CountingIds) -> SeatTrustBoundary {
    let directory = directory_of(
        vec![
            SeatDirectoryEntry::new(subject("alpha"), recipient("seat-alpha")),
            SeatDirectoryEntry::new(subject("beta"), recipient("seat-beta")),
        ],
        8,
        ids,
    );
    SeatTrustBoundary::new(policy(), directory)
}

fn principal_for(boundary: &SeatTrustBoundary, name: &str) -> AuthenticatedSeatPrincipal {
    match boundary.authenticate_fixture(subject(name)) {
        Ok(principal) => principal,
        Err(refusal) => panic!("fixture authentication for {name:?} was refused: {refusal}"),
    }
}

/// A capability request. The coordinates are F4's own — a derived
/// [`grapt_types::CapabilityId`] and a [`grapt_types::GrantScope`] — because
/// R4HA-04 forbids A from forking the action model with a vocabulary of its
/// own.
fn request() -> AuthorizationRequest {
    use grapt_types::{
        CapabilityId, EffectClass, EgressAuthority, FilesystemAuthority, GrantScope,
        RealizedEnforcement, ResolvedCapability,
    };
    let resolved = ResolvedCapability::new(
        EffectClass::ToolInvoke,
        None,
        FilesystemAuthority::default(),
        EgressAuthority::new(Vec::new()),
        RealizedEnforcement::HarnessOnly,
    );
    AuthorizationRequest::new(CapabilityId::derive(&resolved), GrantScope::Attempt)
}

// ---------------------------------------------------------------------------
// Corpus scenario: sealed-mapping-mints-principal          (rows QA01, QA24, QA25)
// ---------------------------------------------------------------------------

#[test]
fn a_verified_subject_resolves_server_side_and_mints_the_sealed_principal() {
    let ids = CountingIds::new();
    let boundary = two_seat_boundary(&ids);
    let principal = principal_for(&boundary, "alpha");

    assert_eq!(
        principal.recipient(),
        &recipient("seat-alpha"),
        "the recipient must come from the server-owned directory"
    );
    assert_eq!(
        principal.receipt().subject(),
        &subject("alpha"),
        "the principal must carry a receipt for the subject that was verified"
    );
    assert_eq!(
        principal.generation(),
        boundary.directory().generation(),
        "the principal must be stamped with the generation it was minted under"
    );
    assert!(
        boundary.directory().is_current(&principal),
        "a freshly minted principal is current"
    );
}

#[test]
fn one_subject_is_issued_exactly_one_client_identity() {
    let ids = CountingIds::new();
    let boundary = two_seat_boundary(&ids);

    let first = principal_for(&boundary, "alpha");
    let second = principal_for(&boundary, "alpha");

    assert_eq!(
        first.client(),
        second.client(),
        "R4HA-03: one verified subject maps to exactly one transport client \
         identity, so re-authenticating must not issue a second one"
    );

    let other = principal_for(&boundary, "beta");
    assert_ne!(
        first.client(),
        other.client(),
        "two distinct subjects must not collapse onto one client identity"
    );
}

#[test]
fn two_subjects_may_map_to_one_recipient() {
    let ids = CountingIds::new();
    let directory = directory_of(
        vec![
            SeatDirectoryEntry::new(subject("alpha"), recipient("shared-seat")),
            SeatDirectoryEntry::new(subject("beta"), recipient("shared-seat")),
        ],
        8,
        &ids,
    );
    let boundary = SeatTrustBoundary::new(policy(), directory);

    let first = principal_for(&boundary, "alpha");
    let second = principal_for(&boundary, "beta");

    assert_eq!(
        first.recipient(),
        second.recipient(),
        "R4HA-03 permits multiple distinct verified subjects to map to the \
         same recipient"
    );
    assert_ne!(
        first.client(),
        second.client(),
        "sharing a recipient must not merge two subjects' client identities"
    );
}

// ---------------------------------------------------------------------------
// Corpus scenario: dto-recipient-never-selects                     (rows QA02, QA03)
// ---------------------------------------------------------------------------

#[test]
fn a_dto_recipient_never_selects_the_principal() {
    let ids = CountingIds::new();
    let boundary = two_seat_boundary(&ids);

    // Authenticated as `alpha`; the request names seat B.
    let principal = principal_for(&boundary, "alpha");
    let spoofed = request().with_dto_recipient(recipient("seat-beta"));

    assert_eq!(
        principal.recipient(),
        &recipient("seat-alpha"),
        "the server-derived recipient is authoritative and the DTO cannot move it"
    );

    let mut application = CountingApplication::default();
    let decision = govern(
        boundary.directory(),
        &principal,
        &spoofed,
        None,
        &mut application,
    );

    // Matched on the VARIANT rather than compared for equality. R1 finding H2
    // removed `PartialEq` from `ApiRefusal` because a derived equality over a
    // `Redacted` field is an offline guess oracle; a witness must not be the
    // reason such an oracle exists.
    assert!(
        matches!(
            decision,
            AuthorizationDecision::Refused(ApiRefusal::RecipientDisagreement { .. })
        ),
        "naming another seat is refused, not silently overridden; got {decision:?}"
    );
    assert_eq!(
        application.calls, 0,
        "a spoofed recipient must never reach the application"
    );
}

// ---------------------------------------------------------------------------
// Corpus scenario: dto-recipient-disagreement-refused                  (row QA04)
// ---------------------------------------------------------------------------

#[test]
fn a_disagreeing_dto_recipient_is_refused_before_the_application_call() {
    let ids = CountingIds::new();
    let boundary = two_seat_boundary(&ids);
    let principal = principal_for(&boundary, "alpha");
    let mut application = CountingApplication::default();

    // Agreement grants nothing: with the matching recipient the request still
    // has no capability proof, so it is refused for THAT reason. If agreement
    // were treated as evidence this would be allowed.
    let agreeing = request().with_dto_recipient(recipient("seat-alpha"));
    let decision = govern(
        boundary.directory(),
        &principal,
        &agreeing,
        None,
        &mut application,
    );
    assert!(
        matches!(
            decision,
            AuthorizationDecision::Refused(ApiRefusal::NoCapabilityProof)
        ),
        "a DTO recipient that happens to agree grants nothing; got {decision:?}"
    );

    let disagreeing = request().with_dto_recipient(recipient("seat-beta"));
    let decision = govern(
        boundary.directory(),
        &principal,
        &disagreeing,
        None,
        &mut application,
    );
    assert!(
        matches!(
            decision.refusal(),
            Some(ApiRefusal::RecipientDisagreement { .. })
        ),
        "disagreement is its own typed refusal, distinct from a missing proof"
    );
    assert_eq!(
        application.calls, 0,
        "neither path may reach the application"
    );
}

// ---------------------------------------------------------------------------
// Corpus scenario: inert-client-id-is-refused                          (row QA05)
// ---------------------------------------------------------------------------

#[test]
fn a_caller_authored_client_identity_is_refused_for_provenance() {
    use grapt_types::AuthenticatedClientId;

    let ids = CountingIds::new();
    let boundary = two_seat_boundary(&ids);

    // `AuthenticatedClientId::new` is PUBLIC, so this is a syntactically
    // perfect identity. It is refused because of where it came from, not
    // because of how it looks.
    let Some(authored) = AuthenticatedClientId::new("cli-0") else {
        panic!("the fixture client identity must be constructible");
    };
    let presented = PresentedCredential::claiming("any-material", authored);

    let refusal = match boundary.authenticate(&presented) {
        Ok(_) => panic!("a caller-authored client identity must never authenticate"),
        Err(refusal) => refusal,
    };
    assert!(
        matches!(refusal, ApiRefusal::ClientIdentityNotIssuedHere { .. }),
        "provenance is its own refusal, ahead of and distinct from the generic \
         credential refusal; got {refusal:?}"
    );

    // And `cli-0` is exactly what the directory issued to `alpha`, so the
    // refusal is not an accident of the value being unknown.
    let principal = principal_for(&boundary, "alpha");
    assert_eq!(
        principal.client().as_str(),
        "cli-0",
        "the authored value matches a REAL issued identity, which is what makes \
         this a provenance test rather than a validity test"
    );
}

// ---------------------------------------------------------------------------
// Corpus scenario: unauthenticated-never-reaches-application           (row QA06)
// ---------------------------------------------------------------------------

#[test]
fn an_unauthenticated_request_never_reaches_the_application() {
    let ids = CountingIds::new();
    let boundary = two_seat_boundary(&ids);
    let mut application = CountingApplication::default();

    let presented = PresentedCredential::new("no-backend-accepts-this");
    let outcome =
        authenticate_and_govern(&boundary, &presented, &request(), None, &mut application);

    assert!(
        matches!(outcome, Err(ApiRefusal::CredentialRejected { .. })),
        "with no credential backend the only honest answer is a refusal"
    );
    assert_eq!(
        application.calls, 0,
        "the application must not be reached when authentication fails"
    );
}

// ---------------------------------------------------------------------------
// Corpus scenario: authentication-is-not-authorization                 (row QA07)
// ---------------------------------------------------------------------------

#[test]
fn an_authentic_principal_is_still_refused_without_a_capability_proof() {
    let ids = CountingIds::new();
    let boundary = two_seat_boundary(&ids);
    let principal = principal_for(&boundary, "alpha");
    let mut application = CountingApplication::default();

    assert!(
        boundary.directory().is_current(&principal),
        "the principal is authentic and current, so the refusal below is about \
         authorization rather than authentication"
    );

    let decision = govern(
        boundary.directory(),
        &principal,
        &request(),
        None,
        &mut application,
    );

    assert!(
        matches!(
            decision,
            AuthorizationDecision::Refused(ApiRefusal::NoCapabilityProof)
        ),
        "absence of a proof is a refusal, never a default allow; got {decision:?}"
    );
    assert_eq!(
        application.calls, 0,
        "refuse before effect: zero invocations"
    );
}

// ---------------------------------------------------------------------------
// Corpus scenario: revocation-invalidates-live-principal         (rows QA08, QA09)
// ---------------------------------------------------------------------------

#[test]
fn a_principal_minted_under_a_superseded_generation_authorizes_nothing() {
    let ids = CountingIds::new();
    let mut boundary = two_seat_boundary(&ids);

    // Minted under generation 1, while the mapping is live.
    let principal = principal_for(&boundary, "alpha");
    assert!(boundary.directory().is_current(&principal));

    // Revoke a DIFFERENT subject. The principal for `alpha` is still mapped,
    // and it still becomes inert, because the generation is directory-wide.
    if let Err(refusal) = boundary.revoke(&subject("beta")) {
        panic!("revoking a live mapping must succeed: {refusal}");
    }

    assert!(
        !boundary.directory().is_current(&principal),
        "a principal minted under a superseded generation is not current"
    );

    let mut application = CountingApplication::default();
    let decision = govern(
        boundary.directory(),
        &principal,
        &request(),
        None,
        &mut application,
    );

    assert!(
        matches!(
            decision,
            AuthorizationDecision::Refused(ApiRefusal::StaleDirectoryGeneration)
        ),
        "currency is compared for EQUALITY: a stale principal is refused as \
         stale, ahead of and distinctly from an unproven one. Under `>=` this \
         would fall through and report NoCapabilityProof instead; got {decision:?}"
    );
    assert_eq!(application.calls, 0);
}

#[test]
fn revoking_a_mapping_advances_the_directory_generation() {
    let ids = CountingIds::new();
    let mut boundary = two_seat_boundary(&ids);
    let before = boundary.directory().generation();

    if let Err(refusal) = boundary.revoke(&subject("beta")) {
        panic!("revoking a live mapping must succeed: {refusal}");
    }
    let after = boundary.directory().generation();

    assert_eq!(
        after.value(),
        before.value() + 1,
        "revocation must advance the generation, or nothing already minted \
         notices the withdrawal"
    );

    // The revoked subject can no longer authenticate at all.
    match boundary.authenticate_fixture(subject("beta")) {
        Ok(_) => panic!("a revoked subject must not resolve"),
        Err(refusal) => assert!(matches!(refusal, ApiRefusal::CredentialRejected { .. })),
    }
}

// ---------------------------------------------------------------------------
// Corpus scenario: directory-bound-refuses-oversize              (rows QA10, QA11)
// ---------------------------------------------------------------------------

#[test]
fn an_oversized_directory_configuration_is_refused_whole() {
    let ids = CountingIds::new();
    let clock = FixedClock(1);
    let authorities = DeterministicAuthorities::new(&clock, &ids);

    let entries = vec![
        SeatDirectoryEntry::new(subject("alpha"), recipient("seat-alpha")),
        SeatDirectoryEntry::new(subject("beta"), recipient("seat-beta")),
        SeatDirectoryEntry::new(subject("gamma"), recipient("seat-gamma")),
    ];
    let outcome = SeatPrincipalDirectory::configure(entries, max_mappings(2), &authorities);

    match outcome {
        Ok(directory) => panic!(
            "an oversized configuration must be refused, but a directory of {} \
             was built",
            directory.len()
        ),
        Err(refusal) => assert!(
            matches!(refusal, ApiRefusal::DirectoryBoundExceeded { max: 2 }),
            "the refusal names the BOUND and not the observed count; got {refusal:?}"
        ),
    }
}

#[test]
fn a_duplicate_verified_subject_is_refused_at_configuration() {
    let ids = CountingIds::new();
    let clock = FixedClock(1);
    let authorities = DeterministicAuthorities::new(&clock, &ids);

    let entries = vec![
        SeatDirectoryEntry::new(subject("alpha"), recipient("seat-alpha")),
        SeatDirectoryEntry::new(subject("alpha"), recipient("seat-beta")),
    ];
    let outcome = SeatPrincipalDirectory::configure(entries, max_mappings(8), &authorities);

    match outcome {
        Ok(_) => panic!(
            "R4HA-03: one subject may never map to multiple recipients, so a \
             duplicate is a refusal rather than a last-one-wins overwrite"
        ),
        Err(refusal) => assert!(matches!(refusal, ApiRefusal::DuplicateSubject { .. })),
    }
}

// ---------------------------------------------------------------------------
// Corpus scenario: takeover-displaces-prior-lease           (rows QA12, QA13, QA14)
// ---------------------------------------------------------------------------

#[test]
fn a_second_acquisition_stamps_a_new_lease_generation() {
    let ids = CountingIds::new();
    let boundary = two_seat_boundary(&ids);
    let principal = principal_for(&boundary, "alpha");
    let Ok(slots) = MaxTakeoverSlots::try_new(4) else {
        panic!("the fixture slot bound must be positive");
    };
    let mut registry = RecipientConnectionRegistry::new(slots);

    let Ok(first) = registry.acquire(&principal) else {
        panic!("the first acquisition must succeed");
    };
    let Ok(second) = registry.acquire(&principal) else {
        panic!("a same-recipient takeover must succeed");
    };

    assert_eq!(
        second.lease_ref().generation().value(),
        first.lease_ref().generation().value() + 1,
        "each acquisition stamps a new generation, which is what makes \
         displacement observable"
    );
}

#[test]
fn a_displaced_lease_is_not_current() {
    let ids = CountingIds::new();
    let boundary = two_seat_boundary(&ids);
    let principal = principal_for(&boundary, "alpha");
    let Ok(slots) = MaxTakeoverSlots::try_new(4) else {
        panic!("the fixture slot bound must be positive");
    };
    let mut registry = RecipientConnectionRegistry::new(slots);

    let Ok(first) = registry.acquire(&principal) else {
        panic!("the first acquisition must succeed");
    };
    let first = first.lease();
    assert!(registry.is_current(&first), "generation 1 starts current");

    let Ok(second) = registry.acquire(&principal) else {
        panic!("the takeover must succeed");
    };
    let second = second.lease();

    assert!(
        !registry.is_current(&first),
        "currency is EQUALITY: the displaced lease must not claim ownership. \
         Under `>=` it would keep claiming it forever."
    );
    assert!(
        registry.is_current(&second),
        "the newest lease owns the seat"
    );

    // And a displaced lease cannot evict the holder that displaced it.
    match registry.release(first) {
        Ok(()) => panic!("a displaced lease must not release the live slot"),
        Err(refusal) => assert!(
            matches!(refusal, ApiRefusal::LeaseDisplaced),
            "a displaced lease has its own refusal; got {refusal:?}"
        ),
    }
    assert!(
        registry.is_current(&second),
        "the live lease survives a displaced lease's release attempt"
    );
}

#[test]
fn an_acquisition_that_displaces_reports_the_prior_generation() {
    let ids = CountingIds::new();
    let boundary = two_seat_boundary(&ids);
    let principal = principal_for(&boundary, "alpha");
    let Ok(slots) = MaxTakeoverSlots::try_new(4) else {
        panic!("the fixture slot bound must be positive");
    };
    let mut registry = RecipientConnectionRegistry::new(slots);

    let Ok(first) = registry.acquire(&principal) else {
        panic!("the first acquisition must succeed");
    };
    assert!(
        first.displaced().is_none(),
        "a first acquisition displaces nobody"
    );
    let first_generation = first.lease_ref().generation();

    let Ok(second) = registry.acquire(&principal) else {
        panic!("the takeover must succeed");
    };
    let Some(displacement) = second.displaced() else {
        panic!("a takeover must signal the displacement to the prior owner");
    };
    assert_eq!(
        displacement.displaced_generation(),
        first_generation,
        "the signal names exactly the generation that lost the seat"
    );
}

// ---------------------------------------------------------------------------
// Corpus scenario: takeover-slot-bound-refuses                   (rows QA15, QA16)
// ---------------------------------------------------------------------------

#[test]
fn a_new_recipient_is_refused_when_the_registry_is_full() {
    let ids = CountingIds::new();
    let boundary = two_seat_boundary(&ids);
    let alpha = principal_for(&boundary, "alpha");
    let beta = principal_for(&boundary, "beta");
    let Ok(slots) = MaxTakeoverSlots::try_new(1) else {
        panic!("the fixture slot bound must be positive");
    };
    let mut registry = RecipientConnectionRegistry::new(slots);

    let Ok(_held) = registry.acquire(&alpha) else {
        panic!("the first acquisition must succeed");
    };
    match registry.acquire(&beta) {
        Ok(_) => panic!("a new distinct recipient must be refused when full"),
        Err(refusal) => assert!(
            matches!(refusal, ApiRefusal::TakeoverBoundExceeded { max: 1 }),
            "the refusal names the takeover bound; got {refusal:?}"
        ),
    }
}

#[test]
fn a_same_recipient_takeover_does_not_grow_the_slot_set() {
    let ids = CountingIds::new();
    let boundary = two_seat_boundary(&ids);
    let alpha = principal_for(&boundary, "alpha");
    let Ok(slots) = MaxTakeoverSlots::try_new(1) else {
        panic!("the fixture slot bound must be positive");
    };
    let mut registry = RecipientConnectionRegistry::new(slots);

    let Ok(_first) = registry.acquire(&alpha) else {
        panic!("the first acquisition must succeed");
    };
    // The registry is FULL, and this still succeeds, because a same-recipient
    // handoff replaces a key that is already counted. A takeover storm on one
    // seat therefore cannot consume another seat's slot.
    for _ in 0..8 {
        if registry.acquire(&alpha).is_err() {
            panic!("a same-recipient takeover must not be refused at capacity");
        }
        assert_eq!(
            registry.len(),
            1,
            "the slot set must stay bounded at one entry per recipient"
        );
    }
}

// ---------------------------------------------------------------------------
// Corpus scenario: errors-do-not-echo                                  (row QA23)
// ---------------------------------------------------------------------------

#[test]
fn a_refusal_never_renders_what_it_observed() {
    let credential = "hunter2-super-secret";
    let seat = "seat-victim-name";

    let cases: Vec<ApiRefusal> = vec![
        ApiRefusal::CredentialRejected {
            observed: Redacted::new(credential.to_owned()),
        },
        ApiRefusal::ClientIdentityNotIssuedHere {
            observed: Redacted::new(credential.to_owned()),
        },
        ApiRefusal::RecipientDisagreement {
            observed: Redacted::new(seat.to_owned()),
        },
        ApiRefusal::RecipientMalformed {
            observed: Redacted::new(seat.to_owned()),
        },
        ApiRefusal::DuplicateSubject {
            observed: Redacted::new(credential.to_owned()),
        },
        ApiRefusal::DtoTooLarge {
            max: 1_024,
            observed: Redacted::new(8_192_usize),
        },
    ];

    for refusal in &cases {
        let displayed = refusal.to_string();
        let debugged = format!("{refusal:?}");
        for rendering in [&displayed, &debugged] {
            assert!(
                !rendering.contains(credential),
                "a refusal must never echo credential material: {rendering}"
            );
            assert!(
                !rendering.contains(seat),
                "a refusal must never echo the rejected recipient text: {rendering}"
            );
            assert!(
                !rendering.contains("8192") && !rendering.contains("8_192"),
                "a refusal must never report the observed length — that is a \
                 probe oracle telling a caller how much was accepted before the \
                 refusal: {rendering}"
            );
        }
    }

    // The bound itself IS named: an error has to be actionable.
    let oversize = ApiRefusal::DtoTooLarge {
        max: 1_024,
        observed: Redacted::new(8_192_usize),
    };
    assert!(
        oversize.to_string().contains("1024"),
        "a refusal names the BOUND, so a caller can comply"
    );
}

#[test]
fn a_presented_credential_never_renders_its_material() {
    let credential = "SECRET-MATERIAL-hunter2";
    let claimed_client = "CLAIMED-CLIENT-secret-shaped";
    let Some(claimed) = AuthenticatedClientId::new(claimed_client) else {
        panic!("the secret-render fixture client id must be valid");
    };
    let presented = PresentedCredential::claiming(credential, claimed);

    let rendering = format!("{presented:?}");
    assert!(
        !rendering.contains(credential),
        "the raw credential holder must redact its material: {rendering}"
    );
    assert!(
        !rendering.contains(claimed_client),
        "the raw credential holder must not print caller-authored identity text: {rendering}"
    );
    assert!(
        rendering.contains("<redacted>"),
        "the redaction must be visible rather than silently omitting the fields: {rendering}"
    );
}

// ---------------------------------------------------------------------------
// A sealed-value witness that is not a corpus row: the principal cannot be
// separated from the directory that minted it.
// ---------------------------------------------------------------------------

#[test]
fn a_principal_from_one_directory_is_not_current_in_another() {
    let first_ids = CountingIds::new();
    let mut first = two_seat_boundary(&first_ids);
    let principal = principal_for(&first, "alpha");

    // A second, independently configured directory holding the SAME mapping.
    let second_ids = CountingIds::new();
    let second = two_seat_boundary(&second_ids);

    assert!(
        second.directory().is_current(&principal),
        "two directories at the same generation agree, which is what makes the \
         next assertion meaningful rather than incidental"
    );

    if let Err(refusal) = first.revoke(&subject("beta")) {
        panic!("revoking a live mapping must succeed: {refusal}");
    }
    assert!(
        !first.directory().is_current(&principal),
        "the directory that advanced no longer recognises the principal"
    );

    // `authorize` is the decision without the effect, so there is no counter to
    // assert here — an untouched counter would be a self-derived expectation
    // that cannot fail. The zero-invocation law is witnessed against `govern`,
    // which is the seam that can actually invoke.
    let decision = authorize(first.directory(), &principal, &request(), None);
    assert!(
        matches!(
            decision,
            AuthorizationDecision::Refused(ApiRefusal::StaleDirectoryGeneration)
        ),
        "a principal from the superseded directory must be refused as stale; got {decision:?}"
    );
}
