//! R3-e acceptance witnesses for the effective-authority foundation.
//!
//! # Exact executable coverage in this file
//!
//! Seven laws, and only these seven:
//!
//! 1. duplicate and conflicting contributor keys are TYPED rejections, never a
//!    silent last-writer-wins (`duplicate_and_conflicting_keys_are_typed_rejections`);
//! 2. config precedence conflicts are typed and independent of input order
//!    (`config_precedence_conflicts_are_typed_and_order_independent`);
//! 3. a graph with no model contributor is rejected (`a_graph_without_a_model_is_rejected`);
//! 4. illegal empty identities are unrepresentable (`illegal_empty_identities_are_unrepresentable`);
//! 5. a changed environment value is a typed CONFLICT and an unchanged one a
//!    duplicate, so value rotation is observable through the builder
//!    (`a_rotated_environment_value_is_observable_as_a_typed_conflict`);
//! 6. two identity authorities that differ only in key version, or only in key
//!    material, mint different contributions
//!    (`rotating_the_identity_authority_changes_every_contribution_it_touches`);
//! 7. the same value under two authorities is not comparable across them, and
//!    under one authority is stable
//!    (`one_authority_is_content_addressed_and_two_are_not_comparable`).
//!
//! Laws 5-7 are the reachable half of the §8.4 correction. The other half — that
//! no unkeyed secret-derived bytes reach the canonical stream — needs the
//! contributor node rather than a whole snapshot and is witnessed inside the
//! crate, in `contributor::tests`, because the byte surface is deliberately not
//! public. Neither half is the snapshot-level witness, which L2 released into
//! `tests/graph_receipt.rs`.
//!
//! # What this header used to claim
//!
//! It listed eleven laws — receipt content-addressing, per-class semantic
//! change, provenance input-order equivalence, delimiter/collision adversaries,
//! plugin-vs-MCP class separation, config discriminant spoofing, permission
//! authority scope, secret absence, graph immutability, catalog issuance
//! binding, and the tool-visibility query. Twelve scenarios were moved to the
//! pending inventory when `SnapshotBuilder::model` began requiring a
//! registry-issued `ResolvedModelTarget`, and the header was not updated. A test
//! file that advertises coverage it no longer has is worse than one that
//! advertises nothing: a reader checking whether a law is witnessed finds it
//! named here and stops looking.
//!
//! Those laws are no longer absent. L2 MODEL-REGISTRY-ISSUANCE built the
//! issuing owner, so all twelve were restored through the real path and now
//! live in `tests/graph_receipt.rs` and `tests/graph_contributors.rs`, over the
//! shared surface in `tests/support/`. The pending inventory, its manifest and
//! its validator were removed in the same lane rather than left as an empty
//! claim: a record of nothing deferred is a record that drifts.

use grapt_policy::{
    AuthorityRealm, ConfigLayer, ConfigSource, ContributionAuthority, CredentialFingerprint,
    CustomSourceName, EnvExposure, EnvVarName, KeyVersion, PolicyError, Secret, SnapshotBuilder,
    ToolName,
};
use grapt_types::{EffectiveGraphId, LayerName, ProviderId, ServiceSlotId};

type TestResult = Result<(), Box<dyn std::error::Error>>;

// ---- helpers (test-only; panic on a malformed literal, never unwrap/expect) -

fn provider(s: &str) -> ProviderId {
    match ProviderId::new(s) {
        Some(v) => v,
        None => panic!("provider id {s:?}"),
    }
}

/// One injected identity authority, declared with a STABLE realm and key
/// version. Every witness that needs two authorities builds the second
/// explicitly, so a test can never be passing because the authority happened to
/// differ.
fn authority(version: &str, key: &[u8]) -> ContributionAuthority {
    let realm = match AuthorityRealm::new("secode-effective-graph") {
        Some(v) => v,
        None => panic!("realm"),
    };
    let version = match KeyVersion::new(version) {
        Some(v) => v,
        None => panic!("key version"),
    };
    ContributionAuthority::new(realm, version, Secret::new(key.to_vec()))
}

fn env_name(s: &str) -> EnvVarName {
    match EnvVarName::new(s) {
        Some(v) => v,
        None => panic!("env var name {s:?}"),
    }
}

