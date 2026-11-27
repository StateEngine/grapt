//! The per-action authorization seam.
//!
//! Authentication and authorization are different questions and this module
//! exists so that they cannot be confused: an authentic principal reaches here
//! and can still be refused.
//!
//! ## What this seam consumes, and what it will never mint
//!
//! Ratified F4 (master spec L2991) requires that send/wake/interrupt name a
//! capability covering the allowed recipients and actions. That vocabulary
//! already exists, in `grapt-types::capability`, and ruling R4HA-04 is explicit
//! that A consumes it and must not fork it: no second grant type, no second
//! scope enum, no second allowed-recipients list. A second carrier of one law is
//! not extra safety — it is the one an endpoint lane accidentally trusts.
//!
//! So this seam takes a [`SeatCapabilityProof`], which is a borrow of a real
//! [`CapabilityGrant`] plus the recipient it was issued for. That borrow is the
//! seal, and it is a strong one: `CapabilityGrant::from_live` is the only route
//! to a grant, it requires a `LivePermissionDecision`, and that type's mint is
//! `pub(crate)` inside `grapt-types`. **No crate in this workspace can mint one
//! today.**
//!
//! The consequence is deliberate and is what R4HA-04 rules: production A can
//! authenticate and refuse, and an allow requires a proof supplied through the
//! later composition seam. A fabricates no positive authority — not through a
//! public trait a dependent could implement, not through a test hook, and not
//! through a "for now" constructor.
//!
//! ## The order of the checks is the law
//!
//! Currency is checked before the proof. A revoked principal must be refused as
//! *stale*, not as *unproven*, because the two are different facts and a
//! caller that can tell them apart learns whether its capability would
//! otherwise have sufficed. It also means a stale principal is refused even
//! when it presents a perfectly valid proof.

use grapt_types::{CapabilityGrant, CapabilityId, GrantScope, SeatRecipientId};

use crate::auth::AuthenticatedSeatPrincipal;
use crate::directory::SeatPrincipalDirectory;
use crate::error::{ApiRefusal, Redacted};

/// A borrow of real F4 authority, bound to the recipient it was issued for.
///
/// Holding one requires holding a [`CapabilityGrant`], which is unmintable
/// outside `grapt-types`. That is why this is a plain public constructor and
/// still not a bypass: the unforgeable part is the borrow, not the wrapper.
#[derive(Debug, Clone, Copy)]
pub struct SeatCapabilityProof<'a> {
    grant: &'a CapabilityGrant,
    recipient: &'a SeatRecipientId,
}

impl<'a> SeatCapabilityProof<'a> {
    pub fn new(grant: &'a CapabilityGrant, recipient: &'a SeatRecipientId) -> Self {
        Self { grant, recipient }
    }

    /// The recipient this proof was issued for.
    pub fn recipient(&self) -> &SeatRecipientId {
        self.recipient
    }

    /// Whether the underlying grant authorizes exactly this capability at
    /// exactly this scope. Delegated to `CapabilityGrant::authorizes` rather
    /// than re-derived, so the lattice law has one owner.
    fn authorizes(&self, capability: &CapabilityId, scope: &GrantScope) -> bool {
        self.grant.authorizes(capability, scope)
    }
}

/// What a request is asking to do, in F4's own coordinates.
///
/// The capability and scope are F4's; `dto_recipient` is the recipient a route's
/// DTO legitimately carried, if any. It is here to be REFUSED against, never to
/// select: [`SeatPrincipalDirectory::resolve`] has already chosen the recipient
/// and this field cannot reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationRequest {
    capability: CapabilityId,
    scope: GrantScope,
    dto_recipient: Option<SeatRecipientId>,
}

impl AuthorizationRequest {
    /// A request on a route whose DTO carries no recipient at all.
    pub fn new(capability: CapabilityId, scope: GrantScope) -> Self {
        Self {
            capability,
            scope,
            dto_recipient: None,
        }
    }

    /// A request on a route whose DTO legitimately carries a recipient field.
    ///
    /// Supplying one does not widen anything. It can only cause a refusal:
    /// agreement grants nothing and is recorded nowhere, and disagreement is
    /// refused before any application call.
    pub fn with_dto_recipient(mut self, recipient: SeatRecipientId) -> Self {
        self.dto_recipient = Some(recipient);
        self
    }

    pub fn capability(&self) -> &CapabilityId {
        &self.capability
    }

    pub fn scope(&self) -> &GrantScope {
        &self.scope
    }

    pub fn dto_recipient(&self) -> Option<&SeatRecipientId> {
        self.dto_recipient.as_ref()
    }
}

