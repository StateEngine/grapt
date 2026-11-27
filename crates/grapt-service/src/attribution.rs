//! R4D-2E — the retained engine's owner identity as a **declared mapping**, and
//! the scope a cross-engine comparison is allowed to have.
//!
//! # Why the retained owner is declared and never read
//!
//! D1's owner identity is `{ServiceSlotId, ServiceInstanceId, EnvironmentReceipt}`,
//! with PID and endpoint travelling as attributes. The retained product's own
//! notion of "same owner" is `{id, version, url, pid}` — it *includes* the two
//! coordinates D1 deliberately excludes. The two laws therefore disagree in both
//! directions:
//!
//! * a retained restart that reuses PID and URL and keeps `id` is the **same**
//!   owner retained-side, while D1 refuses it as a restart that kept its
//!   instance identity; and
//! * a retained restart onto a new PID is a **different** owner retained-side
//!   while remaining one owner under D1.
//!
//! So a retained-side owner receipt cannot be *read*. It is a mapping an
//! operator declares, with a named refusal when the declaration cannot be made —
//! and the sharpest reason it cannot be read is that the only engine-identity
//! correlation anywhere near the frozen wire is the retained client's `probe()`,
//! which rejects a health answer whose `pid` differs from the registration's.
//! That correlates on **PID**: the exact coordinate D1 forbids as identity.
//!
//! # The receipt is the recorder's only input
//!
//! [`RetainedOwnerMapping::declare`] takes a [`MappingReceipt`] and nothing else.
//! That is deliberate. A constructor taking the coordinates *and* a receipt
//! could produce a mapping that disagrees with the receipt it cites, and the
//! receipt would then be witnessing a value the caller also supplied — which
//! witnesses nothing. With the receipt as the sole input there is no second
//! source for the two to disagree about.
//!
//! # What this module deliberately does NOT do
//!
//! It performs no conversion. There is no function here that turns a retained
//! registration into an [`grapt_types::service::OwnerReceipt`], because which
//! concrete conversion is correct is not this lane's to rule — the ruling
//! authorised the **mapping and refusal seam**, not an unruled identity
//! conversion. What is authorable now is which sources are admissible for which
//! coordinate, and what happens when the required one is absent.

use crate::operator::OperatorRefusal;

/// A D1 owner-identity coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OwnerCoordinate {
    /// `ServiceSlotId`.
    Slot,
    /// `ServiceInstanceId`.
    Instance,
    /// `EnvironmentReceipt`.
    Environment,
}

impl OwnerCoordinate {
    pub fn label(self) -> &'static str {
        match self {
            OwnerCoordinate::Slot => "slot",
            OwnerCoordinate::Instance => "instance",
            OwnerCoordinate::Environment => "environment",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        OwnerCoordinate::ALL
            .into_iter()
            .find(|candidate| candidate.label() == label)
    }

    /// The one retained-side source admissible for this coordinate.
    ///
    /// Stated as a function of the coordinate rather than checked at the call
    /// site, so adding a coordinate or a source forces a reviewer to decide the
    /// pairing rather than letting a plausible one be inferred.
    pub fn admissible_source(self) -> RetainedSource {
        match self {
            OwnerCoordinate::Slot => RetainedSource::RegistrationFilePath,
            OwnerCoordinate::Instance => RetainedSource::RegistrationId,
            OwnerCoordinate::Environment => RetainedSource::EnvironmentSidecarDigest,
        }
    }

    pub const ALL: [OwnerCoordinate; 3] = [
        OwnerCoordinate::Slot,
        OwnerCoordinate::Instance,
        OwnerCoordinate::Environment,
    ];
}

