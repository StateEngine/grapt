//! Bounded acquisition for a child process's stdout and stderr.
//!
//! Split out of `direct_include_format.rs` when that file crossed the
//! god-file ceiling at 1_061 lines against a 1_000 limit. The ratchet
//! refused the growth and was right to: this is a subsystem — a failure
//! vocabulary, a capture routine and the ignored-path policy built on it —
//! and it had been accreting inside a file about include formatting.

use super::*;

/// Why a bounded child capture failed.
///
/// Distinct VARIANTS rather than one string, because these are distinct
/// conditions with distinct cures: a missing git is an environment fault, a
/// nonzero exit is git refusing the request, and an overflow on one pipe says
/// which producer went unbounded. Collapsing them into a message means the
/// witnesses can only assert on prose, and prose is the thing most likely to be
/// reworded.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CaptureFailure {
    Spawn(String),
    StdoutPipeAbsent,
    StderrPipeAbsent,
    /// The child wrote more than the stdout cap and was terminated.
    StdoutOverflow {
        cap: usize,
    },
    /// The child wrote more than the stderr cap and was terminated.
    StderrOverflow {
        cap: usize,
    },
    /// The child outlived its poll budget and was killed and reaped.
    Timeout,
    Wait(String),
    Read(String),
    ThreadPanicked(&'static str),
    NonZeroExit {
        code: Option<i32>,
        stderr: String,
    },
    NotUtf8(String),
}

impl CaptureFailure {
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::Spawn(reason) => format!(
                "source inventory cannot determine ignored paths because git could not \
                 be started ({reason}); it refuses rather than counting build artifacts \
                 as source"
            ),
            Self::StdoutPipeAbsent => "git stdout pipe absent".to_owned(),
            Self::StderrPipeAbsent => "git stderr pipe absent".to_owned(),
            Self::StdoutOverflow { cap } => {
                format!("git stdout exceeded its {cap}-byte bounded pipe")
            }
            Self::StderrOverflow { cap } => {
                format!("git stderr exceeded its {cap}-byte bounded pipe")
            }
            Self::Timeout => "git timed out and was killed and waited".to_owned(),
            Self::Wait(reason) => format!("cannot wait for git: {reason}"),
            Self::Read(reason) => format!("git pipe read failed: {reason}"),
            Self::ThreadPanicked(stream) => format!("git {stream} thread panicked"),
            Self::NonZeroExit { code, stderr } => format!(
                "source inventory cannot determine ignored paths: git exited {code:?}: {stderr}"
            ),
            Self::NotUtf8(reason) => {
                format!("ignored-path listing was not UTF-8: {reason}")
            }
        }
    }
}

/// What a bounded capture yielded when it did not fail.
#[derive(Debug)]
pub(crate) struct BoundedCapture {
    pub(crate) code: Option<i32>,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

/// Run a child, capturing both pipes under ACQUISITION-TIME caps.
///
/// # The defect this replaces
///
/// The first version called `Command::output()` and then compared the returned
/// stdout length against a constant. `output()` reads BOTH pipes to end first,
/// so the allocation had already happened before anything was checked — and
/// stderr was never checked at all. That is a declared bound that is not a
/// resource bound, which is precisely the shape this crate's own transport
/// documentation condemns: "an implementation that read the whole body and then
/// measured it would satisfy the signature and violate the law." I wrote that
/// sentence and then shipped its opposite here.
///
/// # Why two threads and a poll loop
///
/// Both pipes are drained CONCURRENTLY because draining one to completion first
/// deadlocks the moment the child fills the other pipe's OS buffer.
///
/// And the drain threads are not simply joined: [`drain_pipe`] stops reading at
/// `cap + 1`, so a child that keeps producing then blocks forever writing into a
/// pipe nobody is emptying — joining would hang. The overflow flags are POLLED,
/// the child is killed the moment either trips, and only then are the threads
/// joined. This mirrors [`run_bounded`], which already solved the same problem
/// for the formatter; the timeout budget is deliberately the same pair of
/// constants rather than a second policy free to drift from it.
pub(crate) fn capture_bounded(
    mut command: Command,
    stdout_cap: usize,
    stderr_cap: usize,
) -> Result<BoundedCapture, CaptureFailure> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| CaptureFailure::Spawn(error.to_string()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or(CaptureFailure::StdoutPipeAbsent)?;
    let stderr = child
        .stderr
        .take()
        .ok_or(CaptureFailure::StderrPipeAbsent)?;

    // SEPARATE flags per pipe. One shared flag cannot say which producer went
    // unbounded, and "which" is the whole diagnostic value of the refusal.
    let out_over = Arc::new(AtomicBool::new(false));
    let err_over = Arc::new(AtomicBool::new(false));
    let out_flag = Arc::clone(&out_over);
    let err_flag = Arc::clone(&err_over);
    let out_thread = thread::spawn(move || drain_pipe(stdout, stdout_cap, out_flag));
    let err_thread = thread::spawn(move || drain_pipe(stderr, stderr_cap, err_flag));

    let mut status = None;
    let mut forced = None;
    for _ in 0..FORMATTER_POLLS {
        if out_over.load(Ordering::SeqCst) || err_over.load(Ordering::SeqCst) {
            let _ = child.kill();
            status = Some(
                child
                    .wait()
                    .map_err(|error| CaptureFailure::Wait(error.to_string()))?,
            );
            break;
        }
        status = child
            .try_wait()
            .map_err(|error| CaptureFailure::Wait(error.to_string()))?;
        if status.is_some() {
            break;
        }
        thread::sleep(FORMATTER_POLL_DELAY);
    }
    let status = match status {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let status = child
                .wait()
                .map_err(|error| CaptureFailure::Wait(error.to_string()))?;
            forced = Some(CaptureFailure::Timeout);
            status
        }
    };

