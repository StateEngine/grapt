//! Receipt-shaped acceptance witnesses for the effective-authority foundation.
//!
//! Released from `tests/pending/` by L2 MODEL-REGISTRY-ISSUANCE, through the
//! real issuance path — see `support/mod.rs` for why that is not a widening.
//!
//! Split from the contributor-shaped witnesses because the two together, with
//! their shared support surface, would have put one file within a few lines of
//! the `no_god_files` ceiling. The bar is that no file sits within 200 lines of
//! it, and compressing the reasoning to fit is the thing the bar exists to
//! prevent.

use crate::support;

use grapt_policy::{
    CredentialFingerprint, EnvExposure, PermissionDecision, PermissionScope, ToolVisibility,
};
use support::{
    baseline, capability, env_exposure, layer, provider, session, tool_name, TestResult,
};

#[test]
fn receipt_is_content_addressed_and_well_formed() -> TestResult {
    let receipt = baseline().snapshot()?.receipt()?;

    // Well formed: lowercase SHA-256 hex, and the identity is exactly that digest
    // under the `efg_` prefix.
    assert_eq!(receipt.digest_hex().len(), 64, "sha-256 hex");
    assert!(
        receipt
            .digest_hex()
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f')),
        "the digest must render as lowercase hex only"
    );
    assert_eq!(
        receipt.id().as_str(),
        format!("efg_{}", receipt.digest_hex())
    );

    // Content addressed, half one: the same authority inputs resolve to the same
    // receipt. This is what a per-snapshot or randomly keyed contribution identity
    // would have destroyed, so it is worth stating here and not only at the mint.
    //
    // It is also, since L2, what proves the catalog GENERATION stays out of the
    // graph preimage: the two snapshots are issued from two separate one-row
    // releases, so a receipt that folded in issuance identity would differ here.
    assert_eq!(baseline().snapshot()?.receipt()?, receipt);

    // Content addressed, half two: a semantically different graph is a different
    // receipt. Without it the assertion above is satisfied by a constant.
    let mut changed = baseline();
    changed.environment = vec![env_exposure("PATH", b"/opt/bin")];
    assert_ne!(changed.snapshot()?.receipt()?, receipt);
    Ok(())
}

#[test]
fn contributor_insertion_order_does_not_change_the_receipt() -> TestResult {
    let mut reordered = baseline();
    reordered.config.reverse();
    reordered.credentials.reverse();
    reordered.services.reverse();
    reordered.permissions.reverse();
    reordered.tools.reverse();
    reordered.environment.reverse();
    // Constructor-only contributors (§8.4). Both sides mint through the one
    // shared authority, so the two exposures are equal and only their ORDER
    // differs — the property under test.
    reordered
        .environment
        .push(env_exposure("HOME", b"/home/example"));

    let mut same_but_forward = baseline();
    same_but_forward
        .environment
        .push(env_exposure("HOME", b"/home/example"));

    assert_eq!(
        reordered.receipt_id()?,
        same_but_forward.receipt_id()?,
        "equal contributors in any order must hash equally"
    );
    Ok(())
}

#[test]
fn config_provenance_input_order_is_irrelevant_to_the_resolved_graph() -> TestResult {
    let forward = baseline();
    let mut backward = baseline();
    backward.config.reverse();
    assert_eq!(forward.receipt_id()?, backward.receipt_id()?);
    Ok(())
}

#[test]
fn the_graph_is_immutable_and_its_receipt_is_stable() -> TestResult {
    let snapshot = baseline().snapshot()?;
    let first = snapshot.receipt()?;
    let second = snapshot.receipt()?;
    assert_eq!(first, second, "receipt is a pure function of the snapshot");

    let rebuilt = baseline().snapshot()?.receipt()?;
    assert_eq!(
        first.id(),
        rebuilt.id(),
        "equal contributors, equal identity"
    );
    Ok(())
}

#[test]
fn delimiter_adversaries_cannot_forge_an_equal_receipt() -> TestResult {
    // permission: ("ab", layer "c") vs ("a", layer "bc").
    let mut left = baseline();
    left.permissions = vec![PermissionDecision {
        capability: capability("ab"),
        authority: layer("c"),
        scope: PermissionScope::Once,
    }];
    let mut right = baseline();
    right.permissions = vec![PermissionDecision {
        capability: capability("a"),
        authority: layer("bc"),
        scope: PermissionScope::Once,
    }];
    assert_ne!(left.receipt_id()?, right.receipt_id()?, "permission split");

    // tool: ("ab","c") vs ("a","bc")
    let mut lt = baseline();
    lt.tools = vec![ToolVisibility {
        tool: tool_name("ab"),
        required: capability("c"),
        visible: true,
    }];
    let mut rt = baseline();
    rt.tools = vec![ToolVisibility {
        tool: tool_name("a"),
        required: capability("bc"),
        visible: true,
    }];
    assert_ne!(lt.receipt_id()?, rt.receipt_id()?, "tool split");
    Ok(())
}

