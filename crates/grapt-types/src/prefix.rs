//! Authoritative, bounded identity for one exact prepared request prefix.
//!
//! The values here are inert durable data. Only `grapt-policy` can mint or
//! compare the keyed identities; callers cannot assert that a prefix is stable.

use crate::{AdapterVersion, ContextEpoch, ContributionId, PolicyVersion, ProtocolVersion};

pub const COMMITTED_PREFIX_ID_BYTES: usize = 32;
pub const MAX_PREFIX_LABEL_BYTES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefixLabelKind {
    KeyRealm,
    KeyVersion,
    ProviderNamespace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefixLabelError {
    Empty {
        kind: PrefixLabelKind,
    },
    TooLong {
        kind: PrefixLabelKind,
        maximum: usize,
        offered: usize,
    },
}

impl std::fmt::Display for PrefixLabelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid prefix label: {self:?}")
    }
}

impl std::error::Error for PrefixLabelError {}

fn bounded_label(raw: String, kind: PrefixLabelKind) -> Result<String, PrefixLabelError> {
    let offered = raw.len();
    if offered == 0 {
        return Err(PrefixLabelError::Empty { kind });
    }
    if offered > MAX_PREFIX_LABEL_BYTES {
        return Err(PrefixLabelError::TooLong {
            kind,
            maximum: MAX_PREFIX_LABEL_BYTES,
            offered,
        });
    }
    Ok(raw)
}

macro_rules! prefix_label {
    ($(#[$doc:meta])* $name:ident, $kind:expr) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            pub fn new(raw: impl Into<String>) -> Result<Self, PrefixLabelError> {
                bounded_label(raw.into(), $kind).map(Self)
            }

            pub fn as_str(&self) -> &str { &self.0 }
        }
    };
}

