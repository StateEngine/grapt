//! Bounded coding effects, prepared and executed by the existing permission runner.

pub(in crate::dogfood) mod checkpoints;
mod exploration;
pub(super) mod file_effects;
mod missing;
pub(super) mod patch;
#[cfg(test)]
mod tests;

use std::path::{Component, Path, PathBuf};

use super::*;
pub(super) use grapt_tools_local::filesystem::read_bounded;
use grapt_tools_local::filesystem::{is_link, reject_links, walk_without_links};
use grapt_tools_local::read_lines as source_pages;
use grapt_tools_local::search as discovery;
use grapt_tools_local::search::{OUTPUT_BYTES, PATH_BYTES, RESULT_LIMIT};

impl ProviderFieldSchema for bool {
    fn provider_schema() -> serde_json::Value {
        serde_json::json!({"type": "boolean"})
    }
}

pub(super) const WRITE_DESCRIPTION: &str = "Create/replace UTF-8 (≤1 MiB; JSON≤256 KiB). Existing files require fs.overwrite; parent must exist. No symlinks/traversal. Prefer edit/patch for small changes.";
pub(super) const GLOB_DESCRIPTION: &str = "Sorted glob: *, ?, [], {}, whole-component **. path/exclude; limit=100 caps results, not traversal. Honors ignores; include_ignored adds ignored/hidden. Name build dirs in path. No links/control metadata. totalMatches/truncated show coverage.";
pub(super) const GREP_DESCRIPTION: &str = "Text search: case-sensitive literals; regex=true uses Rust regex. path/glob/exclude; limit=100. Honors ignores; include_ignored adds ignored/hidden. Name build dirs in path. Binary/oversized skips; complete/truncated show coverage.";
pub(super) const READ_LINES_DESCRIPTION: &str = "UTF-8≤1MiB; 400/max2000 lines; ~192KiB/page. Default N→text+patch sha256; resume 1-based Unicode cursors. structured=true: JSON offsets/cursors.";
pub(super) const LIST_DESCRIPTION: &str = "Sorted entries/kinds. Honors ignores; include_ignored adds ignored/hidden. Name build dirs in path. No links/control metadata. Reports truncated/skipped.";

const DEFAULT_RESULTS: u64 = 100;

tool_arguments! {
    pub(super) struct WriteArguments => WRITE_TOOL {
        required path: NonEmptyString,
        required content: String,
    }
}
tool_arguments! {
    pub(super) struct GlobArguments => GLOB_TOOL {
        required pattern: NonEmptyString,
        optional include_ignored: bool,
        optional path: NonEmptyString,
        optional limit: BoundedU64<1, RESULT_LIMIT>,
        optional exclude: Exclusions,
    }
}
tool_arguments! {
    pub(super) struct GrepArguments => GREP_TOOL {
        required pattern: NonEmptyString,
        optional regex: bool,
        optional case_sensitive: bool,
        optional include_ignored: bool,
        optional path: NonEmptyString,
        optional glob: NonEmptyString,
        optional limit: BoundedU64<1, RESULT_LIMIT>,
        optional exclude: Exclusions,
    }
}
tool_arguments! {
    pub(super) struct ReadLinesArguments => READ_LINES_TOOL {
        required path: NonEmptyString,
        optional start_line: BoundedU64<1, 1_000_000>,
        optional start_column: BoundedU64<1, 1_048_577>,
        optional line_count: BoundedU64<1, 2000>,
        optional structured: bool,
    }
}
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct ListArguments {
    pub(super) path: Option<NonEmptyString>,
    pub(super) limit: Option<BoundedU64<1, RESULT_LIMIT>>,
    pub(super) include_ignored: Option<bool>,
}

impl ToolArguments for ListArguments {
    const TOOL_NAME: &'static str = LIST_TOOL;

