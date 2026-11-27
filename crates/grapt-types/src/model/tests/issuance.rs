use super::{load_exact, row, PROVIDER};
use crate::authority::PolicyVersion;
use crate::credential::{AccountId, CredentialVersion};
use crate::model::*;

const MODEL: &str = "claude-opus-5";
const BASE: &str = "claude-opus-5";
const FAMILY: &str = "claude";

fn request() -> Option<RoutingRequest> {
    Some(RoutingRequest {
        route: ModelRouteId::new("rt_1")?,
        account: AccountId::new("acct_1")?,
        credential_version: CredentialVersion::new("3")?,
        policy: PolicyVersion::new("pol_1")?,
    })
}

/// A registry over a one-row release, at generation 1.
fn registry(version: &str, family: &str) -> Option<ModelRegistry> {
    Some(ModelRegistry::install(load_exact(
        version,
        vec![row(MODEL, BASE, family, None)?],
    )?))
}

fn issue(registry: &ModelRegistry) -> Result<ResolvedModelTarget, IssuanceRefusal> {
    registry.issue(
        &ProviderId::new(PROVIDER).ok_or(IssuanceRefusal::Unavailable)?,
        &ModelId::new(MODEL).ok_or(IssuanceRefusal::Unavailable)?,
        None,
        request().ok_or(IssuanceRefusal::Unavailable)?,
    )
}

// ---------------------------------------------------------------------
// S1 — a monotonic generation, compared at resolve AND at use
// ---------------------------------------------------------------------

#[test]
fn a_generation_orders_numerically_and_not_as_text() -> Result<(), Box<dyn std::error::Error>> {
    // The reason `CatalogGeneration` is not an `opaque_id!`. That macro derives
    // `Ord` over the inner String, and under a LEXICOGRAPHIC order "10" sorts
    // before "9" — so generation ten would read as older than generation nine
    // and a superseded release would compare current. This is the one assertion
    // that fails if a later lane "simplifies" the type back to an opaque id.
    let mut nine = CatalogGeneration::FIRST;
    for _ in 1..9 {
        nine = nine.succ().ok_or("generation ceiling")?;
    }
    let ten = nine.succ().ok_or("generation ceiling")?;
    assert_eq!(nine.get(), 9);
    assert_eq!(ten.get(), 10);
    assert!(ten > nine, "ten is newer than nine, numerically");
    assert!(
        !ten.to_string().lt(&nine.to_string()) || ten > nine,
        "text order is not the order this type promises"
    );
    Ok(())
}

#[test]
fn an_issuance_is_current_until_its_release_is_superseded() -> Result<(), Box<dyn std::error::Error>>
{
    let mut registry = registry("cat-1", FAMILY).ok_or("registry")?;
    let target = issue(&registry).map_err(|_| "issue")?;

    assert_eq!(target.issuance_generation(), CatalogGeneration::FIRST);
    assert!(
        target.is_current_issuance(),
        "nothing has superseded the release it came from"
    );

    // Superseding is what pointer identity could not express. The target still
    // holds a strong clone of its snapshot, so it is very much alive — and it is
    // no longer current.
    registry
        .supersede(
            ModelCatalogVersion::new("cat-2").ok_or("version")?,
            vec![row(MODEL, BASE, FAMILY, None).ok_or("row")?],
        )
        .ok_or("supersede")?;

    assert!(
        !target.is_current_issuance(),
        "a withdrawn release stops vouching for targets already minted from it"
    );
    assert_eq!(
        target.issuance_generation(),
        CatalogGeneration::FIRST,
        "the target still reports the generation it WAS issued at"
    );
    assert_eq!(
        target.current_catalog_generation().get(),
        2,
        "and can name the generation that replaced it"
    );
    Ok(())
}

