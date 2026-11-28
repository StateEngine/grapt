//! Capability identity, grant scope lattice and the consuming authority lease.
//!
//! Law (`CORRECT`, Agency/Continuity Amendment §8.1-§8.3 and survey rows
//! C1/C4/C5/C7): the earlier "closed capability name plus `String` scope" is
//! superseded. A governed effect is identified from its *resolved*
//! security-relevant coordinates, not from a name a model can author.
//!
//! Three separate laws live here:
//!
//! 1. [`CapabilityId`] is **derived**, never authored. It has no public
//!    constructor and no `FromStr`; the only route is
//!    [`CapabilityId::derive`] from a [`ResolvedCapability`]. This is what
//!    makes ownership probe #8 ("constructing a capability from an authored
//!    string") a compile error rather than a runtime check.
//! 2. [`GrantScope`] is a typed lattice, so a downgrade is expressible and
//!    checkable instead of being a string comparison.
//! 3. [`SessionGrant`] is process-local authority with no `Clone`, no
//!    `Display`/`ToString` and a redacted `Debug`. [`AuthorityLease`] is
//!    consumed by the one governed effect entry, so an effect cannot run
//!    twice on one authorization.

use crate::location::{LocationId, ProjectId};
use crate::path::PathIdentity;
use crate::session::SessionId;
use core::fmt;

/// The class of governed effect a capability authorizes.
///
/// Closed: adding a class breaks every exhaustive owner match, which is the
/// intended review pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EffectClass {
    /// Read a filesystem path.
    FilesystemRead,
    /// Create, modify or delete a filesystem path.
    FilesystemWrite,
    /// Spawn a process.
    ProcessSpawn,
    /// Open a network connection.
    NetworkEgress,
    /// Invoke a declared tool.
    ToolInvoke,
}

/// How an executable is identified for authorization.
///
/// Law C5: approval binds the exact program and argument vector. A later argv
/// rewrite is a different capability, not the approved one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProgramIdentity {
    program: PathIdentity,
    argv: Vec<String>,
    cwd: PathIdentity,
    /// Environment names the child may receive. Law C11: an allowlist, never
    /// an inherited environment with a denylist scrub.
    environment: Vec<String>,
}

impl ProgramIdentity {
    /// Build a program identity. `environment` is sorted and deduplicated so
    /// two orderings of the same allowlist derive the same capability.
    pub fn new(
        program: PathIdentity,
        argv: Vec<String>,
        cwd: PathIdentity,
        environment: Vec<String>,
    ) -> Self {
        let mut environment = environment;
        environment.sort();
        environment.dedup();
        Self {
            program,
            argv,
            cwd,
            environment,
        }
    }

    pub fn program(&self) -> &PathIdentity {
        &self.program
    }

    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    pub fn cwd(&self) -> &PathIdentity {
        &self.cwd
    }

    pub fn environment(&self) -> &[String] {
        &self.environment
    }
}

/// Resolved filesystem authority.
///
/// Law C6: protected control metadata is removed from writable authority
/// *before* the capability is derived, so a workspace write cannot
/// self-escalate through harness, VCS or agent configuration. `deny` is
/// therefore part of identity, not an afterthought applied at call time.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct FilesystemAuthority {
    read: Vec<PathIdentity>,
    write: Vec<PathIdentity>,
    deny: Vec<PathIdentity>,
    read_deny: Vec<PathIdentity>,
}

impl FilesystemAuthority {
    /// Build filesystem authority. Each root list is sorted and deduplicated
    /// so ordering cannot produce two identities for one authority.
    pub fn new(read: Vec<PathIdentity>, write: Vec<PathIdentity>, deny: Vec<PathIdentity>) -> Self {
        let mut this = Self {
            read,
            write,
            deny,
            read_deny: Vec::new(),
        };
        this.read.sort();
        this.read.dedup();
        this.write.sort();
        this.write.dedup();
        this.deny.sort();
        this.deny.dedup();
        this
    }

    /// Attach read-denied paths as a distinct authority domain from write-protected paths.
    pub fn with_read_deny(mut self, mut read_deny: Vec<PathIdentity>) -> Self {
        read_deny.sort();
        read_deny.dedup();
        self.read_deny = read_deny;
        self
    }

