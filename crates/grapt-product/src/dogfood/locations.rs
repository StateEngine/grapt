//! Read-only location discovery matching the retained project identity contract.
//! Parallel metadata and direct origin reads.
// Copyright 2025 OpenAI. Modified for Grapt's retained identity and bounded discovery.

use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use grapt_api::u3a::ProjectCurrentLocationQueryDto;
use grapt_types::{LocationRef, ProjectId};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt};

use super::CatalogProjection;

mod directory;
mod program;
mod saved;
pub(super) use saved::resolve_for;
mod session;
pub(super) use session::SessionLocations;

const MAX_PATH_BYTES: usize = 4_096;
const MAX_ANCESTORS: usize = 256;
const MAX_DISCOVERY_BYTES: u64 = 64 * 1_024;
const DISCOVERY_DEADLINE: Duration = Duration::from_secs(3);

#[derive(Debug)]
pub(super) struct ResolvedLocation {
    pub directory: String,
    pub project: ProjectId,
    pub project_directory: String,
}

impl ResolvedLocation {
    pub fn wire(&self) -> Value {
        json!({"directory": self.directory, "project": {
            "id": self.project.as_str(), "directory": self.project_directory,
        }})
    }
}

pub(super) async fn resolve(
    default: &CatalogProjection,
    query: &ProjectCurrentLocationQueryDto,
) -> Result<ResolvedLocation, String> {
    resolve_default(&default.directory, query).await
}

pub(super) async fn resolve_default(
    default: &str,
    query: &ProjectCurrentLocationQueryDto,
) -> Result<ResolvedLocation, String> {
    if query.session().as_present().is_some() {
        return Err("Session location context requires canonical validation".to_owned());
    }
    let requested = query.location().as_value();
    if requested.is_some_and(|value| value.workspace().as_value().is_some()) {
        return Err("requested workspace has no registered location resolver".to_owned());
    }
    let directory = requested
        .and_then(|value| value.directory().as_value())
        .map_or(default, |value| value.as_str());
    if directory.len() > MAX_PATH_BYTES {
        return Err("location directory exceeds its byte bound".to_owned());
    }
    LocationRef::new(directory, None).map_err(|error| error.to_string())?;
    resolve_directory(directory).await
}

pub(super) async fn resolve_directory(directory: &str) -> Result<ResolvedLocation, String> {
    if directory.len() > MAX_PATH_BYTES {
        return Err("location directory exceeds its byte bound".to_owned());
    }
    LocationRef::new(directory, None).map_err(|error| error.to_string())?;
    tokio::time::timeout(Duration::from_secs(10), discover(directory))
        .await
        .map_err(|_| "location discovery deadline exceeded")?
}

async fn discover(directory: &str) -> Result<ResolvedLocation, String> {
    let path = Path::new(directory);
    let metadata =
        std::fs::metadata(path).map_err(|error| format!("location is unreadable: {error}"))?;
    if !metadata.is_dir() {
        return Err("requested location is not a directory".to_owned());
    }
    let Some(git_root) = repository_root(path)? else {
        return self::directory::resolve(path);
    };
    // Git emits the topology before checking HEAD, including for an unborn
    // repository. Read them together and resolve origin independently, paying
    // no extra process launch just to check whether either value exists.
    let (topology, origin) = tokio::join!(
        git(
            git_root,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
                "--show-toplevel",
                "--verify",
                "--quiet",
                "HEAD",
            ],
        ),
        git(git_root, &["remote", "get-url", "origin"]),
    );
    let (head_status, topology) = topology?;
    if topology.is_empty() {
        return Err("Git repository topology could not be resolved".to_owned());
    }
    let mut lines = topology.lines();
    let common = lines
        .next()
        .filter(|line| !line.is_empty())
        .ok_or("Git common directory is absent")?;
    let worktree = lines
        .next()
        .filter(|line| !line.is_empty())
        .ok_or("Git worktree is absent")?;
    let head = lines.next().filter(|line| !line.is_empty());
    if (head_status == 0) != head.is_some() || lines.next().is_some() {
        return Err("Git topology is ambiguous".to_owned());
    }
    let (origin_status, origin) = origin?;
    // `remote get-url` returns 2 when origin has no URL. Other failures are
    // unreadable configuration, never evidence of an absent origin.
    if origin_status != 0 && origin_status != 2 {
        return Err("Git origin configuration is unreadable".to_owned());
    }
    let remote = if origin_status == 0 {
        remote_identity(origin.trim())
    } else {
        None
    };
    if let Some(id) = remote {
        return location(directory, &id, Path::new(worktree));
    }
    if let Some(id) = cached_id(&Path::new(common).join("secode"))? {
        return location(directory, &id, Path::new(worktree));
    }
    if head_status == 1 {
        return location(directory, "global", Path::new(worktree));
    }
    if head_status != 0 {
        return Err("Git HEAD is unreadable".to_owned());
    }
    let (root_status, roots) = git(git_root, &["rev-list", "--max-parents=0", "HEAD"]).await?;
    if root_status != 0 {
        return Err("Git root history is unreadable".to_owned());
    }
    let root = roots
        .lines()
        .filter(|line| !line.is_empty())
        .min()
        .ok_or("Git root history is unexpectedly empty")?;
    location(directory, root, Path::new(worktree))
}

