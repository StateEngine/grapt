//! Source decoding is separate from acquisition and grants no network authority.
//! Git operand and full-SHA validation.
//! Copyright 2023-2026 SpaceXAI.
//! Apache-2.0 (licenses/xai-grok-build/). Modified for bounded Claude source objects.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginSource {
    Relative(PathBuf),
    Git(GitSource),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitSource {
    pub url: String,
    pub git_ref: Option<String>,
    pub git_sha: Option<String>,
    pub subdir: PathBuf,
}
impl GitSource {
    /// Normalize the marketplace CLI's GitHub shorthand or explicit Git URL.
    /// Filesystem paths are classified by the product before calling this decoder.
    pub fn marketplace(value: &str) -> Result<Self, String> {
        operand(value, "marketplace source")?;
        let (url, git_ref) = match value.split_once('#') {
            Some((url, revision)) => {
                operand(revision, "ref")?;
                if revision.contains('#') {
                    return Err("marketplace Git source has more than one ref fragment".into());
                }
                (url, Some(revision.to_owned()))
            }
            None => (value, None),
        };
        let url = if url.contains(':') || url.contains('@') {
            operand(url, "URL")?;
            if !["https://", "http://", "ssh://", "git@"]
                .iter()
                .any(|prefix| url.starts_with(prefix))
            {
                return Err("marketplace Git URL has an unsupported protocol".into());
            }
            url.to_owned()
        } else {
            github(url)?
        };
        Ok(Self {
            url,
            git_ref,
            git_sha: None,
            subdir: PathBuf::new(),
        })
    }
}
impl PluginSource {
    pub fn parse(value: &Value) -> Result<Self, String> {
        if let Some(path) = value.as_str() {
            return relative(path).map(Self::Relative);
        }
        let source = value
            .as_object()
            .ok_or("plugin source must be a path or object")?;
        let kind = source
            .get("source")
            .and_then(Value::as_str)
            .ok_or("plugin source object requires source")?;
        let url = match kind {
            "github" => {
                fields(source, &["source", "repo", "ref", "sha"])?;
                github(required(source, "repo")?)?
            }
            "url" | "git-subdir" => {
                fields(
                    source,
                    if kind == "git-subdir" {
                        &["source", "url", "ref", "sha", "path"]
                    } else {
                        &["source", "url", "ref", "sha"]
                    },
                )?;
                let url = required(source, "url")?;
                if kind == "git-subdir" && !url.contains(':') && !url.contains('@') {
                    github(url)?
                } else {
                    operand(url, "URL")?;
                    if !["https://", "http://", "ssh://", "file://", "git@"]
                        .iter()
                        .any(|prefix| url.starts_with(prefix))
                    {
                        return Err("plugin git URL has an unsupported protocol".into());
                    }
                    url.to_owned()
                }
            }
            _ => return Err("plugin source type is not supported yet".into()),
        };
        let git_ref = source
            .get("ref")
            .map(|value| {
                let value = value.as_str().ok_or("plugin git ref must be text")?;
                operand(value, "ref").map(str::to_owned)
            })
            .transpose()?;
        let git_sha = source
            .get("sha")
            .map(|value| {
                let value = value.as_str().ok_or("plugin git sha must be text")?;
                if !(value.len() == 40 || value.len() == 64)
                    || !value.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err("plugin git sha must be a complete commit identity".to_owned());
                }
                Ok(value.to_ascii_lowercase())
            })
            .transpose()?;
        let subdir = if kind == "git-subdir" {
            let path = required(source, "path")?;
            if path.is_empty() || path.starts_with('/') || path.starts_with('\\') {
                return Err("plugin git subdirectory must be relative".into());
            }
            relative(&format!("./{}", path.strip_prefix("./").unwrap_or(path)))?
        } else {
            PathBuf::new()
        };
        Ok(Self::Git(GitSource {
            url,
            git_ref,
            git_sha,
            subdir,
        }))
    }
}
fn required<'a>(source: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    source
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("plugin source requires {key}"))
}
fn operand<'a>(value: &'a str, kind: &str) -> Result<&'a str, String> {
    if value.is_empty()
        || value.len() > 4096
        || value.starts_with('-')
        || value.chars().any(char::is_control)
    {
        return Err(format!("plugin git {kind} must be a bounded operand"));
    }
    Ok(value)
}
fn github(value: &str) -> Result<String, String> {
    operand(value, "repository")?;
    let parts = value.split('/').collect::<Vec<_>>();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || matches!(*part, "." | "..")
                || !part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
    {
        return Err("GitHub plugin repository requires owner/repo".into());
    }
    Ok(format!("https://github.com/{value}"))
}

#[cfg(test)]
#[test]
fn source_objects_keep_git_pins_and_contained_subdirectories() -> Result<(), String> {
    use serde_json::json;
    let market = GitSource::marketplace("team/plugins#release")?;
    assert_eq!(market.url, "https://github.com/team/plugins");
    assert_eq!(market.git_ref.as_deref(), Some("release"));
    assert!(GitSource::marketplace("team/plugins#--config").is_err());
    assert!(GitSource::marketplace("ext::command").is_err());
    let PluginSource::Git(git) = PluginSource::parse(
        &json!({"source":"git-subdir","url":"team/plugins","path":"tools/review","ref":"release","sha":"ab".repeat(20)}),
    )?
    else {
        return Err("git source was not decoded".into());
    };
    assert_eq!(git.url, "https://github.com/team/plugins");
    assert_eq!(git.subdir, PathBuf::from("tools/review"));
    assert_eq!(git.git_ref.as_deref(), Some("release"));
    assert_eq!(
        git.git_sha.as_deref(),
        Some("abababababababababababababababababababab")
    );
    for source in [
        json!({"source":"github","repo":"../repo"}),
        json!({"source":"github","repo":"team/repo","sha":"short"}),
        json!({"source":"url","url":"ext::command"}),
        json!({"source":"url","url":"https://host/repo","ref":"--config"}),
        json!({"source":"git-subdir","url":"team/repo","path":"../outside"}),
        json!({"source":"git-subdir","url":"team/repo","path":"/outside"}),
    ] {
        assert!(PluginSource::parse(&source).is_err());
    }
    Ok(())
}
