//! The `skill` tool: load one installed skill's instructions on purpose.
//!
//! The model discovers skill metadata on demand; full content enters the
//! conversation when this tool loads it, under the same permission
//! runner as every other tool. The catalog is read again at call time, so a
//! skill changed or removed since the guidance was rendered is served as it is
//! now, or refused.
//!
//! A skill's own files (templates, references) are loaded through this tool
//! too, by path relative to the selected skill's directory. An installed skill
//! usually lives outside the project, where workspace reads are refused; this
//! reaches exactly that skill's directory and nothing beside it.

use super::*;
use crate::dogfood::source_file::open_contained;
#[cfg(all(test, unix))]
use crate::dogfood::source_file::NONBLOCK;
mod discovery;
mod recovery;
pub(in crate::dogfood) use recovery::public_detail as public_missing_detail;

pub(super) const SKILL_TOOL: &str = "skill";
pub(super) const SKILL_DESCRIPTION: &str = "Skills: query searches; {} lists eight/page, cursor continues. Exact id loads instructions; arguments fills plugin skill placeholders; file loads a relative resource. Discovery forbids id/file; unused=null.";

/// Sibling files listed with a loaded skill, as the oracle does.
const FILE_LIMIT: usize = 10;
const SCAN_ENTRIES: usize = 512;
const SCAN_DEPTH: usize = 4;
/// One skill file loaded into the conversation, the same bound as one AGENTS.md.
const RESOURCE_MAX_BYTES: u64 = 65_536;
const MANUAL_INVOCATION_REQUIRED: &str =
    "this skill requires user invocation through its slash command";

mod arguments;
pub(super) use arguments::SkillArguments;

pub(super) const EXCLUSIVE: &str = "skill discovery and loading are exclusive; omit id/file to search, or omit query/cursor to load";

#[derive(Debug, Clone)]
pub(super) enum Request {
    Load {
        id: String,
        file: Option<String>,
        arguments: String,
    },
    Discover {
        query: Option<String>,
        cursor: usize,
    },
}

pub(super) fn prepare(root: &Path, input: &BoundedToolInput) -> Result<PreparedEffect, String> {
    let args: SkillArguments = decode_input(input)?;
    let id = args.id.and_then(arguments::nonblank);
    let file = args.file.and_then(arguments::nonblank);
    let query = args.query.and_then(arguments::nonblank);
    let prompt_arguments = args.arguments.unwrap_or_default();
    if prompt_arguments.len() > 65536
        || (!prompt_arguments.is_empty() && (id.is_none() || file.is_some()))
    {
        return Err(
            "skill arguments require an instruction load with id and no resource file".into(),
        );
    }
    let (request, resource) = if let Some(id) = id {
        if query.is_some() || args.cursor.is_some() {
            return Err(EXCLUSIVE.to_owned());
        }
        if let Some(file) = &file {
            relative_resource(file)?;
        }
        let resource = file
            .as_ref()
            .map_or_else(|| id.clone(), |file| format!("{id}/{file}"));
        (
            Request::Load {
                id,
                file,
                arguments: prompt_arguments,
            },
            resource,
        )
    } else {
        if file.is_some() {
            return Err("skill file requires an id".to_owned());
        }
        if query
            .as_ref()
            .is_some_and(|query| query.is_empty() || query.len() > 256)
        {
            return Err("skill query must contain text and fit 256 bytes".to_owned());
        }
        let cursor = usize::try_from(args.cursor.map_or(0, |cursor| cursor.0))
            .map_err(|_| "skill cursor is not addressable")?;
        (
            Request::Discover { query, cursor },
            "skill catalog".to_owned(),
        )
    };
    // Loading instructions into the conversation is a harness action, not a
    // filesystem read the model chose a path for.
    let resolved = ResolvedCapability::new(
        EffectClass::ToolInvoke,
        None,
        FilesystemAuthority::new(Vec::new(), Vec::new(), protected_paths(root)),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    );
    Ok(PreparedEffect {
        capability: CapabilityId::derive(&resolved),
        resource,
        target: EffectTarget::Skill(request),
        action: SKILL_TOOL.to_owned(),
    })
}

pub(super) async fn run(
    plugins: &Arc<super::super::plugins::PluginRuntime>,
    root: &Path,
    request: Request,
) -> Result<Vec<u8>, String> {
    match request {
        Request::Load {
            id,
            file,
            arguments,
        } => run_skill(plugins, root, &id, file.as_deref(), &arguments).await,
        Request::Discover { query, cursor } => {
            let catalog = super::super::catalogs::current_skills(plugins, root).await?;
            discovery::page_with_warnings(
                &catalog.entries,
                query.as_deref(),
                cursor,
                &catalog.warnings,
            )
        }
    }
}

