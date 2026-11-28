//! Read-only configuration inputs for retained frontend catalogs. Domain
//! metadata folds consume these ordered documents; this is not a policy mint.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub(super) use super::catalog_origin::source_origin;
use super::legacy_names;

use grapt_config::{
    ConfigBounds, ConfigSourcePath, ConfigValue, MaxAncestorDepth, MaxDocumentBytes,
    MaxDocumentNodes, MaxDocuments, MaxPathBytes, MaxValueDepth,
};
use grapt_types::profile_names::Spelling;

pub(super) const MAX_ANCESTORS: usize = 64;

#[cfg(test)]
#[path = "catalog_boundary_tests.rs"]
mod boundary_tests;
#[cfg(test)]
#[path = "../../tests/support/catalog_boundary.rs"]
pub(super) mod fixture_diagnostics;
mod owned;
#[cfg(test)]
pub(in crate::dogfood) use owned::documents_in;
pub(in crate::dogfood) use owned::user_documents;
const MAX_DOCUMENTS: usize = 256;
pub(super) const DOCUMENT_BYTES: usize = 64 * 1024;
const TOTAL_BYTES: usize = 4 * 1024 * 1024;
const PATH_BYTES: usize = 4096;

#[derive(Clone)]
pub(super) struct CatalogEnvironment {
    pub home: PathBuf,
    pub config: PathBuf,
    pub repositories: PathBuf,
    /// The profile's own `grapt.jsonc` (or its legacy name), which every
    /// reader includes first.
    service_root: PathBuf,
    explicit: Option<PathBuf>,
    inline: Option<InlineConfig>,
    #[cfg(any(test, feature = "loopback-provider-testkit"))]
    fixture_root: Option<PathBuf>,
}

/// The current inline configuration variable.
const INLINE_CONTENT: &str = "GRAPT_CONFIG_CONTENT";
/// The acquired repository cache below the product data directory.
const REPOS: &str = "repos";

/// Configuration content given inline in the launch environment, and the
/// variable that carried it.
#[derive(Clone)]
pub(super) struct InlineConfig {
    text: String,
    variable: &'static str,
}

impl From<&str> for InlineConfig {
    /// Content as if carried by the current variable.
    fn from(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            variable: INLINE_CONTENT,
        }
    }
}

/// Configuration the user wrote for themselves, rather than a repository's:
/// the only documents that may widen what a Session may do (decision
/// of 2026-10-07, project config is narrow-only). Every reader that must tell
/// the two apart asks this.
/// It is the plugin gate's own test, so the two cannot drift apart.
pub(super) fn user_owned(origin: grapt_plugin::PluginOrigin) -> bool {
    origin.permits_executable()
}

impl CatalogEnvironment {
    pub(super) fn clear_explicit(&mut self) {
        self.explicit = None;
        self.inline = None;
    }
    pub(super) fn service_root(&self) -> &Path {
        &self.service_root
    }

    #[cfg(test)]
    pub(super) fn isolated(root: &Path) -> Self {
        Self {
            home: root.join("home"),
            config: root.join("config"),
            repositories: root.join("repos"),
            service_root: root.join("service"),
            explicit: None,
            inline: None,
            fixture_root: Some(root.to_path_buf()),
        }
    }

    /// The process environment's sources, under the profile at `service_root`.
    pub(super) fn read(service_root: &Path) -> Result<Self, String> {
        Self::configured(service_root)?
            .ok_or_else(|| "catalog home directory is not configured".to_owned())
    }

