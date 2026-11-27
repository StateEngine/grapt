//! Service identity and replacement vocabulary (contract §"Service
//! replacement lifecycle"). Owner identity is
//! `{ServiceSlotId, ServiceInstanceId, EnvironmentReceipt}` — PID and URL are
//! instance attributes, never identity: a reused PID cannot become ownership.
//!
//! Registration versions additionally carry a semantic precedence projection,
//! which is what separates an upgrade from an ordinary replacement.

use core::cmp::Ordering;

crate::id::opaque_id! { ServiceSlotId }
crate::id::opaque_id! { ServiceInstanceId }
crate::id::opaque_id! { EnvironmentReceipt }

/// The registration version recorded in the discovery contract.
///
/// Deliberately NOT declared through `opaque_id!`. That macro derives
/// `PartialOrd`/`Ord`, and a derived order on this type is a lexicographic
/// order over the raw bytes — under which `"0.10.0" < "0.9.0"`. A caller
/// reaching for `<` to decide "is the candidate newer" would compile and
/// silently answer backwards. Semantic precedence is therefore reachable only
/// through [`ServiceRegistrationVersion::semantic_precedence`], which refuses
/// a value it cannot order rather than inventing one.
///
/// The stored value stays opaque and exact: the oracle writes an optional
/// free-form string (`client/src/effect/service.ts` declares
/// `version: Schema.optional(Schema.String)`) and decides compatibility by
/// exact equality. A constructor that demanded SemVer could not represent an
/// incumbent registration the oracle is entitled to write, so the controller
/// could no longer read that registration in order to report a version
/// mismatch at all. Validation belongs to the projection, not to construction.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ServiceRegistrationVersion(String);

impl ServiceRegistrationVersion {
    /// An empty registration version is not a version. Every other byte
    /// sequence the oracle can write is representable.
    pub fn new(raw: impl Into<String>) -> Option<Self> {
        let raw = raw.into();
        if raw.is_empty() {
            None
        } else {
            Some(Self(raw))
        }
    }

    /// The exact recorded bytes, unchanged.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether two registrations name the same version under the compatibility
    /// rule the oracle actually implements: exact equality of the recorded
    /// value.
    ///
    /// This is deliberately NOT semantic precedence. `1.0.0+a` and `1.0.0+b`
    /// have equal precedence but are not the same recorded version, and the
    /// incumbent-reuse decision keys on the recorded value.
    pub fn is_exactly(&self, other: &ServiceRegistrationVersion) -> bool {
        self.0 == other.0
    }

    /// Project this opaque value onto its SemVer precedence, or refuse with the
    /// exact reason it has none.
    pub fn semantic_precedence(&self) -> Result<SemanticVersion, VersionOrderError> {
        parse_semantic(&self.0)
    }

    /// Classify a candidate against the incumbent it proposes to replace.
    ///
    /// An upgrade is STRICTLY GREATER semantic precedence. Equal precedence is
    /// not an upgrade — including a build-metadata-only change, which SemVer
    /// excludes from precedence entirely — and a lower candidate is a
    /// distinguishable downgrade rather than a refusal.
    ///
    /// The refusal names which side failed: an unorderable incumbent and an
    /// unorderable candidate are different operational facts and must not
    /// collapse into one record.
    pub fn upgrade_disposition(
        incumbent: &ServiceRegistrationVersion,
        candidate: &ServiceRegistrationVersion,
    ) -> Result<UpgradeDisposition, UpgradeRefusal> {
        let incumbent = incumbent
            .semantic_precedence()
            .map_err(UpgradeRefusal::Incumbent)?;
        let candidate = candidate
            .semantic_precedence()
            .map_err(UpgradeRefusal::Candidate)?;
        Ok(match candidate.cmp(&incumbent) {
            Ordering::Greater => UpgradeDisposition::Upgrade,
            Ordering::Equal => UpgradeDisposition::SamePrecedence,
            Ordering::Less => UpgradeDisposition::Downgrade,
        })
    }
}

impl core::fmt::Display for ServiceRegistrationVersion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// How a candidate registration version stands against the incumbent's.
///
/// A closed vocabulary because the caller must not infer "newer" from a
/// boolean: `Replace` and `Upgrade` are distinct typed service transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgradeDisposition {
    /// Strictly greater precedence than the incumbent.
    Upgrade,
    /// Neither side takes precedence. Not an upgrade.
    SamePrecedence,
    /// Strictly lower precedence than the incumbent.
    Downgrade,
}