/// A value the retained side actually publishes.
///
/// Closed, and it deliberately includes the three inadmissible ones. A source
/// that could not be named could not be refused, and "PID is not identity" has
/// to be a law something can violate before it is a law at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RetainedSource {
    /// The registration file's absolute path. Sound as a slot **only** when the
    /// run pins an isolated state home: four registration files matching the
    /// frozen discovery regex were observed in one state home on this host, and
    /// discovery takes the first that probes, in `readdir` order.
    RegistrationFilePath,
    /// The registration's `id` field. The only instance-identity candidate that
    /// exists — and it is optional in the schema, which is why its absence has
    /// its own refusal.
    RegistrationId,
    /// The `*.environment.json` sidecar's `digest`: a sha256 over the complete
    /// sorted environment map, keys and values.
    EnvironmentSidecarDigest,
    /// The registration's `pid`. **Inadmissible as identity** — an attribute
    /// under D1, and a reused PID must never inherit ownership.
    ProcessIdentifier,
    /// The registration's `url`. **Inadmissible as identity** — an attribute
    /// under D1; the same owner keeps its slot across a new ephemeral port.
    Endpoint,
    /// The registration's `version`. **Inadmissible as identity** — two
    /// independent engines can report the same version string, and one engine
    /// changes it across an upgrade without changing owner.
    Version,
}

impl RetainedSource {
    pub fn label(self) -> &'static str {
        match self {
            RetainedSource::RegistrationFilePath => "registration-file-path",
            RetainedSource::RegistrationId => "registration-id",
            RetainedSource::EnvironmentSidecarDigest => "environment-sidecar-digest",
            RetainedSource::ProcessIdentifier => "process-identifier",
            RetainedSource::Endpoint => "endpoint",
            RetainedSource::Version => "version",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        RetainedSource::ALL
            .into_iter()
            .find(|candidate| candidate.label() == label)
    }

    /// Whether D1 treats this value as an identity coordinate at all.
    ///
    /// The three that return `false` are exactly the three the retained
    /// product's own `sameOwner` treats as identity. That overlap is the whole
    /// disagreement between the two laws, expressed as a predicate.
    pub fn is_identity_bearing(self) -> bool {
        match self {
            RetainedSource::RegistrationFilePath
            | RetainedSource::RegistrationId
            | RetainedSource::EnvironmentSidecarDigest => true,
            RetainedSource::ProcessIdentifier
            | RetainedSource::Endpoint
            | RetainedSource::Version => false,
        }
    }

    pub const ALL: [RetainedSource; 6] = [
        RetainedSource::RegistrationFilePath,
        RetainedSource::RegistrationId,
        RetainedSource::EnvironmentSidecarDigest,
        RetainedSource::ProcessIdentifier,
        RetainedSource::Endpoint,
        RetainedSource::Version,
    ];
}

/// Whether a mapping was declared by an operator or claimed to be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MappingBasis {
    /// An operator declared the mapping, taking responsibility for it. The only
    /// admissible basis.
    DeclaredByOperator,
    /// The mapping was read off the wire. **Inadmissible**: the frozen wire
    /// carries no engine identity at all, and the one correlation near it
    /// correlates on PID.
    ReadFromWire,
}

impl MappingBasis {
    pub fn label(self) -> &'static str {
        match self {
            MappingBasis::DeclaredByOperator => "declared-by-operator",
            MappingBasis::ReadFromWire => "read-from-wire",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "declared-by-operator" => Some(MappingBasis::DeclaredByOperator),
            "read-from-wire" => Some(MappingBasis::ReadFromWire),
            _ => None,
        }
    }
}

/// An operator's declaration of how retained values map onto D1 coordinates.
///
/// This is the sole input to [`RetainedOwnerMapping::declare`] — see the module
/// note on why it is sole rather than one of several.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingReceipt {
    receipt_id: String,
    slot_source: RetainedSource,
    instance_source: RetainedSource,
    environment_source: RetainedSource,
    registration_carries_id: bool,
    isolated_state_home_pinned: bool,
    basis: MappingBasis,
}

