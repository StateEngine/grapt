// Copyright 2025 OpenAI
// SPDX-License-Identifier: Apache-2.0
// Modified by Grapt: bounded async output and execution, existing dependencies only.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;

const MAX_OUTPUT: usize = 65_536;
const MAX_INDEX_FLAG_OUTPUT: usize = 4 * 1024 * 1024;
const COMMAND_ALLOWANCE: Duration = Duration::from_secs(120);
const CLEANUP_ALLOWANCE: Duration = Duration::from_secs(5);

fn command(cwd: &Path) -> Command {
    let mut command = Command::new("git");
    // Select this repository from cwd rather than inheriting a wrapper's selectors.
    for name in [
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_CONFIG",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
        "GIT_OBJECT_DIRECTORY",
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_IMPLICIT_WORK_TREE",
        "GIT_GRAFT_FILE",
        "GIT_INDEX_FILE",
        "GIT_NO_REPLACE_OBJECTS",
        "GIT_REPLACE_REF_BASE",
        "GIT_PREFIX",
        "GIT_SHALLOW_FILE",
        "GIT_COMMON_DIR",
    ] {
        command.env_remove(name);
    }
    command
        .current_dir(cwd)
        .args(["-c", "safe.bareRepository=explicit", "-c"])
        .arg(if cfg!(windows) {
            "core.hooksPath=NUL"
        } else {
            "core.hooksPath=/dev/null"
        })
        .args([
            "-c",
            "core.fsmonitor=",
            "-c",
            "attr.tree=",
            "-c",
            "core.attributesFile=",
            "-c",
            "maintenance.auto=false",
            "-c",
            "gc.auto=0",
            "-c",
            "submodule.recurse=false",
            "-c",
            "merge.autoStash=false",
            "-c",
            "merge.verifySignatures=false",
            "-c",
            "commit.gpgSign=false",
        ])
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

pub(super) async fn git_output<S: AsRef<OsStr>>(
    cwd: &Path,
    args: &[S],
    checkout: bool,
) -> Result<Vec<u8>, String> {
    let mut command = prepare(cwd, args, checkout).await?;
    successful_output(&mut command).await
}

pub(super) async fn git_mutation<S: AsRef<OsStr>>(
    cwd: &Path,
    args: &[S],
) -> Result<Vec<u8>, crate::WorktreeError> {
    // Filter discovery only reads configuration. It must not be reported as a
    // mutation attempt if it fails before the requested command can start.
    let mut command = prepare(cwd, args, true)
        .await
        .map_err(crate::WorktreeError::NotStarted)?;
    successful_output(&mut command)
        .await
        .map_err(crate::WorktreeError::MayHaveChanged)
}

async fn prepare<S: AsRef<OsStr>>(
    cwd: &Path,
    args: &[S],
    checkout: bool,
) -> Result<Command, String> {
    let mut command = command(cwd);
    if checkout {
        let filters = configured_filters(cwd).await?;
        let mut count = 0;
        for filter in filters {
            for (attribute, value) in [
                ("process", ""),
                ("clean", ""),
                ("smudge", ""),
                ("required", "false"),
            ] {
                command.env(
                    format!("GIT_CONFIG_KEY_{count}"),
                    format!("filter.{filter}.{attribute}"),
                );
                command.env(format!("GIT_CONFIG_VALUE_{count}"), value);
                count += 1;
            }
        }
        command.env("GIT_CONFIG_COUNT", count.to_string());
    }
    command.args(args);
    Ok(command)
}

async fn successful_output(command: &mut Command) -> Result<Vec<u8>, String> {
    let (status, stdout, stderr) = execute(command).await?;
    if !status.success() {
        return Err(format!(
            "Git command failed: {}",
            String::from_utf8_lossy(&stderr).trim()
        ));
    }
    Ok(stdout)
}

pub(super) async fn git_path(cwd: &Path, args: &[&str]) -> Result<PathBuf, String> {
    let output = git_output(cwd, args, false).await?;
    let bytes = output.strip_suffix(b"\n").unwrap_or(&output);
    #[cfg(windows)]
    let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
    path_from_bytes(bytes)
}

pub(super) async fn hidden_index_entries(cwd: &Path) -> Result<bool, String> {
    let stdout = index_listing(cwd).await?;
    for entry in stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        // -v lowercases tags for assume-unchanged; S/s is skip-worktree.
        if entry[0].is_ascii_lowercase() || entry[0] == b'S' {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) async fn tracked_paths(cwd: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let stdout = index_listing(cwd).await?;
    let mut paths = BTreeSet::new();
    for (number, entry) in stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .enumerate()
    {
        if number >= 65_536 {
            return Err("tracked-path inspection exceeds 65536 entries; refusing removal".into());
        }
        if entry[0].is_ascii_lowercase() || entry[0] == b'S' {
            return Err("index flags changed or hide local edits; clear assume-unchanged/skip-worktree before retrying".into());
        }
        let path = path_from_bytes(&entry[2..])?;
        if path.as_os_str().as_encoded_bytes().len() > 4096
            || path.components().count() > 128
            || path
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(
                "tracked index path exceeds inspection bounds or escapes the checkout".into(),
            );
        }
        paths.insert(path);
    }
    Ok(paths)
}

pub(super) async fn path_is_ignored(cwd: &Path, path: &Path) -> Result<bool, String> {
    let mut command = command(cwd);
    command.args([
        OsStr::new("--work-tree=."),
        OsStr::new("check-ignore"),
        OsStr::new("--no-index"),
        OsStr::new("--quiet"),
        OsStr::new("--"),
        path.as_os_str(),
    ]);
    let (status, _, stderr) = execute(&mut command).await?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(format!(
            "cannot classify retained path with Git check-ignore: {}",
            String::from_utf8_lossy(&stderr).trim()
        )),
    }
}

