//! Copyright 2025 OpenAI.
//! Apache-2.0. Modified by Grapt: existing resolved backend location,
//! bounded output/files/deadline, process-tree cleanup and staged changes.
use std::path::{Component, Path};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use grapt_api::u3a::{
    ApplicationFuture, InvalidRequestErrorDto, ProjectCurrentLocationQueryDto,
    WorkspaceDiffResponseDto,
};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt};

use super::support::{decode, invalid};
use super::{locations, CatalogProjection};

const MAX_DIFF_BYTES: usize = grapt_api::u3a::MAX_WORKSPACE_DIFF_BYTES;
const MAX_UNTRACKED_FILES: usize = 128;
const MAX_FILTER_DRIVERS: usize = 64;
const DIFF_DEADLINE: Duration = Duration::from_secs(10);
const NULL_DEVICE: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

pub(crate) fn response(
    catalog: Arc<CatalogProjection>,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<WorkspaceDiffResponseDto, InvalidRequestErrorDto> {
    Box::pin(async move {
        let location = locations::resolve(&catalog, &query)
            .await
            .map_err(invalid)?;
        let result = tokio::time::timeout(DIFF_DEADLINE, snapshot(Path::new(&location.directory)))
            .await.map_err(|_| invalid("Workspace diff deadline exceeded; try again or inspect the repository with Git."))?
            .map_err(invalid)?;
        decode(json!({"location":location.wire(), "data":{"isGit":result.0,"text":result.1}}))
            .map_err(invalid)
    })
}

async fn snapshot(directory: &Path) -> Result<(bool, String), String> {
    let Some(root) = locations::repository_root(directory)? else {
        return Ok((false, String::new()));
    };
    // A repository's core.worktree must not redirect this read to another tree.
    let (_, actual_root) = git(
        root,
        &["rev-parse", "--show-toplevel"],
        &[],
        MAX_DIFF_BYTES,
        false,
    )
    .await?;
    let actual_root = String::from_utf8(actual_root).map_err(|_| "Git root is not UTF-8")?;
    let expected = root
        .canonicalize()
        .map_err(|error| format!("Git root is unreadable: {error}"))?;
    let actual = Path::new(actual_root.trim_end_matches(['\r', '\n']))
        .canonicalize()
        .map_err(|error| format!("Git worktree is unreadable: {error}"))?;
    if expected != actual {
        return Err("Git worktree differs from the selected repository root.".to_owned());
    }

    let filters = filter_overrides(root).await?;
    let base = [
        "diff",
        "--no-textconv",
        "--no-ext-diff",
        "--submodule=short",
        "--ignore-submodules=dirty",
        "--no-color",
        "--no-renames",
    ];
    let mut text = String::new();
    for (title, cached) in [("STAGED", true), ("UNSTAGED", false)] {
        let mut args = base.to_vec();
        if cached {
            args.push("--cached");
        }
        args.push("--");
        let (_, output) = git(root, &args, &filters, remaining(&text)?, false).await?;
        append(&mut text, title, &output)?;
    }
    // NUL separation preserves spaces, newlines and leading dashes in filenames.
    let (_, paths) = git(
        root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        &filters,
        MAX_DIFF_BYTES,
        false,
    )
    .await?;
    for path in untracked_paths(&paths)? {
        let path = std::str::from_utf8(path).map_err(|_| "An untracked path is not UTF-8.")?;
        if path.len() > 4096
            || Path::new(path)
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err("An untracked path is outside the selected repository.".to_owned());
        }
        let mut args = base.to_vec();
        args.extend(["--no-index", "--", NULL_DEVICE, path]);
        let (_, output) = git(root, &args, &filters, remaining(&text)?, true).await?;
        append(&mut text, "UNTRACKED", &output)?;
    }
    Ok((true, text))
}

fn untracked_paths(paths: &[u8]) -> Result<impl Iterator<Item = &[u8]>, String> {
    let paths = paths
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty());
    // Decide the file bound before generating any per-file diffs. Besides
    // avoiding wasted subprocesses, this keeps the resource limit separate
    // from the response's wall-clock deadline.
    if paths.clone().take(MAX_UNTRACKED_FILES + 1).count() > MAX_UNTRACKED_FILES {
        return Err(
            "Workspace diff exceeds 128 untracked files; inspect the repository with Git."
                .to_owned(),
        );
    }
    Ok(paths)
}