prefix_label! {
    /// Public realm selecting the owner-held identity key.
    PrefixKeyRealm, PrefixLabelKind::KeyRealm
}
prefix_label! {
    /// Public version naming the owner-held key generation.
    PrefixKeyVersion, PrefixLabelKind::KeyVersion
}
prefix_label! {
    /// Provider-neutral cache namespace.
    ProviderCacheNamespace, PrefixLabelKind::ProviderNamespace
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommittedPrefixId([u8; COMMITTED_PREFIX_ID_BYTES]);

impl CommittedPrefixId {
    pub fn from_validated_bytes(bytes: [u8; COMMITTED_PREFIX_ID_BYTES]) -> Self {
        Self(bytes)
    }
    pub fn as_bytes(&self) -> &[u8; COMMITTED_PREFIX_ID_BYTES] {
        &self.0
    }
}

impl std::fmt::Debug for CommittedPrefixId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CommittedPrefixId(<opaque-32-bytes>)")
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PrefixComponentId([u8; COMMITTED_PREFIX_ID_BYTES]);

impl PrefixComponentId {
    pub fn from_validated_bytes(bytes: [u8; COMMITTED_PREFIX_ID_BYTES]) -> Self {
        Self(bytes)
    }
    pub fn as_bytes(&self) -> &[u8; COMMITTED_PREFIX_ID_BYTES] {
        &self.0
    }
}

impl std::fmt::Debug for PrefixComponentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrefixComponentId(<opaque-32-bytes>)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PrefixComponentOrdinal(u16);

impl PrefixComponentOrdinal {
    pub fn new(value: u16) -> Self {
        Self(value)
    }
    pub fn value(self) -> u16 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrefixIdentityBounds {
    max_components: u16,
    max_component_bytes: u32,
    max_total_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefixBoundsError {
    ZeroComponents,
    ZeroComponentBytes,
    ZeroTotalBytes,
}

impl std::fmt::Display for PrefixBoundsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid prefix bound: {self:?}")
    }
}

impl std::error::Error for PrefixBoundsError {}

impl PrefixIdentityBounds {
    pub fn new(
        max_components: u16,
        max_component_bytes: u32,
        max_total_bytes: u64,
    ) -> Result<Self, PrefixBoundsError> {
        if max_components == 0 {
            return Err(PrefixBoundsError::ZeroComponents);
        }
        if max_component_bytes == 0 {
            return Err(PrefixBoundsError::ZeroComponentBytes);
        }
        if max_total_bytes == 0 {
            return Err(PrefixBoundsError::ZeroTotalBytes);
        }
        Ok(Self {
            max_components,
            max_component_bytes,
            max_total_bytes,
        })
    }
    pub fn max_components(self) -> u16 {
        self.max_components
    }
    pub fn max_component_bytes(self) -> u32 {
        self.max_component_bytes
    }
    pub fn max_total_bytes(self) -> u64 {
        self.max_total_bytes
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixComponentReceipt {
    ordinal: PrefixComponentOrdinal,
    contribution: ContributionId,
    identity: PrefixComponentId,
    source_bytes: u32,
}

impl PrefixComponentReceipt {
    pub fn from_validated_parts(
        ordinal: PrefixComponentOrdinal,
        contribution: ContributionId,
        identity: PrefixComponentId,
        source_bytes: u32,
    ) -> Self {
        Self {
            ordinal,
            contribution,
            identity,
            source_bytes,
        }
    }
    pub fn ordinal(&self) -> PrefixComponentOrdinal {
        self.ordinal
    }
    pub fn contribution(&self) -> &ContributionId {
        &self.contribution
    }
    pub fn identity(&self) -> &PrefixComponentId {
        &self.identity
    }
    pub fn source_bytes(&self) -> u32 {
        self.source_bytes
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPrefixReceiptParts {
    pub id: CommittedPrefixId,
    pub realm: PrefixKeyRealm,
    pub key_version: PrefixKeyVersion,
    pub namespace: ProviderCacheNamespace,
    pub context_epoch: ContextEpoch,
    pub adapter: AdapterVersion,
    pub protocol: ProtocolVersion,
    pub policy: PolicyVersion,
    pub bounds: PrefixIdentityBounds,
    pub components: Vec<PrefixComponentReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefixReceiptError {
    Empty,
    TooManyComponents {
        limit: u16,
        offered: usize,
    },
    NonContiguousOrdinal {
        expected: PrefixComponentOrdinal,
        offered: PrefixComponentOrdinal,
    },
    ComponentTooLarge {
        ordinal: PrefixComponentOrdinal,
        limit: u32,
        offered: u32,
    },
    TotalBytesOverflow,
    TotalTooLarge {
        limit: u64,
        offered: u64,
    },
}

impl std::fmt::Display for PrefixReceiptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "prefix receipt refused: {self:?}")
    }
}

impl std::error::Error for PrefixReceiptError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPrefixReceipt {
    id: CommittedPrefixId,
    realm: PrefixKeyRealm,
    key_version: PrefixKeyVersion,
    namespace: ProviderCacheNamespace,
    context_epoch: ContextEpoch,
    adapter: AdapterVersion,
    protocol: ProtocolVersion,
    policy: PolicyVersion,
    bounds: PrefixIdentityBounds,
    total_source_bytes: u64,
    components: Vec<PrefixComponentReceipt>,
}

impl ContextPrefixReceipt {
    pub fn from_validated_parts(
        parts: ContextPrefixReceiptParts,
    ) -> Result<Self, PrefixReceiptError> {
        if parts.components.is_empty() {
            return Err(PrefixReceiptError::Empty);
        }
        if parts.components.len() > usize::from(parts.bounds.max_components()) {
            return Err(PrefixReceiptError::TooManyComponents {
                limit: parts.bounds.max_components(),
                offered: parts.components.len(),
            });
        }
        let mut total = 0u64;
        for (index, component) in parts.components.iter().enumerate() {
            let expected = PrefixComponentOrdinal::new(u16::try_from(index).map_err(|_| {
                PrefixReceiptError::TooManyComponents {
                    limit: parts.bounds.max_components(),
                    offered: parts.components.len(),
                }
            })?);
            if component.ordinal() != expected {
                return Err(PrefixReceiptError::NonContiguousOrdinal {
                    expected,
                    offered: component.ordinal(),
                });
            }
            if component.source_bytes() > parts.bounds.max_component_bytes() {
                return Err(PrefixReceiptError::ComponentTooLarge {
                    ordinal: component.ordinal(),
                    limit: parts.bounds.max_component_bytes(),
                    offered: component.source_bytes(),
                });
            }
            total = total
                .checked_add(u64::from(component.source_bytes()))
                .ok_or(PrefixReceiptError::TotalBytesOverflow)?;
            if total > parts.bounds.max_total_bytes() {
                return Err(PrefixReceiptError::TotalTooLarge {
                    limit: parts.bounds.max_total_bytes(),
                    offered: total,
                });
            }
        }
        Ok(Self {
            id: parts.id,
            realm: parts.realm,
            key_version: parts.key_version,
            namespace: parts.namespace,
            context_epoch: parts.context_epoch,
            adapter: parts.adapter,
            protocol: parts.protocol,
            policy: parts.policy,
            bounds: parts.bounds,
            total_source_bytes: total,
            components: parts.components,
        })
    }
    pub fn id(&self) -> &CommittedPrefixId {
        &self.id
    }
    pub fn realm(&self) -> &PrefixKeyRealm {
        &self.realm
    }
    pub fn key_version(&self) -> &PrefixKeyVersion {
        &self.key_version
    }
    pub fn namespace(&self) -> &ProviderCacheNamespace {
        &self.namespace
    }
    pub fn context_epoch(&self) -> ContextEpoch {
        self.context_epoch
    }
    pub fn adapter(&self) -> &AdapterVersion {
        &self.adapter
    }
    pub fn protocol(&self) -> &ProtocolVersion {
        &self.protocol
    }
    pub fn policy(&self) -> &PolicyVersion {
        &self.policy
    }
    pub fn bounds(&self) -> PrefixIdentityBounds {
        self.bounds
    }
    pub fn total_source_bytes(&self) -> u64 {
        self.total_source_bytes
    }
    pub fn components(&self) -> &[PrefixComponentReceipt] {
        &self.components
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrefixComponentDiagnostic {
    ordinal: PrefixComponentOrdinal,
    committed_bytes: u32,
    offered_bytes: u32,
}

impl PrefixComponentDiagnostic {
    pub fn new(ordinal: PrefixComponentOrdinal, committed_bytes: u32, offered_bytes: u32) -> Self {
        Self {
            ordinal,
            committed_bytes,
            offered_bytes,
        }
    }
    pub fn ordinal(self) -> PrefixComponentOrdinal {
        self.ordinal
    }
    pub fn committed_bytes(self) -> u32 {
        self.committed_bytes
    }
    pub fn offered_bytes(self) -> u32 {
        self.offered_bytes
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefixInvalidation {
    KeyRealmChanged,
    KeyVersionChanged,
    NamespaceChanged,
    ContextEpochChanged {
        committed: ContextEpoch,
        offered: ContextEpoch,
    },
    AdapterVersionChanged,
    ProtocolVersionChanged,
    PolicyVersionChanged,
    ComponentCountChanged {
        committed: u16,
        offered: u16,
    },
    OrderChanged {
        ordinal: PrefixComponentOrdinal,
    },
    ContributionChanged(PrefixComponentDiagnostic),
    ReceiptIdentityMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn component(ordinal: u16, bytes: u32) -> TestResult<PrefixComponentReceipt> {
        Ok(PrefixComponentReceipt::from_validated_parts(
            PrefixComponentOrdinal::new(ordinal),
            ContributionId::new(format!("c{ordinal}")).ok_or("contribution")?,
            PrefixComponentId::from_validated_bytes([ordinal as u8; 32]),
            bytes,
        ))
    }

    fn parts(components: Vec<PrefixComponentReceipt>) -> TestResult<ContextPrefixReceiptParts> {
        Ok(ContextPrefixReceiptParts {
            id: CommittedPrefixId::from_validated_bytes([7; 32]),
            realm: PrefixKeyRealm::new("service-root")?,
            key_version: PrefixKeyVersion::new("v1")?,
            namespace: ProviderCacheNamespace::new("responses")?,
            context_epoch: ContextEpoch::new(2),
            adapter: AdapterVersion::new("adapter-1").ok_or("adapter")?,
            protocol: ProtocolVersion::new("protocol-1").ok_or("protocol")?,
            policy: PolicyVersion::new("policy-1").ok_or("policy")?,
            bounds: PrefixIdentityBounds::new(2, 4, 6)?,
            components,
        })
    }

    #[test]
    fn fixed_identities_and_debug_never_expose_bytes() {
        let id = CommittedPrefixId::from_validated_bytes([0xab; 32]);
        assert_eq!(std::mem::size_of_val(&id), COMMITTED_PREFIX_ID_BYTES);
        assert_eq!(format!("{id:?}"), "CommittedPrefixId(<opaque-32-bytes>)");
        assert!(!format!("{id:?}").contains("ab"));
    }

    #[test]
    fn labels_count_utf8_bytes_and_refuse_empty() {
        assert_eq!(
            PrefixKeyRealm::new(""),
            Err(PrefixLabelError::Empty {
                kind: PrefixLabelKind::KeyRealm
            })
        );
        let over = "é".repeat((MAX_PREFIX_LABEL_BYTES / 2) + 1);
        assert_eq!(
            PrefixKeyVersion::new(over),
            Err(PrefixLabelError::TooLong {
                kind: PrefixLabelKind::KeyVersion,
                maximum: MAX_PREFIX_LABEL_BYTES,
                offered: MAX_PREFIX_LABEL_BYTES + 2
            })
        );
    }

    #[test]
    fn zero_bounds_and_empty_receipts_are_unrepresentable() -> TestResult {
        assert_eq!(
            PrefixIdentityBounds::new(0, 1, 1),
            Err(PrefixBoundsError::ZeroComponents)
        );
        assert_eq!(
            ContextPrefixReceipt::from_validated_parts(parts(Vec::new())?),
            Err(PrefixReceiptError::Empty)
        );
        Ok(())
    }

    #[test]
    fn count_component_total_and_ordinal_bounds_refuse() -> TestResult {
        assert!(matches!(
            ContextPrefixReceipt::from_validated_parts(parts(vec![
                component(0, 1)?,
                component(1, 1)?,
                component(2, 1)?
            ])?),
            Err(PrefixReceiptError::TooManyComponents { .. })
        ));
        assert!(matches!(
            ContextPrefixReceipt::from_validated_parts(parts(vec![component(0, 5)?])?),
            Err(PrefixReceiptError::ComponentTooLarge { .. })
        ));
        assert!(matches!(
            ContextPrefixReceipt::from_validated_parts(parts(vec![
                component(0, 4)?,
                component(1, 3)?
            ])?),
            Err(PrefixReceiptError::TotalTooLarge { .. })
        ));
        assert!(matches!(
            ContextPrefixReceipt::from_validated_parts(parts(vec![component(1, 1)?])?),
            Err(PrefixReceiptError::NonContiguousOrdinal { .. })
        ));
        Ok(())
    }

    #[test]
    fn valid_receipt_retains_versions_bounds_and_fixed_component_receipts() -> TestResult {
        let receipt = ContextPrefixReceipt::from_validated_parts(parts(vec![
            component(0, 4)?,
            component(1, 2)?,
        ])?)?;
        assert_eq!(receipt.total_source_bytes(), 6);
        assert_eq!(receipt.components().len(), 2);
        assert_eq!(receipt.adapter().as_str(), "adapter-1");
        assert_eq!(receipt.protocol().as_str(), "protocol-1");
        assert_eq!(receipt.policy().as_str(), "policy-1");
        Ok(())
    }
}
