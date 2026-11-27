//! Contributor-shaped acceptance witnesses for the effective-authority
//! foundation.
//!
//! Released from `tests/pending/` by L2 MODEL-REGISTRY-ISSUANCE, through the
//! real issuance path — see `support/mod.rs`.

use crate::support;

use grapt_policy::{
    ConfigSource, CustomSourceName, PermissionDecision, PermissionScope, PolicyError,
    ServiceActivation, ServiceKind, SnapshotBuilder, ToolVisibility,
};
use grapt_types::ServiceGeneration;
use support::{
    baseline, capability, config_layer, coordinates, env_exposure, fallback_authority, issue,
    layer, project, registry, session, slot, tool_name, TestResult,
};

#[test]
fn every_contributor_class_changes_the_receipt_when_semantically_changed() -> TestResult {
    let base = baseline().receipt_id()?;

    // model
    let mut m = baseline();
    m.model = coordinates("anthropic", "claude-opus-5", Some("2026"));
    assert_ne!(base, m.receipt_id()?, "model change");

    // model variant present vs absent (absent != empty string)
    let mut mv = baseline();
    mv.model = coordinates("anthropic", "claude-fable-5", None);
    assert_ne!(base, mv.receipt_id()?, "model variant presence");

    // config — the layer's content is a keyed identity on a private field, so
    // the element is replaced rather than mutated. Rank and source are held
    // identical, so the content is still the only variable.
    let mut c = baseline();
    c.config[1] = config_layer(10, ConfigSource::ProjectFile, b"project-config-EDITED");
    assert_ne!(base, c.receipt_id()?, "config content change");

    // credential
    let mut cr = baseline();
    cr.credentials[0].version = match grapt_types::CredentialVersion::new("2") {
        Some(v) => v,
        None => panic!("credential version"),
    };
    assert_ne!(base, cr.receipt_id()?, "credential version rotation");

    // plugin/MCP generation
    let mut s = baseline();
    s.services[0].generation = ServiceGeneration::new(4);
    assert_ne!(base, s.receipt_id()?, "service generation change");

    // permission scope (same capability + authority, different scope)
    let mut p = baseline();
    p.permissions[0].scope = PermissionScope::Rejected;
    assert_ne!(base, p.receipt_id()?, "permission scope change");

    // permission authority (same capability + scope, different authority)
    let mut pa = baseline();
    pa.permissions[0].authority = fallback_authority().clone();
    assert_ne!(base, pa.receipt_id()?, "permission authority change");

    // tool visibility
    let mut t = baseline();
    t.tools[0].visible = false;
    assert_ne!(base, t.receipt_id()?, "tool visibility change");

    // environment exposure value — the NAME is held at "PATH" so the changed
    // value is the only variable; a rewrite that also moved the name would
    // witness the pair, not the value.
    let mut e = baseline();
    e.environment[0] = env_exposure("PATH", b"/opt/bin");
    assert_ne!(base, e.receipt_id()?, "env value change");
    Ok(())
}

#[test]
fn plugin_and_mcp_are_distinct_classes_not_one_slot() -> TestResult {
    // Same raw slot + generation, different kind => distinct receipt AND not a
    // conflict (both admitted).
    let mut plugin = baseline();
    plugin.services = vec![ServiceActivation {
        kind: ServiceKind::Plugin,
        slot: slot("shared.slot"),
        generation: ServiceGeneration::new(1),
    }];
    let mut mcp = baseline();
    mcp.services = vec![ServiceActivation {
        kind: ServiceKind::Mcp,
        slot: slot("shared.slot"),
        generation: ServiceGeneration::new(1),
    }];
    assert_ne!(
        plugin.receipt_id()?,
        mcp.receipt_id()?,
        "kind is part of identity"
    );

    // Both kinds on the same slot coexist without conflict.
    let mut both = baseline();
    both.services = vec![
        ServiceActivation {
            kind: ServiceKind::Plugin,
            slot: slot("shared.slot"),
            generation: ServiceGeneration::new(1),
        },
        ServiceActivation {
            kind: ServiceKind::Mcp,
            slot: slot("shared.slot"),
            generation: ServiceGeneration::new(1),
        },
    ];
    both.snapshot()?; // must build: distinct conflict keys
    Ok(())
}

#[test]
fn a_custom_config_source_cannot_spoof_a_builtin_and_must_be_non_empty() -> TestResult {
    let custom_name = match CustomSourceName::new("default") {
        Some(n) => n,
        None => panic!("non-empty custom name"),
    };
    // Same rank and same content on both sides, so the discriminant is the only
    // thing that can separate them — which is exactly what this witness claims.
    let mut builtin = baseline();
    builtin.config = vec![config_layer(0, ConfigSource::Default, b"same")];
    let mut custom = baseline();
    custom.config = vec![config_layer(0, ConfigSource::Custom(custom_name), b"same")];
    assert_ne!(
        builtin.receipt_id()?,
        custom.receipt_id()?,
        "Custom(\"default\") must not encode like the built-in Default"
    );
    // An empty custom source name is unrepresentable.
    assert!(CustomSourceName::new("").is_none());
    Ok(())
}

