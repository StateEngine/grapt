use super::{load_exact, row, PROVIDER};
use crate::authority::PolicyVersion;
use crate::capability::ModelTargetCapabilityId;
use crate::credential::{AccountId, CredentialVersion};
use crate::model::test_catalog::{catalog, model_ref};
use crate::model::*;

#[test]
fn a_resolved_target_derives_its_own_capability() -> Result<(), Box<dyn std::error::Error>> {
    let catalog = catalog("cat-1", &[("anthropic", "claude", "claude-opus-5")]).ok_or("catalog")?;
    let build = |account: &str, credential: &str| -> Option<ResolvedModelTarget> {
        Some(ResolvedModelTarget::resolve(
            model_ref(&catalog, "anthropic", "claude-opus-5")?,
            ModelRouteId::new("rt_1")?,
            AccountId::new(account)?,
            CredentialVersion::new(credential)?,
            PolicyVersion::new("pol_1")?,
        ))
    };

    let target = build("acct_1", "3").ok_or("target")?;
    assert_eq!(target.account().as_str(), "acct_1");
    assert_eq!(target.credential_version().as_str(), "3");

    // The capability is DERIVED, so it moves with every coordinate. An
    // unrelated capability cannot be attached to a target it does not
    // describe, and account or credential rotation resolves a different
    // target — a new receipted attempt, never a silent swap.
    let same = build("acct_1", "3").ok_or("target")?;
    assert_eq!(target.capability(), same.capability());

    let other_account = build("acct_2", "3").ok_or("target")?;
    assert_ne!(
        target.capability(),
        other_account.capability(),
        "another account is another resolved target"
    );

    let rotated = build("acct_1", "4").ok_or("target")?;
    assert_ne!(
        target.capability(),
        rotated.capability(),
        "a rotated credential version is another resolved target"
    );
    Ok(())
}

#[test]
fn every_inert_routing_coordinate_resolves_a_different_target(
) -> Result<(), Box<dyn std::error::Error>> {
    // Master §10: account is a first-class routing dimension, and capability
    // belongs to the exact resolved target. A consumer that stores only
    // "provider + model + variant" cannot tell two of these apart, which is
    // what made a prepared attempt lossy.
    let catalog = catalog(
        "cat-1",
        &[
            ("anthropic", "claude", "claude-opus-5"),
            ("anthropic", "claude", "claude-fable-5"),
        ],
    )
    .ok_or("catalog")?;
    let build = |model: &str,
                 route: &str,
                 account: &str,
                 credential: &str,
                 policy: &str|
     -> Option<ResolvedModelTarget> {
        Some(ResolvedModelTarget::resolve(
            model_ref(&catalog, "anthropic", model)?,
            ModelRouteId::new(route)?,
            AccountId::new(account)?,
            CredentialVersion::new(credential)?,
            PolicyVersion::new(policy)?,
        ))
    };
    let baseline = || build("claude-opus-5", "rt_1", "acct_1", "3", "pol_1");

    for (coordinate, mutated) in [
        (
            "model",
            build("claude-fable-5", "rt_1", "acct_1", "3", "pol_1"),
        ),
        (
            "route",
            build("claude-opus-5", "rt_2", "acct_1", "3", "pol_1"),
        ),
        (
            "account",
            build("claude-opus-5", "rt_1", "acct_2", "3", "pol_1"),
        ),
        (
            "credential version",
            build("claude-opus-5", "rt_1", "acct_1", "4", "pol_1"),
        ),
        (
            "policy version",
            build("claude-opus-5", "rt_1", "acct_1", "3", "pol_2"),
        ),
    ] {
        let mutated = mutated.ok_or(coordinate)?;
        let baseline = baseline().ok_or(coordinate)?;
        assert!(
            !baseline.describes_same_target(&mutated),
            "changing the {coordinate} must resolve a different target"
        );
        assert!(
            !baseline.is_same_issuance(&mutated),
            "and a different target is never the same issuance ({coordinate})"
        );
    }

    // The policy version is the one coordinate the capability does NOT derive
    // from — it scopes the decision rather than naming the endpoint — so it is
    // recorded here rather than left as a surprise: it changes the TARGET
    // without changing the capability, which is why a consumer must bind the
    // tuple and not the capability alone.
    let baseline = baseline().ok_or("baseline")?;
    let repolicied = build("claude-opus-5", "rt_1", "acct_1", "3", "pol_2").ok_or("target")?;
    assert_eq!(baseline.capability(), repolicied.capability());
    assert!(!baseline.describes_same_target(&repolicied));
    Ok(())
}

