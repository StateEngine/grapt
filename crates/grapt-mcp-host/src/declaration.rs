//! Transport declarations and bounded validation, independent of configuration sources.
use crate::timeouts::McpTimeouts;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// One declared server, as configuration described it.
#[derive(Clone, PartialEq, Eq)]
pub enum Declaration {
    /// Declared with `disabled`/`enabled: false`. Never contacted.
    Disabled,
    /// A local server started as a child process.
    ///
    /// EVERY FIELD HERE IS PART OF CONNECTION IDENTITY, because `Declaration`
    /// derives `PartialEq` and a held connection is reused only while its
    /// declaration still matches. That is not incidental: changing a server's
    /// `environment` or `cwd` produces a DIFFERENT server -- a different
    /// program view of the world, possibly different credentials -- and
    /// handing a caller the old child would dispatch their tool call into a
    /// process the current configuration does not describe. Adding these
    /// fields therefore makes a configuration edit restart the server, which
    /// is the correct behaviour and comes for free from the existing rule.
    Local {
        command: Vec<String>,
        /// The child's working directory, ALREADY RESOLVED against the
        /// location. Stored resolved rather than as written so identity
        /// compares the directory the child will actually get: `.` and an
        /// absolute path naming the same place are the same server.
        cwd: String,
        /// Configured environment entries, layered over this product's
        /// ISOLATED base -- never over the parent's ambient environment.
        environment: BTreeMap<String, String>,
        /// Phase budgets for this server.
        timeouts: McpTimeouts,
    },
    /// A remote server, with the same held-session identity as local servers.
    /// Its transport inherits an obligation stdio does not have:
    /// every request after the handshake must carry the negotiated revision in
    /// an `MCP-Protocol-Version` header. That is the only behaviour the pinned
    /// oracle dependency varies by negotiated revision, and
    /// `ServerIdentity::negotiated_protocol` exists to supply it. Composing a
    /// remote transport without it would send a server a revision it never
    /// agreed to.
    Remote {
        url: String,
        headers: BTreeMap<String, String>,
        oauth: bool,
        timeouts: McpTimeouts,
    },
}

impl std::fmt::Debug for Declaration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => formatter.write_str("Disabled"),
            Self::Local {
                command,
                cwd,
                environment,
                timeouts,
            } => formatter
                .debug_struct("Local")
                .field("command", command)
                .field("cwd", cwd)
                .field("environment_keys", &environment.keys().collect::<Vec<_>>())
                .field("timeouts", timeouts)
                .finish(),
            Self::Remote {
                url,
                headers,
                oauth,
                timeouts,
            } => formatter
                .debug_struct("Remote")
                .field(
                    "origin",
                    &url::Url::parse(url)
                        .ok()
                        .map(|url| url.origin().ascii_serialization()),
                )
                .field("header_names", &headers.keys().collect::<Vec<_>>())
                .field("oauth", oauth)
                .field("timeouts", timeouts)
                .finish(),
        }
    }
}

/// The child's working directory: the location, or a configured value resolved
/// against it.
///
/// RESOLVED AND LEXICALLY NORMALISED HERE, because the result is part of
/// connection identity and of the approval a user grants. `.` and the
/// location's own path name one directory, and an edit between those spellings
/// changes nothing a child would observe; without normalisation they compare
/// unequal, so such an edit would retire a healthy connection and invalidate a
/// standing approval.
///
/// LEXICAL, NOT `canonicalize`. Resolving through the filesystem would consult
/// symlinks and require the directory to exist, making connection identity
/// depend on state outside the configuration -- two identical configs could
/// then disagree, and a server could be retired by something moving on disk.
/// `.` and `..` are folded textually and nothing else is.
fn resolve_cwd(value: Option<&Value>, location: &str, name: &str) -> Result<String, String> {
    let Some(value) = value else {
        return Ok(normalise_lexically(Path::new(location)));
    };
    let declared = value
        .as_str()
        .ok_or_else(|| format!("MCP server {name} cwd must be a string"))?;
    if declared.is_empty() {
        return Err(format!("MCP server {name} cwd must not be empty"));
    }
    if declared.len() > 4096 {
        return Err(format!("MCP server {name} cwd exceeds its 4096-byte bound"));
    }
    // Relative against the workspace, as the frozen contract resolves it; an
    // absolute path is taken as given, which `join` already does.
    Ok(normalise_lexically(&Path::new(location).join(declared)))
}