impl MappingReceipt {
    /// Record a declaration. Total: this is the operator's statement, and the
    /// laws that judge it belong to [`RetainedOwnerMapping::declare`].
    ///
    /// Keeping the recording total and the judging separate matters: a receipt
    /// that refused to record an inadmissible declaration could never be used to
    /// witness the refusal of one.
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        receipt_id: impl Into<String>,
        slot_source: RetainedSource,
        instance_source: RetainedSource,
        environment_source: RetainedSource,
        registration_carries_id: bool,
        isolated_state_home_pinned: bool,
        basis: MappingBasis,
    ) -> Self {
        Self {
            receipt_id: receipt_id.into(),
            slot_source,
            instance_source,
            environment_source,
            registration_carries_id,
            isolated_state_home_pinned,
            basis,
        }
    }

    pub fn receipt_id(&self) -> &str {
        &self.receipt_id
    }

    pub fn slot_source(&self) -> RetainedSource {
        self.slot_source
    }

    pub fn instance_source(&self) -> RetainedSource {
        self.instance_source
    }

    pub fn environment_source(&self) -> RetainedSource {
        self.environment_source
    }

    pub fn registration_carries_id(&self) -> bool {
        self.registration_carries_id
    }

    pub fn isolated_state_home_pinned(&self) -> bool {
        self.isolated_state_home_pinned
    }

    pub fn basis(&self) -> MappingBasis {
        self.basis
    }

    /// The declared source for one coordinate.
    fn source_for(&self, coordinate: OwnerCoordinate) -> RetainedSource {
        match coordinate {
            OwnerCoordinate::Slot => self.slot_source,
            OwnerCoordinate::Instance => self.instance_source,
            OwnerCoordinate::Environment => self.environment_source,
        }
    }
}

/// A retained-side owner mapping the contract admits.
///
/// Holding one is the claim that an operator declared a mapping this contract
/// considers sound — never that any retained engine was observed, and never that
/// a conversion was performed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedOwnerMapping {
    receipt: MappingReceipt,
}

impl RetainedOwnerMapping {
    /// Admit a declared mapping, or refuse it.
    ///
    /// The laws, in refusal order, and why that order:
    ///
    /// 1. **A receipt with no id names nothing.** Refused first because every
    ///    later refusal quotes the receipt.
    /// 2. **The basis must be a declaration.** A mapping claiming to be read off
    ///    the wire is refused before its coordinates are examined: if it was
    ///    read, *which* values it read is beside the point.
    /// 3. **No attribute may be offered as identity.** PID, endpoint and version
    ///    each get their own refusal rather than sharing one, because they are
    ///    three separate confusions and a battery that scored them through one
    ///    record would witness one law three times.
    /// 4. **Each coordinate takes its own admissible source.** This catches the
    ///    sound-but-crossed declaration — an environment digest offered as the
    ///    slot — which no per-source check can see, since every source involved
    ///    is identity-bearing.
    /// 5. **The instance identity must actually be present.** `id` is optional
    ///    in the retained schema. When it is absent there is no instance
    ///    identity, and the run stops rather than synthesizing one.
    /// 6. **The slot must be unambiguous.** A registration file path is a sound
    ///    slot only when the run pinned an isolated state home; without it, four
    ///    observed candidates share one notional slot and `readdir` order picks
    ///    the winner.
    pub fn declare(receipt: MappingReceipt) -> Result<Self, OperatorRefusal> {
        if receipt.receipt_id().trim().is_empty() {
            return Err(OperatorRefusal::MappingReceiptUnnamed);
        }
        if receipt.basis() == MappingBasis::ReadFromWire {
            return Err(OperatorRefusal::MappingReadFromWire);
        }

        for coordinate in OwnerCoordinate::ALL {
            let source = receipt.source_for(coordinate);
            if !source.is_identity_bearing() {
                return Err(match source {
                    RetainedSource::ProcessIdentifier => {
                        OperatorRefusal::PidOfferedAsIdentity { coordinate }
                    }
                    RetainedSource::Endpoint => {
                        OperatorRefusal::EndpointOfferedAsIdentity { coordinate }
                    }
                    RetainedSource::Version => {
                        OperatorRefusal::VersionOfferedAsIdentity { coordinate }
                    }
                    // Unreachable while `is_identity_bearing` and this match
                    // agree; written as an explicit arm rather than a catch-all
                    // so that adding a seventh source breaks compilation here
                    // instead of silently falling into a wrong refusal.
                    RetainedSource::RegistrationFilePath
                    | RetainedSource::RegistrationId
                    | RetainedSource::EnvironmentSidecarDigest => {
                        OperatorRefusal::CoordinateSourceMismatch {
                            coordinate,
                            offered: source,
                        }
                    }
                });
            }
            if source != coordinate.admissible_source() {
                return Err(OperatorRefusal::CoordinateSourceMismatch {
                    coordinate,
                    offered: source,
                });
            }
        }

        if !receipt.registration_carries_id() {
            return Err(OperatorRefusal::RetainedInstanceIdentityAbsent);
        }
        if !receipt.isolated_state_home_pinned() {
            return Err(OperatorRefusal::RetainedSlotAmbiguous);
        }

        Ok(Self { receipt })
    }

