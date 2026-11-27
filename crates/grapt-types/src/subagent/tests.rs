use super::*;
use crate::model::test_catalog::{catalog, model_ref};
use crate::model::ModelCatalog;

fn fixture() -> Option<ModelCatalog> {
    catalog(
        "cat-1",
        &[
            ("anthropic", "claude", "claude-opus-5"),
            ("anthropic", "claude", "claude-haiku-4-5"),
            ("anthropic", "other", "other-1"),
            ("openai", "claude", "claude-opus-5"),
            ("openai", "gpt", "gpt-x"),
            ("openai", "gpt", "gpt-y"),
        ],
    )
}

fn surface() -> Option<UserSelectionSurface> {
    Some(UserSelectionSurface::establish(
        AuthenticatedClientId::new("client-1")?,
        SelectorVersion::new("sel-1")?,
    ))
}

fn selection(surface: &UserSelectionSurface, target: ModelRef) -> Option<UserModelSelection> {
    Some(surface.select(
        RequestId::new("req-1")?,
        DecisionNonce::new("nonce-1")?,
        target,
    ))
}

#[test]
fn agent_default_refuses_a_cross_family_child() -> Result<(), Box<dyn std::error::Error>> {
    let catalog = fixture().ok_or("catalog")?;
    let parent = model_ref(&catalog, "anthropic", "claude-opus-5").ok_or("parent")?;
    let other_family = model_ref(&catalog, "anthropic", "other-1").ok_or("child")?;

    assert!(
        !child_target_is_admissible(
            &parent,
            &other_family,
            &SubagentModelPreference::AgentDefault,
            DelegationSelectionProvenance::SameFamilyEconomy,
        ),
        "economy routing may never leave the family"
    );
    Ok(())
}

#[test]
fn agent_default_refuses_a_cross_provider_child() -> Result<(), Box<dyn std::error::Error>> {
    let catalog = fixture().ok_or("catalog")?;
    let parent = model_ref(&catalog, "anthropic", "claude-opus-5").ok_or("parent")?;
    let other_provider = model_ref(&catalog, "openai", "claude-opus-5").ok_or("child")?;
    assert!(
        !child_target_is_admissible(
            &parent,
            &other_provider,
            &SubagentModelPreference::AgentDefault,
            DelegationSelectionProvenance::InheritedExact,
        ),
        "the provider boundary is as hard as the family boundary"
    );
    Ok(())
}

#[test]
fn same_family_economy_is_allowed_under_agent_default() -> Result<(), Box<dyn std::error::Error>> {
    let catalog = fixture().ok_or("catalog")?;
    let parent = model_ref(&catalog, "anthropic", "claude-opus-5").ok_or("parent")?;
    let cheaper = model_ref(&catalog, "anthropic", "claude-haiku-4-5").ok_or("child")?;
    assert!(child_target_is_admissible(
        &parent,
        &cheaper,
        &SubagentModelPreference::AgentDefault,
        DelegationSelectionProvenance::SameFamilyEconomy,
    ));
    Ok(())
}

#[test]
fn a_user_selection_admits_only_the_exact_selected_target() -> Result<(), Box<dyn std::error::Error>>
{
    let catalog = fixture().ok_or("catalog")?;
    let surface = surface().ok_or("surface")?;
    let parent = model_ref(&catalog, "anthropic", "claude-opus-5").ok_or("parent")?;
    let selected = model_ref(&catalog, "openai", "gpt-x").ok_or("child")?;
    let preference = SubagentModelPreference::user_selected(
        selection(&surface, selected.clone()).ok_or("selection")?,
    );

    assert!(child_target_is_admissible(
        &parent,
        &selected,
        &preference,
        DelegationSelectionProvenance::UserSelected,
    ));

    // A DIFFERENT cross-family target under the same selection is refused:
    // the selection authorizes one target, not a family of them.
    let substituted = model_ref(&catalog, "openai", "gpt-y").ok_or("child")?;
    assert!(
        !child_target_is_admissible(
            &parent,
            &substituted,
            &preference,
            DelegationSelectionProvenance::UserSelected,
        ),
        "a near-miss target is not the user's selection"
    );
    Ok(())
}

