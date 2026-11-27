//! Opt-in formatter declarations. Repository documents can only disable them.
use crate::dogfood::catalog_config::{
    user_owned, CatalogDocument, CatalogEnvironment, CatalogSources,
};
use grapt_config::ConfigValue;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Name(String);
impl Name {
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Default)]
pub(super) struct Entry {
    pub disabled: bool,
    pub command: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub extensions: Vec<String>,
}

#[derive(Default)]
pub(in crate::dogfood::tools) struct Settings(pub(super) BTreeMap<Name, Entry>);
impl Settings {
    pub(in crate::dogfood::tools) fn load(root: &Path, service: &Path) -> Result<Self, String> {
        let sources = CatalogSources::read(root, CatalogEnvironment::read(service)?)?;
        Self::from_sources(&sources.documents)
    }

    pub(super) fn from_sources(documents: &[CatalogDocument]) -> Result<Self, String> {
        let mut settings = Self::default();
        for document in documents.iter().filter(|d| user_owned(d.origin)) {
            if let Some(value) = document.value.get("formatter") {
                settings
                    .apply(value.value())
                    .map_err(|e| document.refusal(&e))?;
            }
        }
        // Repository input never declares argv, environment or extension bindings,
        // even when it precedes a user document in catalog order.
        for document in documents.iter().filter(|d| !user_owned(d.origin)) {
            let Some(value) = document.value.get("formatter") else {
                continue;
            };
            let value = value.value();
            if value.as_bool() == Some(false) {
                settings.0.clear();
                continue;
            }
            let entries = value.as_object().ok_or_else(|| {
                document.refusal("project formatter configuration may only disable user formatters")
            })?;
            for (name, value) in entries {
                let fields = value
                    .value()
                    .as_object()
                    .ok_or_else(|| document.refusal("formatter entry must be an object"))?;
                if fields.len() != 1
                    || value
                        .value()
                        .get("disabled")
                        .and_then(|v| v.value().as_bool())
                        != Some(true)
                {
                    return Err(document
                        .refusal("project formatter configuration may only set disabled: true"));
                }
                if let Some(entry) = settings.0.get_mut(&Name(name.value().as_str().into())) {
                    entry.disabled = true;
                }
            }
        }
        Ok(settings)
    }

    fn apply(&mut self, value: &ConfigValue) -> Result<(), String> {
        if let Some(enabled) = value.as_bool() {
            self.0.clear();
            if enabled {
                self.builtins();
            }
            return Ok(());
        }
        let entries = value
            .as_object()
            .ok_or("formatter must be a boolean or named object")?;
        // An object enables the built-in registry on the first declaration.
        if self.0.is_empty() {
            self.builtins();
        }
        for (name, value) in entries {
            let name = name.value().as_str();
            if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
                return Err("formatter name must have 1..128 bytes without controls".into());
            }
            let fields = value
                .value()
                .as_object()
                .ok_or("formatter entry must be an object")?;
            let entry = self.0.entry(Name(name.into())).or_default();
            for (key, value) in fields {
                let value = value.value();
                match key.value().as_str() {
                    "disabled" => {
                        entry.disabled = value
                            .as_bool()
                            .ok_or("formatter disabled must be boolean")?
                    }
                    "command" => {
                        entry.command = strings(value, 128)?;
                        if entry.command.is_empty()
                            || entry.command.iter().map(String::len).sum::<usize>() > 8192
                            || entry.command[0].contains("$FILE")
                        {
                            return Err(
                                "formatter command requires bounded argv and a fixed executable"
                                    .into(),
                            );
                        }
                    }
                    "extensions" => {
                        entry.extensions = strings(value, 64)?;
                        if entry
                            .extensions
                            .iter()
                            .any(|s| !s.starts_with('.') || s.len() > 64 || s.contains(['/', '\\']))
                        {
                            return Err("formatter extensions must be dot-prefixed suffixes".into());
                        }
                    }
                    "environment" => {
                        let variables = value
                            .as_object()
                            .ok_or("formatter environment must be a string object")?;
                        for (key, value) in variables {
                            let name = key.value().as_str();
                            let value = value
                                .value()
                                .as_str()
                                .ok_or("formatter environment values must be strings")?;
                            if name.is_empty()
                                || name.len() > 256
                                || name.contains(['=', '\0'])
                                || value.len() > 32768
                                || value.contains('\0')
                            {
                                return Err("formatter environment name or value is invalid".into());
                            }
                            if cfg!(windows) {
                                entry
                                    .environment
                                    .retain(|key, _| !key.eq_ignore_ascii_case(name));
                            }
                            entry.environment.insert(name.into(), value.into());
                        }
                        if entry.environment.len() > 128
                            || entry
                                .environment
                                .iter()
                                .map(|(k, v)| k.len() + v.len())
                                .sum::<usize>()
                                > 65536
                        {
                            return Err("formatter environment exceeds its bound".into());
                        }
                    }
                    _ => return Err("unknown formatter entry field".into()),
                }
            }
        }
        if self.0.len() > 32 {
            return Err("formatter registry exceeds 32 entries".into());
        }
        Ok(())
    }

    fn builtins(&mut self) {
        for (name, extension, args) in [
            ("gofmt", ".go", vec!["gofmt", "-w", "$FILE"]),
            (
                "rustfmt",
                ".rs",
                vec!["rustfmt", "--config", "skip_children=true", "$FILE"],
            ),
        ] {
            self.0.insert(
                Name(name.into()),
                Entry {
                    command: args.into_iter().map(str::to_owned).collect(),
                    extensions: vec![extension.into()],
                    ..Entry::default()
                },
            );
        }
    }
}
fn strings(value: &ConfigValue, maximum: usize) -> Result<Vec<String>, String> {
    let values = value
        .as_array()
        .ok_or("formatter field must be a string array")?;
    if values.len() > maximum {
        return Err("formatter array exceeds its bound".into());
    }
    values
        .iter()
        .map(|v| {
            let s = v
                .value()
                .as_str()
                .ok_or("formatter array element must be a string")?;
            if s.is_empty() || s.len() > 8192 || s.contains('\0') {
                return Err("formatter array element is empty or invalid".into());
            }
            Ok(s.into())
        })
        .collect()
}