fn derived_capability(s: &str) -> Option<grapt_types::CapabilityId> {
    // Capability identity is derived from resolved coordinates, never authored
    // from a string (amendment §8.1). These tests model one capability per
    // action name, so the action becomes the program coordinate.
    if s.is_empty() {
        return None;
    }
    Some(grapt_types::CapabilityId::derive(
        &grapt_types::ResolvedCapability::new(
            grapt_types::EffectClass::ToolInvoke,
            Some(grapt_types::ProgramIdentity::new(
                grapt_types::PathIdentity::canonical(s),
                Vec::new(),
                grapt_types::PathIdentity::Unresolvable,
                Vec::new(),
            )),
            grapt_types::FilesystemAuthority::default(),
            grapt_types::EgressAuthority::default(),
            grapt_types::RealizedEnforcement::HarnessOnly,
        ),
    ))
}

// ---- witnesses --------------------------------------------------------------

#[test]
fn duplicate_and_conflicting_keys_are_typed_rejections() -> TestResult {
    // Credential: same provider, different fingerprint => conflict.
    let mut b = SnapshotBuilder::new();
    b.credential(CredentialFingerprint {
        provider: provider("anthropic"),
        record: match grapt_types::CredentialRecordId::new("cred-one") {
            Some(v) => v,
            None => panic!("record"),
        },
        version: match grapt_types::CredentialVersion::new("1") {
            Some(v) => v,
            None => panic!("version"),
        },
    })?;
    let conflict = b
        .credential(CredentialFingerprint {
            provider: provider("anthropic"),
            record: match grapt_types::CredentialRecordId::new("cred-two") {
                Some(v) => v,
                None => panic!("record"),
            },
            version: match grapt_types::CredentialVersion::new("1") {
                Some(v) => v,
                None => panic!("version"),
            },
        })
        .map(|_| ());
    assert_eq!(
        conflict,
        Err(PolicyError::ConflictingContributor {
            class: "credential",
            key: "anthropic".to_owned()
        })
    );

    // Credential: same provider, same fingerprint => duplicate.
    let mut b2 = SnapshotBuilder::new();
    b2.credential(CredentialFingerprint {
        provider: provider("anthropic"),
        record: match grapt_types::CredentialRecordId::new("cred-one") {
            Some(v) => v,
            None => panic!("record"),
        },
        version: match grapt_types::CredentialVersion::new("1") {
            Some(v) => v,
            None => panic!("version"),
        },
    })?;
    let dup = b2
        .credential(CredentialFingerprint {
            provider: provider("anthropic"),
            record: match grapt_types::CredentialRecordId::new("cred-one") {
                Some(v) => v,
                None => panic!("record"),
            },
            version: match grapt_types::CredentialVersion::new("1") {
                Some(v) => v,
                None => panic!("version"),
            },
        })
        .map(|_| ());
    assert_eq!(
        dup,
        Err(PolicyError::DuplicateContributor {
            class: "credential",
            key: "anthropic".to_owned()
        })
    );
    Ok(())
}

#[test]
fn config_precedence_conflicts_are_typed_and_order_independent() -> TestResult {
    let authority = authority("v1", b"owner-held");
    let a = ConfigLayer::new(
        &authority,
        5,
        ConfigSource::GlobalFile,
        &Secret::new(b"A".to_vec()),
    );
    let b = ConfigLayer::new(
        &authority,
        5,
        ConfigSource::ProjectFile,
        &Secret::new(b"B".to_vec()),
    );

    let mut forward = SnapshotBuilder::new();
    forward.config_layer(a.clone())?;
    let e1 = forward.config_layer(b.clone()).map(|_| ());
    let mut backward = SnapshotBuilder::new();
    backward.config_layer(b.clone())?;
    let e2 = backward.config_layer(a.clone()).map(|_| ());

    assert_eq!(e1, Err(PolicyError::PrecedenceConflict { rank: 5 }));
    assert_eq!(
        e2,
        Err(PolicyError::PrecedenceConflict { rank: 5 }),
        "deterministic in either order"
    );

    // Same rank, identical content => duplicate, not precedence conflict.
    let mut dup = SnapshotBuilder::new();
    dup.config_layer(a.clone())?;
    assert_eq!(
        dup.config_layer(a.clone()).map(|_| ()),
        Err(PolicyError::DuplicateContributor {
            class: "config",
            key: "5".to_owned()
        })
    );
    Ok(())
}

#[test]
fn a_graph_without_a_model_is_rejected() -> TestResult {
    let builder = SnapshotBuilder::new();
    assert_eq!(builder.build().err(), Some(PolicyError::MissingModel));
    Ok(())
}

#[test]
fn illegal_empty_identities_are_unrepresentable() {
    assert!(ToolName::new("").is_none());
    assert!(EnvVarName::new("").is_none());
    assert!(CustomSourceName::new("").is_none());
    assert!(ProviderId::new("").is_none());
    assert!(derived_capability("").is_none());
    assert!(ServiceSlotId::new("").is_none());
    assert!(LayerName::new("").is_none());
    assert!(EffectiveGraphId::new("").is_none());
    assert!(grapt_types::ModelId::new("").is_none());
    assert!(AuthorityRealm::new("").is_none());
    assert!(KeyVersion::new("").is_none());
}