#[test]
fn only_an_explicit_user_selection_crosses_the_boundary() -> Result<(), Box<dyn std::error::Error>>
{
    let catalog = fixture().ok_or("catalog")?;
    let surface = surface().ok_or("surface")?;
    let parent = model_ref(&catalog, "anthropic", "claude-opus-5").ok_or("parent")?;
    let elsewhere = model_ref(&catalog, "openai", "gpt-x").ok_or("child")?;
    let preference = SubagentModelPreference::user_selected(
        selection(&surface, elsewhere.clone()).ok_or("selection")?,
    );

    assert!(child_target_is_admissible(
        &parent,
        &elsewhere,
        &preference,
        DelegationSelectionProvenance::UserSelected,
    ));

    // The same target with a policy-shaped provenance is still refused:
    // policy may constrain a selection but cannot manufacture one.
    assert!(
        !child_target_is_admissible(
            &parent,
            &elsewhere,
            &preference,
            DelegationSelectionProvenance::ProjectPolicyConstrained,
        ),
        "policy cannot supply the cross-family authority itself"
    );
    Ok(())
}

#[test]
fn user_selected_provenance_requires_a_user_selection() {
    assert!(!DelegationSelectionProvenance::UserSelected
        .is_admissible_under(&SubagentModelPreference::AgentDefault));
    assert!(DelegationSelectionProvenance::InheritedExact
        .is_admissible_under(&SubagentModelPreference::AgentDefault));
}

#[test]
fn a_selection_carries_the_authoritys_client_not_a_caller_supplied_one(
) -> Result<(), Box<dyn std::error::Error>> {
    let catalog = fixture().ok_or("catalog")?;
    let surface = surface().ok_or("surface")?;
    let target = model_ref(&catalog, "openai", "gpt-x").ok_or("target")?;
    let selection = selection(&surface, target.clone()).ok_or("selection")?;

    // `select` takes no client and no selector version: both are read off
    // the authority, so a selection cannot be attributed to a client that
    // did not make it.
    assert_eq!(selection.client().as_str(), "client-1");
    assert_eq!(selection.selector().as_str(), "sel-1");
    assert_eq!(selection.request().as_str(), "req-1");
    assert_eq!(selection.nonce().as_str(), "nonce-1");
    assert_eq!(selection.target(), &target);
    Ok(())
}

#[test]
fn agent_default_is_not_authority_and_carries_no_selection() {
    let default = SubagentModelPreference::AgentDefault;
    assert!(default.selection().is_none());
    assert!(default.selected_target().is_none());
    assert!(
        !default.permits_cross_family(),
        "the absence of authority never crosses the boundary"
    );
}

#[test]
fn a_selection_from_another_catalog_cannot_smuggle_a_family_claim(
) -> Result<(), Box<dyn std::error::Error>> {
    // Under a real user selection the child must EQUAL the selected target,
    // so a rogue catalog buys nothing there. Every other route through
    // `child_target_is_admissible` ends at the parent's family check, and
    // that is where a rogue release would pay off: it copies the parent's
    // real family name onto a model the runtime places elsewhere, so
    // provider and family both match by string.
    let runtime = fixture().ok_or("catalog")?;
    let rogue = catalog("cat-1", &[("anthropic", "claude", "other-1")]).ok_or("catalog")?;
    let surface = surface().ok_or("surface")?;

    let parent = model_ref(&runtime, "anthropic", "claude-opus-5").ok_or("parent")?;
    let smuggled = model_ref(&rogue, "anthropic", "other-1").ok_or("child")?;
    assert_eq!(parent.provider(), smuggled.provider());
    assert_eq!(
        parent.family(),
        smuggled.family(),
        "the rogue release bought a matching family name"
    );
    // The runtime's honest answer: `other-1` is not in the parent's family.
    assert_ne!(
        parent.family(),
        model_ref(&runtime, "anthropic", "other-1")
            .ok_or("child")?
            .family()
    );

    assert!(
        !child_target_is_admissible(
            &parent,
            &smuggled,
            &SubagentModelPreference::AgentDefault,
            DelegationSelectionProvenance::SameFamilyEconomy,
        ),
        "`agent default` is exactly where a forged family claim would have mattered"
    );

    let preference = SubagentModelPreference::user_selected(
        selection(
            &surface,
            model_ref(&runtime, "openai", "gpt-x").ok_or("target")?,
        )
        .ok_or("selection")?,
    );
    assert!(
        !child_target_is_admissible(
            &parent,
            &smuggled,
            &preference,
            DelegationSelectionProvenance::ProjectPolicyConstrained,
        ),
        "and policy narrowing under a selection falls back to the same check"
    );
    Ok(())
}