async fn index_listing(cwd: &Path) -> Result<Vec<u8>, String> {
    let mut command = command(cwd);
    command.args(["--work-tree=.", "ls-files", "-v", "-z"]);
    // Unlike ordinary command output, this contains every tracked path. Keep
    // the larger allowance local to index inspection; stderr retains 64 KiB.
    let (status, stdout, stderr) =
        execute_with_stdout_limit(&mut command, MAX_INDEX_FLAG_OUTPUT)
            .await
            .map_err(|e| format!("cannot inspect all tracked index flags safely (4 MiB limit); inspect the checkout with Git before retrying: {e}"))?;
    if !status.success() {
        return Err(format!(
            "cannot inspect index flags: {}",
            String::from_utf8_lossy(&stderr).trim()
        ));
    }
    if stdout.is_empty() {
        return Ok(stdout);
    }
    let records = stdout
        .strip_suffix(b"\0")
        .ok_or("Git returned an incomplete index flag listing; refusing operation")?;
    for entry in records.split(|byte| *byte == 0) {
        if entry.len() < 3
            || entry[1] != b' '
            || !matches!(
                entry[0].to_ascii_uppercase(),
                b'H' | b'S' | b'M' | b'R' | b'C' | b'K'
            )
        {
            return Err("Git returned a malformed index flag entry; refusing operation".into());
        }
    }
    Ok(stdout)
}

pub(super) fn path_from_bytes(bytes: &[u8]) -> Result<PathBuf, String> {
    if bytes.is_empty() {
        return Err("Git returned an empty path".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec())))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(bytes)
            .map(PathBuf::from)
            .map_err(|_| "Git path is not UTF-8".into())
    }
}

async fn configured_filters(cwd: &Path) -> Result<BTreeSet<String>, String> {
    let mut command = command(cwd);
    command.args([
        "config",
        "--null",
        "--name-only",
        "--get-regexp",
        "^filter\\.",
    ]);
    let (status, stdout, stderr) = execute(&mut command).await?;
    if status.code() == Some(1) {
        return Ok(BTreeSet::new());
    }
    if !status.success() {
        return Err(format!(
            "cannot inspect Git filters: {}",
            String::from_utf8_lossy(&stderr)
        ));
    }
    let names = std::str::from_utf8(&stdout).map_err(|_| "Git filter names are not UTF-8")?;
    let mut filters = BTreeSet::new();
    for name in names.split('\0') {
        if let Some((driver, attribute)) = name
            .strip_prefix("filter.")
            .and_then(|name| name.rsplit_once('.'))
        {
            if !driver.is_empty()
                && matches!(attribute, "clean" | "smudge" | "process" | "required")
            {
                filters.insert(driver.to_owned());
            }
        }
    }
    Ok(filters)
}

async fn execute(
    command: &mut Command,
) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), String> {
    execute_with_stdout_limit(command, MAX_OUTPUT).await
}

async fn execute_with_stdout_limit(
    command: &mut Command,
    stdout_limit: usize,
) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), String> {
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot start Git: {error}"))?;
    let stdout = child.stdout.take().ok_or("Git stdout is unavailable")?;
    let stderr = child.stderr.take().ok_or("Git stderr is unavailable")?;
    let io = async {
        let (status, stdout, stderr) = tokio::try_join!(
            child.wait(),
            read_with_limit(stdout, stdout_limit),
            read(stderr)
        )?;
        Ok::<_, std::io::Error>((status, stdout, stderr))
    };
    let error = match tokio::time::timeout(COMMAND_ALLOWANCE, io).await {
        Ok(Ok(result)) => return Ok(result),
        Ok(Err(error)) => format!("Git output could not be read: {error}"),
        Err(_) => "Git exceeded its 120 second allowance".to_owned(),
    };
    match tokio::time::timeout(CLEANUP_ALLOWANCE, child.kill()).await {
        Ok(Ok(())) => Err(error),
        Ok(Err(cleanup)) => Err(format!("{error}; Git cleanup failed: {cleanup}")),
        Err(_) => Err(format!(
            "{error}; Git cleanup exceeded its 5 second allowance"
        )),
    }
}

async fn read(stream: impl tokio::io::AsyncRead + Unpin) -> std::io::Result<Vec<u8>> {
    read_with_limit(stream, MAX_OUTPUT).await
}

async fn read_with_limit(
    stream: impl tokio::io::AsyncRead + Unpin,
    limit: usize,
) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    stream
        .take((limit + 1) as u64)
        .read_to_end(&mut output)
        .await?;
    if output.len() > limit {
        return Err(std::io::Error::other(format!(
            "Git output exceeds {limit} bytes"
        )));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn oversized_git_output_is_refused_instead_of_truncated_into_a_path() {
        let allowed = vec![b'x'; MAX_OUTPUT];
        assert_eq!(read(allowed.as_slice()).await.ok(), Some(allowed));
        let excess = vec![b'x'; MAX_OUTPUT + 1];
        assert!(read(excess.as_slice()).await.is_err());
        assert!(read_with_limit(excess.as_slice(), MAX_INDEX_FLAG_OUTPUT)
            .await
            .is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn oversized_child_output_stops_the_child_without_waiting_for_its_normal_exit() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "printf '%065537d' 0; exec sleep 60"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let result = tokio::time::timeout(Duration::from_secs(10), execute(&mut command)).await;
        match result {
            Ok(Err(error)) => assert!(error.contains("exceeds 65536"), "{error}"),
            other => panic!("oversized output did not stop the process: {other:?}"),
        }
    }
}