/// The repository a directory belongs to: the nearest ancestor holding
/// `.git`, read from the filesystem alone, or `None` outside any repository.
/// The one rule for "what is this directory's repo root".
pub(super) fn repository_root(path: &Path) -> Result<Option<&Path>, String> {
    repository_root_with_limit(path, MAX_ANCESTORS)
}

pub(super) fn repository_root_with_limit(
    path: &Path,
    max_ancestors: usize,
) -> Result<Option<&Path>, String> {
    let raw = std::env::var_os("GIT_CEILING_DIRECTORIES").unwrap_or_default();
    if raw.len() > 16_384 {
        return Err("Git discovery ceilings exceed their byte bound".to_owned());
    }
    let ceilings: Vec<_> = std::env::split_paths(&raw).take(65).collect();
    if ceilings.len() > 64 {
        return Err("Git discovery exceeds its ceiling count bound".to_owned());
    }
    repository_root_below(path, &ceilings, max_ancestors.min(MAX_ANCESTORS))
}

fn repository_root_below<'a>(
    path: &'a Path,
    ceilings: &[std::path::PathBuf],
    max_ancestors: usize,
) -> Result<Option<&'a Path>, String> {
    let mut normalized = Vec::new();
    for ceiling in ceilings.iter().filter(|path| path.is_absolute()) {
        match std::fs::canonicalize(ceiling) {
            Ok(path) => normalized.push(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Git ceiling is unreadable: {error}")),
        }
    }
    for (index, parent) in path.ancestors().enumerate() {
        // Git may recognize an explicitly selected repository at a ceiling,
        // but must not discover one there while searching from a child.
        if index > 0 && !normalized.is_empty() {
            let coordinate = std::fs::canonicalize(parent)
                .map_err(|error| format!("Git discovery ancestor is unreadable: {error}"))?;
            if normalized.contains(&coordinate) {
                return Ok(None);
            }
        }
        if index >= max_ancestors {
            return Err("location discovery exceeds its ancestor bound".to_owned());
        }
        match std::fs::symlink_metadata(parent.join(".git")) {
            #[cfg(target_os = "linux")]
            Ok(metadata)
                if metadata.is_dir()
                    && grapt_sandbox::Bwrap::synthetic_directory(&parent.join(".git"))? => {}
            Ok(_) => return Ok(Some(parent)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Git discovery is unreadable: {error}")),
        }
        // An unimplemented VCS must not be silently reassigned to global.
        match std::fs::symlink_metadata(parent.join(".hg")) {
            Ok(_) => return Err("Mercurial location resolution is not composed yet".to_owned()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("VCS discovery is unreadable: {error}")),
        }
    }
    Ok(None)
}

fn location(directory: &str, id: &str, root: &Path) -> Result<ResolvedLocation, String> {
    let project = ProjectId::new(id).ok_or("resolved project identity is invalid")?;
    let project_directory = root.to_str().ok_or("project root is not UTF-8")?;
    if project_directory.len() > MAX_PATH_BYTES || id.len() > MAX_PATH_BYTES {
        return Err("resolved project exceeds its byte bound".to_owned());
    }
    let directory = LocationRef::new(directory, None).map_err(|error| error.to_string())?;
    let project_directory =
        LocationRef::new(project_directory, None).map_err(|error| error.to_string())?;
    Ok(ResolvedLocation {
        directory: directory.directory().to_owned(),
        project,
        project_directory: project_directory.directory().to_owned(),
    })
}

fn cached_id(path: &Path) -> Result<Option<String>, String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cached project identity is unreadable: {error}")),
    };
    let mut bytes = Vec::new();
    file.take(MAX_PATH_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cached project identity read failed: {error}"))?;
    if bytes.len() > MAX_PATH_BYTES {
        return Err("cached project identity exceeds its byte bound".to_owned());
    }
    let value = String::from_utf8(bytes).map_err(|_| "cached project identity is not UTF-8")?;
    Ok((!value.trim().is_empty()).then(|| value.trim().to_owned()))
}