#[test]
fn a_reused_label_at_a_new_generation_is_a_new_address() -> Result<(), Box<dyn std::error::Error>> {
    let label = SubagentLabel::new("reviewer").ok_or("label")?;
    let first = SubagentAddress::new(
        SubagentOperationId::new("op-1").ok_or("operation")?,
        Some(label.clone()),
        SubagentGeneration::FIRST,
    );
    let reused = SubagentAddress::new(
        SubagentOperationId::new("op-2").ok_or("operation")?,
        Some(label),
        SubagentGeneration::FIRST.next().ok_or("generation")?,
    );
    assert_ne!(
        first, reused,
        "a retired label cannot receive the new operation's mail"
    );
    assert_ne!(first.operation(), reused.operation());
    Ok(())
}

// ---------------------------------------------------------------------
// L2 S3 — carrying a live selection across a semantically-null reload
//
// The binding checks are the point. A `CatalogReissuanceReceipt` names only two
// generations, so on its own it would authorise carrying ANY selection across —
// a credential that answers "was this ever issued" rather than "was this issued
// FOR THIS TARGET". Each witness below removes one of the three structural
// bindings and shows the carry refused.
// ---------------------------------------------------------------------

mod carry_forward {
    use crate::authority::PolicyVersion;
    use crate::credential::{AccountId, CredentialVersion};
    use crate::model::{
        BaseModelId, ModelCatalog, ModelCatalogRecord, ModelCatalogVersion, ModelFamilyId, ModelId,
        ModelRef, ModelRegistry, ModelRouteId, ModelVariantId, ProviderId, ResolvedModelTarget,
        RoutingRequest,
    };
    use crate::subagent::{
        CarryForwardRefusal, SelectorVersion, UserModelSelection, UserSelectionAuthority,
        UserSelectionSurface,
    };
    use crate::transport::{AuthenticatedClientId, DecisionNonce, RequestId};

    type R = Result<(), Box<dyn std::error::Error>>;

    fn row(model: &str, family: &str) -> Option<ModelCatalogRecord> {
        Some(ModelCatalogRecord::new(
            ProviderId::new("anthropic")?,
            ModelId::new(model)?,
            BaseModelId::new(model)?,
            ModelFamilyId::new(family)?,
            None,
        ))
    }

    fn registry(version: &str, rows: Vec<ModelCatalogRecord>) -> Option<ModelRegistry> {
        Some(ModelRegistry::install(ModelCatalog::load(
            ModelCatalogVersion::new(version)?,
            rows,
        )?))
    }

    fn request() -> Option<RoutingRequest> {
        Some(RoutingRequest {
            route: ModelRouteId::new("rt_1")?,
            account: AccountId::new("acct_1")?,
            credential_version: CredentialVersion::new("3")?,
            policy: PolicyVersion::new("pol_1")?,
        })
    }

    fn issue(reg: &ModelRegistry, model: &str) -> Option<ResolvedModelTarget> {
        reg.issue(
            &ProviderId::new("anthropic")?,
            &ModelId::new(model)?,
            None::<&ModelVariantId>,
            request()?,
        )
        .ok()
    }

    fn selection(target: ModelRef) -> Option<UserModelSelection> {
        let surface = UserSelectionSurface::establish(
            AuthenticatedClientId::new("client-1")?,
            SelectorVersion::new("sel-1")?,
        );
        Some(surface.select(
            RequestId::new("req-1")?,
            DecisionNonce::new("nonce-1")?,
            target,
        ))
    }

    fn reload(reg: &mut ModelRegistry, version: &str, rows: Vec<ModelCatalogRecord>) -> Option<()> {
        reg.supersede(ModelCatalogVersion::new(version)?, rows)?;
        Some(())
    }