    fn provider_schema() -> String {
        // Responses strict mode requires every published property. A path has
        // no safe invented value (a guess would list the wrong directory), so
        // it must accept null for the working directory.
        serde_json::json!({"type":"object","additionalProperties":false,
            "properties":{
                "path":{"type":["string","null"],"minLength":1,"description":"Directory to list; null for the working directory."},
                "limit":{"type":"integer","minimum":1,"maximum":RESULT_LIMIT},
                "include_ignored":{"type":"boolean"}
            },
            "required":[]}).to_string()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Exclusions(Vec<String>);

impl<'de> Deserialize<'de> for Exclusions {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let patterns = Vec::<String>::deserialize(deserializer)?;
        if patterns.len() > 32 {
            return Err(D::Error::custom("exclude accepts at most 32 glob patterns"));
        }
        for pattern in &patterns {
            discovery::validate_glob(pattern).map_err(D::Error::custom)?;
        }
        Ok(Self(patterns))
    }
}

impl ProviderFieldSchema for Exclusions {
    fn provider_schema() -> serde_json::Value {
        serde_json::json!({"type":"array","maxItems":32,
            "items":{"type":"string","minLength":1,"maxLength":256}})
    }
}

#[derive(Debug, Clone)]
pub(super) enum CodingTarget {
    /// Directory metadata for a failed read, authorized as `list` on this parent.
    MissingRead {
        requested: String,
        parent: PathBuf,
    },
    Write(PathBuf),
    /// An existing regular file, as approved for replacement under fs.overwrite.
    Overwrite {
        path: PathBuf,
        before: FileSnapshot,
    },
    Glob(PathBuf),
    Grep(PathBuf),
    ReadLines(PathBuf),
    List(PathBuf),
    Patch(patch::PatchPlan),
}

pub(super) fn prepare(
    root: &Path,
    tool: &str,
    input: &BoundedToolInput,
) -> Result<PreparedEffect, String> {
    if tool == APPLY_PATCH_TOOL {
        return patch::apply_patch::prepare(root, input);
    }
    if tool == PATCH_TOOL {
        return patch::prepare(root, input);
    }
    let (target, path, write) = match tool {
        WRITE_TOOL => {
            let args: WriteArguments = decode_input(input)?;
            match write_target(root, args.path.as_str())? {
                WriteTarget::New(path) => (CodingTarget::Write(path.clone()), path, true),
                WriteTarget::Existing(path) => {
                    let (before, _) = snapshot(&path)?;
                    (
                        CodingTarget::Overwrite {
                            path: path.clone(),
                            before,
                        },
                        path,
                        true,
                    )
                }
            }
        }
        GLOB_TOOL => {
            let args: GlobArguments = decode_input(input)?;
            discovery::validate_glob(args.pattern.as_str())?;
            let path = match search_root(root, args.path.as_ref()) {
                Ok(path) => path,
                Err(error) => {
                    return prepare_missing(
                        root,
                        args.path.as_ref().map_or(".", NonEmptyString::as_str),
                        error,
                    )
                }
            };
            (CodingTarget::Glob(path.clone()), path, false)
        }
        GREP_TOOL => {
            let args: GrepArguments = decode_input(input)?;
            compile_search(&args)?;
            discovery::validate_glob(args.glob.as_ref().map_or("**", NonEmptyString::as_str))?;
            let path = match grep_scope(root, args.path.as_ref()) {
                Ok(path) => path,
                Err(error) => {
                    return prepare_missing(
                        root,
                        args.path.as_ref().map_or(".", NonEmptyString::as_str),
                        error,
                    )
                }
            };
            (CodingTarget::Grep(path.clone()), path, false)
        }
        READ_LINES_TOOL => {
            let args: ReadLinesArguments = decode_input(input)?;
            let path = match exploration::read_target(root, args.path.as_str()) {
                Ok(path) => path,
                Err(error) => return prepare_missing(root, args.path.as_str(), error),
            };
            (CodingTarget::ReadLines(path.clone()), path, false)
        }
        LIST_TOOL => {
            let args: ListArguments = decode_input(input)?;
            let path = match search_root(root, args.path.as_ref()) {
                Ok(path) => path,
                Err(error) => {
                    return prepare_missing(
                        root,
                        args.path.as_ref().map_or(".", NonEmptyString::as_str),
                        error,
                    )
                }
            };
            (CodingTarget::List(path.clone()), path, false)
        }
        _ => return Err("unknown coding tool".into()),
    };
    let resource = path_text(&path);
    // Historical write/fs.modify actions mean edit. A separate create action
    // preserves those receipts and identifies the new write tool; replacing
    // an existing file is asked separately as fs.overwrite, so no create or
    // edit approval covers it.
    let action = match (&target, write) {
        (CodingTarget::Overwrite { .. }, _) => "fs.overwrite",
        (_, true) => "fs.create",
        (_, false) => tool,
    }
    .to_owned();
    Ok(PreparedEffect {
        capability: filesystem_capability(
            if write {
                EffectClass::FilesystemWrite
            } else {
                EffectClass::FilesystemRead
            },
            &resource,
            write,
        ),
        target: EffectTarget::Coding(target),
        action,
        resource,
    })
}

pub(super) fn run(
    root: &Path,
    target: &CodingTarget,
    input: &BoundedToolInput,
) -> Result<Vec<u8>, String> {
    match target {
        CodingTarget::MissingRead { requested, parent } => missing::run(root, requested, parent),
        CodingTarget::Write(_) | CodingTarget::Overwrite { .. } => {
            file_effects::prepare_write(root, target, input)?
                .run(root, None)?
                .output
        }
        CodingTarget::Glob(expected) => {
            let args: GlobArguments = decode_input(input)?;
            let path = search_root(root, args.path.as_ref())?;
            if path != *expected {
                return Err("glob root changed after authorization".into());
            }
            discovery::glob(
                root,
                &path,
                args.pattern.as_str(),
                args.limit.map_or(DEFAULT_RESULTS, BoundedU64::get),
                args.exclude
                    .as_ref()
                    .map_or(&[], |patterns| patterns.0.as_slice()),
                args.include_ignored.unwrap_or(false),
            )
        }
        CodingTarget::Grep(expected) => {
            let args: GrepArguments = decode_input(input)?;
            let matcher = compile_search(&args)?;
            let path = grep_scope(root, args.path.as_ref())?;
            if path != *expected {
                return Err("grep root changed after authorization".into());
            }
            discovery::grep(
                root,
                &path,
                &matcher,
                args.glob.as_ref().map_or("**", NonEmptyString::as_str),
                args.limit.map_or(DEFAULT_RESULTS, BoundedU64::get),
                args.exclude
                    .as_ref()
                    .map_or(&[], |patterns| patterns.0.as_slice()),
                args.include_ignored.unwrap_or(false),
            )
        }
        CodingTarget::ReadLines(expected) => {
            let args: ReadLinesArguments = decode_input(input)?;
            let path = exploration::read_target(root, args.path.as_str())?;
            if path != *expected {
                return Err("read_lines target changed after authorization".into());
            }
            exploration::read_lines(
                root,
                &path,
                args.start_line.map_or(1, BoundedU64::get),
                args.start_column.map_or(1, BoundedU64::get),
                args.line_count.map_or(400, BoundedU64::get),
                args.structured.unwrap_or(false),
            )
        }
        CodingTarget::List(expected) => {
            let args: ListArguments = decode_input(input)?;
            let path = search_root(root, args.path.as_ref())?;
            if path != *expected {
                return Err("list root changed after authorization".into());
            }
            discovery::list(
                root,
                &path,
                args.limit.map_or(DEFAULT_RESULTS, BoundedU64::get),
                args.include_ignored.unwrap_or(false),
            )
        }
        CodingTarget::Patch(plan) => patch::run(root, plan, input),
    }
}

pub(super) fn prepare_missing(
    root: &Path,
    requested: &str,
    original: String,
) -> Result<PreparedEffect, String> {
    match missing::prepare(root, requested) {
        Ok(Some(prepared)) => Ok(prepared),
        Ok(None) | Err(_) => Err(original),
    }
}

fn compile_search(args: &GrepArguments) -> Result<regex::Regex, String> {
    let pattern = args.pattern.as_str();
    if pattern.len() > PATH_BYTES || pattern.contains(['\n', '\r']) {
        return Err("grep pattern must be one line of at most 4096 bytes".into());
    }
    let source = if args.regex.unwrap_or(false) {
        pattern.to_owned()
    } else {
        regex::escape(pattern)
    };
    regex::RegexBuilder::new(&source)
        .case_insensitive(!args.case_sensitive.unwrap_or(true))
        .size_limit(1024 * 1024)
        .dfa_size_limit(1024 * 1024)
        .nest_limit(64)
        .build()
        .map_err(|error| format!("grep regex is invalid or exceeds its compile bound: {error}"))
}

/// Coding tools use the session's placement directory; other local trees and
/// remote worktrees remain accessible through the shell's existing permissions.
fn outside_workspace(root: &Path) -> String {
    let root = root.to_string_lossy();
    let root = crate::dogfood::path_display::failure_text(&root);
    format!(
        "coding path is outside the workspace rooted at {root}; use shell/exec_command for \
         other local trees, or shell/exec_command with ssh for remote worktrees. To root \
         coding tools elsewhere, run `grapt run --new` from a directory that contains the \
         path, or open the project containing it in the TUI and start a new session there"
    )
}

fn relative_path<'a>(root: &Path, requested: &'a str) -> Result<&'a Path, String> {
    let path = Path::new(requested);
    super::paths::validate_spelling(path)?;
    if requested.len() > PATH_BYTES
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("coding path exceeds its bound or contains parent traversal".into());
    }
    #[cfg(windows)]
    let path = if path.is_absolute() {
        windows_relative(root, path)?
    } else {
        path
    };
    let path = if path.is_absolute() {
        path.strip_prefix(root)
            .map_err(|_| outside_workspace(root))?
    } else {
        path
    };
    let requested = path.to_str().ok_or("coding path is not UTF-8")?;
    if requested.len() > PATH_BYTES
        || requested.contains(':')
        || requested.contains('\0')
        || path.is_absolute()
        || path.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err("coding path must stay within the workspace".into());
    }
    Ok(path)
}