    // Joined only AFTER the child is reaped, so neither thread can be blocked on
    // a pipe belonging to a live process.
    let stdout = out_thread
        .join()
        .map_err(|_| CaptureFailure::ThreadPanicked("stdout"))?
        .map_err(CaptureFailure::Read)?;
    let stderr = err_thread
        .join()
        .map_err(|_| CaptureFailure::ThreadPanicked("stderr"))?
        .map_err(CaptureFailure::Read)?;

    // Overflow outranks the timeout: an overflowing child is killed by the loop
    // above, and reporting that as a timeout would name the wrong cause.
    if out_over.load(Ordering::SeqCst) {
        return Err(CaptureFailure::StdoutOverflow { cap: stdout_cap });
    }
    if err_over.load(Ordering::SeqCst) {
        return Err(CaptureFailure::StderrOverflow { cap: stderr_cap });
    }
    if let Some(failure) = forced {
        return Err(failure);
    }
    Ok(BoundedCapture {
        code: status.code(),
        stdout,
        stderr,
    })
}

/// The exact command whose ignored listing this harness consumes.
///
/// Built in one place so a witness drives the SAME command the policy does. A
/// witness that assembled its own would be proving a bound on a command nobody
/// runs.
pub(crate) fn ignored_listing_command(root: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(root).args([
        "ls-files",
        "--others",
        "--ignored",
        "--exclude-standard",
        "--directory",
        "--",
        "crates",
    ]);
    command
}

/// Every path under `crates/` that git ignores, as normalized relatives.
///
/// # Why this exists
///
/// This inventory walks the FILESYSTEM, which is deliberate: a git-only census
/// cannot see an untracked file that is present, and seeing those is half the
/// point of the pin. But it also means every ignored BUILD ARTIFACT lands inside
/// the count. Running the `grapt-client` mutation runner's own unit suite creates
/// `crates/grapt-client/mutations/__pycache__/`, which is ignored, so
/// `git status` reads clean while this walk counts one extra directory and two
/// extra files — and the gate flips from pass to fail with HEAD and every
/// tracked byte unchanged.
///
/// That made the pin depend on whether the machine had happened to run a Python
/// test. It was first "handled" by a comment telling the next walker to clean up
/// first. A comment requiring manual cleanup is not an invariant, and review was
/// right to refuse it.
///
/// # Why git decides, and not a name list
///
/// The obvious fix is to skip `__pycache__`. That is the same unbounded
/// ban-list mistake this campaign has now made twice: `target/`,
/// `.pytest_cache/`, `*.pyc` somewhere else, and the next tool's cache are all
/// the same defect one spelling over. Ignoredness has exactly one authority —
/// `.gitignore`, as git resolves it — so this ASKS THAT AUTHORITY instead of
/// enumerating spellings. A new ignored artifact type needs no edit here.
///
/// # Honest failure
///
/// If git cannot answer, this REFUSES. It does not fall back to counting
/// everything: that would silently restore the exact non-determinism the
/// function exists to remove, and it would do so on precisely the machines
/// where something is already wrong.
pub(crate) fn ignored_relatives(root: &Path) -> Result<BTreeSet<String>, String> {
    let capture = capture_bounded(
        ignored_listing_command(root),
        MAX_IGNORED_LISTING_BYTES,
        MAX_IGNORED_STDERR_BYTES,
    )
    .map_err(|failure| failure.describe())?;
    if capture.code != Some(0) {
        return Err(CaptureFailure::NonZeroExit {
            code: capture.code,
            stderr: String::from_utf8_lossy(&capture.stderr).trim().to_owned(),
        }
        .describe());
    }
    let listing = String::from_utf8(capture.stdout)
        .map_err(|error| CaptureFailure::NotUtf8(error.to_string()).describe())?;
    Ok(listing
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .map(|line| line.trim_end_matches('/').to_owned())
        .collect())
}

/// Whether one relative path is, or sits beneath, an ignored path.
///
/// Prefix-matched on SEGMENT boundaries. `git ls-files --directory` collapses a
/// wholly-ignored directory to one entry, so its descendants never appear in the
/// listing and must be matched by prefix — but matching raw prefixes would make
/// an ignored `foo` swallow a tracked `foobar`.
pub(crate) fn is_ignored(relative: &str, ignored: &BTreeSet<String>) -> bool {
    if ignored.contains(relative) {
        return true;
    }
    ignored.iter().any(|prefix| {
        relative.starts_with(prefix) && relative.as_bytes().get(prefix.len()) == Some(&b'/')
    })
}

pub(crate) fn drain_pipe<R: Read>(
    reader: R,
    cap: usize,
    oversized: Arc<AtomicBool>,
) -> Result<Vec<u8>, String> {
    let take = u64::try_from(cap)
        .map_err(|_| "pipe cap does not fit u64".to_owned())?
        .checked_add(1)
        .ok_or_else(|| "pipe cap overflow".to_owned())?;
    let mut bytes = Vec::new();
    reader
        .take(take)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("pipe read failed: {error}"))?;
    if bytes.len() > cap {
        oversized.store(true, Ordering::SeqCst);
    }
    Ok(bytes)
}