/// The outcome of one authorization.
///
/// Not a `bool`, and not `Result<(), _>` with a stringly reason: the refusal is
/// a closed typed value so a caller can distinguish "stale" from "unproven"
/// internally while rendering neither outward.
///
/// No `PartialEq`/`Eq`: it carries an [`ApiRefusal`], and a derived equality
/// here would reconstitute exactly the offline guess oracle R1 finding H2
/// removed from the refusal itself. Callers match on the variant, which is what
/// the vocabulary is closed for.
#[derive(Debug, Clone)]
pub enum AuthorizationDecision {
    /// The action is authorized by the supplied proof.
    Allowed,
    /// The action is refused, for exactly this reason.
    Refused(ApiRefusal),
}

impl AuthorizationDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, AuthorizationDecision::Allowed)
    }

    /// The refusal, if this is one.
    pub fn refusal(&self) -> Option<&ApiRefusal> {
        match self {
            AuthorizationDecision::Allowed => None,
            AuthorizationDecision::Refused(refusal) => Some(refusal),
        }
    }
}

/// The application this boundary guards.
///
/// A owns no application, so this is the seam rather than the thing: one
/// method, no return value, and the only property the boundary asserts about it
/// is HOW MANY TIMES it was invoked. Refuse-before-effect means zero.
pub trait SeatApplication {
    /// Invoke the guarded action.
    ///
    /// No principal parameter: by the time this is reached the principal has
    /// already decided the outcome, and the only property the boundary asserts
    /// is the invocation COUNT. Passing the principal through would also make
    /// the "never invoked" law unwitnessable, because an edit that invoked
    /// early would not compile for want of a principal — and a mutation that
    /// cannot compile witnesses nothing.
    fn invoke(&mut self, request: &AuthorizationRequest);
}

/// Decide one action for one principal.
///
/// The checks, in order, and the order is stated in the module note:
///
/// 1. **currency** — the principal's directory generation must EQUAL the live
///    one;
/// 2. **recipient agreement** — a DTO-supplied recipient that disagrees with
///    the server-derived one is refused;
/// 3. **a proof must be present** — absence is a refusal, never a default
///    allow;
/// 4. **the proof must be bound to this principal's recipient**, so a proof
///    issued for seat B cannot authorize an action for seat A;
/// 5. **the proof must authorize this capability at this scope**, decided by
///    `CapabilityGrant::authorizes`.
pub fn authorize(
    directory: &SeatPrincipalDirectory,
    principal: &AuthenticatedSeatPrincipal,
    request: &AuthorizationRequest,
    proof: Option<SeatCapabilityProof<'_>>,
) -> AuthorizationDecision {
    if !directory.is_current(principal) {
        return AuthorizationDecision::Refused(ApiRefusal::StaleDirectoryGeneration);
    }
    if let Some(named) = request.dto_recipient() {
        if named != principal.recipient() {
            return AuthorizationDecision::Refused(ApiRefusal::RecipientDisagreement {
                observed: Redacted::new(named.as_str().to_owned()),
            });
        }
    }
    let Some(proof) = proof else {
        return AuthorizationDecision::Refused(ApiRefusal::NoCapabilityProof);
    };
    if proof.recipient() != principal.recipient() {
        return AuthorizationDecision::Refused(ApiRefusal::NoCapabilityProof);
    }
    if !proof.authorizes(request.capability(), request.scope()) {
        return AuthorizationDecision::Refused(ApiRefusal::CapabilityProofDoesNotAuthorize);
    }
    AuthorizationDecision::Allowed
}

/// Authorize, and invoke the application only if authorized.
///
/// The single seam through which a guarded action may run. A refusal performs
/// **zero** invocations — the refusal happens before the call, not around it —
/// and an allow performs exactly one.
pub fn govern(
    directory: &SeatPrincipalDirectory,
    principal: &AuthenticatedSeatPrincipal,
    request: &AuthorizationRequest,
    proof: Option<SeatCapabilityProof<'_>>,
    application: &mut dyn SeatApplication,
) -> AuthorizationDecision {
    let decision = authorize(directory, principal, request, proof);
    if decision.is_allowed() {
        application.invoke(request);
    }
    decision
}

/// The single production path from a presented credential to the application.
///
/// Authenticate, then govern. Composing the two here rather than leaving it to
/// each caller is what makes "an unauthenticated request never reaches the
/// application" a property of this crate instead of an instruction to whoever
/// wires it up next: there is no ordering for a caller to get wrong, because
/// the caller does not do the ordering.
///
/// A failure to authenticate returns `Err` and performs **zero** invocations.
pub fn authenticate_and_govern(
    boundary: &crate::auth::SeatTrustBoundary,
    presented: &crate::auth::PresentedCredential,
    request: &AuthorizationRequest,
    proof: Option<SeatCapabilityProof<'_>>,
    application: &mut dyn SeatApplication,
) -> Result<AuthorizationDecision, ApiRefusal> {
    let principal = boundary.authenticate(presented)?;
    Ok(govern(
        boundary.directory(),
        &principal,
        request,
        proof,
        application,
    ))
}