    pub fn receipt(&self) -> &MappingReceipt {
        &self.receipt
    }
}

/// Proof that both engines read the same session store.
///
/// # Why this has no public constructor
///
/// Comparing session bodies is only meaningful if both engines are reading the
/// same data. Nobody has proven that, and the ruling scoped comparison to shape
/// and status "unless a future lane proves common-store equivalence". A type
/// this crate could mint would let the automation supply its own proof of the
/// premise it is meant to be waiting for.
///
/// So the field is private and the constructor is `pub(crate)`, exactly as
/// [`crate::AttendedWindowReceipt`] is, and for the same reason. The witness for
/// the proven branch lives in `src/` because `tests/` cannot reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommonStoreProof {
    lane: String,
}

impl CommonStoreProof {
    /// The lane that proved store equivalence.
    pub fn lane(&self) -> &str {
        &self.lane
    }

    /// Mint a proof for an in-crate witness only.
    ///
    /// `pub(crate)` **and** `#[cfg(test)]`, for the same reason
    /// [`crate::AttendedWindowReceipt`]'s constructor is: in a non-test build
    /// this type has no constructor, so a body-content comparison is
    /// unconstructible rather than merely refused. The lane that proves store
    /// equivalence opens the seam by editing this file.
    #[cfg(test)]
    pub(crate) fn sealed_for_witness(lane: impl Into<String>) -> Self {
        Self { lane: lane.into() }
    }
}

/// How much of a response two engines may be compared on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ComparisonScope {
    /// Status code, header names, `additionalProperties` faithfulness, cursor
    /// presence and body **digest**. Never body content.
    ShapeAndStatusOnly,
    /// Response body content. Admissible only behind a [`CommonStoreProof`].
    BodyContent,
}

impl ComparisonScope {
    pub fn label(self) -> &'static str {
        match self {
            ComparisonScope::ShapeAndStatusOnly => "shape-and-status-only",
            ComparisonScope::BodyContent => "body-content",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "shape-and-status-only" => Some(ComparisonScope::ShapeAndStatusOnly),
            "body-content" => Some(ComparisonScope::BodyContent),
            _ => None,
        }
    }

    /// Admit a comparison scope, or refuse it.
    ///
    /// `SessionsResponse` is `{data: Session.Info[], cursor: {...}}` and its
    /// content is workspace state. Two engines reading different data roots
    /// differ in every row, so a body comparison without a store-equivalence
    /// proof would record a **data** difference as an **engine** difference —
    /// the false-parity move in its other direction.
    pub fn admit(self, proof: Option<&CommonStoreProof>) -> Result<Self, OperatorRefusal> {
        match (self, proof) {
            (ComparisonScope::ShapeAndStatusOnly, _) => Ok(self),
            (ComparisonScope::BodyContent, Some(_)) => Ok(self),
            (ComparisonScope::BodyContent, None) => {
                Err(OperatorRefusal::BodyContentComparisonUnproven)
            }
        }
    }
}

/// One operation's observed response, recorded as shape and status.
///
/// # Why there is no body field
///
/// The ruling says never compare or record session contents as parity. A
/// refusal would leave the body representable and rely on a check; omitting the
/// field entirely makes recording one **unrepresentable**, which is the stronger
/// form. The digest is retained because it compares two responses without
/// carrying either of them.
///
/// [`ComparisonScope::admit`] still exists alongside this, because the two
/// guard different things: this type stops a body being *stored*, and the scope
/// stops a body comparison being *declared*. A plan that declared a body
/// comparison and then stored only digests would be claiming a comparison it did
/// not make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShapeObservation {
    operation_id: String,
    status: u16,
    header_names: Vec<String>,
    body_digest: String,
    body_bytes: u64,
}