#[cfg(windows)]
fn windows_relative<'a>(root: &Path, requested: &'a Path) -> Result<&'a Path, String> {
    crate::dogfood::authority_path::relative(root, requested, || outside_workspace(root))
}

/// What a write would do to its target, decided before permission is asked.
enum WriteTarget {
    New(PathBuf),
    Existing(PathBuf),
}

/// An existing file's identity when its overwrite was approved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileSnapshot {
    len: u64,
    sha256: Vec<u8>,
}

/// The file's bytes, bounded like edit, and the snapshot they make.
fn snapshot(path: &Path) -> Result<(FileSnapshot, Vec<u8>), String> {
    // A read-only file is refused rather than replaced: edit cannot write it
    // either, and a rename would otherwise bypass its protection on Unix.
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("write target metadata unreadable: {error}"))?;
    if metadata.permissions().readonly() {
        return Err(format!(
            "write refuses read-only {}; make it writable first if replacing it is intended",
            path_text(path)
        ));
    }
    let bytes = read_bounded(path, FILE_EFFECT_MAX_BYTES).map_err(|error| {
        format!("write overwrite target is unavailable or over the 1 MiB bound: {error}")
    })?;
    let snapshot = FileSnapshot {
        len: bytes.len() as u64,
        sha256: ring::digest::digest(&ring::digest::SHA256, &bytes)
            .as_ref()
            .to_vec(),
    };
    Ok((snapshot, bytes))
}