    /// Startup can remain available without a catalog home; execution still
    /// requires it through `read`. Invalid configured sources remain errors.
    pub(super) fn configured(service_root: &Path) -> Result<Option<Self>, String> {
        let Some(home) = renamed("GRAPT_TEST_HOME", legacy_names::LEGACY_TEST_HOME)?.or(
            environment(if cfg!(windows) { "USERPROFILE" } else { "HOME" })?,
        ) else {
            return Ok(None);
        };
        let home = PathBuf::from(home);
        // An explicit root is used as given; otherwise the XDG (or home)
        // configuration base holds the current or the legacy product
        // directory.
        let config = match renamed("GRAPT_CONFIG_DIR", legacy_names::LEGACY_CONFIG_DIR)? {
            Some(path) => PathBuf::from(path),
            None => legacy_names::directory(
                &environment("XDG_CONFIG_HOME")?
                    .map_or_else(|| home.join(".config"), PathBuf::from),
                Spelling::profiles_dir,
                None,
            ),
        };
        let data = environment("XDG_DATA_HOME")?
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        // The data directory also holds an install; only the cache itself
        // decides whether a legacy one is still in use.
        let repositories =
            legacy_names::directory(&data, Spelling::profiles_dir, Some(REPOS)).join(REPOS);
        // A refusal names the variable the content actually came from.
        let variable = if environment(INLINE_CONTENT)?.is_some() {
            INLINE_CONTENT
        } else {
            legacy_names::LEGACY_CONFIG_CONTENT
        };
        let inline = renamed(INLINE_CONTENT, legacy_names::LEGACY_CONFIG_CONTENT)?
            .map(|text| InlineConfig { text, variable });
        let value = Self {
            home,
            config,
            repositories,
            service_root: service_root.to_path_buf(),
            explicit: renamed("GRAPT_CONFIG", legacy_names::LEGACY_CONFIG)?.map(PathBuf::from),
            inline,
            #[cfg(any(test, feature = "loopback-provider-testkit"))]
            fixture_root: super::catalog_origin::fixture_ceiling(service_root)?,
        };
        for path in [
            &value.home,
            &value.config,
            &value.repositories,
            &value.service_root,
        ] {
            checked_path(path)?;
        }
        Ok(Some(value))
    }

    fn fixture_root(&self) -> Option<&Path> {
        #[cfg(any(test, feature = "loopback-provider-testkit"))]
        {
            self.fixture_root.as_deref()
        }
        #[cfg(not(any(test, feature = "loopback-provider-testkit")))]
        {
            None
        }
    }
}

pub(super) struct CatalogDocument {
    pub directory: PathBuf,
    pub source: String,
    pub origin: grapt_plugin::PluginOrigin,
    pub value: ConfigValue,
}

impl CatalogDocument {
    /// `reason`, naming this document as every document refusal does:
    /// "<reason>: <directories>/<file>" ([`refusal_text`]).
    pub(super) fn refusal(&self, reason: &str) -> String {
        refusal_text(reason, Path::new(&self.source))
    }
}

pub(super) struct CatalogSources {
    pub directory: PathBuf,
    pub documents: Vec<CatalogDocument>,
    pub document_paths: Vec<PathBuf>,
    pub command_sources: Vec<CommandSource>,
    pub skill_directories: Vec<PathBuf>,
    pub environment: CatalogEnvironment,
}

pub(super) enum CommandSource {
    Document(usize),
    Directory(PathBuf),
}

enum Input {
    Document {
        path: PathBuf,
        origin: grapt_plugin::PluginOrigin,
    },
    Directory(PathBuf),
}

