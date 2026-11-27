use super::*;

const MAX_PATTERNS: usize = 64;
const MAX_PATTERN_BYTES: usize = 256;
const MAX_SET: usize = 64;

impl Policy {
    pub(super) fn fold<'a>(
        documents: impl IntoIterator<Item = &'a ConfigValue>,
    ) -> Result<Self, String> {
        let mut policy = Self::default();
        for document in documents {
            if let Some(name) = document
                .get("provider")
                .and_then(|p| p.value().get("credential"))
                .and_then(|c| c.value().as_str())
                .and_then(|s| s.strip_prefix("env:"))
            {
                policy.credentials.push(name.to_owned());
            }
            let Some(value) = document.get("shell_environment_policy") else {
                continue;
            };
            let fields = value
                .value()
                .as_object()
                .ok_or("config shell_environment_policy must be an object")?;
            for (key, value) in fields {
                match key.value().as_str() {
                    "inherit" => policy.inherit = match value.value().as_str() {
                        Some("all") => Inherit::All,
                        Some("core") => Inherit::Core,
                        Some("none") => Inherit::None,
                        _ => return Err("config shell_environment_policy.inherit must be all, core or none".into()),
                    },
                    "exclude" => policy.exclude = patterns(value.value(), "exclude")?,
                    "include_only" => policy.include_only = patterns(value.value(), "include_only")?,
                    "set" => {
                        let pairs = value.value().as_object().ok_or("config shell_environment_policy.set must be an object of strings")?;
                        if pairs.len() > MAX_SET { return Err("shell environment set exceeds 64 entries".into()); }
                        for (name, value) in pairs {
                            let value = value.value().as_str().ok_or("shell environment set values must be strings")?;
                            // Configuration overrides names case-insensitively on
                            // Windows, just as the actual process environment does.
                            insert(&mut policy.set, name.value().as_str().into(), value.into(), cfg!(windows))?;
                        }
                        if policy.set.len() > MAX_SET { return Err("shell environment set exceeds 64 entries".into()); }
                    },
                    _ => return Err("unsupported shell_environment_policy field; supported: inherit, exclude, include_only, set".into()),
                }
            }
        }
        Ok(policy)
    }
}

fn patterns(value: &ConfigValue, field: &str) -> Result<Vec<regex::Regex>, String> {
    let entries = value
        .as_array()
        .ok_or_else(|| format!("shell_environment_policy.{field} must be an array of globs"))?;
    if entries.len() > MAX_PATTERNS {
        return Err("shell environment filters exceed 64 patterns".into());
    }
    entries
        .iter()
        .map(|entry| {
            let text = entry
                .value()
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= MAX_PATTERN_BYTES && !s.contains('\0'))
                .ok_or("shell environment glob must contain 1..256 bytes")?;
            let source = regex::escape(text).replace(r"\*", ".*").replace(r"\?", ".");
            regex::RegexBuilder::new(&format!("\\A{source}\\z"))
                .case_insensitive(true)
                .dot_matches_new_line(true)
                .build()
                .map_err(|_| "shell environment glob could not be compiled".to_owned())
        })
        .collect()
}