#[test]
fn the_same_tuple_from_another_release_is_a_foreign_issuance(
) -> Result<(), Box<dyn std::error::Error>> {
    // Two releases that agree about a target name the same target — and are
    // still two authorities. A consumer must be able to say "same target,
    // different issuance" without either answer leaking a family fact.
    let build = |release: &str| -> Option<ResolvedModelTarget> {
        let catalog = catalog(release, &[("anthropic", "claude", "claude-opus-5")])?;
        Some(ResolvedModelTarget::resolve(
            model_ref(&catalog, "anthropic", "claude-opus-5")?,
            ModelRouteId::new("rt_1")?,
            AccountId::new("acct_1")?,
            CredentialVersion::new("3")?,
            PolicyVersion::new("pol_1")?,
        ))
    };
    let first = build("cat-1").ok_or("target")?;
    let reloaded = build("cat-2").ok_or("target")?;
    assert!(
        first.describes_same_target(&reloaded),
        "the inert coordinates are identical"
    );
    assert!(
        !first.is_same_issuance(&reloaded),
        "a target resolved under another release is a foreign issuance"
    );

    // Even a byte-identical release is a distinct authority: identity is the
    // loaded snapshot, not the version text.
    let impostor = build("cat-1").ok_or("target")?;
    assert!(!first.is_same_issuance(&impostor));
    Ok(())
}

#[test]
fn a_capability_does_not_depend_on_which_process_loaded_the_catalog(
) -> Result<(), Box<dyn std::error::Error>> {
    // The snapshot is live authority and must never leak into a durable
    // capability: two processes loading the same release must derive the
    // same key.
    let build = |version: &str| -> Option<ModelTargetCapabilityId> {
        let catalog = catalog(version, &[("anthropic", "claude", "claude-opus-5")])?;
        Some(
            ResolvedModelTarget::resolve(
                model_ref(&catalog, "anthropic", "claude-opus-5")?,
                ModelRouteId::new("rt_1")?,
                AccountId::new("acct_1")?,
                CredentialVersion::new("3")?,
                PolicyVersion::new("pol_1")?,
            )
            .capability()
            .clone(),
        )
    };
    assert_eq!(build("cat-1").ok_or("cap")?, build("cat-1").ok_or("cap")?);
    Ok(())
}

// ---------------------------------------------------------------------
// L1 MODEL-TARGET-CAPABILITY-IDENTITY
//
// `test_catalog::record` fixes base == model and variant == None, which is
// right for the common case and wrong for every law below: these witnesses
// vary exactly one catalog fact at a time, so they need rows whose base,
// family and variant are set independently.
//
// The inert routing coordinates are held at these constants everywhere except
// the two witnesses that vary them on purpose, so a reader can see at a glance
// which coordinate a given witness is actually moving.
// ---------------------------------------------------------------------

const ROUTE: &str = "rt_1";
const ACCOUNT: &str = "acct_1";
const CREDENTIAL: &str = "3";

/// Resolve one row of a loaded snapshot against caller-chosen inert routing
/// coordinates, so a witness can hold everything but one fact constant.
fn resolve_exact(
    catalog: &ModelCatalog,
    provider: &str,
    model: &str,
    variant: Option<&str>,
    route: &str,
    account: &str,
    credential: &str,
) -> Option<ResolvedModelTarget> {
    let variant = match variant {
        None => None,
        Some(raw) => Some(ModelVariantId::new(raw)?),
    };
    let entry = catalog.entry(
        &ProviderId::new(provider)?,
        &ModelId::new(model)?,
        variant.as_ref(),
    )?;
    Some(ResolvedModelTarget::resolve(
        ModelRef::from_catalog(entry),
        ModelRouteId::new(route)?,
        AccountId::new(account)?,
        CredentialVersion::new(credential)?,
        PolicyVersion::new("pol_1")?,
    ))
}

/// Resolve under the constant inert coordinates. Every witness that is NOT
/// about routing uses this, so two capabilities it compares can differ only by
/// a catalog fact.
fn resolved(
    catalog: &ModelCatalog,
    model: &str,
    variant: Option<&str>,
) -> Option<ResolvedModelTarget> {
    resolve_exact(
        catalog, PROVIDER, model, variant, ROUTE, ACCOUNT, CREDENTIAL,
    )
}