impl ShapeObservation {
    /// Record an observation, or refuse it.
    ///
    /// A digest is required even for an empty body: an observation with no
    /// digest records that a request happened and nothing about what came back,
    /// and two such observations would compare equal for the wrong reason.
    pub fn record(
        operation_id: impl Into<String>,
        status: u16,
        header_names: Vec<String>,
        body_digest: impl Into<String>,
        body_bytes: u64,
    ) -> Result<Self, OperatorRefusal> {
        let operation_id = operation_id.into();
        let body_digest = body_digest.into();
        if body_digest.trim().is_empty() {
            return Err(OperatorRefusal::ObservationDigestAbsent { operation_id });
        }
        Ok(Self {
            operation_id,
            status,
            header_names,
            body_digest,
            body_bytes,
        })
    }

    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn header_names(&self) -> &[String] {
        &self.header_names
    }

    pub fn body_digest(&self) -> &str {
        &self.body_digest
    }

    pub fn body_bytes(&self) -> u64 {
        self.body_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sound_receipt() -> MappingReceipt {
        MappingReceipt::record(
            "mapping-r4d2e-01",
            RetainedSource::RegistrationFilePath,
            RetainedSource::RegistrationId,
            RetainedSource::EnvironmentSidecarDigest,
            true,
            true,
            MappingBasis::DeclaredByOperator,
        )
    }

    #[test]
    fn a_sound_declaration_is_admitted_and_keeps_its_receipt() {
        let mapping = match RetainedOwnerMapping::declare(sound_receipt()) {
            Ok(mapping) => mapping,
            Err(refusal) => panic!("the sound control must be admitted, got {refusal}"),
        };
        assert_eq!(
            mapping.receipt().receipt_id(),
            "mapping-r4d2e-01",
            "the mapping carries the receipt it was declared from and nothing else"
        );
    }

    #[test]
    fn every_retained_source_label_round_trips() {
        for source in RetainedSource::ALL {
            assert_eq!(RetainedSource::from_label(source.label()), Some(source));
        }
    }

    #[test]
    fn every_owner_coordinate_label_round_trips() {
        for coordinate in OwnerCoordinate::ALL {
            assert_eq!(
                OwnerCoordinate::from_label(coordinate.label()),
                Some(coordinate)
            );
        }
    }

    #[test]
    fn exactly_the_three_retained_sameowner_coordinates_are_not_identity_bearing() {
        let attributes: Vec<RetainedSource> = RetainedSource::ALL
            .into_iter()
            .filter(|source| !source.is_identity_bearing())
            .collect();
        assert_eq!(
            attributes,
            vec![
                RetainedSource::ProcessIdentifier,
                RetainedSource::Endpoint,
                RetainedSource::Version
            ],
            "these three are exactly what the retained sameOwner treats as identity and D1 does not"
        );
    }

    /// The sealed branch, exercised from `src/` because `tests/` cannot reach it.
    #[test]
    fn a_body_comparison_is_admitted_only_behind_a_store_equivalence_proof() {
        assert_eq!(
            ComparisonScope::BodyContent.admit(None),
            Err(OperatorRefusal::BodyContentComparisonUnproven),
            "without a proof, a body difference is a data difference wearing an engine's name"
        );
        let proof = CommonStoreProof::sealed_for_witness("a-future-common-store-lane");
        assert_eq!(
            ComparisonScope::BodyContent.admit(Some(&proof)),
            Ok(ComparisonScope::BodyContent),
            "the proven branch must admit, or the seam could never open"
        );
        assert_eq!(proof.lane(), "a-future-common-store-lane");
        assert_eq!(
            ComparisonScope::ShapeAndStatusOnly.admit(None),
            Ok(ComparisonScope::ShapeAndStatusOnly),
            "shape and status need no proof; that is why they are the default scope"
        );
    }
}
