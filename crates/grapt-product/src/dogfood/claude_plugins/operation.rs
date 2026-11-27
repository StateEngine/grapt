use super::*;
use grapt_api::u3a::{
    ApplicationFuture, ClaudePluginRequestDto, ClaudePluginResponseDto, InvalidRequestErrorDto,
    ProjectCurrentLocationQueryDto,
};

pub(crate) struct Installer {
    root: PathBuf,
}
impl Installer {
    pub(crate) fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }
}
impl grapt_api::u3a::ClaudePluginApplicationPort for Installer {
    fn install(
        &self,
        _principal: grapt_api::SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
        request: ClaudePluginRequestDto,
    ) -> ApplicationFuture<ClaudePluginResponseDto, InvalidRequestErrorDto> {
        let root = self.root.clone();
        Box::pin(async move {
            let location =
                super::super::locations::resolve_default(&root.to_string_lossy(), &query)
                    .await
                    .map_err(super::super::support::invalid)?;
            let environment =
                CatalogEnvironment::read(&root).map_err(super::super::support::invalid)?;
            let directory = location.directory;
            let (environment, directory, request, plan) = tokio::task::spawn_blocking(move || {
                let plan = remote::preflight(environment.clone(), &directory, &request)?;
                Ok::<_, String>((environment, directory, request, plan))
            })
            .await
            .map_err(|_| super::super::support::invalid("plugin preflight could not finish"))?
            .map_err(super::super::support::invalid)?;
            let prepared = match plan {
                Some(plan) => Some(
                    plan.acquire()
                        .await
                        .map_err(super::super::support::invalid)?,
                ),
                None => None,
            };
            tokio::task::spawn_blocking(move || {
                publish_with(environment, &directory, request, prepared)
            })
            .await
            .map_err(|_| super::super::support::invalid("plugin installer could not finish"))?
            .map_err(super::super::support::invalid)
        })
    }
}