impl CatalogSources {
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(super) fn read(directory: &Path, environment: CatalogEnvironment) -> Result<Self, String> {
        checked_path(directory)?;
        let mut ancestors = Vec::new();
        for path in directory.ancestors() {
            if ancestors.len() == MAX_ANCESTORS {
                return Err("catalog ancestor walk exceeds 64 directories".to_owned());
            }
            ancestors.push(path.to_path_buf());
            if let Some(root) = environment.fixture_root() {
                let canonical = std::fs::canonicalize(path).map_err(|error| {
                    format!("fixture ancestor cannot be resolved: {:?}", error.kind())
                })?;
                let root = std::fs::canonicalize(root).map_err(|error| {
                    format!("fixture root cannot be resolved: {:?}", error.kind())
                })?;
                if canonical == root {
                    break;
                }
            }
        }
        let mut directories = vec![environment.config.clone()];
        if directory != environment.config {
            directories.extend(
                ancestors
                    .iter()
                    .rev()
                    .map(|path| legacy_names::project_directory(path)),
            );
        }
        let mut files = Vec::new();
        // The profile's own document first: every later one folds over it,
        // and when the service root is also a project ancestor it keeps this
        // class (a path is read once, where it is first named).
        append_documents(
            &mut files,
            &environment.service_root,
            grapt_plugin::PluginOrigin::UserGlobal,
        )?;
        append_documents(
            &mut files,
            &environment.config,
            grapt_plugin::PluginOrigin::UserGlobal,
        )?;
        files.push(Input::Directory(environment.config.clone()));
        if let Some(path) = &environment.explicit {
            let path = if path.is_absolute() {
                path.clone()
            } else {
                directory.join(path)
            };
            checked_path(&path)?;
            files.push(Input::Document {
                path,
                origin: grapt_plugin::PluginOrigin::LaunchExplicit,
            });
        }
        if directory != environment.config {
            for ancestor in ancestors.iter().rev() {
                append_documents(&mut files, ancestor, grapt_plugin::PluginOrigin::Project)?;
            }
            for path in directories.iter().skip(1) {
                append_documents(&mut files, path, grapt_plugin::PluginOrigin::Project)?;
                files.push(Input::Directory(path.clone()));
            }
        }
        if files
            .iter()
            .filter(|entry| matches!(entry, Input::Document { .. }))
            .count()
            > MAX_DOCUMENTS
        {
            return Err("catalog source census exceeds 256 documents".to_owned());
        }
        let bounds = bounds()?;
        let mut total = 0;
        let mut seen = BTreeSet::new();
        let mut documents = Vec::new();
        let mut document_paths = Vec::new();
        let mut command_sources = Vec::new();
        for input in files {
            match input {
                Input::Directory(path) => {
                    command_sources.push(CommandSource::Directory(path));
                }
                Input::Document { path, origin } => {
                    let source = path.to_string_lossy().into_owned();
                    if !seen.insert(
                        document_identity(&path).map_err(|reason| refusal_text(&reason, &path))?,
                    ) {
                        continue;
                    }
                    document_paths.push(path.clone());
                    // A refusal names its source after its reason, never its
                    // contents.
                    let named = |error: String| refusal_text(&error, &path);
                    let Some((size, value)) = read_document(&path, bounds).map_err(named)? else {
                        continue;
                    };
                    total += size;
                    if total > TOTAL_BYTES {
                        return Err(
                            "catalog configuration exceeds the four MiB aggregate bound".to_owned()
                        );
                    }
                    let origin = super::catalog_origin::source_origin_below(
                        &path,
                        origin,
                        directory,
                        &environment.service_root,
                        environment.fixture_root(),
                    )
                    .map_err(named)?;
                    let parent = path
                        .parent()
                        .ok_or_else(|| "catalog document has no parent".to_owned())?;
                    command_sources.push(CommandSource::Document(documents.len()));
                    documents.push(CatalogDocument {
                        directory: parent.to_path_buf(),
                        source,
                        origin,
                        value,
                    });
                }
            }
        }
        if let Some(inline) = &environment.inline {
            if total + inline.text.len() > TOTAL_BYTES {
                return Err("inline catalog configuration exceeds the aggregate bound".to_owned());
            }
            command_sources.push(CommandSource::Document(documents.len()));
            documents.push(CatalogDocument {
                directory: directory.to_path_buf(),
                source: inline.variable.to_owned(),
                origin: grapt_plugin::PluginOrigin::LaunchExplicit,
                value: decode(inline.text.as_bytes(), bounds)
                    .map_err(|error| refusal_text(&error, Path::new(inline.variable)))?,
            });
        }
        let mut skill_directories = Vec::new();
        for family in [".claude", ".agents"] {
            skill_directories.push(environment.home.join(family).join("skills"));
            skill_directories.extend(
                ancestors
                    .iter()
                    .map(|path| path.join(family).join("skills")),
            );
        }
        for path in &directories {
            skill_directories.push(path.join("skill"));
            skill_directories.push(path.join("skills"));
        }
        for path in &skill_directories {
            checked_path(path)?;
        }
        Ok(Self {
            directory: directory.to_path_buf(),
            documents,
            document_paths,
            command_sources,
            skill_directories,
            environment,
        })
    }
}

/// The variable `current`, else the pre-rename `legacy` one, both bounded.
fn renamed(current: &str, legacy: &str) -> Result<Option<String>, String> {
    legacy_names::environment(current, legacy, environment)
}

