use std::fs::{File, Metadata};
use std::io::Read;
use std::path::Path;

use crate::compose::ProductRefusal;

const CONFIG_BYTES: usize = 6_144;
const CREDENTIAL_BYTES: usize = 256;

#[derive(Debug)]
pub(crate) struct ServiceConfig {
    pub(crate) slot: String,
    pub(crate) instance: String,
    pub(crate) environment: String,
    pub(crate) build: String,
    pub(crate) version: String,
    pub(crate) serve_window_ms: u64,
    pub(crate) provider_connect_ms: u64,
    pub(crate) provider_response_head_ms: u64,
    pub(crate) provider_active_attempts: usize,
    /// How long a Session survives losing its last window without being told
    /// why. Absent takes the product default; a deployment sets it here.
    pub(crate) close_grace_ms: Option<u64>,
    pub(crate) max_inflight_streams: Option<usize>,
}

/// Which provider cache bucket a Session's requests name: a hidden,
/// undocumented diagnostic. `Session` is
/// today's behaviour, byte for byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PromptCacheScope {
    #[default]
    Session,
    /// The body's prompt_cache_key is the project's; `session-id` stays the
    /// Session's.
    ProjectKey,
    /// Both are the project's.
    ProjectKeyAndHeader,
}

impl PromptCacheScope {
    fn parse(token: &str) -> Option<Self> {
        match token {
            "session" => Some(Self::Session),
            "project-key" => Some(Self::ProjectKey),
            "project-key-and-header" => Some(Self::ProjectKeyAndHeader),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Credential {
    pub(crate) username: String,
    pub(crate) secret: String,
}

/// The profile's default permission preset from its `service.conf`, which
/// startup validated; `None` when the profile sets none. Read per turn, so
/// an edit applies to the next one.
pub(crate) fn read_permission_preset(path: &Path) -> Result<Option<String>, ProductRefusal> {
    let text = read_regular(path, CONFIG_BYTES)?;
    Ok(text
        .lines()
        .find_map(|line| line.strip_prefix("permission_preset="))
        .map(str::to_owned))
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(crate) fn read_config(
    path: &Path,
    published_build: &str,
) -> Result<ServiceConfig, ProductRefusal> {
    let text = read_regular(path, CONFIG_BYTES)?;
    let mut values = std::collections::BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let (key, value) = line.split_once('=').ok_or_else(|| {
            ProductRefusal::Input(format!("invalid config line {number}: expected key=value"))
        })?;
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(ProductRefusal::Input(format!(
                "invalid config key at line {number}"
            )));
        }
        if !matches!(
            key,
            "slot"
                | "instance"
                | "environment"
                | "version"
                | "serve_window_ms"
                | "provider_mode"
                | "provider_connect_ms"
                | "provider_response_head_ms"
                | "provider_active_attempts"
                | "max_inflight_streams"
                | "prompt_cache_scope"
                | "permission_preset"
                | "provider_error_text"
                | "close_grace_ms"
        ) {
            return Err(ProductRefusal::Input(format!(
                "unknown config key {:?} at line {number}",
                diagnostic_token(key)
            )));
        }
        if values.insert(key, (value, number)).is_some() {
            return Err(ProductRefusal::Input(format!(
                "duplicate config key {key:?} at line {number}"
            )));
        }
    }
    let value = |key| {
        values
            .get(key)
            .copied()
            .filter(|(value, _)| !value.is_empty() && value.len() <= 1_024)
            .ok_or_else(|| ProductRefusal::Input(format!("missing or invalid config key {key}")))
    };
    if let Some((preset, line)) = values.get("permission_preset").copied() {
        if grapt_permission::preset::Preset::parse(preset).is_none() {
            return Err(ProductRefusal::Input(format!(
                "config key permission_preset at line {line} has unsupported token {:?}; expected inspect|author|maintainer|bypass",
                diagnostic_token(preset)
            )));
        }
    }
    if let Some((token, line)) = values.get("provider_error_text").copied() {
        provider_error_text_token(token, line)?;
    }
    let window_ms = value("serve_window_ms")?
        .0
        .parse::<u64>()
        .map_err(|_| ProductRefusal::Input("serve_window_ms is not numeric".to_owned()))?;
    if window_ms == 0 || window_ms > 86_400_000 {
        return Err(ProductRefusal::Input(
            "serve_window_ms must be within 1..=86400000".to_owned(),
        ));
    }
    let (mode, line) = value("provider_mode")?;
    if mode == "deterministic" {
        return Err(ProductRefusal::TerminalSetup(
            grapt_platform::ConfigPublicationError::LocalModeRemoved.to_string(),
        ));
    }
    if mode != "production" {
        return Err(ProductRefusal::Input(format!("config key provider_mode at line {line} has unsupported token {:?}; expected production; start grapt code and use Connect (/connect)", diagnostic_token(mode))));
    }
    let provider_connect_ms =
        duration_millis(value("provider_connect_ms")?.0, "provider_connect_ms")?;
    let provider_response_head_ms = duration_millis(
        value("provider_response_head_ms")?.0,
        "provider_response_head_ms",
    )?;
    let provider_active_attempts = provider_attempts(value("provider_active_attempts")?.0)?;
    // Optional; refused at launch when present and unknown.
    if let Some((token, line)) = values.get("prompt_cache_scope").copied() {
        prompt_cache_scope_token(token, line)?;
    }
    // Optional: a deployment's own silent-loss grace for a window lease.
    let close_grace_ms = match values.get("close_grace_ms").copied() {
        Some((raw, _)) => {
            let millis = raw
                .parse::<u64>()
                .map_err(|_| ProductRefusal::Input("close_grace_ms is not numeric".to_owned()))?;
            if millis == 0 || millis > 86_400_000 {
                return Err(ProductRefusal::Input(
                    "close_grace_ms must be within 1..=86400000".to_owned(),
                ));
            }
            Some(millis)
        }
        None => None,
    };
    Ok(ServiceConfig {
        slot: value("slot")?.0.to_owned(),
        instance: value("instance")?.0.to_owned(),
        environment: value("environment")?.0.to_owned(),
        build: published_build.to_owned(),
        version: value("version")?.0.to_owned(),
        serve_window_ms: window_ms,
        provider_connect_ms,
        provider_response_head_ms,
        provider_active_attempts,
        close_grace_ms,
        max_inflight_streams: optional_usize(&values, "max_inflight_streams")?,
    })
}

fn provider_attempts(value: &str) -> Result<usize, ProductRefusal> {
    let attempts = value
        .parse::<usize>()
        .map_err(|_| ProductRefusal::Input("provider_active_attempts is not numeric".to_owned()))?;
    if !(1..=64).contains(&attempts) {
        return Err(ProductRefusal::Input(
            "provider_active_attempts must be within 1..=64".to_owned(),
        ));
    }
    Ok(attempts)
}

fn optional_usize(
    values: &std::collections::BTreeMap<&str, (&str, usize)>,
    key: &'static str,
) -> Result<Option<usize>, ProductRefusal> {
    values
        .get(key)
        .map(|(value, _)| {
            value
                .parse::<usize>()
                .map_err(|_| ProductRefusal::Input(format!("{key} must be a nonnegative integer")))
        })
        .transpose()
}

fn prompt_cache_scope_token(token: &str, line: usize) -> Result<PromptCacheScope, ProductRefusal> {
    PromptCacheScope::parse(token).ok_or_else(|| {
        ProductRefusal::Input(format!(
            "config key prompt_cache_scope at line {line} has unsupported token {:?}; expected session|project-key|project-key-and-header",
            diagnostic_token(token)
        ))
    })
}

/// The profile's prompt-cache scope, read where a Session's key is minted.
/// Absent is `Session`; launch already refused a malformed file or token.
pub(crate) fn read_prompt_cache_scope(path: &Path) -> Result<PromptCacheScope, ProductRefusal> {
    let text = read_regular(path, CONFIG_BYTES)?;
    for (index, line) in text.lines().enumerate() {
        if let Some(token) = line.strip_prefix("prompt_cache_scope=") {
            return prompt_cache_scope_token(token, index + 1);
        }
    }
    Ok(PromptCacheScope::Session)
}

// Identifiers and enum tokens remain useful diagnostics. Arbitrary lines and
// values never go through this path; token rendering has a finite ASCII bound.
fn diagnostic_token(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_graphic)
        .take(32)
        .collect()
}

fn duration_millis(raw: &str, key: &'static str) -> Result<u64, ProductRefusal> {
    let value = raw
        .parse::<u64>()
        .map_err(|_| ProductRefusal::Input(format!("{key} is not numeric")))?;
    if !(1..=300_000).contains(&value) {
        return Err(ProductRefusal::Input(format!(
            "{key} must be within 1..=300000"
        )));
    }
    Ok(value)
}

pub(crate) fn read_credential(path: &Path) -> Result<Credential, ProductRefusal> {
    let text = read_regular(path, CREDENTIAL_BYTES)?;
    if text.contains('\r') || text.lines().count() != 1 {
        return Err(ProductRefusal::Input(
            "basic.credential must contain one LF-free username:secret record".to_owned(),
        ));
    }
    let (username, secret) = text
        .trim_end_matches('\n')
        .split_once(':')
        .ok_or_else(|| ProductRefusal::Input("basic.credential is malformed".to_owned()))?;
    if username.is_empty() || username.len() > 128 || secret.is_empty() || secret.len() > 256 {
        return Err(ProductRefusal::Input(
            "basic.credential violates Basic bounds".to_owned(),
        ));
    }
    Ok(Credential {
        username: username.to_owned(),
        secret: secret.to_owned(),
    })
}

fn read_regular(path: &Path, maximum: usize) -> Result<String, ProductRefusal> {
    use grapt_platform::profile_configuration::{frozen, FrozenRead};
    match frozen(path, maximum).map_err(|error| ProductRefusal::Input(error.to_string()))? {
        FrozenRead::Bytes(bytes) => {
            return String::from_utf8(bytes)
                .map_err(|_| ProductRefusal::Input("approved configuration is not UTF-8".into()))
        }
        FrozenRead::Missing => {
            return Err(ProductRefusal::Input(
                "approved configuration is absent".into(),
            ))
        }
        FrozenRead::Unbound => {}
    }
    let metadata = std::fs::metadata(path)
        .map_err(|error| ProductRefusal::Input(format!("{}: {error}", path.display())))?;
    require_regular(path, &metadata)?;
    if metadata.len() > maximum as u64 {
        return Err(ProductRefusal::Input(format!(
            "{} exceeds {maximum} bytes",
            path.display()
        )));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)
        .and_then(|file| file.take(maximum as u64 + 1).read_to_end(&mut bytes))
        .map_err(|error| ProductRefusal::Input(format!("{}: {error}", path.display())))?;
    if bytes.len() > maximum {
        return Err(ProductRefusal::Input(format!(
            "{} exceeds {maximum} bytes",
            path.display()
        )));
    }
    String::from_utf8(bytes)
        .map_err(|_| ProductRefusal::Input(format!("{} is not UTF-8", path.display())))
}

fn require_regular(path: &Path, metadata: &Metadata) -> Result<(), ProductRefusal> {
    if metadata.file_type().is_file() {
        Ok(())
    } else {
        Err(ProductRefusal::Input(format!(
            "{} is not a regular file",
            path.display()
        )))
    }
}

fn provider_error_text_token(token: &str, line: usize) -> Result<bool, ProductRefusal> {
    match token {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(ProductRefusal::Input(format!(
            "config key provider_error_text at line {line} must be true|false"
        ))),
    }
}

/// Optional deployment diagnostic. Read once when provider openers are built.
pub(crate) fn read_provider_error_text(path: &Path) -> Result<bool, ProductRefusal> {
    let text = read_regular(path, CONFIG_BYTES)?;
    for (index, line) in text.lines().enumerate() {
        if let Some(token) = line.strip_prefix("provider_error_text=") {
            return provider_error_text_token(token, index + 1);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    #[test]
    fn enum_and_unknown_key_diagnostics_keep_bounded_ascii_tokens(
    ) -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(super::diagnostic_token("known_key"), "known_key");
        assert_eq!(super::diagnostic_token(&"x".repeat(100)).len(), 32);
        assert_eq!(super::diagnostic_token("é\tkey\u{1b}"), "key");
        Ok(())
    }

    #[test]
    fn config_refusals_name_positions_without_authored_input(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root =
            std::env::temp_dir().join(format!("grapt-config-refusal-{}", std::process::id()));
        std::fs::create_dir(&root)?;
        let path = root.join("service.conf");
        for input in [
            "slot=product\napi_key sk-test-123456\n",
            "slot=product\napi_key sk-test-123456=ignored\n",
            "slot=product\napi_key=sk-test-123456\n",
            "slot=product\nslot=sk-test-123456\n",
        ] {
            std::fs::write(&path, input)?;
            let refusal = super::read_config(&path, "fixture")
                .err()
                .ok_or("invalid input admitted")?;
            let message = refusal.to_string();
            assert!(
                !message.contains("sk-test") && !message.contains("123456"),
                "config refusal exposed authored input: {message}"
            );
            assert!(
                message.contains("line "),
                "config refusal omitted line number: {message}"
            );
        }
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn the_prompt_cache_scope_is_optional_and_its_token_is_checked_at_launch(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root =
            std::env::temp_dir().join(format!("grapt-config-cache-scope-{}", std::process::id()));
        std::fs::create_dir(&root)?;
        let path = root.join("service.conf");
        let base = "slot=product\ninstance=i\nenvironment=e\nversion=0.1.0\nserve_window_ms=1\nprovider_mode=production\nprovider_connect_ms=1\nprovider_response_head_ms=1\nprovider_active_attempts=1\n";
        for scope in ["", "prompt_cache_scope=project-key-and-header\n"] {
            std::fs::write(&path, format!("{base}{scope}"))?;
            super::read_config(&path, "fixture")?;
        }
        std::fs::write(&path, format!("{base}prompt_cache_scope=everything\n"))?;
        let refusal = super::read_config(&path, "fixture")
            .err()
            .ok_or("an unknown scope was admitted")?
            .to_string();
        assert!(
            refusal.contains("expected session|project-key|project-key-and-header"),
            "{refusal}"
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn provider_error_text_is_optional_off_and_strictly_boolean(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root =
            std::env::temp_dir().join(format!("grapt-error-text-config-{}", std::process::id()));
        std::fs::create_dir(&root)?;
        let path = root.join("service.conf");
        let base = "slot=product\ninstance=i\nenvironment=e\nversion=0.1.0\nserve_window_ms=1\nprovider_mode=production\nprovider_connect_ms=1\nprovider_response_head_ms=1\nprovider_active_attempts=1\n";
        for (flag, on) in [
            ("", false),
            ("provider_error_text=false\n", false),
            ("provider_error_text=true\n", true),
        ] {
            std::fs::write(&path, format!("{base}{flag}"))?;
            super::read_config(&path, "fixture")?;
            assert_eq!(super::read_provider_error_text(&path)?, on);
        }
        for invalid in ["TRUE", "1", "", "sk-private-value"] {
            std::fs::write(&path, format!("{base}provider_error_text={invalid}\n"))?;
            let error = super::read_config(&path, "fixture")
                .err()
                .ok_or("invalid flag accepted")?
                .to_string();
            assert!(error.contains("true|false"));
            assert!(!error.contains("sk-private-value"));
        }
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