/// Fold `.` and `..` textually, touching no filesystem.
///
/// LEXICAL, NOT `canonicalize`: resolving through the filesystem would make
/// connection identity depend on symlinks and on the directory existing, so
/// two identical configurations could disagree and something moving on disk
/// could retire a live server.
///
/// TWO THINGS THE FIRST VERSION GOT WRONG ON WINDOWS, both measured against
/// `path.win32.resolve`, which is what the retained behaviour uses:
///
///   * `..` ABOVE AN ABSOLUTE ROOT WAS KEPT, so `C:\work\..\..\x` produced
///     `C:/../x` where the oracle gives `C:\x`. There is no directory above a
///     root to name, so preserving the component invents a path rather than
///     protecting one; an absolute root CLAMPS. A relative path has no root to
///     clamp against, so a leading `..` there is still meaningful and kept.
///
///   * SEPARATORS WERE REWRITTEN TO `/`, so `C:\x` became `C:/x` and a
///     verbatim prefix had its separators rewritten too -- which Windows does
///     not do inside one, making that a different path rather than a different
///     spelling of the same one. The platform separator is used, and the
///     prefix is emitted exactly as it was parsed.
fn normalise_lexically(path: &Path) -> String {
    use std::path::Component;
    let mut prefix: Option<std::ffi::OsString> = None;
    let mut rooted = false;
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(raw) => prefix = Some(raw.as_os_str().to_owned()),
            Component::RootDir => rooted = true,
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.last().is_some_and(|last| last != "..") {
                    parts.pop();
                } else if !rooted {
                    // Only a RELATIVE path has somewhere above it to name.
                    parts.push(std::ffi::OsString::from(".."));
                }
            }
            Component::Normal(part) => parts.push(part.to_owned()),
        }
    }
    let separator = std::path::MAIN_SEPARATOR;
    let mut text = String::new();
    if let Some(prefix) = prefix {
        text.push_str(&prefix.to_string_lossy());
    }
    if rooted {
        text.push(separator);
    }
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            text.push(separator);
        }
        text.push_str(&part.to_string_lossy());
    }
    if text.is_empty() {
        ".".to_owned()
    } else {
        text
    }
}

/// Configured environment entries for one server.
///
/// These are layered over this product's ISOLATED base at spawn, never over
/// the parent's ambient environment. Values are NOT logged anywhere: an MCP
/// server's environment is exactly where an API token lives, so a diagnostic
/// that echoed one would turn a configuration feature into a credential leak.
/// Every message below names the KEY and never the value.
fn server_environment(
    value: Option<&Value>,
    name: &str,
) -> Result<BTreeMap<String, String>, String> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| format!("MCP server {name} environment must be an object"))?;
    if object.len() > 256 {
        return Err(format!(
            "MCP server {name} environment declares more than 256 entries"
        ));
    }
    let mut entries = BTreeMap::new();
    for (key, value) in object {
        if key.is_empty() || key.len() > 1024 {
            return Err(format!(
                "MCP server {name} environment has an invalid variable name"
            ));
        }
        // A NUL or `=` in a name is rejected by the OS at spawn with a message
        // that names neither the server nor the variable.
        if key.contains('\0') || key.contains('=') {
            return Err(format!(
                "MCP server {name} environment variable {key} contains an invalid character"
            ));
        }
        let text = value.as_str().ok_or_else(|| {
            format!("MCP server {name} environment variable {key} must be a string")
        })?;
        if text.len() > 32_768 {
            return Err(format!(
                "MCP server {name} environment variable {key} exceeds its 32768-byte bound"
            ));
        }
        if text.contains('\0') {
            return Err(format!(
                "MCP server {name} environment variable {key} contains a NUL"
            ));
        }
        entries.insert(key.clone(), text.to_owned());
    }
    Ok(entries)
}

fn string_map(
    value: Option<&Value>,
    name: &str,
    field: &str,
) -> Result<BTreeMap<String, String>, String> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| format!("MCP server {name} {field} must be an object"))?;
    if object.len() > 256 {
        return Err(format!(
            "MCP server {name} {field} declares more than 256 entries"
        ));
    }
    object
        .iter()
        .map(|(key, value)| {
            if key.is_empty()
                || key.len() > 1_024
                || key
                    .chars()
                    .any(|character| matches!(character, '\0' | '\r' | '\n'))
            {
                return Err(format!("MCP server {name} {field} name is invalid"));
            }
            let value = value.as_str().filter(|value| {
                value.len() <= 32_768
                    && !value
                        .chars()
                        .any(|character| matches!(character, '\0' | '\r' | '\n'))
            });
            value
                .map(|value| (key.clone(), value.to_owned()))
                .ok_or_else(|| format!("MCP server {name} {field} value is invalid"))
        })
        .collect()
}