#[test]
fn secret_material_never_reaches_the_receipt_or_any_debug() -> TestResult {
    // The body preserved in `pending/` recorded that the version captured at
    // `bcff5d18` could not have witnessed this law: it rendered a `Debug` for a
    // type that deliberately has none, asserted an uppercase sentinel was absent
    // from lowercase hex, and built a `Secret` it handed to nothing. This is the
    // corrected body, now released.
    let sentinel = b"SENTINEL_TOKEN_do_not_leak_9d3f";

    let realm = match grapt_policy::AuthorityRealm::new("grapt-pending") {
        Some(v) => v,
        None => panic!("realm"),
    };
    let key_version = match grapt_policy::KeyVersion::new("v1") {
        Some(v) => v,
        None => panic!("key version"),
    };
    let authority = grapt_policy::ContributionAuthority::new(
        realm,
        key_version,
        grapt_policy::Secret::new(b"owner-held-key".to_vec()),
    );

    let mut parts = baseline();
    // The credential contributor names the RECORD and VERSION; rotation is
    // observed through the version and nothing is derived from the material.
    parts.credentials = vec![CredentialFingerprint {
        provider: provider("anthropic"),
        record: match grapt_types::CredentialRecordId::new("cred-sentinel") {
            Some(v) => v,
            None => panic!("record"),
        },
        version: match grapt_types::CredentialVersion::new("1") {
            Some(v) => v,
            None => panic!("version"),
        },
    }];
    // The environment contributor is the one that actually carries the value, so
    // it is the one that must be in the graph for this witness to mean anything.
    parts.environment = vec![EnvExposure::new(
        &authority,
        match grapt_policy::EnvVarName::new("GRAPT_TOKEN") {
            Some(v) => v,
            None => panic!("env var name"),
        },
        &grapt_policy::Secret::new(sentinel.to_vec()),
    )];
    let snapshot = parts.snapshot()?;
    let receipt = snapshot.receipt()?;

    // The digest an attacker would compute from a guessed value. Asserting the
    // absence of the MATERIAL is not the law — the material never appeared even
    // in the defective implementation. The law is that nothing recomputable from
    // a candidate appears, so the assertion is over the UNKEYED DIGEST.
    let guessable: String = {
        use sha2::Digest as _;
        let raw: [u8; 32] = sha2::Sha256::digest(sentinel).into();
        raw.iter().map(|byte| format!("{byte:02x}")).collect()
    };
    assert_ne!(
        receipt.digest_hex(),
        guessable,
        "the graph digest must not be derivable from the value"
    );
    assert!(!receipt.digest_hex().contains(&guessable));
    assert!(!receipt.id().as_str().contains(&guessable));

    // A second graph over the SAME authority and the SAME value must produce the
    // same receipt: content-addressing survives the keyed identity. Without this
    // the assertions above would also pass for a random receipt.
    let mut same = baseline();
    same.credentials = parts.credentials.clone();
    same.environment = vec![EnvExposure::new(
        &authority,
        match grapt_policy::EnvVarName::new("GRAPT_TOKEN") {
            Some(v) => v,
            None => panic!("env var name"),
        },
        &grapt_policy::Secret::new(sentinel.to_vec()),
    )];
    assert_eq!(same.snapshot()?.receipt()?, receipt);

    // …and a CHANGED value must move it, so the rotation signal is intact.
    let mut rotated = baseline();
    rotated.credentials = parts.credentials.clone();
    rotated.environment = vec![EnvExposure::new(
        &authority,
        match grapt_policy::EnvVarName::new("GRAPT_TOKEN") {
            Some(v) => v,
            None => panic!("env var name"),
        },
        &grapt_policy::Secret::new(b"a-different-value".to_vec()),
    )];
    assert_ne!(rotated.snapshot()?.receipt()?, receipt);

    // `snapshot` is absent from this list on purpose: the authority snapshot has
    // no `Debug`, which is a stronger guarantee than a redacted one and is
    // witnessed by probe 49c rather than here.
    for rendered in [
        format!("{:?}", grapt_policy::Secret::new(sentinel.to_vec())),
        format!("{authority:?}"),
        format!("{receipt:?}"),
    ] {
        assert!(
            !rendered.contains("SENTINEL"),
            "secret leaked into a Debug rendering: {rendered}"
        );
        assert!(
            !rendered.contains(&guessable),
            "an unkeyed digest of the secret leaked into a Debug rendering: {rendered}"
        );
    }
    // The session id in the shared support surface is unused by this file; the
    // helper stays shared because the contributor witnesses need it.
    let _ = session;
    Ok(())
}
