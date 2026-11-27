//! External witnesses for the read channels a compile probe cannot express.
//!
//! Three of the four leak channels closed this round are sealed by REMOVING
//! something, and the trybuild probes witness those. Two are not: `ModelRef`
//! still has `PartialEq`/`Hash`, and `ModelRef`/`ModelCatalogRecord`/
//! `ModelCatalogEntry` still have `Debug`. They have to — real consumers derive
//! and use them — so what changed is what those impls SAY, and only a running
//! test can witness that.
//!
//! Every witness here is written as the attack. The attacker authors a catalog
//! release of its own, which is a public capability and stays one: a
//! `ModelCatalog` is loadable by anyone. What it must not buy is an answer about
//! the RUNTIME's targets.
//!
//! The fixture keeps the three strings distinguishable on purpose — the model id
//! is `dated-model-20260801`, the base model is `undated-base`, and the family
//! is `premium-tier` — so a `contains` assertion cannot pass by accident when a
//! sealed value happens to be a substring of an inert one.

use grapt_types::{
    BaseModelId, ModelCatalog, ModelCatalogEntry, ModelCatalogRecord, ModelCatalogVersion,
    ModelFamilyId, ModelId, ModelRef, ProviderId,
};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const ENDPOINT: &str = "dated-model-20260801";
const SEALED_BASE: &str = "undated-base";
const SEALED_FAMILY: &str = "premium-tier";

fn record(model: &str, base: &str, family: &str) -> Option<ModelCatalogRecord> {
    Some(ModelCatalogRecord::new(
        ProviderId::new("anthropic")?,
        ModelId::new(model)?,
        BaseModelId::new(base)?,
        ModelFamilyId::new(family)?,
        None,
    ))
}

/// One loaded release carrying one row.
fn catalog(version: &str, model: &str, base: &str, family: &str) -> Option<ModelCatalog> {
    ModelCatalog::load(
        ModelCatalogVersion::new(version)?,
        vec![record(model, base, family)?],
    )
}

fn entry(catalog: &ModelCatalog, model: &str) -> Option<ModelCatalogEntry> {
    catalog.entry(&ProviderId::new("anthropic")?, &ModelId::new(model)?, None)
}

fn target(catalog: &ModelCatalog, model: &str) -> Option<ModelRef> {
    Some(ModelRef::from_catalog(entry(catalog, model)?))
}

fn digest(value: &ModelRef) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

// ---- the guessed-row equality oracle ---------------------------------------

#[test]
fn equality_does_not_answer_the_guessed_family_question() -> TestResult {
    // The runtime's release. The attacker does not know the family this row
    // claims — that is precisely the fact `agent default` may not cross on.
    let runtime = catalog("runtime-release", ENDPOINT, SEALED_BASE, SEALED_FAMILY)
        .ok_or("the runtime release loads")?;
    let genuine = target(&runtime, ENDPOINT).ok_or("genuine target")?;

    // The attack: author the SAME endpoint with a guessed base and family, load
    // it into a catalog of your own, and compare. Under the old row-wide
    // equality the right guess compared equal and the wrong one did not, so
    // each comparison returned one bit of the sealed row and the family space
    // is small enough to enumerate.
    let right_guess = catalog("rogue-release", ENDPOINT, SEALED_BASE, SEALED_FAMILY)
        .ok_or("the rogue release loads")?;
    let wrong_guess = catalog("rogue-release", ENDPOINT, "guessed-base", "economy-tier")
        .ok_or("the rogue release loads")?;

    let right = target(&right_guess, ENDPOINT).ok_or("rogue target")?;
    let wrong = target(&wrong_guess, ENDPOINT).ok_or("rogue target")?;

    assert_eq!(
        genuine, right,
        "equality answers the endpoint question, and the endpoint matches"
    );
    assert_eq!(
        genuine, wrong,
        "and it gives the SAME answer for a wrong guess — no bit is returned. \
         This is the assertion that fails under row-wide equality."
    );

    // Narrowing equality did not open a substitution path: the strict predicate
    // still refuses both, because neither came from the loaded snapshot the
    // genuine ref did.
    assert!(
        !genuine.is_same_issuance(&right),
        "an identical row from another release is not the same issuance"
    );
    assert!(!genuine.is_same_issuance(&wrong));

    // And equality is not vacuous — a different endpoint is still unequal.
    let elsewhere = catalog("rogue-release", "other-model", SEALED_BASE, SEALED_FAMILY)
        .ok_or("the rogue release loads")?;
    assert_ne!(
        genuine,
        target(&elsewhere, "other-model").ok_or("rogue target")?,
        "equality still distinguishes routes to call"
    );
    Ok(())
}