fn environment(name: &str) -> Result<Option<String>, String> {
    match std::env::var(name) {
        Ok(value) if value.len() > DOCUMENT_BYTES => {
            Err(format!("catalog environment {name} exceeds its bound"))
        }
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(format!("catalog environment {name} is not UTF-8"))
        }
    }
}

fn append_documents(
    files: &mut Vec<Input>,
    directory: &Path,
    origin: grapt_plugin::PluginOrigin,
) -> Result<(), String> {
    // Each directory is read under one spelling: the current names, else the
    // legacy ones when only those exist.
    for path in legacy_names::config_files(directory) {
        checked_path(&path)?;
        files.push(Input::Document { path, origin });
    }
    Ok(())
}

/// The longest prefix a Session notice puts before a refusal ("they could
/// not be refreshed: "); every notice caller's prefix fits it.
pub(super) const NOTICE_PREFIX_BYTES: usize = 32;
/// A refusal's whole text: with any notice prefix it stays inside the
/// notice's 256-byte reason, so the notice never cuts it.
pub(super) const REFUSAL_BYTES: usize =
    grapt_session::notice::MAX_NOTICE_TEXT_BYTES - NOTICE_PREFIX_BYTES;

/// A document refusal, "<reason>: <directories>/<file>", within
/// [`REFUSAL_BYTES`]. Priority: the file name is always
/// shown in full form ([`file_name_shown`]); the reason then keeps all the room left, cut behind "…" only past
/// it (so always well above 40 bytes); the directories are elided from the
/// middle, kept nearest-first, and give way first. No character is ever
/// split.
fn refusal_text(reason: &str, path: &Path) -> String {
    let file = file_name_shown(
        &path
            .file_name()
            .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy()),
        path,
    );
    let directories = path
        .parent()
        .filter(|parent| path.file_name().is_some() && !parent.as_os_str().is_empty())
        .map(|parent| {
            let shown = parent.to_string_lossy();
            // Windows paths accept both separators; render one kind before
            // selecting the nearest directories. Unix backslashes are names.
            let shown = if cfg!(windows) {
                shown.replace('\\', "/")
            } else {
                shown.into_owned()
            };
            format!("{shown}/")
        })
        .unwrap_or_default();
    let fixed = ": ".len() + file.len();
    if reason.len() + directories.len() + fixed <= REFUSAL_BYTES {
        return format!("{reason}: {directories}{file}");
    }
    // The reason outranks the directories: it keeps all the room the file
    // name leaves (far above any floor), and the directories take the rest.
    let reason = cut_with_ellipsis(reason, REFUSAL_BYTES.saturating_sub(fixed));
    let directories = elide_directories(
        &directories,
        REFUSAL_BYTES.saturating_sub(fixed + reason.len()),
    );
    format!("{reason}: {directories}{file}")
}

/// The longest file name a refusal shows whole.
const FILE_NAME_BYTES: usize = 96;
/// The ends a longer file name keeps.
const FILE_NAME_END_BYTES: usize = 40;

/// A file name as a refusal shows it: whole up to [`FILE_NAME_BYTES`], else
/// "<first 40>…<last 40>#<8 hex>", the hex a stable FNV-1a of the full path,
/// so two long names that share both ends still read apart.
/// Character-safe and deterministic; the extension stays in
/// the tail.
fn file_name_shown(file: &str, path: &Path) -> String {
    if file.len() <= FILE_NAME_BYTES {
        return file.to_owned();
    }
    let mut head = FILE_NAME_END_BYTES;
    while !file.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = file.len() - FILE_NAME_END_BYTES;
    while !file.is_char_boundary(tail) {
        tail += 1;
    }
    let hash = path_hash(path.to_string_lossy().as_bytes());
    format!("{}…{}#{hash:08x}", &file[..head], &file[tail..])
}

/// 32-bit FNV-1a: stable across builds and platforms, so a path always
/// shows the same tag; identity, not security.
fn path_hash(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5, |hash: u32, byte| {
        (hash ^ u32::from(*byte)).wrapping_mul(0x0100_0193)
    })
}

