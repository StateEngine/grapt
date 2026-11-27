//! Remote acquisition produces captured bytes; only the existing publisher activates them.
use super::destination::Parent;
use super::git_command;
use super::git_tree::Inventory;
use super::snapshot::{io, Snapshot};
use grapt_plugin::claude::GitSource;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static NEXT: AtomicU64 = AtomicU64::new(0);

pub(super) enum Selection {
    Marketplace,
    Plugin,
}
pub(super) struct Acquired {
    pub snapshot: Snapshot,
    pub commit: String,
}
/// Call only after the user-config authority preflight has succeeded.
/// Cancellation retains the private scratch rather than racing a dropped process
/// with deletion. Published trees are never cleanup targets.
pub(super) async fn git(
    scratch: PathBuf,
    source: &GitSource,
    selection: Selection,
) -> Result<Acquired, String> {
    validate(source)?;
    let guard = super::acquisition_guard::Guard::open(&scratch).map_err(io)?;
    let result = tokio::time::timeout(
        Duration::from_secs(240),
        acquire(guard.path(), source, selection),
    )
    .await;
    // Every normal subprocess return has stopped its group and joined its leader.
    // A dropped outer deadline has only requested cleanup, so preserve that scratch.
    let result = match result {
        Ok(result) => result,
        Err(_) => {
            return Err("plugin acquisition exceeded 240 seconds; private scratch retained".into())
        }
    };
    if result.as_ref().is_err_and(|error| error.unreaped) {
        return result.map_err(|error| format!("{}; private scratch retained", error.message));
    }
    guard.cleanup(&scratch).map_err(io)?;
    result.map_err(|error| error.message)
}

pub(super) fn validate(source: &GitSource) -> Result<(), String> {
    if !cfg!(any(windows, target_os = "linux", target_os = "android")) {
        return Err("anchored plugin Git acquisition is not supported on this platform yet".into());
    }
    let url = url::Url::parse(&source.url).map_err(|_| "plugin Git URL is malformed")?;
    if source.url.len() > 4096
        || source.url.chars().any(char::is_control)
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("plugin Git acquisition requires an HTTPS URL without embedded credentials, query or fragment".into());
    }
    if let Some(revision) = &source.git_ref {
        if revision.is_empty()
            || revision.len() > 4096
            || revision.starts_with('-')
            || revision.chars().any(char::is_control)
        {
            return Err("plugin Git ref must be a bounded operand".into());
        }
    }
    if let Some(sha) = &source.git_sha {
        if !matches!(sha.len(), 40 | 64) || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("plugin Git sha must be a complete commit identity".into());
        }
    }
    let subdir = source
        .subdir
        .to_str()
        .ok_or("plugin Git subdirectory is not UTF-8")?;
    if subdir.chars().any(char::is_control) {
        return Err("plugin Git subdirectory contains a control character".into());
    }
    grapt_plugin::claude::relative(&format!("./{subdir}"))?;
    Ok(())
}
pub(super) fn scratch(config: &Path) -> Result<PathBuf, String> {
    let staging = config.join("claude-plugins/acquisition");
    Parent::open(&staging.join(".probe"), true).map_err(io)?;
    // This check is under the installer's existing publication lock. Four retained
    // attempts bound cancellation/crash residue; cleanup is an explicit later action.
    let retained = std::fs::read_dir(&staging)
        .map_err(io)?
        .take(5)
        .try_fold(0usize, |count, entry| entry.map_err(io).map(|_| count + 1))?;
    if retained >= 4 {
        return Err("plugin acquisition has four retained attempts; restart the service to release retained handles, then remove only abandoned private acquisition directories before retrying".into());
    }
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = staging.join(format!("{}-{serial}", std::process::id()));
    Parent::open(&root, false)
        .map_err(io)?
        .create_directory()
        .map_err(io)?;
    Ok(root)
}
async fn acquire(
    scratch: &Path,
    source: &GitSource,
    selection: Selection,
) -> Result<Acquired, git_command::Error> {
    let program = std::ffi::OsStr::new("git");
    let mut init = vec!["init", "--bare", "--quiet"];
    if source.git_sha.as_ref().is_some_and(|sha| sha.len() == 64) {
        init.push("--object-format=sha256");
    }
    git_command::run(program, scratch, &init, 4096).await?;
    let revision = revision(program, scratch, source).await?;
    git_command::run(
        program,
        scratch,
        &[
            "fetch",
            "--quiet",
            "--depth=1",
            "--no-tags",
            "--no-recurse-submodules",
            "--",
            &source.url,
            &revision,
        ],
        4096,
    )
    .await?;
    let commit = git_command::run(
        program,
        scratch,
        &["rev-parse", "--verify", "FETCH_HEAD^{commit}"],
        128,
    )
    .await?;
    let commit = std::str::from_utf8(&commit)
        .map_err(|_| "plugin Git commit is not UTF-8")?
        .trim()
        .to_owned();
    if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("plugin Git did not resolve one complete commit identity".into());
    }
    if source
        .git_sha
        .as_ref()
        .is_some_and(|sha| !sha.eq_ignore_ascii_case(&commit))
    {
        return Err("plugin Git revision does not match its declared commit identity".into());
    }
    let subdir = source
        .subdir
        .to_str()
        .ok_or("plugin Git subdirectory is not UTF-8")?
        .replace('\\', "/");
    let tree = if subdir.is_empty() {
        format!("{commit}^{{tree}}")
    } else {
        format!("{commit}:{subdir}")
    };
    if git_command::run(program, scratch, &["cat-file", "-t", &tree], 16).await? != b"tree\n" {
        return Err("plugin Git subdirectory must be a tree, not a file or submodule".into());
    }
    let mut arguments = vec!["ls-tree", "-r", "-l", "-z", &tree, "--"];
    if matches!(selection, Selection::Marketplace) {
        arguments.push(".claude-plugin/marketplace.json");
    }
    let output = git_command::run(program, scratch, &arguments, 5 * 1024 * 1024).await?;
    let inventory = Inventory::parse(&output)?;
    let mut contents = Vec::with_capacity(inventory.blobs.len());
    for blob in &inventory.blobs {
        contents.push(
            git_command::run(
                program,
                scratch,
                &["cat-file", "blob", &blob.oid],
                blob.bytes,
            )
            .await?,
        );
    }
    Ok(Acquired {
        snapshot: inventory.finish(contents)?,
        commit,
    })
}

