//! Running one approved built-in effect, and the bounded filesystem and shell
//! primitives underneath it.
//!
//! Split out of `tools.rs` under the workspace god-file ceiling. Deciding
//! WHETHER an effect may happen lives with the permission runner; this is what
//! happens once it may.

pub(super) mod developer_help;
pub(super) mod integration;
#[cfg(not(windows))]
mod unix_shell;
#[cfg(windows)]
pub(super) mod windows_shell;
#[cfg(all(not(windows), test))]
pub(super) use unix_shell::supervise as supervise_shell;

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use grapt_runtime::BoundedToolInput;
use tokio::io::{AsyncRead, AsyncReadExt};

// A private sibling of the module whose effects these are; the glob keeps the
// split a MOVE rather than an interface change.
use super::*;

pub(super) fn run_read(
    root: &Path,
    expected: &Path,
    input: &BoundedToolInput,
) -> Result<Vec<u8>, String> {
    let args: ReadArguments = decode_input(input)?;
    let target = resolve_existing(root, &args.path, false)?;
    if target != expected {
        return Err("read target changed after authorization".to_owned());
    }
    if target.is_dir() {
        return Err(format!("read refuses directory {}; use list", args.path));
    }
    let offset = args.offset.map_or(1, BoundedU64::get);
    let limit = args.limit.map_or(read_window::READ_LINES, BoundedU64::get);
    let mut rendered =
        read_window::page(&target, &args.path, offset, limit, FILE_EFFECT_MAX_BYTES)?;
    if offset == 1 {
        let scoped = crate::dogfood::instructions::scoped_for_read(root, &target);
        rendered.push_str(&scoped.render_text());
    }
    Ok(rendered.into_bytes())
}

#[cfg(test)]
pub(super) fn run_edit(
    root: &Path,
    expected: &Path,
    input: &BoundedToolInput,
) -> Result<Vec<u8>, String> {
    prepare_edit(root, expected, input)?.run(root, None)?.output
}

pub(super) fn prepare_edit(
    root: &Path,
    expected: &Path,
    input: &BoundedToolInput,
) -> Result<coding::file_effects::PreparedFile, String> {
    let args: EditArguments = decode_input(input)?;
    let target = resolve_existing(root, &args.path, true)?;
    if target != expected {
        return Err("edit target changed after authorization".to_owned());
    }
    let metadata =
        std::fs::metadata(&target).map_err(|error| format!("edit stat failed: {error}"))?;
    if metadata.len() > FILE_EFFECT_MAX_BYTES {
        return Err("edit target exceeds the product bound".to_owned());
    }
    let before = super::coding::read_bounded(&target, FILE_EFFECT_MAX_BYTES)
        .and_then(|bytes| String::from_utf8(bytes).map_err(|error| error.to_string()))
        .map_err(|error| format!("edit read failed or target is not UTF-8: {error}"))?;
    // read shows lines without their `\r`, so text copied from it names a
    // CRLF file's lines with `\n`. In a file that is CRLF throughout, such
    // text is matched and written in the file's own line endings; a file of
    // mixed endings takes the text exactly as given.
    let crlf_file =
        before.contains("\r\n") && before.matches('\n').count() == before.matches("\r\n").count();
    let in_file_endings = |text: &str| {
        if crlf_file && !text.contains('\r') {
            text.replace('\n', "\r\n")
        } else {
            text.to_owned()
        }
    };
    let old = if before.contains(args.old_text.as_str()) {
        args.old_text.as_str().to_owned()
    } else {
        in_file_endings(args.old_text.as_str())
    };
    let new_text = in_file_endings(&args.new_text);
    let old = old.as_str();
    let replace_all = args.replace_all.unwrap_or(false);
    let replacements = before.matches(old).count();
    if replacements == 0 {
        return Err(format!(
            "edit old_text not found in {}; reread the file and copy the exact text",
            args.path
        ));
    }
    if replacements > 1 && !replace_all {
        return Err(format!(
            "edit old_text occurs {replacements} times in {}; add surrounding lines to make it unique or set replace_all=true",
            args.path
        ));
    }
    // Size the result before building it: replace_all multiplies new_text.
    let after_len = replacements
        .checked_mul(new_text.len())
        .and_then(|added| (before.len() - replacements * old.len()).checked_add(added));
    if after_len.is_none_or(|length| length as u64 > FILE_EFFECT_MAX_BYTES) {
        return Err(format!(
            "edited {} would exceed the {FILE_EFFECT_MAX_BYTES}-byte file bound",
            args.path
        ));
    }
    let after = before.replace(old, &new_text);
    let file = edit_file_metadata(&args.path, &before, old, &new_text)?;
    let output = serde_json::json!({
        "path": path_text(&target),
        "beforeBytes": before.len(),
        "afterBytes": after.len(),
        "exactReplacements": replacements,
        "files": [file],
    })
    .to_string()
    .into_bytes();
    Ok(coding::file_effects::PreparedFile::edit(
        target,
        args.path,
        before.into_bytes(),
        after.into_bytes(),
        output,
    ))
}