/// A skill file path must name something inside the skill: relative, with no
/// parent or root components. Links are checked when the file is read.
fn relative_resource(file: &str) -> Result<&Path, String> {
    let path = Path::new(file);
    let plain = path
        .components()
        .all(|component| matches!(component, std::path::Component::Normal(_)));
    if file.is_empty() || !plain {
        return Err(format!(
            "skill file '{file}' must be a path relative to the skill's base directory"
        ));
    }
    Ok(path)
}

/// Points inside one skill-file acquisition, so the race witnesses can change
/// the filesystem exactly there. Production passes a no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Acquisition {
    /// The path is about to be opened.
    Opening,
    /// The file is open; nothing about it has been checked yet.
    Opened,
    /// Every check passed; the bytes are about to be read.
    Reading,
}

/// One file of the selected skill, read now: bounded, UTF-8, a regular file
/// (not a link) that lives inside the skill's own directory.
fn read_resource(
    directory: &Path,
    file: &str,
    manual_main: Option<&str>,
) -> Result<String, String> {
    acquire_resource(directory, file, manual_main, &mut |_| ())
}

/// The file is opened ONCE. Every check is made on that open file, and the
/// bytes are read from it under the bound: checking a path and then opening it
/// again would let a file or directory replaced in between redirect the read.
fn acquire_resource(
    directory: &Path,
    file: &str,
    manual_main: Option<&str>,
    observe: &mut dyn FnMut(Acquisition),
) -> Result<String, String> {
    use std::io::Read as _;
    let unreadable = |error: std::io::Error| format!("skill file '{file}' is unreadable: {error}");
    let relative = relative_resource(file)?;
    let base = std::fs::canonicalize(directory)
        .map_err(|error| format!("skill directory is unreadable: {error}"))?;
    observe(Acquisition::Opening);
    let opened = open_contained(&base, relative).map_err(unreadable)?;
    observe(Acquisition::Opened);
    let metadata = opened.file.metadata().map_err(unreadable)?;
    let named = std::fs::symlink_metadata(base.join(relative)).map_err(unreadable)?;
    if !metadata.is_file() || named.file_type().is_symlink() {
        return Err(format!("skill file '{file}' is not a regular file"));
    }
    if !opened.inside(&base).map_err(unreadable)? {
        return Err(format!(
            "skill file '{file}' resolves outside the skill's directory"
        ));
    }
    // Only manual-only loads need the main identity. Never resolve either
    // resource by path before the guarded open, and compare the actual handles
    // before reading any bytes. Keep the main's Windows directory pins alive
    // alongside the resource's pins until the read is finished.
    let _main = if let Some(main_file) = manual_main {
        let main_relative = relative_resource(main_file)?;
        let main = open_contained(&base, main_relative).map_err(unreadable)?;
        let main_metadata = main.file.metadata().map_err(unreadable)?;
        let main_named = std::fs::symlink_metadata(base.join(main_relative)).map_err(unreadable)?;
        if !main_metadata.is_file() || main_named.file_type().is_symlink() {
            return Err("main skill file is not a regular file".to_owned());
        }
        if !main.inside(&base).map_err(unreadable)? {
            return Err("main skill file resolves outside the skill's directory".to_owned());
        }
        let resource_identity =
            same_file::Handle::from_file(opened.file.try_clone().map_err(unreadable)?)
                .map_err(unreadable)?;
        let main_identity =
            same_file::Handle::from_file(main.file.try_clone().map_err(unreadable)?)
                .map_err(unreadable)?;
        if resource_identity == main_identity {
            return Err(MANUAL_INVOCATION_REQUIRED.to_owned());
        }
        Some(main)
    } else {
        None
    };
    let exceeds = || format!("skill file '{file}' exceeds {RESOURCE_MAX_BYTES} bytes");
    if metadata.len() > RESOURCE_MAX_BYTES {
        return Err(exceeds());
    }
    observe(Acquisition::Reading);
    // The size above is a snapshot; a file growing now is caught here, after at
    // most one byte past the bound.
    let mut bytes = Vec::new();
    (&opened.file)
        .take(RESOURCE_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    if bytes.len() as u64 > RESOURCE_MAX_BYTES {
        return Err(exceeds());
    }
    String::from_utf8(bytes).map_err(|_| format!("skill file '{file}' is not UTF-8"))
}

pub(super) async fn run_skill(
    plugins: &Arc<super::super::plugins::PluginRuntime>,
    root: &Path,
    id: &str,
    file: Option<&str>,
    arguments: &str,
) -> Result<Vec<u8>, String> {
    let unable = |reason: &str| format!("Unable to load skill {id}: {reason}");
    let catalog = super::super::catalogs::current_skills(plugins, root)
        .await
        .map_err(|error| unable(&error))?;
    let skills = &catalog.entries;
    let skill = skills
        .iter()
        .find(|skill| skill.get("id").and_then(serde_json::Value::as_str) == Some(id))
        .ok_or_else(|| {
            let mut reason = recovery::missing(skills, id);
            for warning in &catalog.warnings {
                reason.push('\n');
                reason.push_str(warning);
            }
            reason
        })?;
    let manual_only = skill.get("autoinvoke").and_then(serde_json::Value::as_bool) == Some(false);
    if manual_only && file.is_none() {
        return Err(unable(MANUAL_INVOCATION_REQUIRED));
    }
    let name = skill
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(id);
    let content = skill
        .get("content")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| unable("its content is absent"))?;
    let location = skill
        .get("location")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| unable("its location is absent"))?;
    let directory = location
        .parent()
        .ok_or_else(|| unable("its location has no directory"))?;
    if let Some(file) = file {
        let manual_main = if manual_only {
            Some(
                location
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| unable("its location has no filename"))?,
            )
        } else {
            None
        };
        let content =
            read_resource(directory, file, manual_main).map_err(|error| unable(&error))?;
        return Ok(format!(
            "<skill_resource name=\"{name}\" file=\"{file}\">\n{content}\n</skill_resource>"
        )
        .into_bytes());
    }
    let activated =
        super::super::claude_plugins::activate_skill(plugins.service_root(), root, id, arguments)?;
    if activated.is_none() && !arguments.is_empty() {
        return Err(unable(
            "arguments are supported for Claude plugin skills only",
        ));
    }
    let content = activated.as_deref().unwrap_or(content);
    let files = if location.file_name().and_then(|name| name.to_str()) == Some("SKILL.md") {
        sibling_files(directory).map_err(|error| unable(&error))?
    } else {
        Vec::new()
    };
    let mut lines = vec![
        format!("<skill_content name=\"{name}\">"),
        format!("# Skill: {name}"),
        String::new(),
        content.trim().to_owned(),
        String::new(),
        format!("Base directory for this skill: {}", directory.display()),
        "Relative paths in this skill (e.g., scripts/, reference/) are relative to this base directory.".to_owned(),
        format!("Load one of these files with the skill tool: {{\"id\": \"{id}\", \"file\": \"<relative path>\"}}."),
        "Note: file list is sampled.".to_owned(),
        String::new(),
        "<skill_files>".to_owned(),
    ];
    lines.extend(files.iter().map(|file| {
        let relative = file.strip_prefix(directory).unwrap_or(file);
        format!("<file>{}</file>", relative.display())
    }));
    lines.push("</skill_files>".to_owned());
    lines.push("</skill_content>".to_owned());
    Ok(lines.join("\n").into_bytes())
}