/// A manifest ref is a literal branch/tag, never a fetch refspec. Resolve it
/// first, then fetch only that object; a moved branch cannot change the selection.
async fn revision(
    program: &std::ffi::OsStr,
    scratch: &Path,
    source: &GitSource,
) -> Result<String, git_command::Error> {
    let Some(reference) = source.git_ref.as_deref() else {
        return Ok(source.git_sha.as_deref().unwrap_or("HEAD").to_owned());
    };
    git_command::run(
        program,
        scratch,
        &["check-ref-format", "--allow-onelevel", reference],
        4096,
    )
    .await
    .map_err(|mut error| {
        if !error.unreaped {
            error.message =
                "plugin Git ref must be a literal branch or tag, not a fetch refspec".into();
        }
        error
    })?;
    if reference == "HEAD" {
        return Ok("HEAD".into());
    }
    let names = if reference.starts_with("refs/") {
        vec![reference.to_owned()]
    } else {
        vec![
            format!("refs/heads/{reference}"),
            format!("refs/tags/{reference}"),
        ]
    };
    let mut arguments = vec!["ls-remote", "--refs", "--", source.url.as_str()];
    arguments.extend(names.iter().map(String::as_str));
    let output = git_command::run(program, scratch, &arguments, 16 * 1024).await?;
    let output = std::str::from_utf8(&output).map_err(|_| "plugin Git refs are not UTF-8")?;
    let mut found = std::collections::BTreeMap::new();
    for line in output.lines() {
        let (oid, name) = line
            .split_once('\t')
            .ok_or("plugin Git ref has no identity")?;
        if !matches!(oid.len(), 40 | 64)
            || !oid.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !names.iter().any(|expected| expected == name)
            || found.insert(name, oid).is_some()
        {
            return Err("plugin Git advertised a malformed or duplicate ref".into());
        }
    }
    for name in names {
        if let Some(oid) = found.get(name.as_str()) {
            return Ok((*oid).to_owned());
        }
    }
    if matches!(reference.len(), 40 | 64) && reference.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Ok(reference.to_owned());
    }
    Err("plugin Git branch or tag was not found".into())
}