pub(super) fn edit_file_metadata(
    path: &str,
    before: &str,
    old: &str,
    new: &str,
) -> Result<serde_json::Value, String> {
    // run_edit proves at least one old-text match and replaces every one it
    // writes; a single hunk from the first to the last occurrence describes it.
    let start = before
        .find(old)
        .ok_or("edit receipt lost its exact match")?;
    let end = before
        .rfind(old)
        .ok_or("edit receipt lost its exact match")?
        + old.len();
    let line_start = before[..start].rfind('\n').map_or(0, |index| index + 1);
    let line_end = before[end..]
        .find('\n')
        .map_or(before.len(), |index| end + index + 1);
    let removed = &before[line_start..line_end];
    let added = format!(
        "{}{}{}",
        &before[line_start..start],
        before[start..end].replace(old, new),
        &before[end..line_end]
    );
    let first_line = before[..line_start]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1;
    let deletions = removed.lines().count();
    let additions = added.lines().count();
    // A display diff is bounded independently of the governed file effect.
    // Its absence is explicit; the durable edit result still names the file.
    let omitted = removed.len().saturating_add(added.len()) > 16 * 1024;
    let mut patch = String::new();
    if !omitted {
        patch = format!(
            "--- {path}\n+++ {path}\n@@ -{first_line},{deletions} +{first_line},{additions} @@\n"
        );
        for (sign, value) in [('-', removed), ('+', added.as_str())] {
            for line in value.split_inclusive('\n') {
                patch.push(sign);
                patch.push_str(line);
                if !line.ends_with('\n') {
                    patch.push_str("\n\\ No newline at end of file\n");
                }
            }
        }
    }
    Ok(
        serde_json::json!({"type":"update","relativePath":path,"filePath":path,
        "patch":patch,"additions":additions,"deletions":deletions,"patchOmitted":omitted}),
    )
}

#[cfg(test)]
pub(super) async fn run_shell(
    root: &Path,
    expected: &str,
    program: &Path,
    environment: &shell_environment::Environment,
    input: &BoundedToolInput,
    credentials: &[String],
    observed_ms: Option<u64>,
) -> Result<ShellOutcome, String> {
    let (_sender, cancellation) = ToolCancellation::channel();
    run_shell_cancellable(
        root,
        expected,
        program,
        environment,
        input,
        credentials,
        ShellControl {
            observed_ms,
            cancellation,
            cleanup: ShellCleanup::default(),
        },
    )
    .await
}

pub(in crate::dogfood) struct ShellControl {
    pub(in crate::dogfood) observed_ms: Option<u64>,
    pub(in crate::dogfood) cancellation: ToolCancellation,
    pub(in crate::dogfood) cleanup: ShellCleanup,
}

/// Separate process settlement from the command's success or cancellation.
#[derive(Clone)]
pub(in crate::dogfood) struct ShellCleanup(Arc<Mutex<Option<Result<(), String>>>>);