/// `directories` within `budget`: whole, else "…/" and the nearest
/// components that fit, else nothing.
fn elide_directories(directories: &str, budget: usize) -> String {
    if directories.len() <= budget {
        return directories.to_owned();
    }
    let mut kept = String::new();
    for component in directories.trim_end_matches('/').rsplit('/') {
        let next = format!("{component}/{kept}");
        if "…/".len() + next.len() > budget {
            break;
        }
        kept = next;
    }
    if "…/".len() + kept.len() <= budget {
        format!("…/{kept}")
    } else {
        String::new()
    }
}

/// `text` within `budget`, cut on a character boundary behind "…".
fn cut_with_ellipsis(text: &str, budget: usize) -> String {
    if text.len() <= budget {
        return text.to_owned();
    }
    let mut end = budget.saturating_sub("…".len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn checked_path(path: &Path) -> Result<ConfigSourcePath, String> {
    let bound = MaxPathBytes::try_new(PATH_BYTES).map_err(|error| error.to_string())?;
    ConfigSourcePath::new(path, bound).map_err(|error| error.to_string())
}

/// Captured documents keep their logical identity after replacement or removal.
/// Only live documents follow the current filesystem for alias deduplication.
fn document_identity(path: &Path) -> Result<(bool, String), String> {
    let captured = grapt_platform::profile_configuration::captured_identity(path)
        .map_err(|error| format!("approved catalog identity refused: {:?}", error.kind()))?;
    let (captured, identity) = match captured {
        Some(identity) => (
            true,
            identity
                .to_str()
                .ok_or("catalog identity is not UTF-8")?
                .to_owned(),
        ),
        None => (false, super::authority_path::resolve(path)?),
    };
    Ok((captured, grapt_types::path::comparison_key(&identity)))
}

fn read_document(
    path: &Path,
    bounds: ConfigBounds,
) -> Result<Option<(usize, ConfigValue)>, String> {
    let source = checked_path(path)?;
    // Approval owns immutable bytes (including absence), not a mutable path's
    // metadata stamp. Only unbound documents may use the live source cache.
    use grapt_platform::profile_configuration::{frozen, FrozenRead};
    match frozen(source.as_path(), bounds.document_bytes().value())
        .map_err(|error| format!("approved catalog capture refused: {:?}", error.kind()))?
    {
        FrozenRead::Missing => return Ok(None),
        FrozenRead::Bytes(bytes) => {
            return decode(&bytes, bounds).map(|value| Some((bytes.len(), value)));
        }
        FrozenRead::Unbound => {}
    }
    let value = super::source_cache::document(source.as_path(), bounds).map_err(|reason| {
        // Preserve the catalog's actionable diagnostic without another stat on cache hits.
        if std::fs::metadata(source.as_path()).is_ok_and(|metadata| !metadata.is_file()) {
            "catalog configuration is not a regular file".to_owned()
        } else {
            reason
        }
    })?;
    if value
        .as_ref()
        .is_some_and(|(_, value)| value.as_object().is_none())
    {
        return Err("catalog configuration root is not an object".to_owned());
    }
    Ok(value)
}

fn decode(bytes: &[u8], bounds: ConfigBounds) -> Result<ConfigValue, String> {
    let value = grapt_config::decode(bytes, bounds)
        .map_err(|error| error.to_string())?
        .into_value();
    if value.as_object().is_none() {
        return Err("catalog configuration root is not an object".to_owned());
    }
    Ok(value)
}

pub(super) fn bounds() -> Result<ConfigBounds, String> {
    Ok(ConfigBounds::new(
        MaxDocumentBytes::try_new(DOCUMENT_BYTES).map_err(|error| error.to_string())?,
        MaxAncestorDepth::try_new(MAX_ANCESTORS).map_err(|error| error.to_string())?,
        MaxDocuments::try_new(MAX_DOCUMENTS).map_err(|error| error.to_string())?,
        MaxValueDepth::try_new(16).map_err(|error| error.to_string())?,
        MaxDocumentNodes::try_new(8192).map_err(|error| error.to_string())?,
    ))
}

#[cfg(test)]
#[path = "catalog_sources_tests.rs"]
mod sources_tests;