    #[test]
    fn a_null_reload_carries_the_selection_and_keeps_its_authentication() -> R {
        let mut reg =
            registry("cat-1", vec![row("claude-opus-5", "claude").ok_or("row")?]).ok_or("reg")?;
        let issued = issue(&reg, "claude-opus-5").ok_or("issue")?;
        let picked = selection(issued.model_ref().clone()).ok_or("selection")?;
        let client = picked.client().clone();
        let nonce = picked.nonce().clone();

        reload(
            &mut reg,
            "cat-1",
            vec![row("claude-opus-5", "claude").ok_or("row")?],
        )
        .ok_or("supersede")?;
        let (receipt, reissued) = reg
            .reissue(&issued, request().ok_or("request")?)
            .map_err(|_| "reissue")?;

        let carried = picked
            .carry_forward(&receipt, reissued.model_ref().clone())
            .map_err(|_| "carry")?;

        assert_eq!(carried.client(), &client, "same authenticated client");
        assert_eq!(
            carried.nonce(),
            &nonce,
            "the SAME single-use interaction, not a new one"
        );
        assert_eq!(
            carried.target().issuance_generation().get(),
            2,
            "now naming the release that replaced the one it was made against"
        );
        Ok(())
    }

    #[test]
    fn a_genuine_receipt_cannot_carry_a_different_target() -> R {
        // The binding that matters most. The receipt is real — minted by the
        // registry for the opus row — and it must still refuse to carry a
        // selection of a DIFFERENT target.
        let rows = || -> Option<Vec<ModelCatalogRecord>> {
            Some(vec![
                row("claude-opus-5", "claude")?,
                row("claude-haiku-4-5", "claude")?,
            ])
        };
        let mut reg = registry("cat-1", rows().ok_or("rows")?).ok_or("reg")?;
        let opus = issue(&reg, "claude-opus-5").ok_or("issue")?;
        let haiku = issue(&reg, "claude-haiku-4-5").ok_or("issue")?;
        let picked_haiku = selection(haiku.model_ref().clone()).ok_or("selection")?;

        reload(&mut reg, "cat-1", rows().ok_or("rows")?).ok_or("supersede")?;
        let (opus_receipt, opus_reissued) = reg
            .reissue(&opus, request().ok_or("request")?)
            .map_err(|_| "reissue")?;

        assert_eq!(
            picked_haiku
                .carry_forward(&opus_receipt, opus_reissued.model_ref().clone())
                .err(),
            Some(CarryForwardRefusal::NotThisTarget),
            "possession of a genuine receipt is not authority over another target"
        );
        Ok(())
    }

    #[test]
    fn a_receipt_that_starts_elsewhere_cannot_carry() -> R {
        // The receipt must START at the generation the selection was issued at.
        // Here the selection is made at generation 2 while the receipt describes
        // 1 to 2, so replaying it is refused.
        let mut reg =
            registry("cat-1", vec![row("claude-opus-5", "claude").ok_or("row")?]).ok_or("reg")?;
        let first = issue(&reg, "claude-opus-5").ok_or("issue")?;
        reload(
            &mut reg,
            "cat-1",
            vec![row("claude-opus-5", "claude").ok_or("row")?],
        )
        .ok_or("supersede")?;
        let (receipt, reissued) = reg
            .reissue(&first, request().ok_or("request")?)
            .map_err(|_| "reissue")?;

        // A selection made AFTER the reload: generation 2, not 1.
        let late = selection(reissued.model_ref().clone()).ok_or("selection")?;
        assert_eq!(
            late.carry_forward(&receipt, reissued.model_ref().clone())
                .err(),
            Some(CarryForwardRefusal::NotThisIssuance),
            "a receipt for 1 to 2 does not carry a selection already at 2"
        );
        Ok(())
    }

    #[test]
    fn a_target_that_is_not_at_the_carried_generation_is_refused() -> R {
        // The receipt must reach the generation the re-issued target sits at.
        // Pairing a 1 to 2 receipt with the ORIGINAL generation-1 ref is refused,
        // which is what stops a carry that never actually moved.
        let mut reg =
            registry("cat-1", vec![row("claude-opus-5", "claude").ok_or("row")?]).ok_or("reg")?;
        let first = issue(&reg, "claude-opus-5").ok_or("issue")?;
        let picked = selection(first.model_ref().clone()).ok_or("selection")?;
        reload(
            &mut reg,
            "cat-1",
            vec![row("claude-opus-5", "claude").ok_or("row")?],
        )
        .ok_or("supersede")?;
        let (receipt, _reissued) = reg
            .reissue(&first, request().ok_or("request")?)
            .map_err(|_| "reissue")?;

        assert_eq!(
            picked
                .carry_forward(&receipt, first.model_ref().clone())
                .err(),
            Some(CarryForwardRefusal::NotTheCarriedGeneration),
            "carrying into the generation it already sits at is not carrying"
        );
        Ok(())
    }
}