#[test]
fn hashing_does_not_answer_the_guessed_family_question() -> TestResult {
    // Hashing is the same oracle through a digest: hash your candidate, hash the
    // genuine ref, compare. It must be exactly as uninformative as `==`.
    let runtime = catalog("runtime-release", ENDPOINT, SEALED_BASE, SEALED_FAMILY)
        .ok_or("the runtime release loads")?;
    let rogue = catalog("rogue-release", ENDPOINT, "guessed-base", "economy-tier")
        .ok_or("the rogue release loads")?;

    let genuine = target(&runtime, ENDPOINT).ok_or("genuine target")?;
    let wrong = target(&rogue, ENDPOINT).ok_or("rogue target")?;

    assert_eq!(
        digest(&genuine),
        digest(&wrong),
        "a wrong guess about the sealed row must hash identically, or the \
         equality oracle simply moves into the hasher"
    );
    Ok(())
}

// ---- Debug redaction --------------------------------------------------------

#[test]
fn debug_redacts_the_sealed_catalog_facts_on_a_model_ref() -> TestResult {
    let runtime = catalog("runtime-release", ENDPOINT, SEALED_BASE, SEALED_FAMILY)
        .ok_or("the runtime release loads")?;
    let genuine = target(&runtime, ENDPOINT).ok_or("genuine target")?;

    let printed = format!("{genuine:?}");
    assert!(
        !printed.contains(SEALED_FAMILY),
        "the family decides whether `agent default` may cross and must not \
         print; got {printed}"
    );
    assert!(
        !printed.contains(SEALED_BASE),
        "nor may the base-model projection; got {printed}"
    );
    assert!(
        printed.contains("<sealed>"),
        "the redaction is explicit, so a reader sees the coordinate was \
         withheld rather than absent; got {printed}"
    );

    // The endpoint question still answers, or the redaction would have cost the
    // diagnostics it was meant to keep.
    assert!(printed.contains(ENDPOINT), "got {printed}");
    assert!(printed.contains("anthropic"), "got {printed}");
    assert!(
        printed.contains("runtime-release"),
        "audit provenance is public and still prints; got {printed}"
    );
    Ok(())
}

#[test]
fn debug_redacts_the_sealed_catalog_facts_on_a_row_and_an_entry() -> TestResult {
    // The row and the entry are redacted in their own right, not merely hidden
    // behind `ModelRef`. A nested `Debug` reaches whatever the inner impl
    // prints, so leaving either derived would leak through any future wrapper —
    // and `ModelCatalogEntry` is handed to callers today.
    let authored = record(ENDPOINT, SEALED_BASE, SEALED_FAMILY).ok_or("row")?;
    let printed = format!("{authored:?}");
    assert!(!printed.contains(SEALED_FAMILY), "got {printed}");
    assert!(!printed.contains(SEALED_BASE), "got {printed}");
    assert!(printed.contains(ENDPOINT), "got {printed}");

    let runtime = catalog("runtime-release", ENDPOINT, SEALED_BASE, SEALED_FAMILY)
        .ok_or("the runtime release loads")?;
    let issued = entry(&runtime, ENDPOINT).ok_or("entry")?;
    let printed = format!("{issued:?}");
    assert!(
        !printed.contains(SEALED_FAMILY),
        "an entry is the runtime catalog's own statement about a coordinate the \
         caller merely named; got {printed}"
    );
    assert!(!printed.contains(SEALED_BASE), "got {printed}");
    assert!(printed.contains(ENDPOINT), "got {printed}");
    Ok(())
}

// ---- the load-time ordering that replaced the row `Ord` ---------------------

#[test]
fn a_release_still_collapses_duplicates_and_refuses_contradictions() -> TestResult {
    // `ModelCatalog::load` used to sort and dedup on the row's derived `Ord`,
    // which compared the sealed coordinates. It now orders by lookup coordinate
    // and collapses with the crate-private row equality; this witnesses that the
    // decision it produces is unchanged, including when the duplicate rows are
    // not adjacent in the input.
    let version = || ModelCatalogVersion::new("cat-1").ok_or("version");
    let row = |family: &str| record(ENDPOINT, SEALED_BASE, family).ok_or("row");

    let repeated = vec![row(SEALED_FAMILY)?, row(SEALED_FAMILY)?];
    assert!(
        ModelCatalog::load(version()?, repeated).is_some(),
        "an exactly repeated row asserts nothing new"
    );

    let contradiction = vec![row(SEALED_FAMILY)?, row("economy-tier")?];
    assert!(
        ModelCatalog::load(version()?, contradiction).is_none(),
        "a release cannot put one endpoint in two families"
    );

    let separated = vec![
        row(SEALED_FAMILY)?,
        row("economy-tier")?,
        row(SEALED_FAMILY)?,
    ];
    assert!(
        ModelCatalog::load(version()?, separated).is_none(),
        "and a duplicate that straddles a contradicting row is still a \
         contradicting release, not a collapsible one"
    );
    Ok(())
}

