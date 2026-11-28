//! User documents may widen roots. Repository documents can only turn on
//! isolation and turn off network; their writable-root declarations never grant.
use super::*;
use grapt_config::ConfigValue;

const INSTRUCTION_FILES: [&str; 2] = ["AGENTS.md", "AGENTS.override.md"];

fn initial_writable_roots(root: &Path) -> Result<Vec<PathBuf>, String> {
    let canonical_root = canonical(root)?;
    let canonical_temp = canonical(&std::env::temp_dir())?;
    let mut writable = vec![canonical_root.clone()];
    // Never widen a workspace into a shared ancestor temp directory (e.g. when
    // `root` itself lives under `/tmp` or `$TMPDIR`): exposing an ancestor lets
    // sandboxed commands write into sibling workspaces and causes synthetic
    // read-only mount targets to be created in the shared parent directory.
    if !canonical_root.starts_with(&canonical_temp) {
        writable.push(canonical_temp);
    }
    Ok(writable)
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn read(root: &Path, sources: &CatalogSources) -> Result<Option<Plan>, String> {
    let mut enabled = true;
    let mut git_metadata_read_only = false;
    let mut network = true;
    let mut declared = Vec::new();
    for document in sources.documents.iter().filter(|d| user_owned(d.origin)) {
        let Some(settings) = settings(&document.value)? else {
            continue;
        };
        git_metadata_read_only |= boolean(settings, "git_metadata_read_only")? == Some(true);
        if let Some(value) = boolean(settings, "enabled")? {
            enabled = value;
        }
        if let Some(value) = boolean(settings, "network")? {
            network = value;
        }
        if let Some(paths) = settings.get("writable_roots") {
            let paths = paths
                .value()
                .as_array()
                .ok_or("sandbox.writable_roots must be an array")?;
            if paths.len() > 32 {
                return Err("sandbox.writable_roots exceeds 32 paths".into());
            }
            declared.clear();
            for path in paths {
                let path = path
                    .value()
                    .as_str()
                    .ok_or("sandbox writable roots must be strings")?;
                let path = PathBuf::from(path);
                if !path.is_absolute() {
                    return Err("sandbox writable roots must be absolute".into());
                }
                declared.push(canonical(&path)?);
            }
        }
    }
    for document in sources.documents.iter().filter(|d| !user_owned(d.origin)) {
        let Some(settings) = settings(&document.value)? else {
            continue;
        };
        git_metadata_read_only |= boolean(settings, "git_metadata_read_only")? == Some(true);
        enabled |= boolean(settings, "enabled")? == Some(true);
        network &= boolean(settings, "network")? != Some(false);
        // Validate the shape without consuming repository authority as a grant.
        if let Some(paths) = settings.get("writable_roots") {
            if paths.value().as_array().is_none() {
                return Err("sandbox.writable_roots must be an array".into());
            }
        }
    }
    if !enabled && !git_metadata_read_only {
        return Ok(None);
    }
    let mut writable = initial_writable_roots(root)?;
    writable.extend(declared);
    writable.sort();
    writable.dedup();
    if writable.iter().any(|path| path.parent().is_none()) {
        return Err("sandbox cannot make a filesystem root writable".into());
    }
    let exe = std::env::current_exe().map_err(|e| format!("sandbox image unavailable: {e}"))?;
    let binaries = exe
        .parent()
        .ok_or("sandbox image has no directory")?
        .to_path_buf();
    let mut protected = sources.document_paths.clone();
    protected.extend([
        sources.environment.config.clone(),
        sources.environment.service_root().to_path_buf(),
        binaries,
        root.join(".stateengine"),
    ]);
    protected.extend(
        crate::dogfood::legacy_names::PROJECT_DIRECTORIES
            .iter()
            .map(|name| root.join(name)),
    );
    let (git_protected, repo_root) = git_control_paths(root, git_metadata_read_only)?;
    protected.extend(git_protected);
    protected = protected
        .iter()
        .map(|path| crate::dogfood::permission_preset::resolved_path(path).map(PathBuf::from))
        .collect::<Result<_, _>>()?;
    // Protect both the literal workspace/repository instruction slots and their
    // resolved targets so a symlinked instruction file cannot be unlinked or replaced.
    push_instruction_paths(&mut protected, &canonical(root)?)?;
    if let Some(repo_root) = repo_root {
        push_instruction_paths(&mut protected, &repo_root)?;
    }
    protected.sort();
    protected.dedup();
    let mut denied = vec![PathBuf::from(
        crate::dogfood::permission_preset::resolved_path(&sources.environment.home.join(".ssh"))?,
    )];
    denied.sort();
    denied.dedup();
    // This write-only policy permits normal toolchains to read their installed
    // libraries. Read authority is explicit for every drive touched by the plan.
    let mut readable = writable
        .iter()
        .chain(&protected)
        .filter_map(|path| path.ancestors().last().map(Path::to_path_buf))
        .collect::<Vec<_>>();
    #[cfg(windows)]
    {
        // Temp and every grant can live on another drive. Windows toolchains
        // still need reads from the drive containing the OS installation.
        let system =
            std::env::var_os("SystemRoot").ok_or("sandbox Windows system root is unavailable")?;
        let system = canonical(&PathBuf::from(system))?;
        let drive = system
            .ancestors()
            .last()
            .ok_or("sandbox Windows system drive is unavailable")?;
        readable.push(drive.to_path_buf());
    }
    readable.sort();
    readable.dedup();
    if readable.len() + writable.len() + protected.len() + denied.len() > 128 {
        return Err("sandbox policy exceeds 128 paths".into());
    }
    Ok(Some(Plan {
        git_metadata_read_only,
        readable,
        writable,
        protected,
        denied,
        network,
    }))
}

fn canonical(path: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(path)
        .map_err(|e| format!("sandbox root {} unavailable: {e}", path.display()))
}
fn settings(value: &ConfigValue) -> Result<Option<&ConfigValue>, String> {
    let Some(value) = value.get("sandbox") else {
        return Ok(None);
    };
    if value.value().as_object().is_none() {
        return Err("sandbox must be an object".into());
    }
    Ok(Some(value.value()))
}
fn boolean(value: &ConfigValue, name: &str) -> Result<Option<bool>, String> {
    value
        .get(name)
        .map(|v| {
            v.value()
                .as_bool()
                .ok_or_else(|| format!("sandbox.{name} must be a boolean"))
        })
        .transpose()
}

fn push_instruction_paths(protected: &mut Vec<PathBuf>, directory: &Path) -> Result<(), String> {
    for name in INSTRUCTION_FILES {
        let slot = directory.join(name);
        let resolved = PathBuf::from(crate::dogfood::permission_preset::resolved_path(&slot)?);
        protected.push(slot);
        protected.push(resolved);
    }
    Ok(())
}

fn git_control_paths(
    root: &Path,
    metadata_read_only: bool,
) -> Result<(Vec<PathBuf>, Option<PathBuf>), String> {
    let root = canonical(root)?;
    let mut dot_git = root.join(".git");
    let mut entry = None;
    // A session may start below the repository root. The nearest Git entry
    // owns its controls, including a linked worktree's common directory.
    for (depth, ancestor) in root.ancestors().enumerate() {
        if depth >= 64 {
            return Err("sandbox repository discovery exceeds 64 ancestors".into());
        }
        let candidate = ancestor.join(".git");
        match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) => {
                dot_git = candidate;
                entry = Some(metadata);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("sandbox Git metadata unavailable: {error}")),
        }
    }
    let protect = |directory: &Path| {
        if metadata_read_only {
            vec![directory.to_path_buf()]
        } else {
            ["config", "config.worktree", "hooks"]
                .map(|name| directory.join(name))
                .to_vec()
        }
    };
    let mut protected = Vec::new();
    let repo_root = dot_git
        .parent()
        .filter(|parent| *parent != root)
        .map(Path::to_path_buf);
    // Reserve session-local metadata unless .git is already a pointer file.
    // Nested sessions still cannot create a repository to shadow this one.
    if dot_git != root.join(".git") || !entry.as_ref().is_some_and(|metadata| metadata.is_file()) {
        protected.extend(protect(&root.join(".git")));
    }
    let directory = match entry {
        Some(metadata) if metadata.is_symlink() => {
            return Err("sandbox refuses a symlinked .git entry".into())
        }
        Some(metadata) if metadata.is_file() => {
            let text = bounded_git_path(&dot_git)?;
            let directory = text
                .strip_prefix("gitdir:")
                .ok_or("sandbox .git pointer is malformed")?
                .trim();
            if metadata_read_only && directory.is_empty() {
                return Err("sandbox .git pointer has no directory".into());
            }
            protected.push(dot_git.clone());
            let repository = dot_git.parent().ok_or("sandbox Git entry has no parent")?;
            git_directory(&repository.join(directory), metadata_read_only)?
        }
        Some(metadata) if metadata.is_dir() => dot_git,
        Some(_) => return Err("sandbox Git entry is not a file or directory".into()),
        None => dot_git,
    };
    protected.extend(protect(&directory));
    let common = directory.join("commondir");
    match std::fs::symlink_metadata(&common) {
        Ok(_) => {
            let path = git_directory(
                &directory.join(bounded_git_path(&common)?.trim()),
                metadata_read_only,
            )?;
            protected.push(common);
            protected.extend(protect(&path));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("sandbox Git common directory unavailable: {error}")),
    }
    Ok((protected, repo_root))
}
fn git_directory(path: &Path, metadata_read_only: bool) -> Result<PathBuf, String> {
    let path = canonical(path)?;
    if metadata_read_only {
        let metadata = std::fs::metadata(&path)
            .map_err(|error| format!("sandbox Git directory unavailable: {error}"))?;
        if !metadata.is_dir() {
            return Err("sandbox Git pointer must name a directory".into());
        }
    }
    Ok(path)
}
fn bounded_git_path(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let mut text = String::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(4097).read_to_string(&mut text))
        .map_err(|error| format!("sandbox Git pointer unavailable: {error}"))?;
    if text.len() > 4096 || text.trim().is_empty() || text.trim().contains(['\n', '\r', '\0']) {
        return Err("sandbox Git pointer is malformed or too large".into());
    }
    Ok(text)
}