/// The approved overwrite still holds: `requested` names the same regular,
/// writable file with the snapshot it was approved for. Returns its bytes.
fn overwrite_still_approved(
    root: &Path,
    requested: &str,
    path: &Path,
    before: &FileSnapshot,
) -> Result<Vec<u8>, String> {
    match write_target(root, requested)? {
        WriteTarget::Existing(current) if current == *path => {}
        _ => return Err("write target changed after authorization".into()),
    }
    let (now, old) = snapshot(path)?;
    if now != *before {
        return Err(format!(
            "{requested} changed after the overwrite was approved; reread it and write again"
        ));
    }
    Ok(old)
}

fn create_target(root: &Path, requested: &str) -> Result<PathBuf, String> {
    match write_target(root, requested)? {
        WriteTarget::New(path) => Ok(path),
        WriteTarget::Existing(_) => Err("write target already exists; use edit".into()),
    }
}

fn write_target(root: &Path, requested: &str) -> Result<WriteTarget, String> {
    let relative = relative_path(root, requested)?;
    let name = relative.file_name().ok_or("write path has no filename")?;
    let parent = relative.parent().ok_or("write path has no parent")?;
    walk_without_links(root, parent, || {
        format!(
            "write parent directory {} does not exist; create it first with the shell",
            parent.display()
        )
    })?;
    let parent = resolve_existing(
        root,
        parent.to_str().ok_or("write parent is not UTF-8")?,
        true,
    )?;
    if !parent.is_dir() {
        return Err("write parent is not a directory".into());
    }
    let target = parent.join(name);
    // Apply the existing control-metadata policy even when the target does not exist.
    if protected_paths(root).iter().any(|protected| {
        protected.as_canonical().is_some_and(|protected| {
            if cfg!(windows) {
                protected.eq_ignore_ascii_case(&path_text(&target))
            } else {
                protected == path_text(&target)
            }
        })
    }) {
        return Err("tool path names protected control metadata".into());
    }
    match std::fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.file_type().is_file() => {
            let target = resolve_existing(
                root,
                target.to_str().ok_or("write target is not UTF-8")?,
                true,
            )?;
            Ok(WriteTarget::Existing(target))
        }
        Ok(_) => {
            Err("write target exists and is not a regular file; write replaces files only".into())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(WriteTarget::New(target)),
        Err(error) => Err(format!("write target metadata unreadable: {error}")),
    }
}

