//! User-config preflight and immutable provenance for remote installation.
use super::*;
use grapt_api::u3a::ClaudePluginRequestDto;
use grapt_plugin::claude::{GitSource, PluginSource};

pub(super) enum Kind {
    Marketplace(GitSource),
    Plugin(Market),
}
pub(super) struct Plan {
    source: GitSource,
    scratch: PathBuf,
    kind: Kind,
}
pub(super) struct Prepared {
    pub kind: Kind,
    pub acquired: acquisition::Acquired,
}
impl Plan {
    pub async fn acquire(self) -> Result<Prepared, String> {
        let selection = match &self.kind {
            Kind::Marketplace(_) => acquisition::Selection::Marketplace,
            Kind::Plugin(_) => acquisition::Selection::Plugin,
        };
        let acquired = acquisition::git(self.scratch, &self.source, selection).await?;
        Ok(Prepared {
            kind: self.kind,
            acquired,
        })
    }
}
pub(super) fn preflight(
    environment: CatalogEnvironment,
    directory: &str,
    request: &ClaudePluginRequestDto,
) -> Result<Option<Plan>, String> {
    super::super::hook_import::check_origin(&environment, directory)?;
    super::super::mcp_config::file::publish(environment, |before, _, environment| {
        let registry = operation::decode_registry(before)?;
        let candidate = match request {
            ClaudePluginRequestDto::MarketplaceAdd { source } => {
                if local_source(directory, source.as_str())?.is_some() {
                    None
                } else {
                    let source = GitSource::marketplace(source.as_str())?;
                    Some((source.clone(), Kind::Marketplace(source)))
                }
            }
            ClaudePluginRequestDto::Install { plugin } => {
                let id = PluginId::new(plugin.as_str())?;
                if registry.installed.contains_key(&id) {
                    None
                } else {
                    let market = registry
                        .marketplaces
                        .get(id.marketplace())
                        .ok_or("marketplace is not added; run plugin marketplace add first")?;
                    let (catalog, digest) = read_market(&market.root)?;
                    if digest != market.digest || &catalog.name != id.marketplace() {
                        return Err("marketplace changed; retry after an explicit update".into());
                    }
                    let source = match catalog.source(id.name())? {
                        PluginSource::Git(source) => Some(source),
                        PluginSource::Relative(path) => {
                            market.remote.as_ref().map(|remote| GitSource {
                                url: remote.source.url.clone(),
                                git_ref: None,
                                git_sha: Some(remote.commit.clone()),
                                subdir: remote.source.subdir.join(path),
                            })
                        }
                    };
                    source.map(|source| (source, Kind::Plugin(market.clone())))
                }
            }
        };
        let plan = match candidate {
            None => None,
            Some((source, kind)) => {
                acquisition::validate(&source)?;
                Some(Plan {
                    source,
                    scratch: acquisition::scratch(&environment.config)?,
                    kind,
                })
            }
        };
        Ok((before.to_vec(), plan))
    })
}
/// Existing explicit paths remain local. A missing relative directory is local
/// only when the user made the path intent explicit, so owner/repo works as-is.
pub(super) fn local_source(directory: &str, source: &str) -> Result<Option<PathBuf>, String> {
    if source.contains("://") || source.starts_with("git@") {
        return Ok(None);
    }
    let path = Path::new(source);
    if path.is_absolute() {
        return Ok(Some(path.to_owned()));
    }
    let relative = Path::new(directory).join(path);
    if source.starts_with('.') {
        return Ok(Some(relative));
    }
    match std::fs::symlink_metadata(&relative) {
        Ok(_) => Ok(Some(relative)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io(error)),
    }
}
pub(super) fn add_market(
    registry: &mut Registry,
    config: &Path,
    prepared: Prepared,
) -> Result<String, String> {
    let Kind::Marketplace(source) = prepared.kind else {
        return Err("prepared plugin source does not match marketplace operation".into());
    };
    let snapshot = prepared.acquired.snapshot;
    let (catalog, digest) = parse_market(
        snapshot
            .bytes(".claude-plugin/marketplace.json")
            .ok_or("remote marketplace has no catalog")?,
    )?;
    let remote = RemoteMarket {
        source,
        commit: prepared.acquired.commit,
    };
    let previous = registry.marketplaces.get(&catalog.name);
    if let Some(previous) = previous {
        if previous.digest != digest || previous.remote.as_ref() != Some(&remote) {
            return Err("marketplace already exists with different contents; use an explicit update when available".into());
        }
        let (retained, retained_digest) = read_market(&previous.root)?;
        if retained.name != catalog.name || retained_digest != digest {
            return Err("retained marketplace catalog changed; activation refused".into());
        }
    } else {
        if registry.marketplaces.len() >= 64 {
            return Err("Claude plugin registry exceeds 64 marketplaces".into());
        }
        let root = snapshot.publish(config)?;
        registry.marketplaces.insert(
            catalog.name.clone(),
            Market {
                root,
                digest,
                remote: Some(remote),
            },
        );
    }
    Ok(format!(
        "Marketplace {} added ({} plugins, {} invalid entries)",
        catalog.name.as_str(),
        catalog.entries.len(),
        catalog.refused.len()
    ))
}