#[test]
fn an_absent_variant_and_a_literal_dash_variant_derive_different_capabilities(
) -> Result<(), Box<dyn std::error::Error>> {
    // The absent variant used to encode as the literal value `-`, which
    // `ModelVariantId::new` accepts. These two rows are DISTINCT lookup
    // coordinates — `coordinates()` compares an `Option`, so `ModelCatalog`
    // admits both in one release — and under that encoding they emitted the
    // identical `variant=-|`, so they derived ONE capability.
    //
    // EVERY OTHER CAPABILITY COORDINATE IS HELD EQUAL HERE, and that is the
    // whole witness. Provider, model, base model, family, route, account,
    // credential version and catalog version are the same for both, so the only
    // thing that can separate the two capabilities is how presence is encoded.
    // An earlier revision of this witness also varied the base model and the
    // family; both remain in `canonical_target_form`, so the inequality held for
    // a reason that had nothing to do with the law, and the MC01 mutant that
    // restores the sentinel SURVIVED it (governed run
    // 20260818030252-512c2bbf058d40a3). A witness whose assertion is true for
    // the wrong reason is not a witness.
    //
    // The direction is what made it serious. Equality on the capability IS
    // the authorization comparison, so the collision read as "same authority",
    // not as a refusal.
    let catalog = load_exact(
        "cat-1",
        vec![
            row("claude-opus-5", "claude-opus-5", "claude", None).ok_or("absent-variant row")?,
            row("claude-opus-5", "claude-opus-5", "claude", Some("-")).ok_or("dash-variant row")?,
        ],
    )
    .ok_or("two distinct coordinates are not a self-contradicting release")?;

    let absent = resolved(&catalog, "claude-opus-5", None).ok_or("absent target")?;
    let dash = resolved(&catalog, "claude-opus-5", Some("-")).ok_or("dash target")?;

    assert_ne!(
        absent.capability(),
        dash.capability(),
        "an absent variant and a variant literally spelled `-` differ in NOTHING \
         but variant presence, and must still never share one capability"
    );
    Ok(())
}

#[test]
fn two_variants_of_one_endpoint_derive_different_capabilities(
) -> Result<(), Box<dyn std::error::Error>> {
    let catalog = load_exact(
        "cat-1",
        vec![
            row("claude-opus-5", "claude-opus-5", "claude", Some("x")).ok_or("row x")?,
            row("claude-opus-5", "claude-opus-5", "claude", Some("y")).ok_or("row y")?,
        ],
    )
    .ok_or("catalog")?;

    let x = resolved(&catalog, "claude-opus-5", Some("x")).ok_or("target x")?;
    let y = resolved(&catalog, "claude-opus-5", Some("y")).ok_or("target y")?;

    assert_ne!(
        x.capability(),
        y.capability(),
        "the variant VALUE is a capability coordinate, not just its presence"
    );
    Ok(())
}

#[test]
fn the_canonical_form_spells_variant_presence_structurally(
) -> Result<(), Box<dyn std::error::Error>> {
    // The exact bytes, because the law is about the ENCODING and not only
    // about the inequality it produces: presence is a label position, which no
    // value can occupy. Nothing here prints the form — it spells the two sealed
    // catalog facts, and a failure message is a read path like any other.
    let catalog = load_exact(
        "cat-1",
        vec![
            row("claude-opus-5", "claude-opus-5", "claude", None).ok_or("absent row")?,
            row("claude-opus-5", "claude-opus-5", "claude", Some("preview"))
                .ok_or("present row")?,
        ],
    )
    .ok_or("catalog")?;

    let absent = resolved(&catalog, "claude-opus-5", None).ok_or("absent target")?;
    let present = resolved(&catalog, "claude-opus-5", Some("preview")).ok_or("present target")?;

    assert!(
        absent
            .capability()
            .as_str()
            .contains("|variant_present=0|route="),
        "an absent variant is one presence field and no value field at all"
    );
    assert!(
        present
            .capability()
            .as_str()
            .contains("|variant_present=1|variant=preview|route="),
        "a present variant sets the presence bit and then carries its value"
    );
    Ok(())
}

#[test]
fn two_variants_in_one_family_still_derive_distinct_capabilities(
) -> Result<(), Box<dyn std::error::Error>> {
    // This was the direction guard for the dash witness, back when that one also
    // varied the base model and the family. The R2 correction moved the dash
    // witness onto exactly this hostile pair — it had to, because `-` is the only
    // variant value the old sentinel encoding could collide with absence — so the
    // two now carry the SAME fixture and differ only in prose.
    //
    // It is left standing rather than quietly deleted: retiring a witness is not
    // inside this correction's authority, and a duplicate that is disclosed is
    // safer than a deletion that is not. Review owns the call on whether it is
    // retired or re-aimed in a successor.
    let catalog = load_exact(
        "cat-1",
        vec![
            row("claude-opus-5", "claude-opus-5", "claude", None).ok_or("absent row")?,
            row("claude-opus-5", "claude-opus-5", "claude", Some("-")).ok_or("dash row")?,
        ],
    )
    .ok_or("catalog")?;

    let absent = resolved(&catalog, "claude-opus-5", None).ok_or("absent target")?;
    let dash = resolved(&catalog, "claude-opus-5", Some("-")).ok_or("dash target")?;

    assert_ne!(
        absent.capability(),
        dash.capability(),
        "variant presence alone separates two targets, with every catalog fact \
         held equal"
    );
    Ok(())
}