// ---- R-d: identity spelling is exact UTF-8 bytes, decided at load ----------

#[test]
fn byte_distinct_spellings_are_distinct_coordinates_at_load() -> TestResult {
    // Ruling R-d: model registry identities use EXACT UTF-8 byte
    // identity. There is no implicit trim, case fold, Unicode normalization or
    // lossy rewrite, and `ModelCatalog::load` is the enforcement point.
    //
    // This is the LOAD-side half, written against the public surface a real
    // consumer has. It is deliberately not the same assertion as the in-crate
    // witness: that one proves a byte-different lookup misses, this one proves
    // what `load` DECIDES about byte-different rows — that they are two
    // coordinates rather than one contradiction and rather than one collapse.
    //
    // Both failure directions matter and they fail differently. If `load` ever
    // folded case, these rows would collapse or be refused as a contradiction,
    // and a release legitimately carrying two spellings would stop loading. If
    // it folded case only at LOOKUP, a caller's spelling would silently resolve
    // another row's sealed base model and family — the dangerous direction,
    // because the value it returns is authority.
    let version = || ModelCatalogVersion::new("cat-1").ok_or("version");
    let spelled = |provider: &str, family: &str| -> Result<ModelCatalogRecord, String> {
        Ok(ModelCatalogRecord::new(
            ProviderId::new(provider).ok_or("provider")?,
            ModelId::new(ENDPOINT).ok_or("model")?,
            BaseModelId::new(SEALED_BASE).ok_or("base")?,
            ModelFamilyId::new(family).ok_or("family")?,
            None,
        ))
    };

    // Two spellings of one provider, carrying DIFFERENT families. Under any
    // normalization this is a self-contradicting release and `load` returns
    // `None`; under exact byte identity they are two endpoints and it loads.
    let loaded = ModelCatalog::load(
        version()?,
        vec![
            spelled("anthropic", SEALED_FAMILY)?,
            spelled("Anthropic", "economy-tier")?,
        ],
    )
    .ok_or("two byte-distinct providers are two coordinates, not a contradiction")?;

    let lower = ProviderId::new("anthropic").ok_or("lower")?;
    let upper = ProviderId::new("Anthropic").ok_or("upper")?;
    let endpoint = ModelId::new(ENDPOINT).ok_or("endpoint")?;
    assert!(
        loaded.entry(&lower, &endpoint, None).is_some(),
        "the lower-case row is present"
    );
    assert!(
        loaded.entry(&upper, &endpoint, None).is_some(),
        "and so is the upper-case row; neither was collapsed into the other"
    );

    // A release that carries only one spelling does not answer for the other.
    let single = ModelCatalog::load(version()?, vec![spelled("anthropic", SEALED_FAMILY)?])
        .ok_or("single-row release")?;
    assert!(
        single.entry(&upper, &endpoint, None).is_none(),
        "a case-different provider spelling is a DIFFERENT coordinate and must \
         never resolve the row this release actually carries"
    );

    // Whitespace is a byte like any other: it is not trimmed on either side.
    let padded = ModelCatalog::load(
        version()?,
        vec![
            spelled("anthropic", SEALED_FAMILY)?,
            spelled("anthropic ", "economy-tier")?,
        ],
    )
    .ok_or("a trailing space is a different provider, not a contradiction")?;
    let trailing = ProviderId::new("anthropic ").ok_or("trailing")?;
    assert!(padded.entry(&trailing, &endpoint, None).is_some());
    assert!(
        single.entry(&trailing, &endpoint, None).is_none(),
        "and an untrimmed lookup does not reach the trimmed row"
    );

    // The exactness is symmetric: identical bytes still collapse, so this
    // witness cannot be satisfied by disabling row identity altogether.
    assert!(
        ModelCatalog::load(
            version()?,
            vec![
                spelled("anthropic", SEALED_FAMILY)?,
                spelled("anthropic", SEALED_FAMILY)?,
            ],
        )
        .is_some(),
        "byte-identical rows making the same claim are still one row"
    );
    Ok(())
}