/// Which side of an upgrade comparison had no semantic precedence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradeRefusal {
    Incumbent(VersionOrderError),
    Candidate(VersionOrderError),
}

impl core::fmt::Display for UpgradeRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            UpgradeRefusal::Incumbent(reason) => {
                write!(f, "incumbent version has no precedence: {reason}")
            }
            UpgradeRefusal::Candidate(reason) => {
                write!(f, "candidate version has no precedence: {reason}")
            }
        }
    }
}

impl std::error::Error for UpgradeRefusal {}

/// Which of the three core fields a refusal is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreField {
    Major,
    Minor,
    Patch,
}

impl core::fmt::Display for CoreField {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            CoreField::Major => "major",
            CoreField::Minor => "minor",
            CoreField::Patch => "patch",
        })
    }
}

/// Why a recorded version has no semantic precedence.
///
/// Carried, never stringly re-derived: a caller that must report why it will
/// not treat a registration as an upgrade needs the exact reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionOrderError {
    /// The core is not exactly three dot-separated fields.
    MalformedCore { fields: usize },
    /// A core field was empty, carried a leading zero, or held a non-digit.
    InvalidCoreField { field: CoreField },
    /// A pre-release identifier was empty, non-alphanumeric, or a
    /// leading-zero number.
    InvalidPreRelease { at: usize },
    /// A build identifier was empty or non-alphanumeric. Build metadata does
    /// not affect precedence, but a malformed value is still not a SemVer
    /// version, so it is refused rather than quietly ignored.
    InvalidBuild { at: usize },
}

impl core::fmt::Display for VersionOrderError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            VersionOrderError::MalformedCore { fields } => write!(
                f,
                "version core needs exactly 3 dot-separated fields, found {fields}"
            ),
            VersionOrderError::InvalidCoreField { field } => {
                write!(f, "{field} field is not a leading-zero-free decimal number")
            }
            VersionOrderError::InvalidPreRelease { at } => {
                write!(f, "pre-release identifier {at} is not a valid identifier")
            }
            VersionOrderError::InvalidBuild { at } => {
                write!(f, "build identifier {at} is not a valid identifier")
            }
        }
    }
}

impl std::error::Error for VersionOrderError {}

/// A canonical SemVer decimal identifier.
///
/// The parser is the only constructor, so the stored bytes are non-empty ASCII
/// digits with no leading zero except for `0` itself. Keeping the canonical
/// decimal text removes every machine-integer ceiling while retaining a total,
/// allocation-bounded order: the projection never stores more numeric bytes
/// than the recorded version already contains.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CanonicalDecimal(Box<str>);

impl CanonicalDecimal {
    fn parse(raw: &str) -> Option<Self> {
        if raw.is_empty()
            || !raw.bytes().all(|byte| byte.is_ascii_digit())
            || (raw.len() > 1 && raw.starts_with('0'))
        {
            return None;
        }
        Some(Self(raw.into()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl Ord for CanonicalDecimal {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0
            .len()
            .cmp(&other.0.len())
            .then_with(|| self.0.cmp(&other.0))
    }
}

impl PartialOrd for CanonicalDecimal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// One pre-release identifier, classified at parse time because SemVer orders
/// the two classes differently and against each other.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PreReleaseIdentifier {
    Numeric(CanonicalDecimal),
    Alphanumeric(String),
}

impl Ord for PreReleaseIdentifier {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            // SemVer 2.0.0 §11: numeric identifiers always have LOWER
            // precedence than alphanumeric ones.
            (PreReleaseIdentifier::Numeric(_), PreReleaseIdentifier::Alphanumeric(_)) => {
                Ordering::Less
            }
            (PreReleaseIdentifier::Alphanumeric(_), PreReleaseIdentifier::Numeric(_)) => {
                Ordering::Greater
            }
            // Numeric identifiers use the canonical decimal order — which is
            // exactly where a raw byte comparison would put `10` below `9`.
            (PreReleaseIdentifier::Numeric(left), PreReleaseIdentifier::Numeric(right)) => {
                left.cmp(right)
            }
            // Alphanumeric identifiers are compared by ASCII sort order.
            (
                PreReleaseIdentifier::Alphanumeric(left),
                PreReleaseIdentifier::Alphanumeric(right),
            ) => left.cmp(right),
        }
    }
}

