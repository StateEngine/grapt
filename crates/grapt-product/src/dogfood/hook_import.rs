//! Explicit hook import keeps the existing hook validator and config publisher.
//! Source-event filtering applies to imported hooks.
//! Copyright 2025 OpenAI. Apache-2.0 (licenses/openai-codex/).
use super::{
    catalog_config::{CatalogDocument, CatalogEnvironment},
    hooks::HookSettings,
    mcp_config, ServerApplication,
};
use grapt_api::u3a::{
    ApplicationFuture, HookImportRequestDto, HookImportResponseDto, InvalidRequestErrorDto,
    ProjectCurrentLocationQueryDto,
};
use serde_json::{Map, Value};
use std::{collections::BTreeSet, path::Path};

pub(crate) fn request(
    application: ServerApplication,
    query: ProjectCurrentLocationQueryDto,
    request: HookImportRequestDto,
) -> ApplicationFuture<HookImportResponseDto, InvalidRequestErrorDto> {
    Box::pin(async move {
        let selection = super::session_config::catalog::select(&application, &query)
            .await
            .map_err(super::support::invalid)?;
        let hooks: Map<String, Value> = serde_json::from_str(request.hooks.as_str())
            .map_err(|_| super::support::invalid("import hooks must be a JSON event object"))?;
        let mut existing = BTreeSet::new();
        for document in &selection.sources.documents {
            if let Some(events) = document.value.get("hooks") {
                for (name, _) in events
                    .value()
                    .as_object()
                    .ok_or_else(|| super::support::invalid("existing hooks must be an object"))?
                {
                    existing.insert(name.value().as_str().to_owned());
                }
            }
        }
        for event in hooks.keys() {
            super::hooks::Event::parse(event).map_err(|_| {
                super::support::invalid("source contains an unsupported hook event")
            })?;
        }
        let environment =
            CatalogEnvironment::read(Path::new(&application.catalog_snapshot().directory))
                .map_err(super::support::invalid)?;
        selection
            .validate_current(&application)
            .map_err(super::support::invalid)?;
        let location = selection.location.directory;
        tokio::task::spawn_blocking(move || {
            check_origin(&environment, &location)?;
            let (additions, skipped) = partition(hooks, &existing);
            // Even a dry run goes through the actual hook shape/bounds validator.
            validate(
                &serde_json::to_vec(&serde_json::json!({"hooks":additions}))
                    .map_err(|_| "cannot encode hooks")?,
                &super::legacy_names::config_jsonc(&environment.config),
                &environment,
            )?;
            if !request.apply {
                return response(additions.keys().cloned().collect(), skipped);
            }
            mcp_config::file::publish(environment, |before, path, environment| {
                check_origin(&environment, &location)?;
                let mut edited = edit(before, additions)?;
                edited.skipped.extend(skipped);
                validate(&edited.output, path, &environment)?;
                Ok((edited.output, response(edited.imported, edited.skipped)?))
            })
        })
        .await
        .map_err(|_| super::support::invalid("hook import writer could not finish"))?
        .map_err(super::support::invalid)
    })
}

fn partition(
    hooks: Map<String, Value>,
    existing: &BTreeSet<String>,
) -> (Map<String, Value>, Vec<String>) {
    let mut additions = Map::new();
    let mut skipped = Vec::new();
    for (event, groups) in hooks {
        if existing.contains(&event) {
            skipped.push(event);
        } else {
            additions.insert(event, groups);
        }
    }
    (additions, skipped)
}

pub(in crate::dogfood) fn check_origin(
    environment: &CatalogEnvironment,
    location: &str,
) -> Result<(), String> {
    if environment.config.components().any(|part| {
        let part = part.as_os_str().to_string_lossy();
        super::legacy_names::PROJECT_DIRECTORIES
            .iter()
            .any(|name| part.eq_ignore_ascii_case(name))
    }) {
        return Err("hook imports require user configuration outside a repository profile".into());
    }
    let existing = environment
        .config
        .ancestors()
        .find(|path| path.exists())
        .ok_or("hook import config parent is unavailable")?;
    let origin = super::catalog_origin::source_origin(
        existing,
        grapt_plugin::PluginOrigin::UserGlobal,
        Path::new(location),
        environment.service_root(),
    )?;
    if !super::catalog_config::user_owned(origin) {
        return Err(
            "hook imports require user configuration outside the project and its repositories"
                .into(),
        );
    }
    Ok(())
}

fn validate(bytes: &[u8], path: &Path, environment: &CatalogEnvironment) -> Result<(), String> {
    let value = grapt_config::decode(bytes, super::catalog_config::bounds()?)
        .map_err(|_| "hook import configuration is malformed")?
        .into_value();
    // This route is an authenticated operator's explicit configuration write,
    // not a project binding. The publisher still refuses repository profiles.
    let document = CatalogDocument {
        directory: environment.config.clone(),
        source: path.to_string_lossy().into_owned(),
        origin: grapt_plugin::PluginOrigin::UserGlobal,
        value,
    };
    HookSettings::from_sources(&[document]).map(|_| ())
}

fn response(imported: Vec<String>, skipped: Vec<String>) -> Result<HookImportResponseDto, String> {
    serde_json::from_value(serde_json::json!({"imported":imported,"skipped":skipped}))
        .map_err(|_| "hook import result exceeds its bounds".into())
}

struct Edited {
    output: Vec<u8>,
    imported: Vec<String>,
    skipped: Vec<String>,
}

fn edit(before: &[u8], hooks: Map<String, Value>) -> Result<Edited, String> {
    let mut output = String::from_utf8(before.to_vec()).map_err(|_| "user config is not UTF-8")?;
    let mut imported = Vec::new();
    let mut skipped = Vec::new();
    for (event, groups) in hooks {
        let document = grapt_config::decode(output.as_bytes(), super::catalog_config::bounds()?)
            .map_err(|_| "user config is malformed")?;
        if document
            .value()
            .get("hooks")
            .and_then(|hooks| hooks.value().get(&event))
            .is_some()
        {
            skipped.push(event);
            continue;
        }
        let encoded =
            serde_json::to_string(&groups).map_err(|_| "hook groups cannot be encoded")?;
        super::mcp_config::edit::change(
            &mut output,
            &document,
            &["hooks", &event],
            Some(&encoded),
        )?;
        if output.len() > super::catalog_config::DOCUMENT_BYTES {
            return Err("hook import exceeds the user config document bound".into());
        }
        imported.push(event);
    }
    Ok(Edited {
        output: output.into_bytes(),
        imported,
        skipped,
    })
}
