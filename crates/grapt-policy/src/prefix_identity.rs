//! Keyed authority for exact prepared-prefix identity and invalidation.

use crate::Secret;
use grapt_types::{
    AdapterVersion, CommittedPrefixId, ContextEpoch, ContextPrefixReceipt,
    ContextPrefixReceiptParts, ContributionId, PolicyVersion, PrefixComponentDiagnostic,
    PrefixComponentId, PrefixComponentOrdinal, PrefixComponentReceipt, PrefixIdentityBounds,
    PrefixInvalidation, PrefixKeyRealm, PrefixKeyVersion, PrefixReceiptError, ProtocolVersion,
    ProviderCacheNamespace,
};
use sha2::Sha256;
use thiserror::Error;

const WHOLE_DOMAIN: &[u8] = b"grapt-policy/prepared-prefix/whole/v2";
const COMPONENT_DOMAIN: &[u8] = b"grapt-policy/prepared-prefix/component/v2";

/// One exact prepared contribution. Identity, byte boundaries, and ordering
/// are all included in the keyed receipt.
#[derive(Debug, Clone, Copy)]
pub struct PreparedPrefixPart<'a> {
    pub contribution: &'a ContributionId,
    pub bytes: &'a [u8],
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PrefixIdentityError {
    #[error("a prepared prefix must contain at least one component")]
    Empty,
    #[error("prefix has {offered} components; maximum is {limit}")]
    TooManyComponents { limit: u16, offered: usize },
    #[error("prefix component {ordinal:?} has {offered} bytes; maximum is {limit}")]
    ComponentTooLarge {
        ordinal: PrefixComponentOrdinal,
        limit: u32,
        offered: usize,
    },
    #[error("prefix total byte count overflowed at {ordinal:?}")]
    TotalBytesOverflow { ordinal: PrefixComponentOrdinal },
    #[error("prefix has {offered} bytes; maximum is {limit}")]
    TotalTooLarge { limit: u64, offered: u64 },
    #[error(transparent)]
    InvalidReceipt(#[from] PrefixReceiptError),
}

/// The result of comparing one validated committed receipt with one exact
/// prepared prefix under the owner-held key.
///
/// Its fields and outcome vocabulary are private on purpose. A caller can
/// inspect the result, but only [`PrefixIdentityAuthority`] can produce the
/// value accepted by cache accounting as reusable-prefix evidence.
#[derive(Debug, PartialEq, Eq)]
pub struct VerifiedPrefixComparison {
    committed: CommittedPrefixId,
    outcome: VerifiedPrefixOutcome,
}

#[derive(Debug, PartialEq, Eq)]
enum VerifiedPrefixOutcome {
    Reusable,
    Invalidated(PrefixInvalidation),
}

impl VerifiedPrefixComparison {
    pub fn is_reusable(&self) -> bool {
        matches!(self.outcome, VerifiedPrefixOutcome::Reusable)
    }

    pub fn invalidation(&self) -> Option<&PrefixInvalidation> {
        match &self.outcome {
            VerifiedPrefixOutcome::Reusable => None,
            VerifiedPrefixOutcome::Invalidated(reason) => Some(reason),
        }
    }

    pub fn committed(&self) -> &CommittedPrefixId {
        &self.committed
    }
}

/// The sole owner of prefix identity key material. Deliberately non-`Clone`.
pub struct PrefixIdentityAuthority {
    realm: PrefixKeyRealm,
    key_version: PrefixKeyVersion,
    key: Secret,
}

impl PrefixIdentityAuthority {
    pub fn new(realm: PrefixKeyRealm, key_version: PrefixKeyVersion, key: Secret) -> Self {
        Self {
            realm,
            key_version,
            key,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn mint(
        &self,
        namespace: ProviderCacheNamespace,
        context_epoch: ContextEpoch,
        adapter: AdapterVersion,
        protocol: ProtocolVersion,
        policy: PolicyVersion,
        bounds: PrefixIdentityBounds,
        parts: &[PreparedPrefixPart<'_>],
    ) -> Result<ContextPrefixReceipt, PrefixIdentityError> {
        self.validate(bounds, parts)?;
        let coordinates = Coordinates {
            namespace: &namespace,
            context_epoch,
            adapter: &adapter,
            protocol: &protocol,
            policy: &policy,
        };
        let mut components = Vec::with_capacity(parts.len());
        for (index, part) in parts.iter().enumerate() {
            let ordinal = self.ordinal(index, bounds, parts.len())?;
            let source_bytes = u32::try_from(part.bytes.len()).map_err(|_| {
                PrefixIdentityError::ComponentTooLarge {
                    ordinal,
                    limit: bounds.max_component_bytes(),
                    offered: part.bytes.len(),
                }
            })?;
            components.push(PrefixComponentReceipt::from_validated_parts(
                ordinal,
                part.contribution.clone(),
                PrefixComponentId::from_validated_bytes(self.component_mac(
                    &coordinates,
                    bounds,
                    ordinal,
                    part,
                )),
                source_bytes,
            ));
        }
        let id =
            CommittedPrefixId::from_validated_bytes(self.whole_mac(&coordinates, bounds, parts));
        ContextPrefixReceipt::from_validated_parts(ContextPrefixReceiptParts {
            id,
            realm: self.realm.clone(),
            key_version: self.key_version.clone(),
            namespace,
            context_epoch,
            adapter,
            protocol,
            policy,
            bounds,
            components,
        })
        .map_err(PrefixIdentityError::from)
    }

    #[allow(clippy::too_many_arguments)]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn compare_committed_prefix(
        &self,
        receipt: &ContextPrefixReceipt,
        namespace: &ProviderCacheNamespace,
        context_epoch: ContextEpoch,
        adapter: &AdapterVersion,
        protocol: &ProtocolVersion,
        policy: &PolicyVersion,
        parts: &[PreparedPrefixPart<'_>],
    ) -> Result<VerifiedPrefixComparison, PrefixIdentityError> {
        if self.realm != *receipt.realm() {
            return Ok(invalidated(receipt, PrefixInvalidation::KeyRealmChanged));
        }
        if self.key_version != *receipt.key_version() {
            return Ok(invalidated(receipt, PrefixInvalidation::KeyVersionChanged));
        }
        if namespace != receipt.namespace() {
            return Ok(invalidated(receipt, PrefixInvalidation::NamespaceChanged));
        }
        if context_epoch != receipt.context_epoch() {
            return Ok(invalidated(
                receipt,
                PrefixInvalidation::ContextEpochChanged {
                    committed: receipt.context_epoch(),
                    offered: context_epoch,
                },
            ));
        }
        if adapter != receipt.adapter() {
            return Ok(invalidated(
                receipt,
                PrefixInvalidation::AdapterVersionChanged,
            ));
        }
        if protocol != receipt.protocol() {
            return Ok(invalidated(
                receipt,
                PrefixInvalidation::ProtocolVersionChanged,
            ));
        }
        if policy != receipt.policy() {
            return Ok(invalidated(
                receipt,
                PrefixInvalidation::PolicyVersionChanged,
            ));
        }

        let committed = receipt.components().len();
        if parts.len() < committed {
            return Ok(invalidated(
                receipt,
                PrefixInvalidation::ComponentCountChanged {
                    committed: u16::try_from(committed).map_err(|_| {
                        PrefixIdentityError::TooManyComponents {
                            limit: receipt.bounds().max_components(),
                            offered: committed,
                        }
                    })?,
                    offered: u16::try_from(parts.len()).map_err(|_| {
                        PrefixIdentityError::TooManyComponents {
                            limit: receipt.bounds().max_components(),
                            offered: parts.len(),
                        }
                    })?,
                },
            ));
        }
        let offered = self.mint(
            namespace.clone(),
            context_epoch,
            adapter.clone(),
            protocol.clone(),
            policy.clone(),
            receipt.bounds(),
            &parts[..committed],
        )?;
        // A reusable decision authenticates the whole durable receipt, not
        // merely its copied whole-prefix id. A caller can reconstruct replay
        // data, so accepting an id match alone would let altered contribution
        // receipts ride a genuine id into the reusable branch.
        if &offered == receipt {
            return Ok(VerifiedPrefixComparison {
                committed: *receipt.id(),
                outcome: VerifiedPrefixOutcome::Reusable,
            });
        }

        for (before, after) in receipt.components().iter().zip(offered.components()) {
            if before.identity() == after.identity() {
                continue;
            }
            if before.contribution() != after.contribution()
                && receipt
                    .components()
                    .iter()
                    .any(|candidate| candidate.contribution() == after.contribution())
            {
                return Ok(invalidated(
                    receipt,
                    PrefixInvalidation::OrderChanged {
                        ordinal: before.ordinal(),
                    },
                ));
            }
            if before.contribution() == after.contribution()
                && before.source_bytes() == after.source_bytes()
            {
                return Ok(invalidated(
                    receipt,
                    PrefixInvalidation::ReceiptIdentityMismatch,
                ));
            }
            return Ok(invalidated(
                receipt,
                PrefixInvalidation::ContributionChanged(PrefixComponentDiagnostic::new(
                    before.ordinal(),
                    before.source_bytes(),
                    after.source_bytes(),
                )),
            ));
        }
        Ok(invalidated(
            receipt,
            PrefixInvalidation::ReceiptIdentityMismatch,
        ))
    }

    fn validate(
        &self,
        bounds: PrefixIdentityBounds,
        parts: &[PreparedPrefixPart<'_>],
    ) -> Result<(), PrefixIdentityError> {
        if parts.is_empty() {
            return Err(PrefixIdentityError::Empty);
        }
        if parts.len() > usize::from(bounds.max_components()) {
            return Err(PrefixIdentityError::TooManyComponents {
                limit: bounds.max_components(),
                offered: parts.len(),
            });
        }
        let mut total = 0u64;
        for (index, part) in parts.iter().enumerate() {
            let ordinal = self.ordinal(index, bounds, parts.len())?;
            if part.bytes.len() > bounds.max_component_bytes() as usize {
                return Err(PrefixIdentityError::ComponentTooLarge {
                    ordinal,
                    limit: bounds.max_component_bytes(),
                    offered: part.bytes.len(),
                });
            }
            let bytes = u64::try_from(part.bytes.len())
                .map_err(|_| PrefixIdentityError::TotalBytesOverflow { ordinal })?;
            total = total
                .checked_add(bytes)
                .ok_or(PrefixIdentityError::TotalBytesOverflow { ordinal })?;
            if total > bounds.max_total_bytes() {
                return Err(PrefixIdentityError::TotalTooLarge {
                    limit: bounds.max_total_bytes(),
                    offered: total,
                });
            }
        }
        Ok(())
    }

    fn ordinal(
        &self,
        index: usize,
        bounds: PrefixIdentityBounds,
        offered: usize,
    ) -> Result<PrefixComponentOrdinal, PrefixIdentityError> {
        u16::try_from(index)
            .map(PrefixComponentOrdinal::new)
            .map_err(|_| PrefixIdentityError::TooManyComponents {
                limit: bounds.max_components(),
                offered,
            })
    }

    fn component_mac(
        &self,
        coordinates: &Coordinates<'_>,
        bounds: PrefixIdentityBounds,
        ordinal: PrefixComponentOrdinal,
        part: &PreparedPrefixPart<'_>,
    ) -> [u8; 32] {
        let mut bytes = self.authenticated_prefix_bytes(COMPONENT_DOMAIN, coordinates, bounds);
        field(&mut bytes, &ordinal.value().to_be_bytes());
        field(&mut bytes, part.contribution.as_str().as_bytes());
        field(&mut bytes, part.bytes);
        hmac_sha256(self.key.expose(), &bytes) // component identity key
    }

    fn whole_mac(
        &self,
        coordinates: &Coordinates<'_>,
        bounds: PrefixIdentityBounds,
        parts: &[PreparedPrefixPart<'_>],
    ) -> [u8; 32] {
        let mut bytes = self.authenticated_prefix_bytes(WHOLE_DOMAIN, coordinates, bounds);
        field(&mut bytes, &(parts.len() as u64).to_be_bytes());
        for (index, part) in parts.iter().enumerate() {
            field(&mut bytes, &(index as u64).to_be_bytes());
            field(&mut bytes, part.contribution.as_str().as_bytes());
            field(&mut bytes, part.bytes);
        }
        hmac_sha256(self.key.expose(), &bytes) // whole prefix identity key
    }

    fn authenticated_prefix_bytes(
        &self,
        domain: &[u8],
        coordinates: &Coordinates<'_>,
        bounds: PrefixIdentityBounds,
    ) -> Vec<u8> {
        let mut bytes = self.coordinate_bytes(domain, coordinates);
        field(&mut bytes, &bounds.max_components().to_be_bytes());
        field(&mut bytes, &bounds.max_component_bytes().to_be_bytes());
        field(&mut bytes, &bounds.max_total_bytes().to_be_bytes());
        bytes
    }

    fn coordinate_bytes(&self, domain: &[u8], c: &Coordinates<'_>) -> Vec<u8> {
        let mut bytes = Vec::new();
        field(&mut bytes, domain);
        field(&mut bytes, self.realm.as_str().as_bytes());
        field(&mut bytes, self.key_version.as_str().as_bytes());
        field(&mut bytes, c.namespace.as_str().as_bytes());
        field(&mut bytes, &c.context_epoch.value().to_be_bytes());
        field(&mut bytes, c.adapter.as_str().as_bytes());
        field(&mut bytes, c.protocol.as_str().as_bytes());
        field(&mut bytes, c.policy.as_str().as_bytes());
        bytes
    }
}

impl std::fmt::Debug for PrefixIdentityAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrefixIdentityAuthority")
            .field("realm", &self.realm)
            .field("key_version", &self.key_version)
            .finish_non_exhaustive()
    }
}

struct Coordinates<'a> {
    namespace: &'a ProviderCacheNamespace,
    context_epoch: ContextEpoch,
    adapter: &'a AdapterVersion,
    protocol: &'a ProtocolVersion,
    policy: &'a PolicyVersion,
}

fn invalidated(
    receipt: &ContextPrefixReceipt,
    reason: PrefixInvalidation,
) -> VerifiedPrefixComparison {
    VerifiedPrefixComparison {
        committed: *receipt.id(),
        outcome: VerifiedPrefixOutcome::Invalidated(reason),
    }
}

fn field(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u64).to_be_bytes());
    out.extend_from_slice(value);
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    <Sha256 as sha2::Digest>::digest(bytes).into()
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut padded = [0u8; BLOCK];
    if key.len() > BLOCK {
        padded[..32].copy_from_slice(&sha256(key));
    } else {
        padded[..key.len()].copy_from_slice(key);
    }
    let mut inner = Vec::with_capacity(BLOCK + message.len());
    inner.extend(padded.iter().map(|byte| byte ^ 0x36));
    inner.extend_from_slice(message);
    let inner = sha256(&inner);
    let mut outer = Vec::with_capacity(BLOCK + inner.len());
    outer.extend(padded.iter().map(|byte| byte ^ 0x5c));
    outer.extend_from_slice(&inner);
    sha256(&outer)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn authority(key: &[u8], version: &str) -> TestResult<PrefixIdentityAuthority> {
        Ok(PrefixIdentityAuthority::new(
            PrefixKeyRealm::new("service-root")?,
            PrefixKeyVersion::new(version)?,
            Secret::new(key.to_vec()),
        ))
    }

    fn coordinates() -> TestResult<(
        ProviderCacheNamespace,
        AdapterVersion,
        ProtocolVersion,
        PolicyVersion,
        PrefixIdentityBounds,
    )> {
        Ok((
            ProviderCacheNamespace::new("responses")?,
            AdapterVersion::new("adapter-1").ok_or("adapter")?,
            ProtocolVersion::new("protocol-1").ok_or("protocol")?,
            PolicyVersion::new("policy-1").ok_or("policy")?,
            PrefixIdentityBounds::new(4, 16, 32)?,
        ))
    }

    fn contribution(value: &str) -> TestResult<ContributionId> {
        ContributionId::new(value).ok_or_else(|| "contribution".into())
    }

    #[test]
    fn exact_append_only_prefix_is_reusable_but_provider_metadata_is_not_an_input() -> TestResult {
        let owner = authority(b"key", "v1")?;
        let (namespace, adapter, protocol, policy, bounds) = coordinates()?;
        let system = contribution("system")?;
        let user = contribution("user")?;
        let parts = [PreparedPrefixPart {
            contribution: &system,
            bytes: b"system",
        }];
        let receipt = owner.mint(
            namespace.clone(),
            ContextEpoch::new(1),
            adapter.clone(),
            protocol.clone(),
            policy.clone(),
            bounds,
            &parts,
        )?;
        let appended = [
            parts[0],
            PreparedPrefixPart {
                contribution: &user,
                bytes: b"user",
            },
        ];
        assert!(
            owner
                .compare_committed_prefix(
                    &receipt,
                    &namespace,
                    ContextEpoch::new(1),
                    &adapter,
                    &protocol,
                    &policy,
                    &appended
                )?
                .is_reusable(),
            "the exact authority comparison did not verify the committed prefix"
        );
        Ok(())
    }

    #[test]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn order_content_and_each_version_invalidate_with_the_exact_reason() -> TestResult {
        let owner = authority(b"key", "v1")?;
        let (namespace, adapter, protocol, policy, bounds) = coordinates()?;
        let a = contribution("a")?;
        let b = contribution("b")?;
        let original = [
            PreparedPrefixPart {
                contribution: &a,
                bytes: b"a",
            },
            PreparedPrefixPart {
                contribution: &b,
                bytes: b"b",
            },
        ];
        let receipt = owner.mint(
            namespace.clone(),
            ContextEpoch::new(1),
            adapter.clone(),
            protocol.clone(),
            policy.clone(),
            bounds,
            &original,
        )?;
        let reversed = [original[1], original[0]];
        assert!(
            matches!(
                owner
                    .compare_committed_prefix(
                        &receipt,
                        &namespace,
                        ContextEpoch::new(1),
                        &adapter,
                        &protocol,
                        &policy,
                        &reversed,
                    )?
                    .invalidation(),
                Some(PrefixInvalidation::OrderChanged { .. })
            ),
            "a contribution reorder was mislabeled as content change"
        );
        let changed = [
            PreparedPrefixPart {
                contribution: &a,
                bytes: b"changed",
            },
            original[1],
        ];
        assert!(matches!(
            owner
                .compare_committed_prefix(
                    &receipt,
                    &namespace,
                    ContextEpoch::new(1),
                    &adapter,
                    &protocol,
                    &policy,
                    &changed,
                )?
                .invalidation(),
            Some(PrefixInvalidation::ContributionChanged(_))
        ));
        assert_eq!(
            owner
                .compare_committed_prefix(
                    &receipt,
                    &namespace,
                    ContextEpoch::new(1),
                    &AdapterVersion::new("adapter-2").ok_or("adapter")?,
                    &protocol,
                    &policy,
                    &original,
                )?
                .invalidation(),
            Some(&PrefixInvalidation::AdapterVersionChanged),
            "an adapter-version change reached content comparison instead of exact invalidation"
        );
        assert_eq!(
            owner
                .compare_committed_prefix(
                    &receipt,
                    &namespace,
                    ContextEpoch::new(1),
                    &adapter,
                    &ProtocolVersion::new("protocol-2").ok_or("protocol")?,
                    &policy,
                    &original,
                )?
                .invalidation(),
            Some(&PrefixInvalidation::ProtocolVersionChanged)
        );
        assert_eq!(
            owner
                .compare_committed_prefix(
                    &receipt,
                    &namespace,
                    ContextEpoch::new(1),
                    &adapter,
                    &protocol,
                    &PolicyVersion::new("policy-2").ok_or("policy")?,
                    &original,
                )?
                .invalidation(),
            Some(&PrefixInvalidation::PolicyVersionChanged)
        );
        Ok(())
    }

    #[test]
    fn key_rotation_is_a_resurrection_counterexample() -> TestResult {
        let original = authority(b"key-one", "v1")?;
        let replacement = authority(b"key-two", "v1")?;
        let (namespace, adapter, protocol, policy, bounds) = coordinates()?;
        let contribution = contribution("same")?;
        let parts = [PreparedPrefixPart {
            contribution: &contribution,
            bytes: b"same",
        }];
        let receipt = original.mint(
            namespace.clone(),
            ContextEpoch::new(1),
            adapter.clone(),
            protocol.clone(),
            policy.clone(),
            bounds,
            &parts,
        )?;
        assert!(
            !replacement
                .compare_committed_prefix(
                    &receipt,
                    &namespace,
                    ContextEpoch::new(1),
                    &adapter,
                    &protocol,
                    &policy,
                    &parts
                )?
                .is_reusable(),
            "replacement key material resurrected an earlier receipt"
        );
        Ok(())
    }

    #[test]
    fn altered_receipt_components_cannot_ride_a_genuine_id_into_reuse() -> TestResult {
        let owner = authority(b"key", "v1")?;
        let (namespace, adapter, protocol, policy, bounds) = coordinates()?;
        let contribution = contribution("system")?;
        let original = [PreparedPrefixPart {
            contribution: &contribution,
            bytes: b"original",
        }];
        let altered = [PreparedPrefixPart {
            contribution: &contribution,
            bytes: b"altered",
        }];
        let genuine = owner.mint(
            namespace.clone(),
            ContextEpoch::FIRST,
            adapter.clone(),
            protocol.clone(),
            policy.clone(),
            bounds,
            &original,
        )?;
        let altered_receipt = owner.mint(
            namespace.clone(),
            ContextEpoch::FIRST,
            adapter.clone(),
            protocol.clone(),
            policy.clone(),
            bounds,
            &altered,
        )?;
        let corrupt = ContextPrefixReceipt::from_validated_parts(ContextPrefixReceiptParts {
            id: *genuine.id(),
            realm: genuine.realm().clone(),
            key_version: genuine.key_version().clone(),
            namespace: genuine.namespace().clone(),
            context_epoch: genuine.context_epoch(),
            adapter: genuine.adapter().clone(),
            protocol: genuine.protocol().clone(),
            policy: genuine.policy().clone(),
            bounds: genuine.bounds(),
            components: altered_receipt.components().to_vec(),
        })?;
        let comparison = owner.compare_committed_prefix(
            &corrupt,
            &namespace,
            ContextEpoch::FIRST,
            &adapter,
            &protocol,
            &policy,
            &original,
        )?;
        assert!(
            !comparison.is_reusable(),
            "altered receipt components rode a genuine whole-prefix id into reuse"
        );
        assert_eq!(
            comparison.invalidation(),
            Some(&PrefixInvalidation::ContributionChanged(
                PrefixComponentDiagnostic::new(PrefixComponentOrdinal::new(0), 7, 8)
            ))
        );
        Ok(())
    }

    #[test]
    fn resource_bounds_fail_before_a_receipt_exists() -> TestResult {
        let owner = authority(b"key", "v1")?;
        let (namespace, adapter, protocol, policy, _) = coordinates()?;
        let contribution = contribution("large")?;
        let parts = [PreparedPrefixPart {
            contribution: &contribution,
            bytes: b"12345",
        }];
        assert!(matches!(
            owner.mint(
                namespace,
                ContextEpoch::FIRST,
                adapter,
                protocol,
                policy,
                PrefixIdentityBounds::new(1, 4, 4)?,
                &parts
            ),
            Err(PrefixIdentityError::ComponentTooLarge { .. })
        ));
        Ok(())
    }

    #[test]
    fn debug_redacts_the_key() -> TestResult {
        let owner = authority(b"SENTINEL", "v1")?;
        let debug = format!("{owner:?}");
        assert!(!debug.contains("SENTINEL"));
        Ok(())
    }
}
