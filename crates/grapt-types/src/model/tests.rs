//! Witnesses for the `model` module, partitioned by subject to match its named
//! `mod` children.
//!
//! The fixtures below stay here rather than in one child because two children
//! need them. [`test_catalog::record`](super::test_catalog::record) fixes
//! base == model and variant == None, which is right for the common case and
//! wrong for every witness that must vary exactly one catalog fact at a time.

use super::{
    BaseModelId, ModelCatalog, ModelCatalogRecord, ModelCatalogVersion, ModelFamilyId, ModelId,
    ModelVariantId, ProviderId,
};

mod catalog;
mod issuance;
mod reference;
mod target;

/// The provider every witness holds constant unless it is varying it.
const PROVIDER: &str = "anthropic";

/// One catalog row with every coordinate stated explicitly.
fn exact_row(
    provider: &str,
    model: &str,
    base: &str,
    family: &str,
    variant: Option<&str>,
) -> Option<ModelCatalogRecord> {
    let variant = match variant {
        None => None,
        Some(raw) => Some(ModelVariantId::new(raw)?),
    };
    Some(ModelCatalogRecord::new(
        ProviderId::new(provider)?,
        ModelId::new(model)?,
        BaseModelId::new(base)?,
        ModelFamilyId::new(family)?,
        variant,
    ))
}

/// The same row under the constant provider.
fn row(model: &str, base: &str, family: &str, variant: Option<&str>) -> Option<ModelCatalogRecord> {
    exact_row(PROVIDER, model, base, family, variant)
}

fn load_exact(version: &str, rows: Vec<ModelCatalogRecord>) -> Option<ModelCatalog> {
    ModelCatalog::load(ModelCatalogVersion::new(version)?, rows)
}