fn remote_identity(origin: &str) -> Option<String> {
    let normalized = if let Ok(url) = url::Url::parse(origin) {
        if url.scheme() == "file" {
            return None;
        }
        remote_parts(url.host_str()?, url.path())?
    } else {
        let (host, path) = origin.split_once(':')?;
        let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
        if host.contains('/') || host.is_empty() {
            return None;
        }
        remote_parts(host, path)?
    };
    let digest = ring::digest::digest(
        &ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
        format!("git-remote:{normalized}").as_bytes(),
    );
    Some(
        digest
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

fn remote_parts(host: &str, path: &str) -> Option<String> {
    let path = path.trim_start_matches('/');
    let path = path
        .strip_suffix(".git/")
        .or_else(|| path.strip_suffix(".git"))
        .unwrap_or(path);
    let path = path.trim_end_matches('/');
    (!host.is_empty() && !path.is_empty()).then(|| format!("{}/{path}", host.to_lowercase()))
}

async fn output(stream: impl AsyncRead + Unpin) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    stream
        .take(MAX_DISCOVERY_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| format!("Git discovery output failed: {error}"))?;
    if bytes.len() as u64 > MAX_DISCOVERY_BYTES {
        return Err("Git discovery output exceeds its byte bound".to_owned());
    }
    Ok(bytes)
}

pub(super) async fn git(cwd: &Path, args: &[&str]) -> Result<(i32, String), String> {
    let executable = program::resolve(cwd).await;
    run(command(cwd, args, &executable)).await
}

fn command(cwd: &Path, args: &[&str], program: &std::ffi::OsStr) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    command
        .arg("--no-pager")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("GCM_INTERACTIVE", "Never")
        .env("GIT_PAGER", "cat")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

async fn run(command: tokio::process::Command) -> Result<(i32, String), String> {
    // Git can start helpers of its own; the group ends them with it.
    let group = processkit::ProcessGroup::new()
        .map_err(|error| format!("Git discovery containment failed: {error}"))?;
    let mut child = group
        .spawn(command)
        .map_err(|error| format!("Git discovery could not start: {error}"))?;
    let stdout = child.stdout.take().ok_or("Git stdout pipe is absent")?;
    let stderr = child.stderr.take().ok_or("Git stderr pipe is absent")?;
    let result = tokio::time::timeout(DISCOVERY_DEADLINE, async {
        tokio::try_join!(output(stdout), output(stderr), async {
            child
                .wait()
                .await
                .map_err(|error| format!("Git discovery wait failed: {error}"))
        })
    })
    .await
    .map_err(|_| "Git discovery deadline exceeded")??;
    let text = String::from_utf8(result.0).map_err(|_| "Git discovery output is not UTF-8")?;
    let status = result
        .2
        .code()
        .ok_or("Git discovery terminated without an exit code")?;
    if status != 0 && status != 1 && status != 2 {
        // Preserve a useful cause without returning remote URLs, credentials,
        // or arbitrary repository-controlled stderr in a public response.
        let stderr = String::from_utf8_lossy(&result.1);
        let reason = if stderr.contains("dubious ownership") {
            "dubious ownership; Git refused this repository"
        } else if stderr.contains("not a git repository") {
            "not a Git repository"
        } else {
            "repository metadata could not be read"
        };
        return Err(format!("Git discovery failed ({status}): {reason}"));
    }
    Ok((status, text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_ceiling_excludes_parent_but_preserves_selected_and_nested_repositories() {
        let root =
            std::env::temp_dir().join(format!("grapt-ceiling-location-{}", std::process::id()));
        let child = root.join("plain");
        let nested = child.join("nested");
        std::fs::create_dir(&root).unwrap_or_else(|error| panic!("ceiling fixture: {error}"));
        std::fs::create_dir(root.join(".git"))
            .unwrap_or_else(|error| panic!("parent marker: {error}"));
        std::fs::create_dir_all(&nested).unwrap_or_else(|error| panic!("nested fixture: {error}"));
        let ceilings = std::slice::from_ref(&root);
        assert!(repository_root_below(&child, &[], 1).is_err());
        assert_eq!(
            repository_root_below(&child, ceilings, MAX_ANCESTORS),
            Ok(None)
        );
        assert_eq!(
            repository_root_below(&root, ceilings, MAX_ANCESTORS),
            Ok(Some(root.as_path()))
        );
        std::fs::create_dir(nested.join(".git"))
            .unwrap_or_else(|error| panic!("nested marker: {error}"));
        assert_eq!(
            repository_root_below(&nested, ceilings, MAX_ANCESTORS),
            Ok(Some(nested.as_path()))
        );
        std::fs::remove_dir_all(&root).unwrap_or_else(|error| panic!("ceiling cleanup: {error}"));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_ceiling_matches_git_for_physical_and_aliased_children() {
        let root =
            std::env::temp_dir().join(format!("grapt-symlink-ceiling-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap_or_else(|error| panic!("ceiling fixture: {error}"));
        std::fs::create_dir(root.join(".git"))
            .unwrap_or_else(|error| panic!("parent marker: {error}"));
        std::fs::create_dir(root.join("plain")).unwrap_or_else(|error| panic!("child: {error}"));
        let alias = root.join("alias");
        std::os::unix::fs::symlink(&root, &alias)
            .unwrap_or_else(|error| panic!("ceiling alias: {error}"));
        assert_eq!(
            repository_root_below(
                &root.join("plain"),
                std::slice::from_ref(&alias),
                MAX_ANCESTORS
            ),
            Ok(None)
        );
        assert_eq!(
            repository_root_below(
                &alias.join("plain"),
                std::slice::from_ref(&root),
                MAX_ANCESTORS
            ),
            Ok(None)
        );
        std::fs::remove_dir_all(&root).unwrap_or_else(|error| panic!("ceiling cleanup: {error}"));
    }

    #[test]
    fn retained_remote_identity_ignores_transport_but_preserves_repository_case() {
        let expected = "0045578201ae0bc52e0933e712cb46649a491c1a";
        for origin in [
            "https://GitHub.com/StateEngine/SECode.git",
            "git@github.com:StateEngine/SECode.git",
            "ssh://git@github.com/StateEngine/SECode.git/",
        ] {
            assert_eq!(remote_identity(origin).as_deref(), Some(expected));
        }
        assert_ne!(
            remote_identity("https://github.com/stateengine/secode.git").as_deref(),
            Some(expected)
        );
        assert!(remote_identity("file:///tmp/local").is_none());
        assert!(remote_identity("/tmp/local").is_none());
    }

    #[tokio::test]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    async fn selected_git_project_is_read_only_and_corrupt_identity_never_becomes_global() {
        let root =
            std::env::temp_dir().join(format!("grapt-retained-location-{}", std::process::id()));
        std::fs::create_dir(&root)
            .unwrap_or_else(|error| panic!("location fixture creation failed: {error}"));
        let run = |args: Vec<&'static str>| {
            let root = root.clone();
            async move {
                let (status, _) = git(&root, &args)
                    .await
                    .unwrap_or_else(|error| panic!("location fixture Git failed: {error}"));
                assert_eq!(status, 0, "location fixture Git did not succeed");
            }
        };
        run(vec!["init", "--quiet"]).await;
        run(vec![
            "config",
            "remote.origin.url",
            "git@github.com:StateEngine/SECode.git",
        ])
        .await;
        let child = root.join("nested");
        std::fs::create_dir(&child)
            .unwrap_or_else(|error| panic!("nested fixture creation failed: {error}"));
        let directory = child
            .to_str()
            .unwrap_or_else(|| panic!("fixture is not UTF-8"));
        let before = std::fs::read(root.join(".git/config"))
            .unwrap_or_else(|error| panic!("Git config evidence absent: {error}"));
        let selected = discover(directory)
            .await
            .unwrap_or_else(|error| panic!("selected project refused: {error}"));
        assert_eq!(
            selected.project.as_str(),
            "0045578201ae0bc52e0933e712cb46649a491c1a"
        );
        assert_eq!(selected.directory, directory.replace('\\', "/"));
        let observed_root = std::fs::canonicalize(&selected.project_directory)
            .unwrap_or_else(|error| panic!("resolved root is unreadable: {error}"));
        let fixture_root = std::fs::canonicalize(&root)
            .unwrap_or_else(|error| panic!("fixture root is unreadable: {error}"));
        assert_eq!(observed_root, fixture_root);
        let configured = CatalogProjection {
            accounts: Default::default(),
            protocols: Default::default(),
            inventory: None,
            project: ProjectId::new("untrusted-configured-project")
                .unwrap_or_else(|| panic!("fixture project invalid")),
            directory: directory.to_owned(),
            provider: "openai".to_owned(),
            model: "fixture".to_owned(),
            default_effort: None,
            credential_record: None,
        };
        for value in [json!({}), json!({"location":{"directory":directory}})] {
            let query = super::super::support::decode(value)
                .unwrap_or_else(|error| panic!("query fixture: {error}"));
            let resolved = resolve(&configured, &query)
                .await
                .unwrap_or_else(|error| panic!("default discovery refused: {error}"));
            assert_eq!(
                resolved.wire(),
                selected.wire(),
                "default location invented a project"
            );
        }
        assert_eq!(std::fs::read(root.join(".git/config")).ok(), Some(before));
        assert!(
            !root.join(".git/secode").exists(),
            "discovery wrote oracle metadata"
        );
        let default = std::sync::Arc::new(CatalogProjection {
            accounts: Default::default(),
            protocols: Default::default(),
            inventory: None,
            project: ProjectId::new("different-project")
                .unwrap_or_else(|| panic!("default fixture identity is invalid")),
            directory: "/different-location".to_owned(),
            provider: "openai".to_owned(),
            model: "fixture".to_owned(),
            default_effort: None,
            credential_record: None,
        });
        let query: grapt_api::u3a::ProviderLocationQueryDto =
            super::super::support::decode(json!({"location": {"directory": directory}}))
                .unwrap_or_else(|error| panic!("catalog query evidence failed: {error}"));
        let data =
            json!([{"id": "openai", "name": "Configured provider", "package": "builtin:live"}]);
        let response: grapt_api::u3a::ProviderListResponseDto =
            super::super::support::catalog_response(
                default,
                query,
                data.clone(),
                super::super::support::invalid,
            )
            .await
            .unwrap_or_else(|error| panic!("selected catalog refused: {error:?}"));
        let response = serde_json::to_value(response)
            .unwrap_or_else(|error| panic!("catalog evidence failed to encode: {error}"));
        assert_eq!(
            response["location"],
            selected.wire(),
            "catalog silently rebound to the service root"
        );
        assert_eq!(
            response["data"], data,
            "location resolution changed the configured inventory"
        );
        run(vec!["config", "--unset", "remote.origin.url"]).await;
        let identity = root.join(".git/secode");
        std::fs::write(&identity, "cached-project")
            .unwrap_or_else(|error| panic!("cached fixture failed: {error}"));
        assert_eq!(
            discover(directory)
                .await
                .unwrap_or_else(|error| panic!("cached identity refused: {error}"))
                .project
                .as_str(),
            "cached-project"
        );
        std::fs::write(&identity, [0xff])
            .unwrap_or_else(|error| panic!("corrupt fixture failed: {error}"));
        assert!(
            discover(directory).await.is_err(),
            "unreadable identity was silently replaced by global"
        );
        std::fs::write(&identity, vec![b'x'; MAX_PATH_BYTES + 1])
            .unwrap_or_else(|error| panic!("oversize fixture failed: {error}"));
        assert!(
            discover(directory).await.is_err(),
            "oversize identity escaped its bound"
        );
        std::fs::remove_dir_all(&root)
            .unwrap_or_else(|error| panic!("fixture cleanup failed: {error}"));
    }

    #[tokio::test]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    async fn batched_discovery_preserves_unborn_history_rewrites_and_linked_worktrees() {
        let fixture =
            std::env::temp_dir().join(format!("grapt-batched-location-{}", std::process::id()));
        let root = fixture.join("main");
        std::fs::create_dir_all(&root)
            .unwrap_or_else(|error| panic!("batch fixture creation: {error}"));
        let run = |args: Vec<&'static str>| {
            let root = root.clone();
            async move {
                let (status, output) = git(&root, &args)
                    .await
                    .unwrap_or_else(|error| panic!("batch fixture Git: {error}"));
                assert_eq!(status, 0, "fixture command failed: {args:?}");
                output
            }
        };
        run(vec!["init", "--quiet"]).await;
        let directory = root
            .to_str()
            .unwrap_or_else(|| panic!("fixture root is not UTF-8"));
        assert_eq!(
            discover(directory)
                .await
                .unwrap_or_else(|error| panic!("unborn discovery: {error}"))
                .project
                .as_str(),
            "global"
        );
        for message in ["first", "second"] {
            run(vec![
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--allow-empty",
                "--quiet",
                "-m",
                message,
            ])
            .await;
        }
        let first = run(vec!["rev-parse", "HEAD~1"]).await;
        assert_eq!(
            discover(directory)
                .await
                .unwrap_or_else(|error| panic!("history discovery: {error}"))
                .project
                .as_str(),
            first.trim(),
            "identity must remain the first commit, not the current HEAD"
        );
        run(vec![
            "config",
            "remote.origin.url",
            "fixture:StateEngine/SECode.git",
        ])
        .await;
        run(vec![
            "config",
            "url.https://github.com/.insteadOf",
            "fixture:",
        ])
        .await;
        let config = std::fs::read(root.join(".git/config"))
            .unwrap_or_else(|error| panic!("config evidence: {error}"));
        assert_eq!(
            discover(directory)
                .await
                .unwrap_or_else(|error| panic!("rewritten remote discovery: {error}"))
                .project
                .as_str(),
            "0045578201ae0bc52e0933e712cb46649a491c1a"
        );
        assert_eq!(
            std::fs::read(root.join(".git/config")).ok(),
            Some(config),
            "discovery changed the configured rewrite"
        );
        run(vec!["config", "--unset", "remote.origin.url"]).await;
        let linked = fixture.join("linked");
        let linked_directory = linked
            .to_str()
            .unwrap_or_else(|| panic!("linked fixture is not UTF-8"));
        let (status, _) = git(
            &root,
            &["worktree", "add", "--quiet", "--detach", linked_directory],
        )
        .await
        .unwrap_or_else(|error| panic!("linked worktree fixture: {error}"));
        assert_eq!(status, 0);
        std::fs::write(root.join(".git/secode"), "shared-cached-project")
            .unwrap_or_else(|error| panic!("common identity fixture: {error}"));
        let selected = discover(linked_directory)
            .await
            .unwrap_or_else(|error| panic!("linked discovery: {error}"));
        assert_eq!(selected.project.as_str(), "shared-cached-project");
        assert_eq!(
            selected.project_directory,
            linked_directory.replace('\\', "/")
        );
        std::fs::remove_dir_all(&fixture)
            .unwrap_or_else(|error| panic!("batch fixture cleanup: {error}"));
    }

    #[tokio::test]
    async fn discovery_output_bound_is_a_rejection_not_truncation() {
        let bytes = vec![b'x'; MAX_DISCOVERY_BYTES as usize + 1];
        assert!(output(bytes.as_slice()).await.is_err());
        assert_eq!(
            output(&b"complete"[..]).await.ok(),
            Some(b"complete".to_vec())
        );
    }
}