    pub fn read(&self) -> &[PathIdentity] {
        &self.read
    }

    pub fn write(&self) -> &[PathIdentity] {
        &self.write
    }

    pub fn deny(&self) -> &[PathIdentity] {
        &self.deny
    }

    pub fn read_deny(&self) -> &[PathIdentity] {
        &self.read_deny
    }
}

/// One authorized egress destination.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EgressDestination {
    scheme: String,
    host: String,
    port: u16,
}

impl EgressDestination {
    /// Reject an empty scheme or host: absence is not a wildcard.
    pub fn new(scheme: impl Into<String>, host: impl Into<String>, port: u16) -> Option<Self> {
        let scheme = scheme.into();
        let host = host.into();
        if scheme.is_empty() || host.is_empty() {
            return None;
        }
        Some(Self {
            scheme: scheme.to_ascii_lowercase(),
            host: host.to_ascii_lowercase(),
            port,
        })
    }

    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

/// Resolved network authority. An empty destination list authorizes nothing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct EgressAuthority {
    destinations: Vec<EgressDestination>,
}

impl EgressAuthority {
    pub fn new(destinations: Vec<EgressDestination>) -> Self {
        let mut destinations = destinations;
        destinations.sort();
        destinations.dedup();
        Self { destinations }
    }

    pub fn destinations(&self) -> &[EgressDestination] {
        &self.destinations
    }
}

/// The backend that actually enforced the capability, and how faithfully.
///
/// Law C1: capability identity names the *realized* enforcement. A capability
/// enforced only by harness bookkeeping is not the same authority as the same
/// coordinates enforced by an OS sandbox, and the receipt must be able to say
/// so.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RealizedEnforcement {
    /// Enforced only by in-process harness checks.
    HarnessOnly,
    /// Enforced by a named platform backend at full declared fidelity.
    PlatformSandbox { backend: String },
    /// A backend was used but could not represent every dimension.
    Degraded { backend: String, reason: String },
}

/// A resolved, multidimensional governed effect.
///
/// This is the authorization *subject*. [`CapabilityId::derive`] turns it into
/// an identity. Two effects with the same coordinates derive the same id;
/// changing any coordinate — including the realized enforcement — derives a
/// different one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResolvedCapability {
    class: EffectClass,
    program: Option<ProgramIdentity>,
    filesystem: FilesystemAuthority,
    egress: EgressAuthority,
    enforcement: RealizedEnforcement,
}

impl ResolvedCapability {
    pub fn new(
        class: EffectClass,
        program: Option<ProgramIdentity>,
        filesystem: FilesystemAuthority,
        egress: EgressAuthority,
        enforcement: RealizedEnforcement,
    ) -> Self {
        Self {
            class,
            program,
            filesystem,
            egress,
            enforcement,
        }
    }

    pub fn class(&self) -> EffectClass {
        self.class
    }

    pub fn program(&self) -> Option<&ProgramIdentity> {
        self.program.as_ref()
    }

    pub fn filesystem(&self) -> &FilesystemAuthority {
        &self.filesystem
    }

    pub fn egress(&self) -> &EgressAuthority {
        &self.egress
    }

    pub fn enforcement(&self) -> &RealizedEnforcement {
        &self.enforcement
    }