/// Regular files beside a skill's SKILL.md, sorted, at most [`FILE_LIMIT`].
/// Links are never followed, so a skill cannot list files outside itself.
fn sibling_files(directory: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let mut pending = vec![(directory.to_path_buf(), 0usize)];
    let mut visited = 0usize;
    while let Some((current, depth)) = pending.pop() {
        let entries = std::fs::read_dir(&current)
            .map_err(|error| format!("skill directory is unreadable: {error}"))?;
        for entry in entries {
            visited += 1;
            if visited > SCAN_ENTRIES {
                return Err("skill directory exceeds its scan bound".to_owned());
            }
            let entry = entry.map_err(|error| format!("skill entry is unreadable: {error}"))?;
            let kind = entry
                .file_type()
                .map_err(|error| format!("skill entry type is unreadable: {error}"))?;
            let path = entry.path();
            if kind.is_dir() && depth + 1 < SCAN_DEPTH {
                pending.push((path, depth + 1));
            } else if kind.is_file() && entry.file_name() != "SKILL.md" {
                files.push(path);
            }
        }
    }
    files.sort();
    files.truncate(FILE_LIMIT);
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_and_loading_remain_distinct_permissioned_requests() -> Result<(), String> {
        let input = |raw: &str| {
            BoundedToolInput::new(raw.as_bytes().to_vec()).map_err(|error| error.to_string())
        };
        let load = prepare(
            Path::new("."),
            &input(r#"{"id":"notes","file":"template.md"}"#)?,
        )?;
        assert!(
            matches!(load.target, EffectTarget::Skill(Request::Load { id, file, .. })
            if id == "notes" && file.as_deref() == Some("template.md"))
        );
        let list = prepare(Path::new("."), &input("{}")?)?;
        assert_eq!(
            load.capability, list.capability,
            "discovery must keep skill authority"
        );
        assert!(matches!(
            list.target,
            EffectTarget::Skill(Request::Discover {
                query: None,
                cursor: 0
            })
        ));
        for raw in [
            r#"{"id":"notes","query":"notes"}"#,
            r#"{"id":"notes","cursor":0}"#,
            r#"{"file":"template.md"}"#,
            r#"{"cursor":65536}"#,
            r#"{"id":"notes","file":"../outside"}"#,
        ] {
            assert!(
                prepare(Path::new("."), &input(raw)?).is_err(),
                "accepted {raw}"
            );
        }
        Ok(())
    }

    /// A temporary directory owned by one test, named for this process and
    /// removed when dropped, including while a failing test unwinds.
    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
            std::fs::create_dir_all(&root)
                .unwrap_or_else(|error| panic!("fixture directory: {error}"));
            Self(root)
        }
    }

    impl std::ops::Deref for TempRoot {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl AsRef<Path> for TempRoot {
        fn as_ref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fixture(label: &str) -> TempRoot {
        let root = TempRoot::new(&format!("grapt-skd-skill-{label}"));
        std::fs::create_dir_all(root.join("scripts/deep"))
            .unwrap_or_else(|error| panic!("fixture directory: {error}"));
        root
    }

    #[test]
    fn skd_sibling_files_are_sorted_bounded_and_exclude_skill_md() {
        let root = fixture("siblings");
        for name in [
            "SKILL.md",
            "b.md",
            "a.md",
            "scripts/run.sh",
            "scripts/deep/SKILL.md",
        ] {
            std::fs::write(root.join(name), "x").unwrap_or_else(|error| panic!("fixture: {error}"));
        }
        for index in 0..12 {
            std::fs::write(root.join(format!("z{index:02}.txt")), "x")
                .unwrap_or_else(|error| panic!("fixture: {error}"));
        }
        let files = sibling_files(&root).unwrap_or_else(|error| panic!("scan: {error}"));
        assert_eq!(files.len(), FILE_LIMIT);
        assert_eq!(files[0], root.join("a.md"));
        assert_eq!(files[1], root.join("b.md"));
        assert!(files
            .iter()
            .all(|file| file.file_name() != Some(std::ffi::OsStr::new("SKILL.md"))));
    }

    #[cfg(unix)]
    #[test]
    fn skd_sibling_files_never_follow_links() {
        let root = fixture("links");
        let outside = TempRoot::new("grapt-skd-outside");
        std::fs::write(outside.join("secret.txt"), "x").unwrap_or_else(|error| panic!("{error}"));
        std::os::unix::fs::symlink(&outside, root.join("linked"))
            .unwrap_or_else(|error| panic!("link: {error}"));
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("file-link.txt"))
            .unwrap_or_else(|error| panic!("link: {error}"));
        let files = sibling_files(&root).unwrap_or_else(|error| panic!("scan: {error}"));
        assert!(
            files
                .iter()
                .all(|file| !file.to_string_lossy().contains("secret")
                    && file.file_name() != Some(std::ffi::OsStr::new("file-link.txt"))),
            "a link was followed: {files:?}"
        );
    }

    #[test]
    fn skd_skill_files_are_read_now_bounded_and_inside_the_skill() {
        let root = fixture("resources");
        std::fs::write(root.join("template.md"), "first").unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(root.join("scripts/run.sh"), "echo").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            read_resource(&root, "template.md", None).as_deref(),
            Ok("first")
        );
        assert_eq!(
            read_resource(&root, "scripts/run.sh", None).as_deref(),
            Ok("echo")
        );
        // Changed since the skill was loaded: served as it is now.
        std::fs::write(root.join("template.md"), "second").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            read_resource(&root, "template.md", None).as_deref(),
            Ok("second")
        );
        std::fs::write(root.join("binary.bin"), [0xff, 0xfe]).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(
            root.join("large.md"),
            vec![b'x'; RESOURCE_MAX_BYTES as usize + 1],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        for (file, reason) in [
            ("missing.md", "unreadable"),
            ("binary.bin", "not UTF-8"),
            ("large.md", "exceeds"),
            ("scripts", "not a regular file"),
            ("../template.md", "relative to the skill's base directory"),
            (
                "scripts/../template.md",
                "relative to the skill's base directory",
            ),
            ("/etc/hostname", "relative to the skill's base directory"),
        ] {
            match read_resource(&root, file, None) {
                Err(error) => assert!(error.contains(reason), "{file}: {error}"),
                Ok(content) => panic!("{file} was served: {content}"),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn skd_skill_files_never_follow_links_out_of_the_skill() {
        let root = fixture("resource-links");
        let outside = TempRoot::new("grapt-skd-res-outside");
        std::fs::write(outside.join("secret.txt"), "secret").unwrap_or_else(|e| panic!("{e}"));
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("file-link.txt"))
            .unwrap_or_else(|e| panic!("{e}"));
        std::os::unix::fs::symlink(&outside, root.join("dir-link"))
            .unwrap_or_else(|e| panic!("{e}"));
        for file in ["file-link.txt", "dir-link/secret.txt"] {
            match read_resource(&root, file, None) {
                Err(error) => assert!(
                    error.contains("not a regular file") || error.contains("resolves outside"),
                    "{file}: {error}"
                ),
                Ok(content) => panic!("{file} escaped the skill: {content}"),
            }
        }
    }

    // SKD-E5/E6: the filesystem changes at the exact points of one acquisition.

    #[test]
    fn skd_a_skill_file_growing_after_its_checks_is_refused_at_the_bound() {
        let root = fixture("resource-growth");
        std::fs::write(root.join("template.md"), "small").unwrap_or_else(|e| panic!("{e}"));
        let grow = |phase: Acquisition| {
            if phase == Acquisition::Reading {
                let past = vec![b'x'; RESOURCE_MAX_BYTES as usize + 1 - "small".len()];
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(root.join("template.md"))
                    .and_then(|mut file| std::io::Write::write_all(&mut file, &past))
                    .unwrap_or_else(|e| panic!("grow: {e}"));
            }
        };
        match acquire_resource(&root, "template.md", None, &mut { grow }) {
            Err(error) => assert!(error.contains("exceeds"), "{error}"),
            Ok(content) => panic!("{} bytes passed the bound", content.len()),
        }
    }

    #[cfg(unix)]
    #[test]
    fn skd_a_directory_swapped_back_after_open_cannot_deliver_outside_bytes() {
        let root = fixture("resource-swap");
        let outside = TempRoot::new("grapt-skd-swap-outside");
        std::fs::write(outside.join("notes.md"), "SKD-OUTSIDE-SECRET")
            .unwrap_or_else(|e| panic!("{e}"));
        // At open time the ancestor points outside; it is put back before any check.
        std::os::unix::fs::symlink(&outside, root.join("sub")).unwrap_or_else(|e| panic!("{e}"));
        let restore = |phase: Acquisition| {
            if phase == Acquisition::Opened {
                std::fs::remove_file(root.join("sub")).unwrap_or_else(|e| panic!("{e}"));
                std::fs::create_dir(root.join("sub")).unwrap_or_else(|e| panic!("{e}"));
                std::fs::write(root.join("sub/notes.md"), "inside")
                    .unwrap_or_else(|e| panic!("{e}"));
            }
        };
        match acquire_resource(&root, "sub/notes.md", None, &mut { restore }) {
            Err(error) => assert!(!error.contains("SKD-OUTSIDE-SECRET"), "{error}"),
            Ok(content) => panic!("outside bytes were delivered: {content}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn skd_a_file_replaced_after_its_checks_still_reads_the_checked_file() {
        let root = fixture("resource-replace");
        let outside = TempRoot::new("grapt-skd-replace-outside");
        std::fs::write(outside.join("secret.md"), "SKD-OUTSIDE-SECRET")
            .unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(root.join("template.md"), "checked").unwrap_or_else(|e| panic!("{e}"));
        let replace = |phase: Acquisition| {
            if phase == Acquisition::Reading {
                std::fs::remove_file(root.join("template.md")).unwrap_or_else(|e| panic!("{e}"));
                std::os::unix::fs::symlink(outside.join("secret.md"), root.join("template.md"))
                    .unwrap_or_else(|e| panic!("{e}"));
            }
        };
        assert_eq!(
            acquire_resource(&root, "template.md", None, &mut { replace }).as_deref(),
            Ok("checked")
        );
    }

    /// Runs one acquisition that must not wait on `fifo`. A worker that
    /// panics has its own failure re-raised. A worker still blocked after the
    /// bound is released by a writer attached without waiting, and the
    /// witness fails rather than hanging.
    #[cfg(unix)]
    fn within_bound(
        fifo: PathBuf,
        acquire: impl FnOnce() -> Result<String, String> + Send + 'static,
    ) -> Result<String, String> {
        use std::os::unix::fs::OpenOptionsExt as _;
        use std::sync::mpsc::RecvTimeoutError;
        let bound = std::time::Duration::from_secs(5);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let _ = sender.send(acquire());
        });
        match receiver.recv_timeout(bound) {
            Ok(result) => {
                let _ = worker.join();
                result
            }
            Err(RecvTimeoutError::Disconnected) => match worker.join() {
                Err(payload) => std::panic::resume_unwind(payload),
                Ok(()) => panic!("acquisition ended without a result"),
            },
            Err(RecvTimeoutError::Timeout) => {
                // Non-blocking: if the reader is still waiting this releases
                // it; if it has just gone, the open fails at once.
                let released = std::fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(NONBLOCK)
                    .open(&fifo)
                    .is_ok();
                if receiver.recv_timeout(bound).is_ok() {
                    let _ = worker.join();
                }
                panic!(
                    "acquisition waited for a writer on {} (writer attached: {released})",
                    fifo.display()
                );
            }
        }
    }

    #[cfg(unix)]
    fn mkfifo(path: &Path) {
        let status = std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .unwrap_or_else(|e| panic!("mkfifo: {e}"));
        assert!(status.success(), "mkfifo {} failed", path.display());
    }

    // SKD-E7: a FIFO, or a file replaced by one as it is opened, is refused at
    // once instead of waiting for a writer that never comes.

    #[cfg(unix)]
    #[test]
    fn skd_a_fifo_skill_file_is_refused_without_waiting() {
        let root = fixture("resource-fifo");
        mkfifo(&root.join("pipe.md"));
        let directory = root.to_path_buf();
        let result = within_bound(root.join("pipe.md"), move || {
            acquire_resource(&directory, "pipe.md", None, &mut |_| ())
        });
        match result {
            Err(error) => assert!(error.contains("not a regular file"), "{error}"),
            Ok(content) => panic!("a FIFO was served: {content}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn skd_a_file_replaced_by_a_fifo_as_it_is_opened_is_refused_without_waiting() {
        let root = fixture("resource-fifo-swap");
        std::fs::write(root.join("template.md"), "regular").unwrap_or_else(|e| panic!("{e}"));
        let directory = root.to_path_buf();
        let target = root.join("template.md");
        let swap = target.clone();
        let result = within_bound(target, move || {
            acquire_resource(&directory, "template.md", None, &mut |phase| {
                if phase == Acquisition::Opening {
                    std::fs::remove_file(&swap).unwrap_or_else(|e| panic!("{e}"));
                    mkfifo(&swap);
                }
            })
        });
        match result {
            Err(error) => assert!(error.contains("not a regular file"), "{error}"),
            Ok(content) => panic!("a FIFO was served: {content}"),
        }
    }

    /// Windows holds every directory on the path and the file itself without
    /// delete sharing, so neither can be moved aside while it is checked and read.
    #[cfg(windows)]
    #[test]
    fn skd_a_held_skill_path_cannot_be_moved_while_it_is_read() {
        let root = fixture("resource-pinned");
        std::fs::create_dir_all(root.join("sub")).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(root.join("sub/notes.md"), "pinned").unwrap_or_else(|e| panic!("{e}"));
        let attempt = |phase: Acquisition| {
            let moved = match phase {
                Acquisition::Opened => std::fs::rename(root.join("sub"), root.join("moved")),
                Acquisition::Reading => std::fs::remove_file(root.join("sub/notes.md")),
                Acquisition::Opening => return,
            };
            assert!(moved.is_err(), "{phase:?}: a held path component was moved");
        };
        assert_eq!(
            acquire_resource(&root, "sub/notes.md", None, &mut { attempt }).as_deref(),
            Ok("pinned")
        );
    }
}
