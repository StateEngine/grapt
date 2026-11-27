//! Installed language-server declarations. Repository documents can only disable them.
use crate::dogfood::catalog_config::{
    user_owned, CatalogDocument, CatalogEnvironment, CatalogSources,
};
use crate::dogfood::catalogs::config_json;
use grapt_config::ConfigValue;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Name(String);
impl Name {
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone)]
pub(super) struct Entry {
    pub disabled: bool,
    pub command: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub extensions: Vec<String>,
    pub initialization: Value,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            disabled: false,
            command: Vec::new(),
            environment: BTreeMap::new(),
            extensions: Vec::new(),
            initialization: Value::Object(Default::default()),
        }
    }
}

impl Entry {
    pub(super) fn language_id<'a>(&self, path: &'a Path) -> &'a str {
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        match extension {
            "rs" => "rust",
            "go" => "go",
            "ts" => "typescript",
            "tsx" => "typescriptreact",
            "js" => "javascript",
            "jsx" => "javascriptreact",
            "py" => "python",
            "c" | "h" => "c",
            "C" | "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => "cpp",
            "zig" => "zig",
            other => other,
        }
    }
}

pub(in crate::dogfood::tools) struct Settings(pub(super) BTreeMap<Name, Entry>);
impl Default for Settings {
    fn default() -> Self {
        let mut settings = Self(BTreeMap::new());
        settings.builtins();
        settings
    }
}

impl Settings {
    pub(in crate::dogfood::tools) fn load(root: &Path, service: &Path) -> Result<Self, String> {
        let sources = CatalogSources::read(root, CatalogEnvironment::read(service)?)?;
        Self::from_sources(&sources.documents)
    }

    pub(super) fn from_sources(documents: &[CatalogDocument]) -> Result<Self, String> {
        let mut settings = Self::default();
        for document in documents.iter().filter(|d| user_owned(d.origin)) {
            if let Some(value) = document.value.get("lsp") {
                settings
                    .apply(value.value())
                    .map_err(|e| document.refusal(&e))?;
            }
        }
        // Apply repository restrictions last so a later user document cannot undo them.
        for document in documents.iter().filter(|d| !user_owned(d.origin)) {
            let Some(value) = document.value.get("lsp") else {
                continue;
            };
            let value = value.value();
            if value.as_bool() == Some(false) {
                settings.0.clear();
                continue;
            }
            let entries = value.as_object().ok_or_else(|| {
                document.refusal("project lsp configuration may only disable language servers")
            })?;
            for (name, value) in entries {
                let name = name.value().as_str();
                validate_name(name).map_err(|e| document.refusal(&e))?;
                let fields = value
                    .value()
                    .as_object()
                    .ok_or_else(|| document.refusal("lsp entry must be an object"))?;
                if fields.len() != 1
                    || value
                        .value()
                        .get("disabled")
                        .and_then(|v| v.value().as_bool())
                        != Some(true)
                {
                    return Err(
                        document.refusal("project lsp configuration may only set disabled: true")
                    );
                }
                if let Some(entry) = settings.0.get_mut(&Name(name.into())) {
                    entry.disabled = true;
                }
            }
        }
        Ok(settings)
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn apply(&mut self, value: &ConfigValue) -> Result<(), String> {
        if let Some(enabled) = value.as_bool() {
            self.0.clear();
            if enabled {
                self.builtins();
                for entry in self.0.values_mut() {
                    entry.disabled = false;
                }
            }
            return Ok(());
        }
        let entries = value
            .as_object()
            .ok_or("lsp must be a boolean or named object")?;
        if self.0.is_empty() {
            self.builtins();
        }
        for (name, value) in entries {
            let name = name.value().as_str();
            validate_name(name)?;
            let fields = value
                .value()
                .as_object()
                .ok_or("lsp entry must be an object")?;
            let name = Name(name.into());
            if !self.0.contains_key(&name) && self.0.len() == 32 {
                return Err("lsp registry exceeds 32 entries".into());
            }
            let entry = self.0.entry(name).or_default();
            for (key, value) in fields {
                let value = value.value();
                match key.value().as_str() {
                    "disabled" => {
                        entry.disabled = value.as_bool().ok_or("lsp disabled must be boolean")?;
                    }
                    "command" => {
                        entry.command = strings(value, 128)?;
                        if entry.command.is_empty()
                            || entry.command.iter().map(String::len).sum::<usize>() > 8192
                            || entry.command[0].contains("$FILE")
                        {
                            return Err(
                                "lsp command requires bounded argv and a fixed executable".into()
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
                            return Err("lsp extensions must be dot-prefixed suffixes".into());
                        }
                    }
                    "env" => {
                        let variables =
                            value.as_object().ok_or("lsp env must be a string object")?;
                        for (key, value) in variables {
                            let name = key.value().as_str();
                            let value = value
                                .value()
                                .as_str()
                                .ok_or("lsp env values must be strings")?;
                            if name.is_empty()
                                || name.len() > 256
                                || name.contains(['=', '\0'])
                                || value.len() > 32768
                                || value.contains('\0')
                            {
                                return Err("lsp env name or value is invalid".into());
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
                            return Err("lsp env exceeds its bound".into());
                        }
                    }
                    "initialization" => {
                        if value.as_object().is_none() {
                            return Err("lsp initialization must be an object".into());
                        }
                        let initialization = config_json(value)?;
                        let encoded = serde_json::to_vec(&initialization)
                            .map_err(|_| "lsp initialization cannot be serialized".to_owned())?;
                        if encoded.len() > 16 * 1024 {
                            return Err("lsp initialization exceeds 16 KiB".into());
                        }
                        entry.initialization = initialization;
                    }
                    _ => return Err("unknown lsp entry field".into()),
                }
            }
        }
        Ok(())
    }

    fn builtins(&mut self) {
        for (name, extensions, args) in [
            ("rust-analyzer", vec![".rs"], vec!["rust-analyzer"]),
            ("gopls", vec![".go"], vec!["gopls"]),
            (
                "typescript-language-server",
                vec![".ts", ".tsx", ".js", ".jsx"],
                vec!["typescript-language-server", "--stdio"],
            ),
            (
                "pyright-langserver",
                vec![".py"],
                vec!["pyright-langserver", "--stdio"],
            ),
            (
                "clangd",
                vec![
                    ".c", ".h", ".C", ".cc", ".cpp", ".cxx", ".hh", ".hpp", ".hxx",
                ],
                vec!["clangd"],
            ),
            ("zls", vec![".zig"], vec!["zls"]),
        ] {
            self.0.insert(
                Name(name.into()),
                Entry {
                    // Installed servers may execute project build scripts or plugins.
                    // Availability is not user authority to run project-controlled code.
                    disabled: true,
                    command: args.into_iter().map(str::to_owned).collect(),
                    extensions: extensions.into_iter().map(str::to_owned).collect(),
                    ..Entry::default()
                },
            );
        }
    }
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err("lsp name must have 1..128 bytes without controls".into());
    }
    Ok(())
}

fn strings(value: &ConfigValue, maximum: usize) -> Result<Vec<String>, String> {
    let values = value.as_array().ok_or("lsp field must be a string array")?;
    if values.len() > maximum {
        return Err("lsp array exceeds its bound".into());
    }
    values
        .iter()
        .map(|v| {
            let s = v
                .value()
                .as_str()
                .ok_or("lsp array element must be a string")?;
            if s.is_empty() || s.len() > 8192 || s.contains('\0') {
                return Err("lsp array element is empty or invalid".into());
            }
            Ok(s.into())
        })
        .collect()
}