    /// Deterministic canonical encoding of every security-relevant coordinate.
    ///
    /// This crate has no dependencies, so identity is the canonical form
    /// itself rather than a digest of it. A digest representation may replace
    /// the encoding later without changing this law: the identity is derived
    /// from exact resolved coordinates and is never authored.
    fn canonical_form(&self) -> String {
        let mut out = String::new();
        out.push_str(match self.class {
            EffectClass::FilesystemRead => "fs.read",
            EffectClass::FilesystemWrite => "fs.write",
            EffectClass::ProcessSpawn => "proc.spawn",
            EffectClass::NetworkEgress => "net.egress",
            EffectClass::ToolInvoke => "tool.invoke",
        });
        out.push('|');
        match &self.program {
            None => out.push_str("prog:-"),
            Some(program) => {
                out.push_str("prog:");
                push_path(&mut out, program.program());
                out.push_str(";argv:");
                for arg in program.argv() {
                    push_escaped(&mut out, arg);
                    out.push(',');
                }
                out.push_str(";cwd:");
                push_path(&mut out, program.cwd());
                out.push_str(";env:");
                for name in program.environment() {
                    push_escaped(&mut out, name);
                    out.push(',');
                }
            }
        }
        out.push_str("|fs:");
        for (label, roots) in [
            ("r", self.filesystem.read()),
            ("w", self.filesystem.write()),
            ("d", self.filesystem.deny()),
        ] {
            out.push_str(label);
            out.push('=');
            for root in roots {
                push_path(&mut out, root);
                out.push(',');
            }
            out.push(';');
        }
        if !self.filesystem.read_deny().is_empty() {
            out.push_str("rd=");
            for root in self.filesystem.read_deny() {
                push_path(&mut out, root);
                out.push(',');
            }
            out.push(';');
        }
        out.push_str("|net:");
        for destination in self.egress.destinations() {
            push_escaped(&mut out, destination.scheme());
            out.push_str("://");
            push_escaped(&mut out, destination.host());
            out.push(':');
            out.push_str(&destination.port().to_string());
            out.push(',');
        }
        out.push_str("|enf:");
        match &self.enforcement {
            RealizedEnforcement::HarnessOnly => out.push_str("harness"),
            RealizedEnforcement::PlatformSandbox { backend } => {
                out.push_str("sandbox=");
                push_escaped(&mut out, backend);
            }
            RealizedEnforcement::Degraded { backend, reason } => {
                out.push_str("degraded=");
                push_escaped(&mut out, backend);
                out.push('/');
                push_escaped(&mut out, reason);
            }
        }
        out
    }
}

/// Escape the canonical-form separators so no two distinct coordinate sets can
/// encode to the same string.
fn push_escaped(out: &mut String, raw: &str) {
    for ch in raw.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '|' => out.push_str("\\p"),
            ';' => out.push_str("\\s"),
            ',' => out.push_str("\\c"),
            ':' => out.push_str("\\o"),
            '=' => out.push_str("\\e"),
            other => out.push(other),
        }
    }
}

fn push_path(out: &mut String, path: &PathIdentity) {
    match path.as_canonical() {
        Some(canonical) => push_escaped(out, canonical),
        None => out.push_str("\\u"),
    }
}

/// A derived capability identity for a governed EFFECT.
///
/// Deliberately has **no public constructor and no `FromStr`**. A model may
/// request a capability but cannot author the stored grant key or widen it
/// (law C5). Ownership probe #8 depends on this absence.
///
/// # This names an effect, and only an effect
///
/// Master §8.1 defines capability identity as "a resolved multidimensional
/// execution/effect". A model TARGET is not an effect: it is a route to call.
/// Carrying both in one nominal type was a category merge, and the merge was
/// what made a collision between them expressible at all — equality is the
/// authorization comparison here, so two canonical forms that could ever
/// coincide would compare equal across the boundary.
///
/// The model-target form therefore has its own type,
/// [`ModelTargetCapabilityId`], and the two canonical namespaces are proven
/// pairwise disjoint by `capability::tests` rather than left disjoint by the
/// accident that no [`EffectClass`] literal happens to spell `model-target`.
///
/// Workspace-restore authority has its own nominal identity,
/// [`CheckpointRestoreCapabilityId`]. A restore plan is not an effect class,
/// and equality between the two must be unnameable rather than merely unlikely.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CapabilityId(String);