impl PartialOrd for PreReleaseIdentifier {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A validated SemVer 2.0.0 version, ordered by precedence.
///
/// Build metadata is validated during parsing and then DISCARDED: SemVer
/// excludes it from precedence, and a field that is never stored cannot leak
/// into a comparison later. `SemanticVersion` is therefore a precedence
/// projection, not a round-trip of the recorded value — the exact bytes stay
/// on [`ServiceRegistrationVersion`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticVersion {
    major: CanonicalDecimal,
    minor: CanonicalDecimal,
    patch: CanonicalDecimal,
    pre_release: Vec<PreReleaseIdentifier>,
}

impl SemanticVersion {
    /// The complete canonical major identifier, with no machine-integer cap.
    pub fn major(&self) -> &str {
        self.major.as_str()
    }

    /// The complete canonical minor identifier, with no machine-integer cap.
    pub fn minor(&self) -> &str {
        self.minor.as_str()
    }

    /// The complete canonical patch identifier, with no machine-integer cap.
    pub fn patch(&self) -> &str {
        self.patch.as_str()
    }

    /// Whether this version carries a pre-release, which lowers its precedence
    /// beneath the otherwise-equal release.
    pub fn is_pre_release(&self) -> bool {
        !self.pre_release.is_empty()
    }
}

impl Ord for SemanticVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then_with(|| self.minor.cmp(&other.minor))
            .then_with(|| self.patch.cmp(&other.patch))
            .then_with(|| compare_pre_release(&self.pre_release, &other.pre_release))
    }
}