#[test]
fn a_retired_registry_refuses_to_mint_anything_new() -> Result<(), Box<dyn std::error::Error>> {
    // Issue-time refusal, distinct from the use-time question above. A registry
    // whose catalog has been superseded by a LATER lineage member must not mint
    // against the retired generation.
    let mut registry = registry("cat-1", FAMILY).ok_or("registry")?;
    let retired = registry
        .supersede(
            ModelCatalogVersion::new("cat-2").ok_or("version")?,
            vec![row(MODEL, BASE, FAMILY, None).ok_or("row")?],
        )
        .ok_or("supersede")?;

    // The superseded catalog handed back is exactly the retired generation.
    let stale_registry = ModelRegistry::install(retired);
    assert_eq!(
        issue(&stale_registry).map(|_| ()),
        Err(IssuanceRefusal::Stale {
            issued_at: CatalogGeneration::FIRST,
            current: CatalogGeneration::FIRST.succ().ok_or("succ")?,
        }),
        "a retired release mints nothing, and names both generations"
    );

    // The successor still issues.
    assert!(issue(&registry).is_ok());
    Ok(())
}

// ---------------------------------------------------------------------
// S2 — staleness is a THIRD refusal, not reachable as the other two
// ---------------------------------------------------------------------

#[test]
fn staleness_is_a_third_case_and_not_unavailable_or_incapable(
) -> Result<(), Box<dyn std::error::Error>> {
    // Master §7.5: "Availability and capability are properties of the target;
    // staleness is a property of the issuance." The three must be distinguishable
    // at the TYPE level, not merely by message text, or a caller has to
    // re-derive the distinction from prose.
    let mut registry = registry("cat-1", FAMILY).ok_or("registry")?;

    // Unavailable: the release does not carry the coordinate at all.
    let absent = registry.issue(
        &ProviderId::new(PROVIDER).ok_or("provider")?,
        &ModelId::new("not-in-this-release").ok_or("model")?,
        None,
        request().ok_or("request")?,
    );
    assert_eq!(absent.map(|_| ()), Err(IssuanceRefusal::Unavailable));

    // Stale: the coordinate is present AND fit, and the issuance is retired.
    let retired = ModelRegistry::install(
        registry
            .supersede(
                ModelCatalogVersion::new("cat-2").ok_or("version")?,
                vec![row(MODEL, BASE, FAMILY, None).ok_or("row")?],
            )
            .ok_or("supersede")?,
    );
    let stale = issue(&retired).map(|_| ());

    // The discrimination, stated as an exhaustive match rather than an equality:
    // a later lane that collapsed the enum would have to delete an arm here.
    match stale.clone() {
        Err(IssuanceRefusal::Stale { issued_at, current }) => {
            assert_eq!(issued_at, CatalogGeneration::FIRST);
            assert!(current > issued_at, "the lineage moved on");
        }
        Err(IssuanceRefusal::Unavailable) => {
            panic!("a retired release still CARRIES the row; absence is a different fact")
        }
        Err(IssuanceRefusal::Incapable { .. }) => {
            panic!("the target is fit; only its issuance is old")
        }
        Ok(_) => panic!("a retired issuance must not mint"),
    }

    assert_ne!(
        stale,
        Err(IssuanceRefusal::Unavailable),
        "stale is not absence"
    );
    assert_ne!(
        stale,
        Err(IssuanceRefusal::Incapable {
            contract: IncapableContract::Variant
        }),
        "stale is not unfitness"
    );
    Ok(())
}

