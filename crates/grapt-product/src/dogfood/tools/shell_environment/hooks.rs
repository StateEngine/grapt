//! Hook commands are user authority: project environment changes may remove
//! values but must never inject loader/interpreter settings or restore names.
use super::*;
use crate::dogfood::catalog_config::{
    user_owned, CatalogDocument, CatalogEnvironment, CatalogSources,
};

impl Environment {
    pub(in crate::dogfood) fn for_hooks(
        service: &Path,
        workspace: &Path,
        credentials: &[String],
    ) -> Result<Self, String> {
        Self::for_configured_command(service, workspace, credentials, &BTreeMap::new())
    }
    #[cfg(test)]
    fn hooks_from_sources(
        parent: impl IntoIterator<Item = (OsString, OsString)>,
        documents: &[CatalogDocument],
        credentials: &[String],
        windows: bool,
    ) -> Result<Self, String> {
        Self::configured_from_sources(parent, documents, credentials, windows, &BTreeMap::new())
    }

    pub(in crate::dogfood) fn for_configured_command(
        service: &Path,
        workspace: &Path,
        credentials: &[String],
        overrides: &BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let sources = CatalogSources::read(workspace, CatalogEnvironment::read(service)?)?;
        let mut environment = Self::configured_from_sources(
            std::env::vars_os(),
            &sources.documents,
            credentials,
            cfg!(windows),
            overrides,
        )?;
        for (name, value) in &mut environment.0 {
            if name.to_str().is_some_and(|name| {
                name == "PATH" || cfg!(windows) && name.eq_ignore_ascii_case("PATH")
            }) {
                *value = super::super::configured_program::search_path(workspace, value)?;
            }
        }
        // Windows command shims must not consult the workspace before PATH.
        #[cfg(windows)]
        insert(
            &mut environment.0,
            "NoDefaultCurrentDirectoryInExePath".into(),
            "1".into(),
            true,
        )?;
        if environment.0.len() > MAX_ENTRIES
            || environment
                .0
                .values()
                .any(|value| value.len() > MAX_VALUE_BYTES)
            || environment
                .0
                .iter()
                .map(|(key, value)| key.len() + value.len())
                .sum::<usize>()
                > MAX_BYTES
        {
            return Err("configured command environment exceeds its entry or byte bounds after PATH resolution".into());
        }
        Ok(environment)
    }

    fn configured_from_sources(
        parent: impl IntoIterator<Item = (OsString, OsString)>,
        documents: &[CatalogDocument],
        credentials: &[String],
        windows: bool,
        overrides: &BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let mut user = Policy::fold(
            documents
                .iter()
                .filter(|doc| user_owned(doc.origin))
                .map(|doc| &doc.value),
        )?;
        for (name, value) in overrides {
            insert(&mut user.set, name.into(), value.into(), windows)?;
        }
        user.credentials.extend_from_slice(credentials);
        let mut environment = Self::from_parent(parent, &user, windows)?;
        for document in documents.iter().filter(|doc| !user_owned(doc.origin)) {
            let project = Policy::fold([&document.value])?;
            if !project.set.is_empty() {
                return Err(document
                    .refusal("project environment cannot set values for user lifecycle hooks"));
            }
            environment.0.retain(|name, _| {
                name.to_str().is_some_and(|name| {
                    !secret_name(name, &project.credentials)
                        && !matches!(project.inherit, Inherit::None)
                        && (!matches!(project.inherit, Inherit::Core) || core_name(name, windows))
                        && !project.exclude.iter().any(|pattern| pattern.is_match(name))
                        && (project.include_only.is_empty()
                            || project
                                .include_only
                                .iter()
                                .any(|pattern| pattern.is_match(name)))
                })
            });
        }
        Ok(environment)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    fn source(origin: grapt_plugin::PluginOrigin, value: Value) -> Result<CatalogDocument, String> {
        let value = grapt_config::decode(
            &serde_json::to_vec(&value).map_err(|e| e.to_string())?,
            crate::dogfood::catalog_config::bounds()?,
        )
        .map_err(|e| e.to_string())?
        .into_value();
        Ok(CatalogDocument {
            directory: std::env::temp_dir(),
            source: "hook-env-fixture.jsonc".into(),
            origin,
            value,
        })
    }
    fn parent() -> Vec<(OsString, OsString)> {
        [
            ("PATH", "tools"),
            ("HOME", "home"),
            ("REMOVE", "removed"),
            ("RESTORE", "must-not-return"),
            ("SC_HOOK_AUTH", "secret"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
    }
    #[test]
    fn project_hook_environment_cannot_inject_even_an_absolute_hook_loader() -> Result<(), String> {
        for name in ["BASH_ENV", "LD_PRELOAD", "PYTHONPATH", "PATH", "COMSPEC"] {
            let project = source(
                grapt_plugin::PluginOrigin::Project,
                json!({"shell_environment_policy":{"set":{name:"project-code-sentinel"}}}),
            )?;
            let reason = Environment::hooks_from_sources(parent(), &[project], &[], false)
                .err()
                .ok_or("project hook environment was accepted")?;
            assert!(reason.contains("cannot set values"));
            assert!(!reason.contains("project-code-sentinel"));
        }
        Ok(())
    }
    #[test]
    fn project_hook_filters_intersect_and_cannot_restore_user_or_project_removals(
    ) -> Result<(), String> {
        let documents = [
            source(
                grapt_plugin::PluginOrigin::UserGlobal,
                json!({"shell_environment_policy":{"exclude":["RESTORE"],"set":{"USER_SELECTED":"kept"}}}),
            )?,
            source(
                grapt_plugin::PluginOrigin::Project,
                json!({"shell_environment_policy":{"exclude":["REMOVE"],"include_only":["PATH","HOME","USER_SELECTED","RESTORE"]}}),
            )?,
            source(
                grapt_plugin::PluginOrigin::Project,
                json!({"shell_environment_policy":{"inherit":"all","exclude":[],"include_only":["*"]}}),
            )?,
        ];
        let environment =
            Environment::hooks_from_sources(parent(), &documents, &["SC_HOOK_AUTH".into()], false)?;
        assert_eq!(environment.names(), ["HOME", "PATH", "USER_SELECTED"]);
        for inherit in ["core", "none"] {
            let docs = [
                source(
                    grapt_plugin::PluginOrigin::UserGlobal,
                    json!({"shell_environment_policy":{"inherit":"none","set":{"CUSTOM":"chosen"}}}),
                )?,
                source(
                    grapt_plugin::PluginOrigin::Project,
                    json!({"shell_environment_policy":{"inherit":inherit}}),
                )?,
                source(
                    grapt_plugin::PluginOrigin::Project,
                    json!({"shell_environment_policy":{"inherit":"all"}}),
                )?,
            ];
            assert!(
                Environment::hooks_from_sources(parent(), &docs, &[], false)?
                    .names()
                    .is_empty()
            );
        }
        Ok(())
    }
}