impl Default for ShellCleanup {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(Some(Ok(())))))
    }
}

impl ShellCleanup {
    pub(in crate::dogfood) fn started(&self) {
        *lock(&self.0) = None;
    }

    pub(in crate::dogfood) fn settled(&self, result: Result<(), String>) {
        *lock(&self.0) = Some(result);
    }

    pub(in crate::dogfood) fn pending(&self) -> bool {
        lock(&self.0).is_none()
    }

    pub(in crate::dogfood) fn result(&self) -> Result<(), String> {
        lock(&self.0)
            .clone()
            .unwrap_or_else(|| Err("shell cleanup has not settled".into()))
    }

    pub(in crate::dogfood) async fn failed_spawn(
        &self,
        group: &processkit::ProcessGroup,
        error: &processkit::Error,
    ) -> Result<(), String> {
        let cleanup = async {
            let terminated = group.kill_all().map_err(|error| {
                format!("failed launch group termination failed: {error}")
            });
            let deadline = tokio::time::Instant::now() + terminal::STOP_SETTLE_BOUND;
            loop {
                let empty = match group.mechanism() {
                    processkit::Mechanism::CgroupV2 | processkit::Mechanism::ProcessReaper => group
                        .members()
                        .map_err(|error| format!("failed launch tree census failed: {error}"))?
                        .is_empty(),
                    #[cfg(windows)]
                    processkit::Mechanism::JobObject => group
                        .members_info()
                        .map_err(|error| format!("failed launch job census failed: {error}"))?
                        .is_empty(),
                    processkit::Mechanism::ProcessGroup => {
                        // A missing executable fails before user code runs, and
                        // POSIX spawn waits for the failed child. Only then can
                        // this fresh group's empty tracked set witness no work.
                        if !matches!(
                            error.reason(),
                            processkit::ErrorReason::Spawn { source, .. }
                                if source.kind() == std::io::ErrorKind::NotFound
                        ) {
                            return Err("failed launch has no reserved group leader; cleanup is unconfirmed".into());
                        }
                        group
                            .members()
                            .map_err(|error| format!("failed launch group census failed: {error}"))?
                            .is_empty()
                    }
                    _ => return Err("failed launch has no supported group exit witness; cleanup is unconfirmed".into()),
                };
                if empty {
                    return terminated;
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err("failed launch group did not exit within the cleanup bound".into());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        .await;
        self.settled(cleanup.clone());
        cleanup
    }
}

/// The session shell owner retains this future through cleanup receipt capture.
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) async fn run_shell_cancellable(
    root: &Path,
    expected: &str,
    program: &Path,
    environment: &shell_environment::Environment,
    input: &BoundedToolInput,
    credentials: &[String],
    control: ShellControl,
) -> Result<ShellOutcome, String> {
    if control.cancellation.cancelled_by().is_some() {
        return Err("shell cancelled before launch; no command ran".into());
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    if environment.sandbox().is_some() {
        return Err(sandbox::refusal(
            &sandbox::degraded("MXC native process containment requires Windows"),
            "sandbox unavailable; no command ran",
        ));
    }
    let args: ShellArguments = decode_input(input)?;
    let _ = args.description;
    if args.command.as_str() != expected {
        return Err("shell argv changed after authorization".to_owned());
    }
    let timeout_ms = args
        .timeout_ms
        .map(BoundedU64::get)
        .unwrap_or(SHELL_DEFAULT_TIMEOUT_MS);
    #[cfg(not(windows))]
    let mut outcome = {
        let mut command = tokio::process::Command::new(program);
        command
            .args(shell_arguments(args.command.as_str()))
            .current_dir(root)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .envs(environment.iter());
        #[cfg(target_os = "linux")]
        let pending = if let Some(plan) = environment.sandbox() {
            use sandbox::posix::{command_line, Pending};
            let arguments = shell_arguments(args.command.as_str())
                .into_iter()
                .map(Into::into)
                .collect::<Vec<_>>();
            let line = command_line(program.as_os_str(), &arguments)?;
            let pending = Pending::prepare(plan, line, root.into(), environment, timeout_ms)?;
            // The installed helper starts before isolation: never load a
            // dynamic linker or shell hook from the command environment here.
            command = tokio::process::Command::new(
                std::env::current_exe()
                    .map_err(|error| format!("sandbox helper unavailable: {error}"))?,
            );
            command
                .args(pending.arguments())
                .current_dir(root)
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            Some(pending)
        } else {
            None
        };
        let group = processkit::ProcessGroup::new().map_err(|error| error.to_string());
        unix_shell::supervise_prepared(
            command,
            timeout_ms,
            group,
            credentials,
            #[cfg(target_os = "linux")]
            pending,
            Some(control),
        )
        .await?
    };
    #[cfg(windows)]
    let mut outcome = {
        let mut command = if let Some(launch) = environment.msys_launch() {
            let mut command = windows_spawn::Command::new(terminal::launch_path(&launch.program)?);
            command.args(&launch.arguments);
            command
        } else {
            let mut command = windows_spawn::Command::new(terminal::launch_path(program)?);
            // cmd /S removes the framing pair; preserve approved text verbatim.
            command
                .args(["/D", "/S", "/C"])
                .raw_arg(format!("\"{}\"", args.command.as_str()));
            command
        };
        command
            .current_dir(terminal::shell_cwd(root, true)?)
            .env_clear()
            .envs(environment.iter());
        let pending = if let Some(plan) = environment.sandbox() {
            use sandbox::windows::{quote, Pending};
            let line = format!(
                "{} /D /S /C \"{}\"",
                quote(terminal::launch_path(program)?.as_os_str())?,
                args.command.as_str()
            );
            let pending = Pending::prepare(
                plan,
                line,
                terminal::shell_cwd(root, true)?,
                environment,
                timeout_ms,
            )?;
            command = windows_spawn::Command::new(
                std::env::current_exe().map_err(|e| format!("sandbox helper unavailable: {e}"))?,
            );
            command
                .args(pending.arguments())
                .current_dir(terminal::shell_cwd(root, true)?)
                .env_clear()
                .envs(environment.iter());
            Some(pending)
        } else {
            None
        };
        windows_shell::supervise_prepared(command, pending, timeout_ms, credentials, Some(control))
            .await?
    };
    if !environment.msys_executables().is_empty() {
        // This effect is reachable only through the separately logged MSYS
        // permission and the command's ordinary authorization.
        outcome.enforcement = environment.intended();
    }
    outcome
        .notices
        .extend(environment.sandbox_notes(&outcome.err.bytes));
    if outcome.status.code().is_some_and(|code| code != 0) {
        outcome.notices.extend(developer_help::failures(
            &outcome.out.bytes,
            &outcome.err.bytes,
        ));
    }
    Ok(outcome)
}

/// How long output keeps draining after the command ends or is killed.
pub(super) const SHELL_DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Closing the owner channel also cancels; an absent channel is test-only.
async fn shell_cancelled(control: &mut Option<ShellControl>) {
    match control {
        Some(control) if control.cancellation.cancelled_by().is_some() => {}
        Some(control) => {
            let _source = control.cancellation.changed().await;
        }
        None => std::future::pending().await,
    }
}

struct ShellWait {
    exit: Option<std::io::Result<std::process::ExitStatus>>,
    cleanup: Result<(), String>,
    cancelled: bool,
    receipt: ShellCleanup,
}

/// Capture the same bounded output and drain grace for both native spawners.
async fn collect_shell(
    stdout: impl AsyncRead + Unpin,
    stderr: impl AsyncRead + Unpin,
    waited: impl std::future::Future<Output = ShellWait>,
    credentials: &[String],
    notices: Vec<String>,
    timeout_ms: u64,
) -> Result<ShellOutcome, String> {
    let mut out = BoundedStream::default();
    let mut err = BoundedStream::default();
    let mut stderr_tail = native_failure::Stderr::new(credentials);
    let ended = tokio::sync::Notify::new();
    let waited = async {
        let result = waited.await;
        ended.notify_one();
        result
    };
    // A survivor outside the tree can hold the pipes open forever; draining
    // stops a grace period after the command ends and keeps what it read.
    let drained = async {
        let drains = async {
            let (out, err) = tokio::join!(
                drain_into(stdout, &mut out, None),
                drain_into(stderr, &mut err, Some(&mut stderr_tail))
            );
            out.and(err)
        };
        let grace = async {
            ended.notified().await;
            tokio::time::sleep(SHELL_DRAIN_GRACE).await;
        };
        tokio::select! {
            drained = drains => Some(drained),
            () = grace => None,
        }
    };
    let (waited, drained) = tokio::join!(waited, drained);
    let output_cleanup = match &drained {
        Some(Ok(())) => Ok(()),
        Some(Err(error)) => Err(format!("shell output drain failed: {error}")),
        None => Err("shell output pipes did not drain within the cleanup grace period".into()),
    };
    waited
        .receipt
        .settled(waited.cleanup.clone().and(output_cleanup));
    let captured = conclude_shell(
        ShellRun {
            stderr_tail: Some(stderr_tail.text()),
            exit: waited.exit,
            cleanup: waited.cleanup,
            drained,
            out,
            err,
        },
        notices,
        timeout_ms,
    );
    if waited.cancelled {
        return Err(match captured {
            Ok(outcome) => format!(
                "shell cancelled; root exit observed after termination was requested; captured output: {}",
                String::from_utf8_lossy(&outcome.model_output()?)
            ),
            Err(reason) => format!("shell cancelled; {reason}"),
        });
    }
    captured
}

/// What one supervised shell run observed.
pub(super) struct ShellRun {
    pub(super) stderr_tail: Option<String>,
    /// `None` when the command timed out.
    pub(super) exit: Option<std::io::Result<std::process::ExitStatus>>,
    pub(super) cleanup: Result<(), String>,
    /// `None` when draining was cut off after the grace period.
    pub(super) drained: Option<std::io::Result<()>>,
    pub(super) out: BoundedStream,
    pub(super) err: BoundedStream,
}

/// The tool result for a run, disclosing whatever supervision could not guarantee.
pub(super) fn conclude_shell(
    run: ShellRun,
    mut notices: Vec<String>,
    timeout_ms: u64,
) -> Result<ShellOutcome, String> {
    let ShellRun {
        stderr_tail,
        exit,
        cleanup,
        drained,
        out,
        err,
    } = run;
    let output_cleanup = match &drained {
        Some(Ok(())) => Ok(()),
        Some(Err(error)) => Err(format!("shell output drain failed: {error}")),
        None => Err("shell output pipes did not drain within the cleanup grace period".into()),
    };
    match drained {
        Some(drained) => drained.map_err(|error| format!("shell output read failed: {error}"))?,
        None => notices.push(format!(
            "output collection stopped {} ms after the command ended because its output pipes were still open",
            SHELL_DRAIN_GRACE.as_millis()
        )),
    }
    if let Err(error) = &cleanup {
        notices.push(format!(
            "killing the command's processes failed ({error}); some may still be running"
        ));
    }
    let Some(exit) = exit else {
        let stderr =
            stderr_tail.unwrap_or_else(|| String::from_utf8_lossy(&err.bytes).into_owned());
        let output = render_shell_output_with_suffix(out, err, &notice_suffix(&notices))?;
        return Err(format!(
            "shell timed out after {timeout_ms} ms; captured output: {}\n[stderr]\n{stderr}",
            String::from_utf8_lossy(&output)
        ));
    };
    let status = exit.map_err(|error| format!("shell wait failed: {error}"))?;
    Ok(ShellOutcome {
        enforcement: RealizedEnforcement::HarnessOnly,
        stderr_tail,
        status,
        cleanup: cleanup.and(output_cleanup),
        out,
        err,
        notices,
    })
}

/// One observed exit and its bounded captures, before consumer rendering.
pub(super) struct ShellOutcome {
    pub enforcement: RealizedEnforcement,
    stderr_tail: Option<String>,
    status: std::process::ExitStatus,
    cleanup: Result<(), String>,
    out: BoundedStream,
    err: BoundedStream,
    // Supervision is product metadata, never bytes produced by the command.
    notices: Vec<String>,
}

impl ShellOutcome {
    /// Model formats currently carry one text value per tool result. Keep the
    /// command text first and the oracle's status in its own final paragraph.
    pub(super) fn model_output(self) -> Result<Vec<u8>, String> {
        let status = match self.status.code() {
            Some(code) => format!("Command exited with code {code}."),
            None => format!("Command terminated by signal ({}).", self.status),
        };
        let mut suffix = notice_suffix(&self.notices);
        suffix.push_str(&format!("\n\n{status}"));
        render_shell_output_with_suffix(self.out, self.err, &suffix)
    }

    /// Tool failures can publish diagnostics without changing machine refusals.
    pub(super) fn tool_command_text(mut self) -> Result<Vec<u8>, String> {
        let stderr = self
            .stderr_tail
            .take()
            .unwrap_or_else(|| String::from_utf8_lossy(&self.err.bytes).into_owned());
        self.command_text()
            .map_err(|error| format!("{error}\n[stderr]\n{stderr}"))
    }

    /// Machine consumers, including command interpolation, receive command
    /// bytes before model rendering and retain their nonzero-exit refusal.
    pub(super) fn command_text(self) -> Result<Vec<u8>, String> {
        let output = render_shell_output(self.out, self.err)?;
        let result = if self.status.success() {
            Ok(())
        } else {
            Err(format!("shell exited with {}", self.status))
        };
        match (result, self.cleanup) {
            (Ok(()), Ok(())) => Ok(output),
            (result, Err(cleanup)) => Err(format!(
                "{}shell cleanup unconfirmed: {cleanup}; captured output: {}",
                result
                    .err()
                    .map_or(String::new(), |reason| format!("{reason}; ")),
                String::from_utf8_lossy(&output)
            )),
            (Err(reason), Ok(())) => Err(format!(
                "{reason}; captured output: {}",
                String::from_utf8_lossy(&output)
            )),
        }
    }
}

#[derive(Default)]
pub(super) struct BoundedStream {
    pub(super) bytes: Vec<u8>,
    pub(super) observed: u64,
}

fn stream_marker(
    name: &str,
    stream: &BoundedStream,
    kept: usize,
) -> Result<Option<String>, String> {
    let kept_u64 = match u64::try_from(kept) {
        Ok(kept) => kept,
        Err(_) => return Err("shell retained byte count exceeds u64".into()),
    };
    if stream.observed > kept_u64 {
        return Ok(Some(shell_marker_text(name, kept, stream.observed)));
    }
    Ok(None)
}

fn shell_marker_text(name: &str, kept: usize, observed: u64) -> String {
    format!("\n[{name} output bounded: kept {kept} of {observed} observed bytes]")
}

pub(super) fn render_shell_output(
    stdout: BoundedStream,
    stderr: BoundedStream,
) -> Result<Vec<u8>, String> {
    render_shell_output_with_suffix(stdout, stderr, "")
}

pub(super) const SHELL_OUTPUT_MAX_BYTES: usize = grapt_provider::MAX_CODEX_TOOL_TEXT_BYTES;

fn render_shell_output_with_suffix(
    stdout: BoundedStream,
    stderr: BoundedStream,
    suffix: &str,
) -> Result<Vec<u8>, String> {
    let streams = [("stdout", stdout), ("stderr", stderr)];
    let mut kept = [streams[0].1.bytes.len(), streams[1].1.bytes.len()];
    // Bound shell output itself at 1 MiB so PostToolUse hook annotations stay
    // inside `grapt_runtime::TOOL_ARTIFACT_MAX_BYTES`'s 64 KiB headroom.
    let bound = SHELL_OUTPUT_MAX_BYTES;
    let markers = [
        stream_marker(streams[0].0, &streams[0].1, kept[0])?,
        stream_marker(streams[1].0, &streams[1].1, kept[1])?,
    ];
    let total = kept
        .iter()
        .copied()
        .chain(
            markers
                .iter()
                .map(|marker| marker.as_ref().map_or(0, String::len)),
        )
        .chain(std::iter::once(suffix.len()))
        .try_fold(0usize, |sum, value| sum.checked_add(value))
        .ok_or("shell output projection length overflowed")?;
    if total > bound {
        let eligible = [
            kept[0] > 0 && (markers[0].is_some() || !suffix.is_empty()),
            kept[1] > 0 && (markers[1].is_some() || !suffix.is_empty()),
        ];
        // Reserve every marker clipping could introduce before clipping once.
        // Its observed count is fixed; its kept count only shrinks, so the
        // decimal width (and therefore the final marker) cannot grow.
        let overhead = (0..streams.len())
            .map(|index| {
                if eligible[index] {
                    shell_marker_text(streams[index].0, kept[index], streams[index].1.observed)
                        .len()
                } else {
                    markers[index].as_ref().map_or(0, String::len)
                }
            })
            .chain(std::iter::once(suffix.len()))
            .try_fold(0usize, |sum, value| sum.checked_add(value))
            .ok_or("shell output marker length overflowed")?;
        let available = bound
            .checked_sub(overhead)
            .ok_or("shell output markers exceeded the artifact bound")?;
        let mut overflow = kept.iter().sum::<usize>().saturating_sub(available);
        let mut remaining_streams = eligible.iter().filter(|eligible| **eligible).count();
        for index in 0..streams.len() {
            if !eligible[index] {
                continue;
            }
            let reduction = kept[index].min(overflow.div_ceil(remaining_streams));
            kept[index] -= reduction;
            overflow -= reduction;
            remaining_streams -= 1;
        }
        if overflow != 0 {
            return Err("shell output could not fit its durable artifact bound".into());
        }
    }
    let mut output = Vec::with_capacity(total.min(bound));
    for (index, (name, stream)) in streams.iter().enumerate() {
        output.extend_from_slice(&stream.bytes[..kept[index]]);
        if let Some(marker) = stream_marker(name, stream, kept[index])? {
            output.extend_from_slice(marker.as_bytes());
        }
    }
    output.extend_from_slice(suffix.as_bytes());
    Ok(output)
}

/// Reads to end of stream into `stream`; if cancelled, what was read stays.
async fn drain_into<R: AsyncRead + Unpin>(
    mut reader: R,
    stream: &mut BoundedStream,
    mut diagnostic: Option<&mut native_failure::Stderr>,
) -> std::io::Result<()> {
    let mut buffer = [0_u8; 8 * 1_024];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            return Ok(());
        }
        if let Some(diagnostic) = diagnostic.as_mut() {
            diagnostic.push(&buffer[..read]);
        }
        stream.observed = stream
            .observed
            .checked_add(
                u64::try_from(read)
                    .map_err(|_| std::io::Error::other("shell stream read size exceeds u64"))?,
            )
            .ok_or_else(|| std::io::Error::other("shell observed byte count overflowed"))?;
        let remaining = SHELL_STREAM_MAX_BYTES.saturating_sub(stream.bytes.len());
        stream
            .bytes
            .extend_from_slice(&buffer[..read.min(remaining)]);
    }
}

/// What process containment cannot promise on this host, said rather than assumed.
#[cfg(not(windows))]
pub(super) fn containment_notice(mechanism: processkit::Mechanism) -> Option<&'static str> {
    match mechanism {
        processkit::Mechanism::ProcessGroup => Some(
            "this host has no cgroup containment; processes that leave the command's process group (setsid, daemons) may outlive it",
        ),
        _ => None,
    }
}