#[test]
fn a_different_base_model_derives_a_different_capability() -> Result<(), Box<dyn std::error::Error>>
{
    // Two releases must disagree about the base model for one coordinate, and
    // one release that did so would be refused, so this needs two snapshots.
    // Neither the snapshot nor the release version enters the capability, so
    // the only surviving difference is the base model.
    let first = load_exact(
        "cat-1",
        vec![row("claude-opus-5", "claude-opus-5", "claude", None).ok_or("first row")?],
    )
    .ok_or("first catalog")?;
    let second = load_exact(
        "cat-1",
        vec![row("claude-opus-5", "claude-opus-5-20260101", "claude", None).ok_or("second row")?],
    )
    .ok_or("second catalog")?;

    let a = resolved(&first, "claude-opus-5", None).ok_or("first target")?;
    let b = resolved(&second, "claude-opus-5", None).ok_or("second target")?;

    assert_ne!(
        a.capability(),
        b.capability(),
        "the base model is catalog truth the capability derives from"
    );
    Ok(())
}

#[test]
fn a_different_family_derives_a_different_capability() -> Result<(), Box<dyn std::error::Error>> {
    let first = load_exact(
        "cat-1",
        vec![row("claude-opus-5", "claude-opus-5", "claude", None).ok_or("first row")?],
    )
    .ok_or("first catalog")?;
    let second = load_exact(
        "cat-1",
        vec![row("claude-opus-5", "claude-opus-5", "claude-preview", None).ok_or("second row")?],
    )
    .ok_or("second catalog")?;

    let a = resolved(&first, "claude-opus-5", None).ok_or("first target")?;
    let b = resolved(&second, "claude-opus-5", None).ok_or("second target")?;

    assert_ne!(
        a.capability(),
        b.capability(),
        "the family is the boundary `agent default` may not cross and the \
         capability must move with it"
    );
    Ok(())
}

#[test]
fn a_different_model_or_route_derives_a_different_capability(
) -> Result<(), Box<dyn std::error::Error>> {
    // The base model is held EQUAL across the two rows so this witness
    // isolates the model coordinate rather than riding the base.
    let catalog = load_exact(
        "cat-1",
        vec![
            row("claude-opus-5", "shared-base", "claude", None).ok_or("opus row")?,
            row("claude-fable-5", "shared-base", "claude", None).ok_or("fable row")?,
        ],
    )
    .ok_or("catalog")?;

    let opus = resolved(&catalog, "claude-opus-5", None).ok_or("opus target")?;
    let fable = resolved(&catalog, "claude-fable-5", None).ok_or("fable target")?;
    assert_ne!(
        opus.capability(),
        fable.capability(),
        "the model is a capability coordinate even when the base is shared"
    );

    let rerouted = resolve_exact(
        &catalog,
        PROVIDER,
        "claude-opus-5",
        None,
        "rt_2",
        ACCOUNT,
        CREDENTIAL,
    )
    .ok_or("rerouted target")?;
    assert_ne!(
        opus.capability(),
        rerouted.capability(),
        "the route is a capability coordinate"
    );
    Ok(())
}

#[test]
fn the_canonical_form_escapes_backslash_pipe_and_equals() -> Result<(), Box<dyn std::error::Error>>
{
    // Why this reads the BYTES rather than forging a collision.
    //
    // Every field is `label=value|` with a fixed label at a fixed position and
    // — since variant presence became structural — a fixed field COUNT. Under
    // those three conditions the concatenation is already injective without any
    // escaping, so no pair of distinct tuples can be made to encode alike by
    // moving separators around, and a forged-collision witness would pass
    // against an unescaped form. The escapes are the guard that keeps
    // injectivity from depending on the field count staying fixed, and the only
    // honest witness for a guard nothing currently needs is the guard itself.
    let catalog = load_exact(
        "cat-1",
        vec![row("claude-opus-5", "claude-opus-5", "claude", None).ok_or("row")?],
    )
    .ok_or("catalog")?;

    let adversary = resolve_exact(
        &catalog,
        PROVIDER,
        "claude-opus-5",
        None,
        "r\\a|b=c",
        ACCOUNT,
        CREDENTIAL,
    )
    .ok_or("adversary target")?;

    assert!(
        adversary
            .capability()
            .as_str()
            .contains("route=r\\\\a\\pb\\ec|"),
        "a backslash escapes to a doubled backslash, a pipe to backslash-p and \
         an equals to backslash-e"
    );
    Ok(())
}