impl CapabilityId {
    /// The only route to a capability identity for a governed effect.
    pub fn derive(resolved: &ResolvedCapability) -> Self {
        Self(resolved.canonical_form())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The authority identity of one exact workspace-restore plan.
///
/// Nominally distinct from both [`CapabilityId`] and
/// [`ModelTargetCapabilityId`]. A restore plan is a governed compound
/// operation, not one of the ordinary effect classes, so a lease for either
/// namespace cannot be substituted for the other even when a future encoding
/// defect made their bytes collide.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CheckpointRestoreCapabilityId(String);

impl CheckpointRestoreCapabilityId {
    /// The single producer is the settled-plan constructor in
    /// `crate::checkpoint`; callers can inspect the identity they must have
    /// authorized but cannot author it from a string.
    pub(crate) fn from_canonical(canonical: String) -> Self {
        Self(canonical)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A non-clone authorization carried through exactly one checkpoint restore.
///
/// This is separate from [`AuthorityLease`]: ordinary effect grants never
/// authorize a compound restore plan. The external checkpoint application
/// owner is the sole project caller of [`Self::issue_at_boundary`]; the
/// workspace architecture gate mechanically rejects any second call site.
#[derive(Debug)]
pub struct CheckpointRestoreAuthorityLease {
    capability: CheckpointRestoreCapabilityId,
}

impl CheckpointRestoreAuthorityLease {
    /// Cross-crate owner seam for `grapt-checkpoint` after its explicit
    /// authority port approves the exact plan. This is intentionally absent
    /// from the crate-root re-exports; project source gates keep the one
    /// application owner exact.
    #[doc(hidden)]
    pub fn issue_at_boundary(capability: CheckpointRestoreCapabilityId) -> Self {
        Self { capability }
    }

    pub fn capability(&self) -> &CheckpointRestoreCapabilityId {
        &self.capability
    }

    pub fn consume(self) -> CheckpointRestoreCapabilityId {
        self.capability
    }
}

impl fmt::Display for CapabilityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The capability identity derived from one exact resolved model target.
///
/// Nominally distinct from [`CapabilityId`], and the distinction is the point:
/// a model target names a route to call, not a governed effect, so the two
/// cannot be substituted for one another and cannot be compared. `PartialEq`
/// is the authorization comparison on both types; keeping them separate means
/// no effect capability can ever equal a routing capability, whatever either
/// canonical form comes to spell.
///
/// # Everything a derive would add is withheld for the same reasons
///
/// There is **no public constructor and no `FromStr`**: the sole producer is
/// `crate::model::ResolvedModelTarget::resolve`, reached only through the
/// registry, and the sole reader is that target's own crate-private accessor.
/// A caller who could author a canonical form could author a routing authority
/// for coordinates nobody vouched for.
///
/// `Display` is ABSENT, and that is a difference from [`CapabilityId`] rather
/// than an omission. The model-target canonical form is not a digest: it spells
/// `…|base=…|family=…|…` literally, and those are the two catalog facts
/// `crate::model` seals. A `Display` here would be the plaintext read path that
/// compile-fail probe `47d` exists to keep shut, reopened one impl lower down.
/// `as_str` is `pub(crate)` for the same reason — in-crate witnesses must read
/// the exact bytes to prove the encoding is injective, and nothing outside the
/// crate can obtain a value to read.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModelTargetCapabilityId(String);

impl ModelTargetCapabilityId {
    /// Crate-private route used by [`crate::model::ResolvedModelTarget`] to
    /// derive a capability from the exact resolved routing tuple.
    ///
    /// Kept `pub(crate)` so no caller outside this crate can supply a
    /// canonical form of their own choosing: derivation stays the only way to
    /// obtain an identity.
    pub(crate) fn from_canonical(canonical: String) -> Self {
        Self(canonical)
    }

    /// The exact canonical bytes. SEALED — see the type note.
    ///
    /// Test-only today, and marked so rather than made public. The sole
    /// producer of a value to read is
    /// [`ResolvedModelTarget::capability`](crate::model::ResolvedModelTarget),
    /// which is itself `pub(crate)` and carries the same attribute, so outside
    /// `cfg(test)` there is nothing in the crate that legitimately reads these
    /// bytes yet. Reaching for `pub` to satisfy the dead-code lint would open
    /// exactly the plaintext read path probe `47d` exists to keep shut.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// The typed grant-scope lattice (law C4).
///
/// Ordered from narrowest to widest. [`GrantScope::contains`] expresses the
/// downgrade invariant: a narrow grant never satisfies a request that needs a
/// wider one, and a wider grant covers a narrower request only within the same
/// dimension.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GrantScope {
    /// One effect, consumed immediately.
    Once,
    /// Every effect in one physical attempt.
    Attempt,
    /// Every effect in one Session.
    Session(SessionId),
    /// Every effect in one Location.
    Location(LocationId),
    /// Every effect in one Project.
    Project(ProjectId),
}

impl GrantScope {
    /// Lattice rank, narrowest first. Rank alone never authorizes: two grants
    /// of equal rank in different dimensions do not cover one another.
    pub fn rank(&self) -> u8 {
        match self {
            GrantScope::Once => 0,
            GrantScope::Attempt => 1,
            GrantScope::Session(_) => 2,
            GrantScope::Location(_) => 3,
            GrantScope::Project(_) => 4,
        }
    }

    /// Whether `self` covers `requested`.
    ///
    /// A dimensioned scope covers a request only when the dimension value
    /// matches exactly; widening across dimensions is never inferred.
    pub fn contains(&self, requested: &GrantScope) -> bool {
        match (self, requested) {
            (GrantScope::Once, GrantScope::Once) => true,
            (GrantScope::Once, _) => false,
            (GrantScope::Attempt, GrantScope::Once | GrantScope::Attempt) => true,
            (GrantScope::Attempt, _) => false,
            (GrantScope::Session(held), GrantScope::Session(want)) => held == want,
            (GrantScope::Session(_), GrantScope::Once | GrantScope::Attempt) => true,
            (GrantScope::Session(_), _) => false,
            (GrantScope::Location(held), GrantScope::Location(want)) => held == want,
            (GrantScope::Location(_), GrantScope::Once | GrantScope::Attempt) => true,
            (GrantScope::Location(_), _) => false,
            (GrantScope::Project(held), GrantScope::Project(want)) => held == want,
            (GrantScope::Project(_), GrantScope::Once | GrantScope::Attempt) => true,
            (GrantScope::Project(_), _) => false,
        }
    }
}

/// A persisted grant built by the authority from a derived capability.
///
/// Law C5 and amendment §8.2: "Persistent grants are built by the authority
/// from the resolved capability. A model may request a capability but cannot
/// author the stored grant key or widen it."
///
/// Construction is sealed. The earlier `CapabilityGrant::new(capability, scope)`
/// was public, and because `CapabilityId::derive` is public too, *any* caller
/// could resolve a capability of its choosing, wrap it in a grant, and then call
/// `lease()` as often as it liked — no permission decision anywhere in the
/// chain. The only route now is [`CapabilityGrant::from_live`], which reads both
/// coordinates out of an authenticated [`crate::authority::LivePermissionDecision`],
/// so the grant
/// can only describe an effect a principal actually authorized.
///
/// A grant is *persistent* authority and is intentionally reusable by the owner
/// that holds it: `lease` revalidates on every call. `GrantScope::Once` is
/// therefore not representable as a grant at all — see
/// [`AuthorityLease::once_from_live`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityGrant {
    capability: CapabilityId,
    scope: GrantScope,
}

impl CapabilityGrant {
    /// The only route to a persistent grant.
    ///
    /// Takes no independently replaceable coordinate: the capability and the
    /// scope both come from the decision, so a grant can never be assembled
    /// around a capability the answerer never saw, or widened past the scope
    /// they agreed to.
    ///
    /// Refuses `GrantScope::Once`. A once-off authorization is not persistent
    /// authority; letting it become a `CapabilityGrant` is precisely how "Once"
    /// would issue a second lease.
    pub fn from_live(decision: &crate::authority::LivePermissionDecision) -> Option<Self> {
        if matches!(decision.scope(), GrantScope::Once) {
            return None;
        }
        Some(Self {
            capability: decision.capability().clone(),
            scope: decision.scope().clone(),
        })
    }

    pub fn capability(&self) -> &CapabilityId {
        &self.capability
    }

    pub fn scope(&self) -> &GrantScope {
        &self.scope
    }

    /// Whether this grant authorizes `capability` at `requested` scope.
    pub fn authorizes(&self, capability: &CapabilityId, requested: &GrantScope) -> bool {
        &self.capability == capability && self.scope.contains(requested)
    }

    /// The one public route to an [`AuthorityLease`].
    ///
    /// Refuses unless this grant actually authorizes `capability` at
    /// `requested`. There is no other way to obtain a lease, so the governed
    /// effect entry cannot be reached without a grant that covers it.
    pub fn lease(
        &self,
        capability: &CapabilityId,
        requested: GrantScope,
    ) -> Option<AuthorityLease> {
        if !self.authorizes(capability, &requested) {
            return None;
        }
        Some(AuthorityLease::mint(capability.clone(), requested))
    }
}

/// Process-local Session authority produced by `PermissionReply::Session`.
///
/// Deliberate trait omissions (ownership probe #2): no `Clone`, no `Display`,
/// hence no `ToString`, and no serialization route. `Debug` is implemented by
/// hand and is redacted, so diagnostics can name the grant without exporting
/// it. A recorded Session approval is audit truth; it is re-evaluated after
/// process loss and is never reconstructed from a durable row.
pub struct SessionGrant {
    session: SessionId,
    capability: CapabilityId,
}

impl SessionGrant {
    /// The only route to process-local Session authority.
    ///
    /// Both coordinates are read out of the authenticated decision: the
    /// capability from the decision, and the Session from the decision's own
    /// `GrantScope::Session(..)`. Nothing is supplied alongside it.
    ///
    /// That closes two distinct defects in the removed
    /// `PermissionSettlement::session_grant`. It accepted a caller-chosen
    /// `SessionId` that was never compared with the settlement's scope, so a
    /// real approval for one Session could mint authority over another. And it
    /// hung off the durable audit receipt, every input to which is publicly
    /// authorable, so authority could be fabricated from invented data. Neither
    /// shape is expressible now: there is no public constructor for
    /// [`crate::authority::LivePermissionDecision`] at all.
    pub fn from_live(decision: &crate::authority::LivePermissionDecision) -> Option<SessionGrant> {
        if decision.decision() != crate::permission::PermissionReply::Session {
            return None;
        }
        // `mint` already refuses a `Session` reply that is not Session-scoped,
        // so this is coherent by construction rather than by convention.
        let session = decision.session()?;
        Some(Self {
            session: session.clone(),
            capability: decision.capability().clone(),
        })
    }

    pub fn session(&self) -> &SessionId {
        &self.session
    }

    pub fn capability(&self) -> &CapabilityId {
        &self.capability
    }
}

impl fmt::Debug for SessionGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionGrant(<process-local>)")
    }
}

/// A non-clone lease carried through one governed effect.
///
/// Law §8.3: every mutation or external effect crosses one governed entry
/// carrying a non-clone authority lease through effect completion.
/// [`AuthorityLease::consume`] takes `self`, so one authorization cannot run
/// two effects (ownership probe #8, third clause).
#[derive(Debug)]
pub struct AuthorityLease {
    capability: CapabilityId,
    scope: GrantScope,
}

impl AuthorityLease {
    /// Minting is crate-private on purpose.
    ///
    /// The only route from a persistent grant is [`CapabilityGrant::lease`],
    /// which refuses unless the grant actually authorizes the requested
    /// capability at the requested scope. An effect therefore cannot be run
    /// under a lease nobody granted.
    pub(crate) fn mint(capability: CapabilityId, scope: GrantScope) -> Self {
        Self { capability, scope }
    }

    /// The one route for a `Once` authorization.
    ///
    /// Consumes the decision by value, so a single authenticated `Once` answer
    /// yields exactly one lease, and that lease is itself consumed by
    /// [`AuthorityLease::consume`] at the governed effect entry. Two effects
    /// from one "Once" is unrepresentable end to end: the decision cannot be
    /// cloned, it cannot become a reusable [`CapabilityGrant`], and the lease
    /// cannot be cloned either.
    pub fn once_from_live(decision: crate::authority::LivePermissionDecision) -> Option<Self> {
        if !matches!(decision.scope(), GrantScope::Once) {
            return None;
        }
        Some(Self {
            capability: decision.capability().clone(),
            scope: GrantScope::Once,
        })
    }

    pub fn capability(&self) -> &CapabilityId {
        &self.capability
    }

    pub fn scope(&self) -> &GrantScope {
        &self.scope
    }

    /// Consume the lease at the one governed effect entry, yielding the
    /// capability the effect actually ran under.
    pub fn consume(self) -> CapabilityId {
        self.capability
    }
}

#[cfg(test)]
mod tests;