fn search_root(root: &Path, requested: Option<&NonEmptyString>) -> Result<PathBuf, String> {
    let relative = relative_path(root, requested.map_or(".", NonEmptyString::as_str))?;
    reject_links(root, relative)?;
    let target = resolve_existing(
        root,
        relative.to_str().ok_or("search path is not UTF-8")?,
        false,
    )?;
    if !target.is_dir() {
        return Err("search path is not a directory".into());
    }
    Ok(target)
}

/// A directory scope, or one regular file outside protected control metadata.
fn grep_scope(root: &Path, requested: Option<&NonEmptyString>) -> Result<PathBuf, String> {
    let relative = relative_path(root, requested.map_or(".", NonEmptyString::as_str))?;
    reject_links(root, relative)?;
    let target = resolve_existing(
        root,
        relative.to_str().ok_or("search path is not UTF-8")?,
        false,
    )?;
    let metadata = std::fs::symlink_metadata(&target)
        .map_err(|error| format!("search path metadata unavailable: {error}"))?;
    if metadata.is_dir() {
        return Ok(target);
    }
    if !metadata.is_file() || is_link(&metadata) {
        return Err("grep path must be a directory or a regular file".into());
    }
    let protected = target
        .strip_prefix(root)
        .map_err(|_| "grep path escaped workspace")?
        .components()
        .any(|part| part.as_os_str().to_str().is_some_and(discovery::excluded));
    if protected {
        return Err("tool path names protected control metadata".into());
    }
    Ok(target)
}

fn encode(value: &serde_json::Value) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    if bytes.len() > OUTPUT_BYTES {
        return Err("coding output byte bound exceeded".into());
    }
    Ok(bytes)
}