/// Notices occupy separate model-facing paragraphs, sharing the artifact bound.
fn notice_suffix(notices: &[String]) -> String {
    notices
        .iter()
        .map(|notice| {
            if notice.starts_with("sandbox: ") {
                format!("\n\n[{notice}]")
            } else {
                format!("\n\n[shell: {notice}]")
            }
        })
        .collect()
}

pub(super) fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(windows)]
pub(super) fn shell_program() -> Result<PathBuf, String> {
    let root = std::env::var_os("SystemRoot")
        .ok_or_else(|| "SystemRoot is unavailable for the shell adapter".to_owned())?;
    std::fs::canonicalize(PathBuf::from(root).join("System32/cmd.exe"))
        .map_err(|error| format!("Windows shell could not be resolved: {error}"))
}

#[cfg(not(windows))]
pub(super) fn shell_program() -> Result<PathBuf, String> {
    std::fs::canonicalize("/bin/sh")
        .map_err(|error| format!("POSIX shell could not be resolved: {error}"))
}

#[cfg(not(windows))]
pub(super) fn shell_arguments(command: &str) -> Vec<&str> {
    vec!["-c", command]
}

#[cfg(windows)]
pub(super) fn isolated_environment_names() -> Vec<String> {
    vec!["SystemRoot".to_owned()]
}

