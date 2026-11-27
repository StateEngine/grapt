//! Native search is an explicit user-owned provider-network choice. Repository
//! configuration can narrow that choice; it can never turn network access on.
use super::catalog_config::{user_owned, CatalogEnvironment, CatalogSources};
use grapt_provider::{WebSearchAccess, WebSearchSpec};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Mode {
    Disabled,
    Cached,
    Live,
}
impl Mode {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "disabled" => Ok(Self::Disabled),
            "cached" => Ok(Self::Cached),
            "live" => Ok(Self::Live),
            _ => Err("web_search must be disabled, cached, or live".into()),
        }
    }
    fn spec(self) -> Option<WebSearchSpec> {
        match self {
            Self::Disabled => None,
            Self::Cached => Some(WebSearchSpec::new(WebSearchAccess::Cached)),
            Self::Live => Some(WebSearchSpec::new(WebSearchAccess::Live)),
        }
    }
}
pub(super) fn read(service_root: &Path, directory: &Path) -> Result<Option<WebSearchSpec>, String> {
    resolve(&CatalogSources::read(
        directory,
        CatalogEnvironment::read(service_root)?,
    )?)
}
fn resolve(sources: &CatalogSources) -> Result<Option<WebSearchSpec>, String> {
    let mut selected = Mode::Disabled;
    let mut ceiling = Mode::Live;
    for document in &sources.documents {
        let Some(value) = document.value.get("web_search") else {
            continue;
        };
        let mode = value
            .value()
            .as_str()
            .ok_or_else(|| document.refusal("web_search must be a string"))
            .and_then(|value| Mode::parse(value).map_err(|error| document.refusal(&error)))?;
        if user_owned(document.origin) {
            selected = mode;
        } else {
            ceiling = ceiling.min(mode);
        }
    }
    Ok(selected.min(ceiling).spec())
}
#[cfg(test)]
mod tests {
    use super::*;
    type TestResult = Result<(), Box<dyn std::error::Error>>;
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn repository_settings_cannot_enable_or_widen_user_owned_search() -> TestResult {
        let root = Scratch(std::env::temp_dir().join(format!(
            "grapt-native-search-{}",
            grapt_store::OsRandomIds::seed()?.instance()
        )));
        let profile = root.0.join("service");
        let repo = root.0.join("repo");
        std::fs::create_dir_all(&profile)?;
        std::fs::create_dir_all(&repo)?;
        let read = || {
            CatalogSources::read(&repo, CatalogEnvironment::isolated(&root.0))
                .and_then(|sources| resolve(&sources))
        };
        std::fs::write(repo.join("grapt.json"), r#"{"web_search":"live"}"#)?;
        assert!(read()?.is_none());
        std::fs::write(profile.join("grapt.json"), r#"{"web_search":"cached"}"#)?;
        assert_eq!(read()?.map(|s| s.access()), Some(WebSearchAccess::Cached));
        std::fs::write(profile.join("grapt.json"), r#"{"web_search":"live"}"#)?;
        std::fs::write(repo.join("grapt.json"), r#"{"web_search":"cached"}"#)?;
        assert_eq!(read()?.map(|s| s.access()), Some(WebSearchAccess::Cached));
        std::fs::write(repo.join("grapt.json"), r#"{"web_search":"disabled"}"#)?;
        assert!(read()?.is_none());
        std::fs::write(repo.join("grapt.json"), r#"{"web_search":true}"#)?;
        assert!(read().is_err());
        Ok(())
    }
}