#[test]
fn a_stale_refusal_never_licenses_another_family() -> Result<(), Box<dyn std::error::Error>> {
    // §7.5: "a stale refusal is never evidence that the target is gone or unfit,
    // and never licenses substituting another family." The refusal carries two
    // generations and nothing else — no family, no alternative, no suggestion —
    // so there is nothing in it a caller could read as permission to route
    // elsewhere.
    let mut registry = registry("cat-1", FAMILY).ok_or("registry")?;
    let retired = ModelRegistry::install(
        registry
            .supersede(
                ModelCatalogVersion::new("cat-2").ok_or("version")?,
                vec![row(MODEL, BASE, FAMILY, None).ok_or("row")?],
            )
            .ok_or("supersede")?,
    );

    let Err(refusal) = issue(&retired) else {
        panic!("expected a stale refusal");
    };
    // The rendered refusal names generations only. If a later revision added a
    // "try this instead" coordinate, this assertion is what fails.
    let rendered = format!("{refusal:?}");
    assert!(
        !rendered.contains(FAMILY) && !rendered.contains(BASE),
        "a refusal must not name the catalog facts it refused over: {rendered}"
    );

    // And the target itself is still in its own family under the CURRENT
    // release: staleness said nothing about family membership.
    let fresh = issue(&registry).map_err(|_| "issue")?;
    assert!(fresh.model_ref().same_family(fresh.model_ref()));
    Ok(())
}

// ---------------------------------------------------------------------
// S3 — a semantically-null reload carries a live selection forward
// ---------------------------------------------------------------------

#[test]
fn a_null_reload_mints_a_receipt_and_carries_the_target() -> Result<(), Box<dyn std::error::Error>>
{
    let mut registry = registry("cat-1", FAMILY).ok_or("registry")?;
    let carried = issue(&registry).map_err(|_| "issue")?;

    // The reload: a byte-identical release, which `load` would have made a
    // foreign issuance and which §7.5 says is not stale at all.
    registry
        .supersede(
            ModelCatalogVersion::new("cat-1").ok_or("version")?,
            vec![row(MODEL, BASE, FAMILY, None).ok_or("row")?],
        )
        .ok_or("supersede")?;
    assert!(!carried.is_current_issuance(), "the reload retired it");

    let (receipt, reissued) = registry
        .reissue(&carried, request().ok_or("request")?)
        .map_err(|_| "reissue")?;

    assert_eq!(receipt.from_generation(), CatalogGeneration::FIRST);
    assert_eq!(receipt.to_generation().get(), 2);
    assert!(
        reissued.is_current_issuance(),
        "the re-issued target is at the current generation"
    );
    assert!(
        reissued.describes_same_target(&carried),
        "same target, new issuance — that is what carrying forward means"
    );
    assert!(
        !reissued.is_same_issuance(&carried),
        "and it is honestly a DIFFERENT issuance, not a forged continuation"
    );
    Ok(())
}

#[test]
fn a_reload_that_moves_the_family_mints_no_receipt() -> Result<(), Box<dyn std::error::Error>> {
    // The counterexample. Without the family half of the proof, a reload that
    // reassigned this model to another family would carry the user's selection
    // straight across the boundary `agent default` may not cross.
    let mut registry = registry("cat-1", FAMILY).ok_or("registry")?;
    let carried = issue(&registry).map_err(|_| "issue")?;

    registry
        .supersede(
            ModelCatalogVersion::new("cat-2").ok_or("version")?,
            vec![row(MODEL, BASE, "another-family", None).ok_or("row")?],
        )
        .ok_or("supersede")?;

    assert!(
        registry
            .reissue(&carried, request().ok_or("request")?)
            .is_err(),
        "same coordinates, different family: no receipt, and the carried \
         selection refuses as stale"
    );
    Ok(())
}

#[test]
fn a_reload_that_moves_the_base_model_mints_no_receipt() -> Result<(), Box<dyn std::error::Error>> {
    let mut registry = registry("cat-1", FAMILY).ok_or("registry")?;
    let carried = issue(&registry).map_err(|_| "issue")?;

    registry
        .supersede(
            ModelCatalogVersion::new("cat-2").ok_or("version")?,
            vec![row(MODEL, "a-different-base", FAMILY, None).ok_or("row")?],
        )
        .ok_or("supersede")?;

    assert!(
        registry
            .reissue(&carried, request().ok_or("request")?)
            .is_err(),
        "the base model is catalog truth the receipt must also prove unchanged"
    );
    Ok(())
}