#[cfg(test)]
pub(super) fn perform_with(
    environment: CatalogEnvironment,
    directory: &str,
    request: ClaudePluginRequestDto,
) -> Result<ClaudePluginResponseDto, String> {
    publish_with(environment, directory, request, None)
}
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn publish_with(
    environment: CatalogEnvironment,
    directory: &str,
    request: ClaudePluginRequestDto,
    prepared: Option<remote::Prepared>,
) -> Result<ClaudePluginResponseDto, String> {
    super::super::hook_import::check_origin(&environment, directory)?;
    let activating = matches!(&request, ClaudePluginRequestDto::Install { .. });
    super::super::mcp_config::file::publish(environment, move |before, config_path, environment| {
        let document = grapt_config::decode(before, super::super::catalog_config::bounds()?)
            .map_err(|_| "user plugin config is malformed")?;
        let mut registry = registry(
            document
                .value()
                .get("claudePlugins")
                .map(|v| super::super::catalogs::config_json(v.value()))
                .transpose()?
                .as_ref(),
        )?;
        let message = match request {
            ClaudePluginRequestDto::MarketplaceAdd { source } => {
                if let Some(prepared) = prepared {
                    remote::add_market(&mut registry, &environment.config, prepared)?
                } else {
                    let source = remote::local_source(directory, source.as_str())?
                        .ok_or("remote marketplace requires acquisition before publication")?;
                    let metadata = std::fs::symlink_metadata(&source).map_err(io)?;
                    snapshot::unlinked(&metadata)?;
                    if !metadata.is_dir() {
                        return Err("local marketplace source must be a directory".into());
                    }
                    let root = std::fs::canonicalize(source).map_err(io)?;
                    let (market, digest) = read_market(&root)?;
                    match registry.marketplaces.get(&market.name) {
                    Some(previous) if previous.root != root || previous.digest != digest || previous.remote.is_some() => return Err("marketplace already exists with different contents; update is unavailable in this slice".into()),
                    _ => {}
                }
                    let message = format!(
                        "Marketplace {} added ({} plugins, {} invalid entries)",
                        market.name.as_str(),
                        market.entries.len(),
                        market.refused.len()
                    );
                    registry.marketplaces.insert(
                        market.name,
                        Market {
                            root,
                            digest,
                            remote: None,
                        },
                    );
                    message
                }
            }
            ClaudePluginRequestDto::Install { plugin } => install(
                &mut registry,
                plugin.as_str(),
                &environment.config,
                prepared,
            )?,
        };
        if registry.marketplaces.len() > 64 || registry.installed.len() > 64 {
            return Err("Claude plugin registry exceeds 64 entries".into());
        }
        let encoded =
            serde_json::to_string(&registry).map_err(|_| "plugin registry cannot be encoded")?;
        let mut output =
            String::from_utf8(before.to_vec()).map_err(|_| "user plugin config is not UTF-8")?;
        super::super::mcp_config::edit::change(
            &mut output,
            &document,
            &["claudePlugins"],
            Some(&encoded),
        )?;
        if output.len() > super::super::catalog_config::DOCUMENT_BYTES {
            return Err("plugin registry exceeds user configuration bound".into());
        }
        grapt_config::decode(output.as_bytes(), super::super::catalog_config::bounds()?)
            .map_err(|_| "updated plugin configuration is invalid")?;
        if activating {
            let mut sources = CatalogSources::read(Path::new(directory), environment.clone())?;
            let value =
                grapt_config::decode(output.as_bytes(), super::super::catalog_config::bounds()?)
                    .map_err(|_| "proposed plugin configuration is invalid")?
                    .into_value();
            if let Some(document) = sources.documents.iter_mut().find(|document| {
                std::fs::canonicalize(&document.source).ok().as_deref() == Some(config_path)
            }) {
                document.value = value;
            } else {
                sources
                    .documents
                    .push(super::super::catalog_config::CatalogDocument {
                        directory: environment.config.clone(),
                        source: config_path.to_string_lossy().into_owned(),
                        origin: grapt_plugin::PluginOrigin::UserGlobal,
                        value,
                    });
            }
            super::super::catalogs::validate_prompt_catalog(&sources)?;
            super::super::agent_definitions::Catalog::from_sources(&sources)?;
            super::super::hooks::HookSettings::from_catalog(&sources)?;
        }
        let result = ClaudePluginResponseDto {
            message: grapt_api::u3a::BoundedString::try_new(message)
                .map_err(|_| "plugin response exceeds its bound")?,
        };
        Ok((output.into_bytes(), result))
    })
}
fn install(
    registry: &mut Registry,
    id: &str,
    config: &Path,
    prepared: Option<remote::Prepared>,
) -> Result<String, String> {
    let identity = PluginId::new(id)?;
    let name = identity.name();
    let market_name = identity.marketplace();
    if let Some(previous) = registry.installed.get(&identity) {
        if Snapshot::capture(&previous.root)?.digest != previous.digest {
            return Err("installed plugin tree changed; activation refused".into());
        }
        return Ok(format!("Plugin {id} is already installed"));
    }
    if registry.installed.len() >= 64 {
        return Err("Claude plugin registry exceeds 64 installs".into());
    }
    let source = registry
        .marketplaces
        .get(market_name)
        .ok_or("marketplace is not added; run plugin marketplace add first")?;
    let (market, digest) = read_market(&source.root)?;
    if digest != source.digest || &market.name != market_name {
        return Err("marketplace catalog changed; update is unavailable in this slice".into());
    }
    let snapshot = match prepared {
        Some(prepared) => {
            let remote::Kind::Plugin(expected) = prepared.kind else {
                return Err("prepared marketplace source does not match plugin operation".into());
            };
            if &expected != source {
                return Err("marketplace provenance changed during acquisition; retry".into());
            }
            prepared.acquired.snapshot
        }
        None => {
            if source.remote.is_some() {
                return Err("remote plugin requires acquisition before publication".into());
            }
            let relative = market.local_source(name)?;
            let root = source.root.join(relative);
            if !std::fs::canonicalize(&root)
                .map_err(io)?
                .starts_with(&source.root)
            {
                return Err("plugin source escapes its marketplace".into());
            }
            Snapshot::capture(&root)?
        }
    };
    let total = registry
        .installed
        .values()
        .try_fold(snapshot.bytes, |sum, entry| sum.checked_add(entry.bytes))
        .ok_or("installed plugin byte count overflowed")?;
    if total > 128 * 1024 * 1024 {
        return Err("installed plugin assets exceed 128 MiB".into());
    }
    let entry = market
        .entries
        .get(name)
        .ok_or("plugin entry disappeared")?
        .clone();
    let manifest = components::validate(&snapshot, &entry)?;
    if registry
        .installed
        .values()
        .any(|entry| entry.namespace == manifest.name)
    {
        return Err("another installed plugin has this namespace".into());
    }
    let root = snapshot.publish(config)?;
    let data = data_path(config, id);
    destination::Parent::open(&data.join(".probe"), true).map_err(io)?;
    #[cfg(unix)]
    for directory in [data.clone(), config.join("claude-plugins/data")] {
        std::fs::File::open(directory)
            .and_then(|file| file.sync_all())
            .map_err(io)?;
    }
    registry.installed.insert(
        identity,
        Installation {
            root,
            digest: snapshot.digest,
            namespace: manifest.name,
            bytes: snapshot.bytes,
            entry,
        },
    );
    Ok(format!("Plugin {id} installed"))
}

pub(super) fn decode_registry(before: &[u8]) -> Result<Registry, String> {
    let document = grapt_config::decode(before, super::super::catalog_config::bounds()?)
        .map_err(|_| "user plugin config is malformed")?;
    registry(
        document
            .value()
            .get("claudePlugins")
            .map(|value| super::super::catalogs::config_json(value.value()))
            .transpose()?
            .as_ref(),
    )
}
