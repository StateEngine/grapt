//! Production-arm endpoint selection for service composition.

use std::path::Path;

use super::ProductRefusal;

/// Resource limits share the same startup snapshot as endpoint selection.
pub(super) fn provider_limits(
    root: &Path,
    config: &crate::input::ServiceConfig,
) -> Result<crate::dogfood::production::ProductionProviderLimits, ProductRefusal> {
    Ok(crate::dogfood::production::ProductionProviderLimits {
        connect: std::time::Duration::from_millis(config.provider_connect_ms),
        response_head: std::time::Duration::from_millis(config.provider_response_head_ms),
        active_attempts: std::num::NonZeroUsize::new(config.provider_active_attempts)
            .ok_or_else(|| ProductRefusal::Input("provider_active_attempts is zero".into()))?,
        child_attempts: crate::dogfood::provider_capacity::global(root)
            .map_err(ProductRefusal::Input)?,
    })
}