pub fn runtime_declaration(
    location: &str,
    name: &str,
    config: &Value,
) -> Result<Declaration, String> {
    if name.is_empty() || name.len() > 1_024 {
        return Err("MCP server name is invalid".to_owned());
    }
    let fields = config
        .as_object()
        .ok_or_else(|| "MCP server config must be an object".to_owned())?;
    let kind = fields
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| "MCP server config requires a type".to_owned())?;
    let allowed = match kind {
        "local" => [
            "type",
            "command",
            "cwd",
            "environment",
            "disabled",
            "codemode",
            "timeout",
        ]
        .as_slice(),
        "remote" => [
            "type", "url", "headers", "oauth", "disabled", "codemode", "timeout",
        ]
        .as_slice(),
        _ => return Err("MCP server config requires a local or remote type".to_owned()),
    };
    if let Some(key) = fields.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("MCP server config contains unknown field {key}"));
    }
    if fields.get("disabled").and_then(Value::as_bool) == Some(true) {
        return Ok(Declaration::Disabled);
    }
    let timeouts = fields
        .get("timeout")
        .map(|value| McpTimeouts::layer(McpTimeouts::default(), value, "the runtime MCP"))
        .transpose()?
        .unwrap_or_default();
    match kind {
        "local" => {
            let command = fields
                .get("command")
                .and_then(Value::as_array)
                .filter(|command| {
                    !command.is_empty()
                        && command.len() <= 128
                        && command.iter().all(|value| {
                            value
                                .as_str()
                                .is_some_and(|text| text.len() <= 32_768 && !text.contains('\0'))
                        })
                })
                .ok_or_else(|| "local MCP command is invalid".to_owned())?;
            Ok(Declaration::Local {
                command: command
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                cwd: resolve_cwd(fields.get("cwd"), location, name)?,
                environment: server_environment(fields.get("environment"), name)?,
                timeouts,
            })
        }
        "remote" => {
            let raw = fields
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| "remote MCP URL is required".to_owned())?;
            let url = url::Url::parse(raw).map_err(|_| "remote MCP URL is invalid".to_owned())?;
            if !matches!(url.scheme(), "http" | "https") {
                return Err("remote MCP URL must use HTTP(S)".to_owned());
            }
            let oauth = fields
                .get("oauth")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if fields.get("oauth").is_some_and(|value| !value.is_boolean()) || oauth {
                return Err("runtime MCP OAuth registration is not composed".to_owned());
            }
            Ok(Declaration::Remote {
                url: url.to_string(),
                headers: string_map(fields.get("headers"), name, "headers")?,
                oauth,
                timeouts,
            })
        }
        _ => Err("MCP server config type changed during validation".to_owned()),
    }
}

/// Decode an enabled selected document's transport declaration. Document precedence,
/// approval, scope and tool-publication policy belong to the application.
pub fn configured_declaration(
    location: &str,
    name: &str,
    fields: &serde_json::Map<String, Value>,
    global: McpTimeouts,
) -> Result<Declaration, String> {
    Ok(match fields.get("type").and_then(Value::as_str) {
        Some("local") => {
            let command = fields
                .get("command")
                .and_then(Value::as_array)
                .ok_or_else(|| "local MCP command must be an array".to_owned())?;
            if command.is_empty() || command.len() > 128 || !command.iter().all(Value::is_string) {
                return Err("local MCP command is invalid or exceeds its argument bound".to_owned());
            }
            // PER-SERVER OVERRIDES THE GLOBAL, which is what makes the
            // global a default rather than a ceiling.
            let timeouts = match fields.get("timeout") {
                Some(value) => McpTimeouts::layer(global, value, &format!("MCP server {name}"))?,
                None => global,
            };
            Declaration::Local {
                command: command
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                cwd: resolve_cwd(fields.get("cwd"), location, name)?,
                environment: server_environment(fields.get("environment"), name)?,
                timeouts,
            }
        }
        Some("remote") => {
            let raw = fields
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| "remote MCP URL is required".to_owned())?;
            let url = url::Url::parse(raw).map_err(|_| "remote MCP URL is invalid".to_owned())?;
            if !matches!(url.scheme(), "http" | "https") {
                return Err("remote MCP URL must use HTTP(S)".to_owned());
            }
            Declaration::Remote {
                url: url.to_string(),
                headers: string_map(fields.get("headers"), name, "headers")?,
                oauth: match fields.get("oauth") {
                    None => false,
                    Some(Value::Bool(enabled)) => *enabled,
                    _ => return Err("MCP OAuth setting must be boolean".to_owned()),
                },
                timeouts: match fields.get("timeout") {
                    Some(value) => {
                        McpTimeouts::layer(global, value, &format!("MCP server {name}"))?
                    }
                    None => global,
                },
            }
        }
        _ => return Err("MCP declaration requires a local or remote type".to_owned()),
    })
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    /// WINDOWS ROOTS, UNC SHARES AND VERBATIM PREFIXES, against the exact
    /// values `path.win32.resolve` produces.
    ///
    /// The first version of the folding was measured wrong on all four: it
    /// kept `..` above an absolute root and rewrote separators to `/`, which
    /// inside a verbatim prefix is a DIFFERENT PATH rather than a different
    /// spelling. Relative-path cases alone could not have caught either.
    #[cfg(windows)]
    #[test]
    fn windows_roots_and_prefixes_fold_as_the_oracle_resolves_them() {
        for (declared, expected) in [
            (r"C:\work\..\..\x", r"C:\x"),
            (r"C:\x", r"C:\x"),
            (r"\\?\C:\work\child\..", r"\\?\C:\work"),
            (r"\\server\share\..\x", r"\\server\share\x"),
        ] {
            assert_eq!(
                normalise_lexically(Path::new(declared)),
                expected,
                "{declared} must fold as path.win32.resolve does"
            );
        }
    }
}
