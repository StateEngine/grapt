use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::source::SourceSnapshot;

const MAX_DECLARATIONS: usize = 64;
const MAX_OPTIONS_BYTES: usize = 64 * 1024;
const MAX_TARGET_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginOrigin {
    UserGlobal,
    LaunchExplicit,
    Project,
    Wellknown,
    Unknown,
}

impl PluginOrigin {
    pub const fn permits_executable(self) -> bool {
        matches!(self, Self::UserGlobal | Self::LaunchExplicit)
    }
}

#[derive(Debug, Clone)]
pub struct ConfigDocument {
    pub directory: PathBuf,
    pub source: String,
    pub origin: PluginOrigin,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PluginSource {
    LocalFile {
        canonical_path: PathBuf,
        /// The admitted source snapshot digest, not only the entrypoint digest.
        sha256: String,
        #[serde(skip)]
        snapshot: Arc<SourceSnapshot>,
    },
    Package {
        name: String,
        version: String,
        integrity: String,
        entrypoint: PathBuf,
        sha256: String,
    },
}

impl PluginSource {
    pub fn entrypoint(&self) -> &Path {
        match self {
            Self::LocalFile { canonical_path, .. } => canonical_path,
            Self::Package { entrypoint, .. } => entrypoint,
        }
    }

    pub fn sha256(&self) -> &str {
        match self {
            Self::LocalFile { sha256, .. } | Self::Package { sha256, .. } => sha256,
        }
    }

    /// The host-admitted identity that owns durable plugin state. It is the
    /// declaration's source, never the id a guest exports, so a different
    /// package cannot claim another package's state by reusing its id, while
    /// the same package keeps its state across source reloads.
    pub fn admitted_identity(&self) -> String {
        match self {
            Self::LocalFile { canonical_path, .. } => {
                format!("file:{}", canonical_path.to_string_lossy())
            }
            Self::Package { name, .. } => format!("package:{name}"),
        }
    }