#[test]
fn permission_authority_scope_is_part_of_the_receipt() -> TestResult {
    // Two Session grants for different sessions must differ.
    let mut sa = baseline();
    sa.permissions = vec![PermissionDecision {
        capability: capability("fs.read"),
        authority: layer("owner-floor"),
        scope: PermissionScope::Session(session("ses_alpha")),
    }];
    let mut sb = baseline();
    sb.permissions = vec![PermissionDecision {
        capability: capability("fs.read"),
        authority: layer("owner-floor"),
        scope: PermissionScope::Session(session("ses_beta")),
    }];
    assert_ne!(sa.receipt_id()?, sb.receipt_id()?, "session scope");

    // Two Always grants for different projects must differ.
    let mut pa = baseline();
    pa.permissions = vec![PermissionDecision {
        capability: capability("fs.read"),
        authority: layer("owner-floor"),
        scope: PermissionScope::Always(project("prj_one")),
    }];
    let mut pb = baseline();
    pb.permissions = vec![PermissionDecision {
        capability: capability("fs.read"),
        authority: layer("owner-floor"),
        scope: PermissionScope::Always(project("prj_two")),
    }];
    assert_ne!(pa.receipt_id()?, pb.receipt_id()?, "project scope");
    Ok(())
}

#[test]
fn tool_is_visible_answers_only_the_visibility_decision() -> TestResult {
    // The narrow query a request boundary needs: true only when a tool is both
    // listed AND visible; false when hidden or unlisted.
    let mut parts = baseline();
    parts.tools = vec![
        ToolVisibility {
            tool: tool_name("bash"),
            required: capability("shell.exec"),
            visible: true,
        },
        ToolVisibility {
            tool: tool_name("delete"),
            required: capability("fs.write"),
            visible: false,
        },
    ];
    let snap = parts.snapshot()?;
    assert!(snap.tool_is_visible(&tool_name("bash")), "listed + visible");
    assert!(
        !snap.tool_is_visible(&tool_name("delete")),
        "listed but not visible"
    );
    assert!(!snap.tool_is_visible(&tool_name("unlisted")), "unlisted");
    Ok(())
}

#[test]
fn the_model_contributor_is_bound_to_one_catalog_issuance() -> TestResult {
    // Each `registry` call authors a one-row release and loads it, so each is a
    // DIFFERENT catalog authority that happens to agree about the name — which
    // is exactly the shape a rogue release takes.
    //
    // The captured body wrote `b.model(runtime.clone())` twice. That is no longer
    // expressible and the change is the point: `ResolvedModelTarget` is not
    // `Clone`, so the idempotent re-set has to be a SECOND ISSUANCE from the same
    // registry — which is the honest version of what it was always claiming.
    let coords = coordinates("anthropic", "claude-opus-5", None);
    let runtime = registry(&coords);

    let mut b = SnapshotBuilder::new();
    b.model(issue(&runtime, &coords))?;
    b.model(issue(&runtime, &coords))?; // the same issuance re-set is idempotent

    let other_catalog = registry(&coords);
    assert_eq!(
        issue(&other_catalog, &coords).model_ref(),
        issue(&runtime, &coords).model_ref(),
        "the durable coordinates are equal — `==` cannot tell them apart"
    );
    assert_eq!(
        b.model(issue(&other_catalog, &coords)).map(|_| ()),
        Err(PolicyError::ForeignModelResolution),
        "a second catalog authority may not take over a resolved contributor"
    );

    // A genuinely different target is still the plain conflict, so the new
    // rejection has not swallowed the old one.
    let haiku = coordinates("anthropic", "claude-haiku-4-5", None);
    let haiku_registry = registry(&haiku);
    assert_eq!(
        b.model(issue(&haiku_registry, &haiku)).map(|_| ()),
        Err(PolicyError::ConflictingModel)
    );
    Ok(())
}

#[test]
fn a_superseded_issuance_cannot_enter_an_authority_graph() -> TestResult {
    // The use-time half of the staleness law, witnessed where it matters: a
    // target minted while generation 1 was current outlives the release it came
    // from, because it holds a strong clone of that snapshot. Without a check
    // here, a release withdrawn for bad family data keeps authorising every
    // target already minted from it for the life of the process.
    //
    // This is also L2's FOREIGN witness for the `MR` family: the row that removes
    // the check in `grapt-types` dies against this `grapt-policy` test, which
    // is what proves the law is carried across the crate boundary rather than
    // merely described inside the crate that states it.
    let coords = coordinates("anthropic", "claude-opus-5", None);
    let mut reg = registry(&coords);
    let issued = issue(&reg, &coords);

    // Withdraw the release the target was minted from.
    let superseded = reg.supersede(
        match grapt_types::ModelCatalogVersion::new("test-catalog-2") {
            Some(v) => v,
            None => panic!("version"),
        },
        vec![grapt_types::ModelCatalogRecord::new(
            match grapt_types::ProviderId::new("anthropic") {
                Some(v) => v,
                None => panic!("provider"),
            },
            match grapt_types::ModelId::new("claude-opus-5") {
                Some(v) => v,
                None => panic!("model"),
            },
            match grapt_types::BaseModelId::new("claude-opus-5") {
                Some(v) => v,
                None => panic!("base"),
            },
            match grapt_types::ModelFamilyId::new("anthropic") {
                Some(v) => v,
                None => panic!("family"),
            },
            None,
        )],
    );
    assert!(superseded.is_some(), "the successor release is well formed");

    let mut b = SnapshotBuilder::new();
    assert_eq!(
        b.model(issued).map(|_| ()),
        Err(PolicyError::StaleModelIssuance {
            issued_at: 1,
            current: 2
        }),
        "a retired issuance is refused by TYPE, and names both generations"
    );

    // The refusal is a THIRD thing: with no contributor set there is nothing to
    // conflict with, so neither of the other two rejections could have fired.
    let fresh = issue(&reg, &coords);
    assert!(
        b.model(fresh).is_ok(),
        "the current issuance still admits, so the refusal was about staleness \
         and not about the builder's state"
    );
    Ok(())
}