impl PartialOrd for SemanticVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// SemVer 2.0.0 §11 pre-release precedence.
fn compare_pre_release(left: &[PreReleaseIdentifier], right: &[PreReleaseIdentifier]) -> Ordering {
    match (left.is_empty(), right.is_empty()) {
        (true, true) => Ordering::Equal,
        // A version WITH a pre-release has lower precedence than the
        // otherwise-equal version without one.
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => {
            for (left, right) in left.iter().zip(right.iter()) {
                let ordering = left.cmp(right);
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            // Every shared identifier is equal: the larger set of fields wins.
            left.len().cmp(&right.len())
        }
    }
}

fn parse_semantic(raw: &str) -> Result<SemanticVersion, VersionOrderError> {
    // SemVer 2.0.0 §10: build metadata is split off first and excluded from
    // precedence. It is validated and then dropped, never carried into
    // `SemanticVersion`.
    let (without_build, build) = match raw.split_once('+') {
        Some((head, build)) => (head, Some(build)),
        None => (raw, None),
    };
    if let Some(build) = build {
        validate_build(build)?;
    }

    // The core carries no `-`, so the first one opens the pre-release.
    let (core, pre_release) = match without_build.split_once('-') {
        Some((core, pre_release)) => (core, Some(pre_release)),
        None => (without_build, None),
    };

    let mut fields = core.split('.');
    let major = fields.next();
    let minor = fields.next();
    let patch = fields.next();
    let (major, minor, patch) = match (major, minor, patch, fields.next()) {
        (Some(major), Some(minor), Some(patch), None) => (major, minor, patch),
        _ => {
            return Err(VersionOrderError::MalformedCore {
                fields: core.split('.').count(),
            })
        }
    };

    let major = parse_core_field(major, CoreField::Major)?;
    let minor = parse_core_field(minor, CoreField::Minor)?;
    let patch = parse_core_field(patch, CoreField::Patch)?;

    let pre_release = match pre_release {
        Some(pre_release) => parse_pre_release(pre_release)?,
        None => Vec::new(),
    };

    Ok(SemanticVersion {
        major,
        minor,
        patch,
        pre_release,
    })
}

fn parse_core_field(raw: &str, field: CoreField) -> Result<CanonicalDecimal, VersionOrderError> {
    CanonicalDecimal::parse(raw).ok_or(VersionOrderError::InvalidCoreField { field })
}

fn parse_pre_release(raw: &str) -> Result<Vec<PreReleaseIdentifier>, VersionOrderError> {
    let mut identifiers = Vec::new();
    for (at, identifier) in raw.split('.').enumerate() {
        if !is_identifier(identifier) {
            return Err(VersionOrderError::InvalidPreRelease { at });
        }
        // The explicit non-empty term is intentionally retained even though
        // `is_identifier` already proves it. It keeps that admission guard
        // load-bearing: removing the guard must resurrect the empty identifier
        // as alphanumeric instead of falling into the decimal parser's second
        // refusal and letting the mutation survive for the wrong reason.
        if !identifier.is_empty() && identifier.bytes().all(|byte| byte.is_ascii_digit()) {
            let value = CanonicalDecimal::parse(identifier)
                .ok_or(VersionOrderError::InvalidPreRelease { at })?;
            identifiers.push(PreReleaseIdentifier::Numeric(value));
        } else {
            identifiers.push(PreReleaseIdentifier::Alphanumeric(identifier.to_owned()));
        }
    }
    Ok(identifiers)
}

fn validate_build(raw: &str) -> Result<(), VersionOrderError> {
    for (at, identifier) in raw.split('.').enumerate() {
        if !is_identifier(identifier) {
            return Err(VersionOrderError::InvalidBuild { at });
        }
    }
    Ok(())
}

fn is_identifier(raw: &str) -> bool {
    !raw.is_empty()
        && raw
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// Monotonic replacement generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ServiceGeneration(u64);

impl ServiceGeneration {
    pub const FIRST: ServiceGeneration = ServiceGeneration(0);

    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn value(self) -> u64 {
        self.0
    }

    /// Ownership transfer advances the generation; it never rewinds.
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// A typed service URL.
///
/// A newtype rather than a `String` because a URL is an instance *attribute*:
/// keeping it nominally distinct is what stops it drifting into identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Endpoint(String);

impl Endpoint {
    /// Reject an endpoint without a scheme separator: a bare host is not an
    /// endpoint.
    pub fn new(raw: impl Into<String>) -> Option<Self> {
        let raw = raw.into();
        if raw.is_empty() || !raw.contains("://") {
            return None;
        }
        Some(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Proof of who owns a registration slot.
///
/// Law: owner identity is `{ServiceSlotId, ServiceInstanceId,
/// EnvironmentReceipt}`. The PID and endpoint travel with the receipt as
/// attributes so a health probe can check them, but two receipts differing
/// only in PID are still different owners — a reused PID can never become
/// ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerReceipt {
    slot: ServiceSlotId,
    instance: ServiceInstanceId,
    environment: EnvironmentReceipt,
    generation: ServiceGeneration,
    pid: ProcessId,
    endpoint: Endpoint,
}

impl OwnerReceipt {
    pub fn new(
        slot: ServiceSlotId,
        instance: ServiceInstanceId,
        environment: EnvironmentReceipt,
        generation: ServiceGeneration,
        pid: ProcessId,
        endpoint: Endpoint,
    ) -> Self {
        Self {
            slot,
            instance,
            environment,
            generation,
            pid,
            endpoint,
        }
    }

    pub fn slot(&self) -> &ServiceSlotId {
        &self.slot
    }

    pub fn instance(&self) -> &ServiceInstanceId {
        &self.instance
    }

    pub fn environment(&self) -> &EnvironmentReceipt {
        &self.environment
    }

    pub fn generation(&self) -> ServiceGeneration {
        self.generation
    }

    pub fn pid(&self) -> ProcessId {
        self.pid
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Whether `other` names the same owner.
    ///
    /// Compares only the three identity coordinates. PID and endpoint are
    /// deliberately excluded: a restarted instance that reused a PID is not
    /// the previous owner, and the same owner behind a new ephemeral port
    /// still owns the slot.
    pub fn is_same_owner(&self, other: &OwnerReceipt) -> bool {
        self.slot == other.slot
            && self.instance == other.instance
            && self.environment == other.environment
    }
}

/// Non-zero OS process id — an attribute of an instance, never its identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProcessId(u32);

impl ProcessId {
    pub fn new(pid: u32) -> Option<Self> {
        if pid == 0 {
            None
        } else {
            Some(Self(pid))
        }
    }

    pub fn value(self) -> u32 {
        self.0
    }
}

/// The closed replacement outcome vocabulary. No caller infers success from a
/// boolean, and nobody prints a rollback message after the candidate became
/// current (`FailedAfterCommit` is not `RolledBack`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplacementOutcome {
    Reused,
    Replaced,
    /// Post-commit incumbent retire failure is success with warning.
    ReplacedRetirePending,
    /// Pre-commit candidate failure; incumbent remains owner.
    RolledBack,
    LostRace,
    /// Candidate failed after registration commit — it IS current.
    FailedAfterCommit,
}

#[cfg(test)]
mod tests;