    pub fn snapshot(&self) -> Option<&SourceSnapshot> {
        match self {
            Self::LocalFile { snapshot, .. } => Some(snapshot),
            Self::Package { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PluginDeclaration {
    pub target: String,
    pub options: Value,
    pub permissions: crate::PluginAccess,
    pub origin: PluginOrigin,
    pub source: String,
    pub executable: PluginSource,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BlockedDeclaration {
    pub target: String,
    pub origin: PluginOrigin,
    pub source: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ResolvedDeclarations {
    pub active: Vec<PluginDeclaration>,
    pub blocked: Vec<BlockedDeclaration>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DeclarationError {
    #[error("plugin configuration root must be an object")]
    Root,
    #[error("plugin declarations must be an array")]
    List,
    #[error("plugin declaration count exceeds {MAX_DECLARATIONS}")]
    Count,
    #[error("plugin declaration is neither a target string nor a package/options object")]
    Shape,
    #[error("plugin target is empty or exceeds {MAX_TARGET_BYTES} bytes")]
    Target,
    #[error("plugin remove operation requires a target")]
    EmptyRemove,
    #[error("plugin options exceed {MAX_OPTIONS_BYTES} bytes")]
    Options,
    #[error("plugin permission declaration is invalid: {0}")]
    Permissions(String),
    #[error("plugin package declaration is missing a string package field")]
    Package,
    #[error("plugin package pin must be name@exact-version#integrity")]
    PackagePin,
    #[error("exact package acquisition is unavailable until an installer receipt supplies its entrypoint")]
    PackageUnavailable,
    #[error("plugin path could not be resolved: {0}")]
    Path(String),
    #[error("plugin executable could not be read: {0}")]
    Read(String),
}

#[derive(Debug)]
enum Operation {
    Add {
        target: String,
        options: Value,
        permissions: Option<Value>,
        origin: PluginOrigin,
        source: String,
        directory: PathBuf,
    },
    Remove(String),
}

pub fn resolve_declarations(
    documents: &[ConfigDocument],
) -> Result<ResolvedDeclarations, DeclarationError> {
    let mut operations = Vec::new();
    for document in documents {
        let object = document.value.as_object().ok_or(DeclarationError::Root)?;
        let Some(entries) = object.get("plugins") else {
            continue;
        };
        let entries = entries.as_array().ok_or(DeclarationError::List)?;
        if operations.len().saturating_add(entries.len()) > MAX_DECLARATIONS {
            return Err(DeclarationError::Count);
        }
        for entry in entries {
            operations.push(parse(document, entry)?);
        }
    }

    let mut selected = BTreeMap::<String, PluginDeclaration>::new();
    let mut blocked = Vec::new();
    for operation in operations {
        match operation {
            Operation::Remove(selector) => {
                selected.retain(|target, _| !matches_selector(&selector, target));
            }
            Operation::Add {
                target,
                options,
                permissions,
                origin,
                source,
                directory,
            } => {
                if !origin.permits_executable() {
                    blocked.push(BlockedDeclaration {
                        target,
                        origin,
                        source,
                        reason: "executable declarations require user-global or launch-explicit provenance"
                            .to_owned(),
                    });
                    continue;
                }
                let identified = crate::PluginAccess::resolve(permissions.as_ref(), &directory)
                    .map_err(DeclarationError::Permissions)
                    .and_then(|permissions| {
                        identify(&target, &directory).map(|executable| (executable, permissions))
                    });
                match identified {
                    Ok((executable, permissions)) => {
                        selected.insert(
                            target.clone(),
                            PluginDeclaration {
                                target,
                                options,
                                permissions,
                                origin,
                                source,
                                executable,
                            },
                        );
                    }
                    Err(error) => blocked.push(BlockedDeclaration {
                        target,
                        origin,
                        source,
                        reason: error.to_string(),
                    }),
                }
            }
        }
    }
    Ok(ResolvedDeclarations {
        active: selected.into_values().collect(),
        blocked,
    })
}

fn parse(document: &ConfigDocument, entry: &Value) -> Result<Operation, DeclarationError> {
    let (target, options) = if let Some(target) = entry.as_str() {
        if let Some(target) = target.strip_prefix('-') {
            if target.is_empty() {
                return Err(DeclarationError::EmptyRemove);
            }
            return Ok(Operation::Remove(target.to_owned()));
        }
        (target.to_owned(), Value::Object(serde_json::Map::new()))
    } else if let Some(object) = entry.as_object() {
        let target = object
            .get("package")
            .and_then(Value::as_str)
            .ok_or(DeclarationError::Package)?;
        let options = object
            .get("options")
            .cloned()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        (target.to_owned(), options)
    } else {
        return Err(DeclarationError::Shape);
    };
    if target.is_empty() || target.len() > MAX_TARGET_BYTES {
        return Err(DeclarationError::Target);
    }
    let options_bytes = serde_json::to_vec(&options).map_err(|_| DeclarationError::Options)?;
    if options_bytes.len() > MAX_OPTIONS_BYTES {
        return Err(DeclarationError::Options);
    }
    Ok(Operation::Add {
        target,
        options,
        permissions: entry
            .as_object()
            .and_then(|object| object.get("permissions"))
            .cloned(),
        origin: document.origin,
        source: document.source.clone(),
        directory: document.directory.clone(),
    })
}

fn identify(target: &str, directory: &Path) -> Result<PluginSource, DeclarationError> {
    if looks_like_path(target) {
        let path = Path::new(target);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            directory.join(path)
        };
        let canonical = std::fs::canonicalize(&path)
            .map_err(|error| DeclarationError::Path(error.to_string()))?;
        let metadata = std::fs::metadata(&canonical)
            .map_err(|error| DeclarationError::Read(error.to_string()))?;
        if !metadata.is_file() {
            return Err(DeclarationError::Read(
                "entrypoint is not a regular file".to_owned(),
            ));
        }
        let snapshot = SourceSnapshot::capture(&canonical)
            .map_err(|error| DeclarationError::Read(error.to_string()))?;
        return Ok(PluginSource::LocalFile {
            canonical_path: canonical,
            sha256: snapshot.digest().to_owned(),
            snapshot: Arc::new(snapshot),
        });
    }
    match exact_package_pin(target) {
        Some(_) => Err(DeclarationError::PackageUnavailable),
        None => Err(DeclarationError::PackagePin),
    }
}

fn exact_package_pin(target: &str) -> Option<(&str, &str, &str)> {
    let (package_version, integrity) = target.rsplit_once('#')?;
    let split = if package_version.starts_with('@') {
        package_version.rfind('@')?
    } else {
        package_version.find('@')?
    };
    let (name, version_with_at) = package_version.split_at(split);
    let version = version_with_at.strip_prefix('@')?;
    if name.is_empty()
        || version.is_empty()
        || integrity.is_empty()
        || version
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && byte != b'.' && byte != b'-')
    {
        return None;
    }
    Some((name, version, integrity))
}

fn looks_like_path(target: &str) -> bool {
    target.starts_with('.')
        || target.starts_with('/')
        || target.starts_with('\\')
        || Path::new(target).is_absolute()
}

fn matches_selector(selector: &str, target: &str) -> bool {
    selector == "*"
        || selector == target
        || selector
            .strip_suffix(".*")
            .is_some_and(|prefix| target.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(origin: PluginOrigin, entries: Value) -> ConfigDocument {
        ConfigDocument {
            directory: std::env::temp_dir(),
            source: "fixture".to_owned(),
            origin,
            value: serde_json::json!({"plugins": entries}),
        }
    }

    #[test]
    fn project_executable_is_blocked_before_resolution() {
        let result = resolve_declarations(&[document(
            PluginOrigin::Project,
            serde_json::json!(["./missing.mjs"]),
        )]);
        let result = result.unwrap_or_else(|error| panic!("declaration fold failed: {error}"));
        assert!(result.active.is_empty());
        assert_eq!(result.blocked.len(), 1);
        assert!(result.blocked[0].reason.contains("launch-explicit"));
    }

    #[test]
    fn removal_is_ordered_and_requires_a_target() {
        let invalid = resolve_declarations(&[document(
            PluginOrigin::LaunchExplicit,
            serde_json::json!(["-"]),
        )]);
        assert_eq!(invalid, Err(DeclarationError::EmptyRemove));
    }

    #[test]
    fn package_input_requires_an_exact_pin() {
        let result = resolve_declarations(&[document(
            PluginOrigin::LaunchExplicit,
            serde_json::json!(["unversioned-package"]),
        )]);
        let result = result.unwrap_or_else(|error| panic!("declaration fold failed: {error}"));
        assert_eq!(result.blocked.len(), 1);
        assert!(result.blocked[0].reason.contains("exact-version"));
    }
}