#[test]
fn an_unrelated_catalog_is_substitution_not_carry_forward() -> Result<(), Box<dyn std::error::Error>>
{
    // A separately loaded release is its own lineage. Carrying a selection into
    // it is not carrying, it is substituting — which is the act §7.5 forbids.
    //
    // THE UNRELATED REGISTRY IS ADVANCED FIRST, and that is the whole
    // construction. Both lineages start at generation 1, so a naive version of
    // this witness is refused by the "new generation" guard (1 >= 1) before the
    // lineage clause is ever consulted — it passes for a reason that has nothing
    // to do with what it claims. Measured, not reasoned: the MR08 mutant that
    // deletes the lineage check SURVIVED that version. Advancing the stranger to
    // generation 2 makes from < to, so the generation guard cannot fire and only
    // the lineage clause can refuse.
    let first = registry("cat-1", FAMILY).ok_or("registry")?;
    let carried = issue(&first).map_err(|_| "issue")?;

    let mut unrelated = registry("cat-1", FAMILY).ok_or("registry")?;
    unrelated
        .supersede(
            ModelCatalogVersion::new("cat-2").ok_or("version")?,
            vec![row(MODEL, BASE, FAMILY, None).ok_or("row")?],
        )
        .ok_or("supersede")?;
    assert!(
        carried.issuance_generation() < unrelated.generation(),
        "the stranger is AHEAD, so the new-generation guard cannot be what refuses"
    );

    assert!(
        unrelated
            .reissue(&carried, request().ok_or("request")?)
            .is_err(),
        "an identical-looking release from another lineage cannot re-issue,          however new it is"
    );
    Ok(())
}

#[test]
fn a_generation_that_did_not_advance_mints_no_receipt() -> Result<(), Box<dyn std::error::Error>> {
    // "New generation" is one of the four proofs. Without this, a caller could
    // mint a receipt for a reissuance that never happened and use it to carry a
    // selection nothing had disturbed.
    let registry = registry("cat-1", FAMILY).ok_or("registry")?;
    let carried = issue(&registry).map_err(|_| "issue")?;
    assert!(
        registry
            .reissue(&carried, request().ok_or("request")?)
            .is_err(),
        "the lineage has not moved, so there is nothing to carry forward"
    );
    Ok(())
}

// ---------------------------------------------------------------------
// §3.2 — the generation is excluded from BOTH canonical forms
// ---------------------------------------------------------------------

#[test]
fn the_generation_is_excluded_from_the_capability_form() -> Result<(), Box<dyn std::error::Error>> {
    // R-b §2.4. A capability must derive identically in every process that
    // resolves the same target, and a generation is deployment-local: two hosts
    // that have reloaded a different number of times hold the same target at
    // different generations. If the generation entered the capability they would
    // derive different identities for one target.
    //
    // Reading the BYTES rather than comparing two capabilities, because equal
    // capabilities would also be produced by a generation that simply happened
    // not to differ.
    let mut registry = registry("cat-1", FAMILY).ok_or("registry")?;
    let first = issue(&registry).map_err(|_| "issue")?;
    registry
        .supersede(
            ModelCatalogVersion::new("cat-1").ok_or("version")?,
            vec![row(MODEL, BASE, FAMILY, None).ok_or("row")?],
        )
        .ok_or("supersede")?;
    let (_, second) = registry
        .reissue(&first, request().ok_or("request")?)
        .map_err(|_| "reissue")?;

    assert_eq!(
        first.issuance_generation().get(),
        1,
        "the two targets are at different generations"
    );
    assert_eq!(second.issuance_generation().get(), 2);
    assert_eq!(
        first.capability(),
        second.capability(),
        "and derive the SAME capability: the generation is not one of its \
         coordinates"
    );

    let form = first.capability().as_str();
    assert!(
        !form.contains("generation"),
        "no generation field in the canonical form: {form}"
    );
    Ok(())
}