#[cfg(not(windows))]
pub(super) fn isolated_environment_names() -> Vec<String> {
    Vec::new()
}

#[cfg(windows)]
pub(in crate::dogfood) fn allow_shell_environment(
    command: &mut tokio::process::Command,
) -> Result<(), String> {
    let root = std::env::var_os("SystemRoot")
        .ok_or_else(|| "SystemRoot is unavailable for the shell adapter".to_owned())?;
    command.env("SystemRoot", root);
    Ok(())
}

#[cfg(not(windows))]
pub(in crate::dogfood) fn allow_shell_environment(
    _command: &mut tokio::process::Command,
) -> Result<(), String> {
    Ok(())
}

pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Prepare the exact-argv effect for one plugin integration command method.
pub(super) fn prepare_integration_command(
    root: &Path,
    input: &BoundedToolInput,
) -> Result<PreparedEffect, String> {
    const MAX_PARTS: usize = 32;
    const MAX_PART_BYTES: usize = 4096;
    let value: serde_json::Value = serde_json::from_slice(input.as_bytes())
        .map_err(|error| format!("integration command input is not JSON: {error}"))?;
    let argv = value
        .get("argv")
        .and_then(serde_json::Value::as_array)
        .filter(|parts| !parts.is_empty() && parts.len() <= MAX_PARTS)
        .ok_or_else(|| format!("integration command argv must hold 1..={MAX_PARTS} parts"))?
        .iter()
        .map(|part| {
            part.as_str()
                .filter(|part| !part.is_empty() && part.len() <= MAX_PART_BYTES)
                .map(str::to_owned)
                .ok_or_else(|| "integration command argv part is invalid".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let root_text = path_text(root);
    let program = ProgramIdentity::new(
        PathIdentity::canonical(argv[0].clone()),
        argv[1..].to_vec(),
        PathIdentity::canonical(root_text.clone()),
        isolated_environment_names(),
    );
    let resolved = ResolvedCapability::new(
        EffectClass::ProcessSpawn,
        Some(program),
        FilesystemAuthority::new(
            vec![PathIdentity::canonical(root_text.clone())],
            vec![PathIdentity::canonical(root_text)],
            protected_paths(root),
        ),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    );
    Ok(PreparedEffect {
        capability: CapabilityId::derive(&resolved),
        resource: argv.join(" "),
        target: EffectTarget::IntegrationCommand { argv },
        action: SHELL_TOOL.to_owned(),
    })
}