// ---- §8.4: the keyed contribution identity ---------------------------------

#[test]
fn a_rotated_environment_value_is_observable_as_a_typed_conflict() -> TestResult {
    // The reason blanking the field was refused: the graph must still SEE that
    // an environment value changed. With a fixed placeholder both calls below
    // would be duplicates and the rotation would be invisible.
    let authority = authority("v1", b"owner-held");

    let mut rotated = SnapshotBuilder::new();
    rotated.environment(EnvExposure::new(
        &authority,
        env_name("GRAPT_TOKEN"),
        &Secret::new(b"old-value".to_vec()),
    ))?;
    assert_eq!(
        rotated
            .environment(EnvExposure::new(
                &authority,
                env_name("GRAPT_TOKEN"),
                &Secret::new(b"new-value".to_vec()),
            ))
            .map(|_| ()),
        Err(PolicyError::ConflictingContributor {
            class: "environment",
            key: "GRAPT_TOKEN".to_owned()
        })
    );

    // …and an UNCHANGED value is a duplicate, not a conflict. Without this the
    // test above would also pass if every exposure were simply unequal.
    let mut unchanged = SnapshotBuilder::new();
    unchanged.environment(EnvExposure::new(
        &authority,
        env_name("GRAPT_TOKEN"),
        &Secret::new(b"old-value".to_vec()),
    ))?;
    assert_eq!(
        unchanged
            .environment(EnvExposure::new(
                &authority,
                env_name("GRAPT_TOKEN"),
                &Secret::new(b"old-value".to_vec()),
            ))
            .map(|_| ()),
        Err(PolicyError::DuplicateContributor {
            class: "environment",
            key: "GRAPT_TOKEN".to_owned()
        })
    );
    Ok(())
}

#[test]
fn rotating_the_identity_authority_changes_every_contribution_it_touches() {
    let value = Secret::new(b"unchanged-value".to_vec());
    let baseline = authority("v1", b"key-one");

    // A declared version bump, key material held constant…
    let versioned = authority("v2", b"key-one");
    assert_ne!(
        EnvExposure::new(&baseline, env_name("GRAPT_TOKEN"), &value),
        EnvExposure::new(&versioned, env_name("GRAPT_TOKEN"), &value),
        "a declared key-version rotation must change the contribution"
    );

    // …and new key material under an unchanged declared version. Both are
    // required: a mint that ignored the key would pass the first, and one that
    // ignored the version would pass the second.
    let rekeyed = authority("v1", b"key-two");
    assert_ne!(
        EnvExposure::new(&baseline, env_name("GRAPT_TOKEN"), &value),
        EnvExposure::new(&rekeyed, env_name("GRAPT_TOKEN"), &value),
        "new key material must change the contribution even under one version"
    );

    // The same for a config layer, whose oracle was the second half of the
    // finding and is not covered by any assertion above.
    assert_ne!(
        ConfigLayer::new(&baseline, 5, ConfigSource::ProjectFile, &value),
        ConfigLayer::new(&rekeyed, 5, ConfigSource::ProjectFile, &value),
        "a config layer's content identity is keyed too"
    );
}

#[test]
fn one_authority_is_content_addressed_and_two_are_not_comparable() {
    // Content-addressing under ONE stable authority is the property a
    // per-snapshot or random key would have destroyed: two independently
    // resolved graphs over identical authority inputs must agree.
    let authority = authority("v1", b"owner-held");
    let value = Secret::new(b"same-value".to_vec());
    assert_eq!(
        EnvExposure::new(&authority, env_name("GRAPT_TOKEN"), &value),
        EnvExposure::new(&authority, env_name("GRAPT_TOKEN"), &value)
    );

    // A second authority in a different realm shares no identity space, so a
    // contribution cannot be carried between them or compared across them.
    let foreign_realm = match AuthorityRealm::new("some-other-realm") {
        Some(v) => v,
        None => panic!("realm"),
    };
    let foreign_version = match KeyVersion::new("v1") {
        Some(v) => v,
        None => panic!("key version"),
    };
    let foreign = ContributionAuthority::new(
        foreign_realm,
        foreign_version,
        Secret::new(b"owner-held".to_vec()),
    );
    assert_ne!(
        EnvExposure::new(&authority, env_name("GRAPT_TOKEN"), &value),
        EnvExposure::new(&foreign, env_name("GRAPT_TOKEN"), &value)
    );
}
