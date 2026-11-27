//! User-owned child concurrency, with project configuration allowed only to narrow.
use super::catalog_config::{user_owned, CatalogEnvironment, CatalogSources};
use std::num::NonZeroUsize;
use std::path::Path;

/// The provider defaults to four total threads and subtracts the root execution.
const DEFAULT_CHILDREN: usize = 3;
const MAX_CHILDREN: usize = 64;

pub(crate) fn global(service: &Path) -> Result<NonZeroUsize, String> {
    let Some(environment) = CatalogEnvironment::configured(service)? else {
        // The service can show the existing queued prompt refusal without a
        // catalog home. No execution can use this budget until `read` succeeds.
        return NonZeroUsize::new(DEFAULT_CHILDREN)
            .ok_or_else(|| "child provider capacity is zero".into());
    };
    resolve(&CatalogSources::read(service, environment)?, false)
}

pub(super) fn effective(service: &Path, directory: &Path) -> Result<NonZeroUsize, String> {
    read(service, directory, true)
}

fn read(service: &Path, directory: &Path, narrow: bool) -> Result<NonZeroUsize, String> {
    let sources = CatalogSources::read(directory, CatalogEnvironment::read(service)?)?;
    resolve(&sources, narrow)
}

fn resolve(sources: &CatalogSources, narrow: bool) -> Result<NonZeroUsize, String> {
    let mut user = DEFAULT_CHILDREN;
    let mut project = MAX_CHILDREN;
    for document in &sources.documents {
        let Some(setting) = document.value.get("provider_capacity") else {
            continue;
        };
        let value = setting.value();
        if value.as_object().is_none() {
            return Err(format!(
                "{}: provider_capacity must be an object",
                document.source
            ));
        }
        let Some(children) = value.get("children") else {
            continue;
        };
        let children = children
            .value()
            .as_number()
            .and_then(|number| number.as_u64())
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| (1..=MAX_CHILDREN).contains(value))
            .ok_or_else(|| {
                format!(
                    "{}: provider_capacity.children must be within 1..=64",
                    document.source
                )
            })?;
        if user_owned(document.origin) {
            user = children;
        } else if narrow {
            project = project.min(children);
        }
    }
    NonZeroUsize::new(user.min(project)).ok_or_else(|| "child provider capacity is zero".into())
}

#[cfg(test)]
mod tests;
