use grapt_policy::{PrefixIdentityAuthority, PreparedPrefixPart, Secret};
use grapt_types::{
    AdapterVersion, ContextEpoch, ContextPrefixReceipt, ContextPrefixReceiptParts, ContributionId,
    PolicyVersion, PrefixIdentityBounds, PrefixInvalidation, PrefixKeyRealm, PrefixKeyVersion,
    ProtocolVersion, ProviderCacheNamespace,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct PublicHarness {
    authority: PrefixIdentityAuthority,
    namespace: ProviderCacheNamespace,
    adapter: AdapterVersion,
    protocol: ProtocolVersion,
    policy: PolicyVersion,
    contribution: ContributionId,
}

impl PublicHarness {
    fn new() -> TestResult<Self> {
        Ok(Self {
            authority: PrefixIdentityAuthority::new(
                PrefixKeyRealm::new("service-root")?,
                PrefixKeyVersion::new("v1")?,
                Secret::new(b"authority-key".to_vec()),
            ),
            namespace: ProviderCacheNamespace::new("responses")?,
            adapter: AdapterVersion::new("adapter-1").ok_or("adapter")?,
            protocol: ProtocolVersion::new("protocol-1").ok_or("protocol")?,
            policy: PolicyVersion::new("policy-1").ok_or("policy")?,
            contribution: ContributionId::new("system").ok_or("contribution")?,
        })
    }

    fn parts(&self) -> [PreparedPrefixPart<'_>; 1] {
        [PreparedPrefixPart {
            contribution: &self.contribution,
            bytes: b"system",
        }]
    }

    fn mint(&self, bounds: PrefixIdentityBounds) -> TestResult<ContextPrefixReceipt> {
        Ok(self.authority.mint(
            self.namespace.clone(),
            ContextEpoch::FIRST,
            self.adapter.clone(),
            self.protocol.clone(),
            self.policy.clone(),
            bounds,
            &self.parts(),
        )?)
    }

    fn compare(
        &self,
        receipt: &ContextPrefixReceipt,
    ) -> TestResult<grapt_policy::VerifiedPrefixComparison> {
        Ok(self.authority.compare_committed_prefix(
            receipt,
            &self.namespace,
            ContextEpoch::FIRST,
            &self.adapter,
            &self.protocol,
            &self.policy,
            &self.parts(),
        )?)
    }
}

fn strict_bounds() -> TestResult<PrefixIdentityBounds> {
    Ok(PrefixIdentityBounds::new(1, 6, 6)?)
}

fn loose_bounds() -> TestResult<[PrefixIdentityBounds; 3]> {
    Ok([
        PrefixIdentityBounds::new(2, 6, 6)?,
        PrefixIdentityBounds::new(1, 7, 6)?,
        PrefixIdentityBounds::new(1, 6, 7)?,
    ])
}

fn reconstruct_with_bounds(
    genuine: &ContextPrefixReceipt,
    bounds: PrefixIdentityBounds,
) -> TestResult<ContextPrefixReceipt> {
    Ok(ContextPrefixReceipt::from_validated_parts(
        ContextPrefixReceiptParts {
            id: *genuine.id(),
            realm: genuine.realm().clone(),
            key_version: genuine.key_version().clone(),
            namespace: genuine.namespace().clone(),
            context_epoch: genuine.context_epoch(),
            adapter: genuine.adapter().clone(),
            protocol: genuine.protocol().clone(),
            policy: genuine.policy().clone(),
            bounds,
            components: genuine.components().to_vec(),
        },
    )?)
}

#[test]
fn authenticated_bounds_change_every_component_mac() -> TestResult {
    let harness = PublicHarness::new()?;
    let strict = harness.mint(strict_bounds()?)?;
    let strict_component = strict
        .components()
        .first()
        .ok_or("strict receipt has no component")?;
    for loose in loose_bounds()? {
        let altered = harness.mint(loose)?;
        let altered_component = altered
            .components()
            .first()
            .ok_or("altered receipt has no component")?;
        assert_ne!(
            strict_component.identity(),
            altered_component.identity(),
            "component MAC ignored authority-selected prefix bounds"
        );
    }
    Ok(())
}

#[test]
fn authenticated_bounds_change_the_whole_prefix_mac() -> TestResult {
    let harness = PublicHarness::new()?;
    let strict = harness.mint(strict_bounds()?)?;
    for loose in loose_bounds()? {
        let altered = harness.mint(loose)?;
        assert_ne!(
            strict.id(),
            altered.id(),
            "whole-prefix MAC ignored authority-selected prefix bounds"
        );
    }
    Ok(())
}

#[test]
fn authenticated_bounds_public_reconstruction_refuses_strict_to_loose_replacement() -> TestResult {
    let harness = PublicHarness::new()?;
    let strict = harness.mint(strict_bounds()?)?;
    for loose in loose_bounds()? {
        let reconstructed = reconstruct_with_bounds(&strict, loose)?;
        assert!(
            !harness.compare(&reconstructed)?.is_reusable(),
            "authority-selected prefix bounds were loosened by public receipt reconstruction"
        );
    }
    Ok(())
}

#[test]
fn authenticated_bounds_reconstruction_has_the_receipt_identity_mismatch_reason() -> TestResult {
    let harness = PublicHarness::new()?;
    let strict = harness.mint(strict_bounds()?)?;
    for loose in loose_bounds()? {
        let reconstructed = reconstruct_with_bounds(&strict, loose)?;
        assert_eq!(
            harness.compare(&reconstructed)?.invalidation(),
            Some(&PrefixInvalidation::ReceiptIdentityMismatch),
            "altered bounds did not refuse as a receipt identity mismatch"
        );
    }
    Ok(())
}