fn remaining(text: &str) -> Result<usize, String> {
    MAX_DIFF_BYTES
        .checked_sub(text.len())
        .filter(|left| *left > 0)
        .ok_or_else(too_large)
}
fn too_large() -> String {
    "Workspace diff exceeds 128 KiB; inspect the repository with Git.".to_owned()
}
fn append(text: &mut String, title: &str, bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() {
        return Ok(());
    }
    let output = std::str::from_utf8(bytes).map_err(|_| "Git diff output is not UTF-8.")?;
    let separator = format!("{title}\n");
    if text
        .len()
        .saturating_add(separator.len())
        .saturating_add(output.len())
        > MAX_DIFF_BYTES
    {
        return Err(too_large());
    }
    text.push_str(&separator);
    text.push_str(output);
    if !text.ends_with('\n') {
        if text.len() == MAX_DIFF_BYTES {
            return Err(too_large());
        }
        text.push('\n');
    }
    Ok(())
}

async fn filter_overrides(root: &Path) -> Result<Vec<(String, String)>, String> {
    let (code, keys) = git(
        root,
        &[
            "config",
            "--null",
            "--name-only",
            "--get-regexp",
            r"^filter\..*\.(clean|process)$",
        ],
        &[],
        MAX_DIFF_BYTES,
        true,
    )
    .await?;
    if code == 1 {
        return Ok(Vec::new());
    }
    let mut drivers = std::collections::BTreeSet::new();
    for key in keys.split(|byte| *byte == 0).filter(|key| !key.is_empty()) {
        let key = std::str::from_utf8(key).map_err(|_| "Git filter configuration is not UTF-8.")?;
        let driver = key
            .strip_suffix(".clean")
            .or_else(|| key.strip_suffix(".process"))
            .ok_or("Git filter configuration has an unexpected key.")?;
        if driver.len() > 4096 {
            return Err("Git filter key exceeds its byte bound.".to_owned());
        }
        drivers.insert(driver.to_owned());
        if drivers.len() > MAX_FILTER_DRIVERS {
            return Err("Git filter configuration exceeds 64 drivers.".to_owned());
        }
    }
    Ok(drivers
        .into_iter()
        .flat_map(|driver| {
            [
                (format!("{driver}.clean"), String::new()),
                (format!("{driver}.process"), String::new()),
                (format!("{driver}.required"), "false".to_owned()),
            ]
        })
        .collect())
}

async fn read(stream: impl AsyncRead + Unpin, cap: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    stream
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| format!("Git output read failed: {error}"))?;
    if bytes.len() > cap {
        return Err(if cap == MAX_DIFF_BYTES {
            too_large()
        } else {
            format!(
                "Git output exceeds its {cap}-byte read bound; inspect the repository with Git."
            )
        });
    }
    Ok(bytes)
}

async fn git(
    root: &Path,
    args: &[&str],
    filters: &[(String, String)],
    cap: usize,
    allow_one: bool,
) -> Result<(i32, Vec<u8>), String> {
    let mut command = tokio::process::Command::new("git");
    // Share the cleared native process environment. Git needs executable and
    // configuration roots so global ignore rules still apply; provider secrets
    // and directory/execution overrides stay out.
    command.env_clear();
    super::tools::allow_shell_environment(&mut command)?;
    for key in [
        "PATH",
        "HOME",
        "USERPROFILE",
        "XDG_CONFIG_HOME",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .current_dir(root)
        .args([
            "-c",
            "safe.bareRepository=explicit",
            "-c",
            "core.fsmonitor=false",
            "-c",
            &format!("core.hooksPath={NULL_DEVICE}"),
            "--no-pager",
        ])
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_COUNT", filters.len().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Env entries handle filter names containing '=' without argv ambiguity.
    for (index, (key, value)) in filters.iter().enumerate() {
        command
            .env(format!("GIT_CONFIG_KEY_{index}"), key)
            .env(format!("GIT_CONFIG_VALUE_{index}"), value);
    }
    let group = processkit::ProcessGroup::new()
        .map_err(|error| format!("Git process containment failed: {error}"))?;
    let mut child = group
        .spawn(command)
        .map_err(|error| format!("Git could not start: {error}"))?;
    let stdout = child.stdout.take().ok_or("Git stdout is absent.")?;
    let stderr = child.stderr.take().ok_or("Git stderr is absent.")?;
    let (out, err, status) = tokio::try_join!(read(stdout, cap), read(stderr, 16 * 1024), async {
        child
            .wait()
            .await
            .map_err(|error| format!("Git wait failed: {error}"))
    })?;
    let code = status
        .code()
        .ok_or("Git terminated without an exit code.")?;
    if code != 0 && !(allow_one && code == 1) {
        return Err(format!(
            "Git diff failed ({code}): {}",
            String::from_utf8_lossy(&err)
        ));
    }
    Ok((code, out))
}

#[cfg(test)]
mod tests;
