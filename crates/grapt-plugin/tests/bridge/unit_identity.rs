//! Inspect bounded cgroup membership and actual executable identity without
//! assuming which process is the service's main process. Fixed readlink helpers
//! run outside the filing namespace, where `/proc/<pid>/exe` is readable.
//! Each helper is stopped and proven absent; inspected processes are not signalled.
//! Unit identity, process start time and membership must remain stable during a read.

use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::OsStr;
use std::io::{ErrorKind, Read};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// The helper unit's own runtime cap.
const HELPER_RUNTIME_MAX_SEC: u64 = 5;
/// Client bound: the helper's runtime cap plus user-manager start/stop slack.
const HELPER_CLIENT_BOUND: Duration = Duration::from_secs(10);
/// Bound for each systemctl query or stop.
pub(super) const QUERY_BOUND: Duration = Duration::from_secs(10);
/// Bound for proving the helper unit absent after its client ended.
const HELPER_ABSENCE_BOUND: Duration = Duration::from_secs(10);
/// Bound for reaping a client after it was killed or its status failed.
const REAP_BOUND: Duration = Duration::from_secs(2);
/// Largest accepted output of any helper or query.
pub(super) const OUTPUT_BYTES: usize = 4096;

pub(super) fn systemd_units(pattern: &str) -> Result<BTreeSet<String>, Box<dyn Error>> {
    unit_inventory(pattern, QUERY_BOUND)
}

static NEXT_HELPER: AtomicU64 = AtomicU64::new(1);

/// The inspected unit's identity as the user manager reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct UnitIdentity {
    invocation: String,
    main_pid: u32,
}

/// Actual executables of the unit's members, or an empty list if it ended.
pub(super) fn unit_executables(unit: &str) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let Some(before) = query_unit_identity(unit)? else {
        return Ok(Vec::new());
    };
    let Some(started) = confined_start_time(before.main_pid)? else {
        return Ok(Vec::new());
    };
    let Some(group) = control_group(unit)? else {
        return Ok(Vec::new());
    };
    let members = member_pids(&group)?;
    let mut executables = Vec::new();
    for pid in members {
        let Some(start) = confined_start_time(pid)? else {
            continue;
        };
        let read = run_readlink_helper(&next_helper_unit()?, pid)?;
        let Some(after) = confined_start_time(pid)? else {
            continue;
        };
        if after != start || !member_pids(&group)?.contains(&pid) {
            return Err(format!("{unit} member {pid} drifted during executable read").into());
        }
        executables.push(read.map_err(|error| format!("{unit} member {pid}: {error}"))?);
    }
    let after = query_unit_identity(unit)?;
    let Some(restarted) = confined_start_time(before.main_pid)? else {
        return Ok(Vec::new());
    };
    check_stable_identity(unit, &before, started, after.as_ref(), Some(restarted))?;
    if control_group(unit)?.as_ref() != Some(&group) {
        return Err(format!("{unit} control group drifted during executable read").into());
    }
    Ok(executables)
}

fn control_group(unit: &str) -> Result<Option<PathBuf>, Box<dyn Error>> {
    let output = bounded_output(
        systemctl(&["show", unit, "--property=ControlGroup", "--value"]),
        QUERY_BOUND,
        OUTPUT_BYTES,
    )?;
    if !output.status.success() {
        return Err(format!("{unit} control group query failed").into());
    }
    let text = std::str::from_utf8(&output.stdout)?.trim_end_matches('\n');
    if text.is_empty() {
        return Ok(None);
    }
    let relative = text.strip_prefix('/').ok_or("relative control group")?;
    let path = std::path::Path::new(relative);
    if path.file_name() != Some(OsStr::new(unit))
        || !path
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
        || text.contains(['\r', '\n', '\0'])
    {
        return Err(format!("{unit} control group is malformed or foreign").into());
    }
    Ok(Some(std::path::Path::new("/sys/fs/cgroup").join(path)))
}

fn member_pids(group: &std::path::Path) -> Result<BTreeSet<u32>, Box<dyn Error>> {
    let mut bytes = Vec::new();
    match std::fs::File::open(group.join("cgroup.procs")) {
        Ok(file) => file
            .take((OUTPUT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(error.into()),
    };
    if bytes.len() > OUTPUT_BYTES {
        return Err("control group membership exceeds its byte bound".into());
    }
    let mut pids = BTreeSet::new();
    for line in std::str::from_utf8(&bytes)?.lines() {
        let pid = line.parse::<u32>()?;
        if pid == 0 || !pids.insert(pid) || pids.len() > 64 {
            return Err("control group membership is malformed or exceeds its bound".into());
        }
    }
    Ok(pids)
}

/// Refuses any drift between the identity read before the helper and the
/// one read after it.
fn check_stable_identity(
    unit: &str,
    before: &UnitIdentity,
    started: u64,
    after: Option<&UnitIdentity>,
    restarted: Option<u64>,
) -> Result<(), Box<dyn Error>> {
    if after != Some(before) {
        return Err(format!(
            "{unit} identity drifted during the executable read: {before:?} then {after:?}"
        )
        .into());
    }
    if restarted != Some(started) {
        return Err(format!(
            "{unit} MainPID {} start time drifted during the executable read: {started} then {restarted:?}",
            before.main_pid
        )
        .into());
    }
    Ok(())
}

fn query_unit_identity(unit: &str) -> Result<Option<UnitIdentity>, Box<dyn Error>> {
    let output = bounded_output(
        systemctl(&["show", unit, "--property=Id,LoadState,InvocationID,MainPID"]),
        QUERY_BOUND,
        OUTPUT_BYTES,
    )?;
    if !output.status.success() {
        return Err(format!("the identity query for {unit} failed").into());
    }
    parse_unit_identity(unit, &output.stdout)
}

/// Parses exactly the four queried properties. A not-found unit or one
/// without a main process is None; every other shape must name this
/// exact unit with a well-formed invocation and a positive MainPID.
fn parse_unit_identity(unit: &str, bytes: &[u8]) -> Result<Option<UnitIdentity>, Box<dyn Error>> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| format!("the identity of {unit} is not UTF-8"))?;
    let mut id = None;
    let mut load = None;
    let mut invocation = None;
    let mut main_pid = None;
    let mut lines = 0usize;
    for line in text.lines() {
        lines += 1;
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("the identity of {unit} has a malformed line {line:?}"))?;
        let slot = match key {
            "Id" => &mut id,
            "LoadState" => &mut load,
            "InvocationID" => &mut invocation,
            "MainPID" => &mut main_pid,
            _ => return Err(format!("the identity of {unit} has an unqueried key {key:?}").into()),
        };
        if slot.replace(value).is_some() {
            return Err(format!("the identity of {unit} repeats {key}").into());
        }
    }
    let (Some(id), Some(load), Some(invocation), Some(main_pid), 4) =
        (id, load, invocation, main_pid, lines)
    else {
        return Err(format!("the identity of {unit} is incomplete: {text:?}").into());
    };
    if id != unit {
        return Err(format!("the identity query for {unit} answered for {id}").into());
    }
    if load == "not-found" {
        return Ok(None);
    }
    let main_pid = main_pid
        .parse::<u32>()
        .map_err(|_| format!("the MainPID of {unit} is malformed: {main_pid:?}"))?;
    if main_pid == 0 {
        return Ok(None);
    }
    if invocation.len() != 32
        || !invocation
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!("the InvocationID of {unit} is malformed: {invocation:?}").into());
    }
    Ok(Some(UnitIdentity {
        invocation: invocation.to_owned(),
        main_pid,
    }))
}

/// The start time of `pid` read from this side of the namespace, or None
/// when the process no longer exists.
fn confined_start_time(pid: u32) -> Result<Option<u64>, Box<dyn Error>> {
    match std::fs::read(format!("/proc/{pid}/stat")) {
        Ok(bytes) => parse_start_time(&bytes).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("the stat of pid {pid} was unreadable: {error}").into()),
    }
}

/// Field 22 of `/proc/<pid>/stat`, counted after the parenthesized command
/// name, which may itself contain spaces and parentheses.
fn parse_start_time(bytes: &[u8]) -> Result<u64, Box<dyn Error>> {
    if bytes.len() > OUTPUT_BYTES {
        return Err("the process stat exceeds its bound".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "the process stat is not UTF-8")?;
    let (_, fields) = text
        .rsplit_once(") ")
        .ok_or("the process stat has no command terminator")?;
    let start = fields
        .split_ascii_whitespace()
        .nth(19)
        .ok_or("the process stat has no start time")?;
    Ok(start
        .parse::<u64>()
        .map_err(|_| format!("the process start time is malformed: {start:?}"))?)
}

fn next_helper_unit() -> Result<String, Box<dyn Error>> {
    let sequence = NEXT_HELPER
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| "the executable helper sequence overflowed")?;
    Ok(format!(
        "secode-plugin-exe-read-{}-{sequence}",
        std::process::id()
    ))
}

/// Runs the fixed readlink helper in `helper`, then stops that unit and
/// proves it absent whatever the helper's outcome. The outer error is a
/// cleanup failure and is never excused; the inner one is the read.
fn run_readlink_helper(helper: &str, pid: u32) -> Result<Result<PathBuf, String>, Box<dyn Error>> {
    run_readlink_helper_within(helper, pid, HELPER_CLIENT_BOUND)
}

/// `run_readlink_helper` with an explicit client bound, so a control can
/// force the client's timeout. The client is bounded through its exit, its
/// stdout and its reap, so the helper stop is always reached.
fn run_readlink_helper_within(
    helper: &str,
    pid: u32,
    client_bound: Duration,
) -> Result<Result<PathBuf, String>, Box<dyn Error>> {
    let mut command = Command::new("/usr/bin/systemd-run");
    command
        .args(["--user", "--pipe", "--wait", "--collect", "--quiet"])
        .arg("--service-type=exec")
        .arg(format!("--unit={helper}"))
        .arg(format!("--property=RuntimeMaxSec={HELPER_RUNTIME_MAX_SEC}"))
        .arg("/usr/bin/readlink")
        .arg("-e")
        .arg(format!("/proc/{pid}/exe"));
    let read = bounded_output(command, client_bound, OUTPUT_BYTES)
        .and_then(|output| {
            if output.status.success() {
                parse_helper_output(&output.stdout)
            } else {
                Err(format!("the executable helper exited {}", output.status).into())
            }
        })
        .map_err(|error| error.to_string());
    stop_helper(helper)?;
    Ok(read)
}

/// Exactly one complete absolute path, newline-terminated, taken as raw
/// bytes rather than lossily decoded.
fn parse_helper_output(bytes: &[u8]) -> Result<PathBuf, Box<dyn Error>> {
    if bytes.len() > OUTPUT_BYTES {
        return Err("the executable helper output exceeds its bound".into());
    }
    let path = bytes
        .strip_suffix(b"\n")
        .ok_or("the executable helper output is incomplete")?;
    if !path.starts_with(b"/") || path.contains(&b'\n') || path.contains(&0) {
        return Err(
            format!("the executable helper output is not one absolute path: {bytes:?}").into(),
        );
    }
    Ok(PathBuf::from(OsStr::from_bytes(path)))
}

/// Stops the helper unit (never the inspected one) and waits for the user
/// manager to report it absent. The stop's own answer is not evidence (an
/// already collected unit refuses it); the absence poll is, and each of its
/// queries is bounded by what remains of the absence deadline.
fn stop_helper(helper: &str) -> Result<(), Box<dyn Error>> {
    let service = format!("{helper}.service");
    let stop = bounded_output(systemctl(&["stop", &service]), QUERY_BOUND, OUTPUT_BYTES);
    let deadline = deadline_after(HELPER_ABSENCE_BOUND)?;
    let mut last = match stop {
        Ok(output) => format!("its stop exited {}", output.status),
        Err(error) => format!("its stop failed: {error}"),
    };
    while let Some(left) = remaining(deadline) {
        match bounded_output(
            systemctl(&[
                "show",
                &service,
                "--property=Id,LoadState,ActiveState,SubState,MainPID,ControlPID",
            ]),
            left.min(QUERY_BOUND),
            OUTPUT_BYTES,
        ) {
            Ok(output) if output.status.success() && helper_absent(&service, &output.stdout) => {
                return Ok(());
            }
            Ok(output) => {
                last = format!(
                    "its last answer was {:?}",
                    String::from_utf8_lossy(&output.stdout)
                );
            }
            Err(error) => last = format!("its last query failed: {error}"),
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!(
        "executable helper {service} was not proven absent within {HELPER_ABSENCE_BOUND:?} \
         and may remain: {last}"
    )
    .into())
}

fn deadline_after(bound: Duration) -> Result<tokio::time::Instant, Box<dyn Error>> {
    Ok(tokio::time::Instant::now()
        .checked_add(bound)
        .ok_or("a command bound overflowed")?)
}

/// What remains before `deadline`, or None once it has passed.
fn remaining(deadline: tokio::time::Instant) -> Option<Duration> {
    deadline
        .checked_duration_since(tokio::time::Instant::now())
        .filter(|left| !left.is_zero())
}

/// Complete absence evidence for exactly this helper unit.
fn helper_absent(service: &str, bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let identity = format!("Id={service}");
    let required = [
        identity.as_str(),
        "LoadState=not-found",
        "ActiveState=inactive",
        "SubState=dead",
        "MainPID=0",
        "ControlPID=0",
    ];
    text.lines().count() == required.len()
        && required
            .iter()
            .all(|expected| text.lines().any(|line| line == *expected))
}

/// Largest accepted unit inventory: the 4096 units `owned_linux_unit`
/// admits at well under 256 bytes per listed line.
const INVENTORY_BYTES: usize = 1024 * 1024;

/// The user manager's units matching `pattern`, bounded through the
/// query's exit and output.
pub(super) fn unit_inventory(
    pattern: &str,
    bound: Duration,
) -> Result<BTreeSet<String>, Box<dyn Error>> {
    let output = bounded_output(
        systemctl(&["list-units", "--all", "--plain", "--no-legend", pattern]),
        bound,
        INVENTORY_BYTES,
    )?;
    if !output.status.success() {
        return Err("systemd unit inventory failed".into());
    }
    Ok(String::from_utf8(output.stdout)?
        .lines()
        .filter_map(|line| line.split_whitespace().next().map(str::to_owned))
        .collect())
}

fn systemctl(arguments: &[&str]) -> Command {
    let mut command = Command::new("/usr/bin/systemctl");
    command.args(["--user", "--no-pager"]).args(arguments);
    command
}

/// Runs `command` under one deadline that covers its exit and its stdout
/// reaching end-of-file, accepting at most `cap` bytes. Stdout is a socket,
/// so every read is bounded: a descendant, or a unit's service, that keeps
/// the stream open after the client exits cannot hold the caller past
/// `bound`. On every failure the client is killed and its reap is bounded
/// too; the error names the command, the failure and any unreaped pid.
pub(super) fn bounded_output(
    mut command: Command,
    bound: Duration,
    cap: usize,
) -> Result<Output, Box<dyn Error>> {
    let deadline = deadline_after(bound)?;
    let (mut reader, writer) = UnixStream::pair()?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(OwnedFd::from(writer)))
        .stderr(Stdio::null());
    let described = format!("{command:?}");
    let spawned = command.spawn();
    // The command owns this side's copy of the write end: drop it, so
    // end-of-file means every holder in the client's tree has closed it.
    drop(command);
    let mut child = spawned?;
    collect_output(&mut child, &mut reader, deadline, cap).map_err(|failure| {
        format!(
            "{described} {failure} ({bound:?} bound); {}",
            kill_and_reap(&mut child)
        )
        .into()
    })
}

fn collect_output(
    child: &mut Child,
    reader: &mut UnixStream,
    deadline: tokio::time::Instant,
    cap: usize,
) -> Result<Output, String> {
    let mut stdout = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let Some(left) = remaining(deadline) else {
            return Err(match child.try_wait() {
                Ok(Some(status)) => format!("exited {status} with its stdout still held open"),
                Ok(None) => "outlived its bound".to_owned(),
                Err(error) => format!("had an unreadable status ({error})"),
            });
        };
        reader
            .set_read_timeout(Some(left))
            .map_err(|error| format!("could not bound its stdout ({error})"))?;
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                stdout.extend_from_slice(&chunk[..read]);
                if stdout.len() > cap {
                    return Err(format!("wrote more than {cap} bytes"));
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(format!("had an unreadable stdout ({error})")),
        }
    }
    let status =
        wait_until(child, deadline).map_err(|error| format!("closed its stdout, but {error}"))?;
    Ok(Output {
        status,
        stdout,
        stderr: Vec::new(),
    })
}

/// Polls `child` until it exits or `deadline` passes.
fn wait_until(child: &mut Child, deadline: tokio::time::Instant) -> Result<ExitStatus, String> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if remaining(deadline).is_some() => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => return Err(format!("pid {} did not exit", child.id())),
            Err(error) => {
                return Err(format!(
                    "pid {} had an unreadable status ({error})",
                    child.id()
                ))
            }
        }
    }
}

/// Kills `child` and waits at most REAP_BOUND for its reap, describing the
/// outcome: a failed kill and an unreaped pid are named, never discarded.
fn kill_and_reap(child: &mut Child) -> String {
    let pid = child.id();
    let kill = match child.kill() {
        Ok(()) => String::new(),
        Err(error) => format!("; its kill failed ({error})"),
    };
    let reap = match deadline_after(REAP_BOUND) {
        Ok(deadline) => match wait_until(child, deadline) {
            Ok(status) => format!("pid {pid} was killed and reaped ({status})"),
            Err(error) => format!("residue: {error} within the {REAP_BOUND:?} reap bound"),
        },
        Err(error) => format!("residue: pid {pid} was not reaped ({error})"),
    };
    format!("{reap}{kill}")
}

const INVOCATION: &str = "0123456789abcdef0123456789abcdef";

fn identity_answer(id: &str, load: &str, invocation: &str, main_pid: &str) -> String {
    format!("Id={id}\nLoadState={load}\nInvocationID={invocation}\nMainPID={main_pid}\n")
}

#[test]
fn unit_identity_refuses_malformed_ambiguous_and_foreign_answers() -> Result<(), Box<dyn Error>> {
    let unit = "owned.service";
    let parsed = parse_unit_identity(
        unit,
        identity_answer(unit, "loaded", INVOCATION, "42").as_bytes(),
    )?;
    assert_eq!(
        parsed,
        Some(UnitIdentity {
            invocation: INVOCATION.to_owned(),
            main_pid: 42
        })
    );
    // An absent inventory service, or one without a main process, is None.
    for answer in [
        identity_answer(unit, "not-found", "", "0"),
        identity_answer(unit, "loaded", "", "0"),
    ] {
        assert_eq!(parse_unit_identity(unit, answer.as_bytes())?, None);
    }
    let refused = [
        identity_answer("foreign.service", "loaded", INVOCATION, "42"),
        identity_answer(unit, "loaded", "", "42"),
        identity_answer(unit, "loaded", "0123456789ABCDEF0123456789ABCDEF", "42"),
        identity_answer(unit, "loaded", &INVOCATION[1..], "42"),
        identity_answer(unit, "loaded", INVOCATION, "-1"),
        identity_answer(unit, "loaded", INVOCATION, "4294967296"),
        format!("Id={unit}\nLoadState=loaded\nMainPID=42\n"),
        format!(
            "{}MainPID=43\n",
            identity_answer(unit, "loaded", INVOCATION, "42")
        ),
        format!(
            "{}ExecMainPID=42\n",
            identity_answer(unit, "loaded", INVOCATION, "42")
        ),
        format!("Id={unit}\nLoadState loaded\nInvocationID={INVOCATION}\nMainPID=42\n"),
    ];
    for answer in refused {
        assert!(
            parse_unit_identity(unit, answer.as_bytes()).is_err(),
            "a malformed identity was admitted: {answer:?}"
        );
    }
    assert!(parse_unit_identity(unit, &[0xff]).is_err());
    Ok(())
}

#[test]
fn identity_drift_refuses_a_reused_pid_or_restarted_unit() -> Result<(), Box<dyn Error>> {
    let before = UnitIdentity {
        invocation: INVOCATION.to_owned(),
        main_pid: 42,
    };
    check_stable_identity("owned.service", &before, 7, Some(&before), Some(7))?;
    let restarted = UnitIdentity {
        invocation: "fedcba9876543210fedcba9876543210".to_owned(),
        ..before.clone()
    };
    let foreign = UnitIdentity {
        main_pid: 43,
        ..before.clone()
    };
    for (after, started_again) in [
        (None, Some(7)),
        (Some(&restarted), Some(7)),
        (Some(&foreign), Some(7)),
        (Some(&before), None),
        (Some(&before), Some(8)),
    ] {
        assert!(
            check_stable_identity("owned.service", &before, 7, after, started_again).is_err(),
            "identity drift was admitted: {after:?} {started_again:?}"
        );
    }
    Ok(())
}

#[test]
fn start_time_and_helper_output_are_read_exactly() -> Result<(), Box<dyn Error>> {
    let mut stat = String::from("42 (node (a) b) S");
    for field in 4..=21 {
        stat.push_str(&format!(" {field}"));
    }
    stat.push_str(" 987654 23 24\n");
    assert_eq!(parse_start_time(stat.as_bytes())?, 987_654);
    assert!(parse_start_time(b"42 node S 4 5\n").is_err());
    assert!(parse_start_time(b"42 (node) S 4 5\n").is_err());
    assert!(parse_start_time(&[b'1'; OUTPUT_BYTES + 1]).is_err());

    assert_eq!(
        parse_helper_output(b"/usr/bin/node\n")?,
        PathBuf::from("/usr/bin/node")
    );
    // Raw bytes, never lossily decoded.
    assert_eq!(
        parse_helper_output(b"/opt/n\xffde\n")?,
        PathBuf::from(OsStr::from_bytes(b"/opt/n\xffde"))
    );
    let mut oversized = vec![b'/'; OUTPUT_BYTES];
    oversized.push(b'\n');
    for bad in [
        b"".as_slice(),
        b"/usr/bin/node",
        b"usr/bin/node\n",
        b"/usr/bin/node\n/usr/bin/other\n",
        b"/usr/bin/no\0de\n",
        b"\n",
        oversized.as_slice(),
    ] {
        assert!(
            parse_helper_output(bad).is_err(),
            "the helper output {bad:?} was admitted"
        );
    }
    Ok(())
}

#[test]
fn helper_absence_requires_complete_evidence_for_exactly_that_unit() {
    let service = "secode-plugin-exe-read-1-1.service";
    let absent = format!(
        "Id={service}\nLoadState=not-found\nActiveState=inactive\nSubState=dead\nMainPID=0\nControlPID=0\n"
    );
    assert!(helper_absent(service, absent.as_bytes()));
    for bad in [
        absent.replace(service, "secode-plugin-exe-read-1-2.service"),
        absent.replace("LoadState=not-found", "LoadState=loaded"),
        absent.replace("SubState=dead", "SubState=running"),
        absent.replace("MainPID=0", "MainPID=42"),
        absent.replace("ControlPID=0\n", ""),
        format!("{absent}Extra=1\n"),
    ] {
        assert!(!helper_absent(service, bad.as_bytes()), "{bad:?}");
    }
    assert!(!helper_absent(service, &[0xff]));
}

#[test]
fn bounded_output_kills_an_overrunning_client_and_refuses_oversized_output(
) -> Result<(), Box<dyn Error>> {
    let bound = Duration::from_millis(300);
    let started = tokio::time::Instant::now();
    let mut sleeper = Command::new("/usr/bin/sleep");
    sleeper.arg("30");
    let overrun = bounded_output(sleeper, bound, OUTPUT_BYTES)
        .err()
        .ok_or("an overrunning client was admitted")?
        .to_string();
    assert!(
        started.elapsed() < bound + Duration::from_millis(700),
        "the overrunning client outlived its bound: {:?}",
        started.elapsed()
    );
    assert!(
        overrun.contains("outlived its bound") && overrun.contains("was killed and reaped"),
        "{overrun}"
    );
    let mut flood = Command::new("/usr/bin/head");
    flood.args(["-c", "5000", "/dev/zero"]);
    let flooded = bounded_output(flood, Duration::from_secs(5), OUTPUT_BYTES)
        .err()
        .ok_or("oversized output was admitted")?
        .to_string();
    assert!(flooded.contains("wrote more than 4096 bytes"), "{flooded}");
    Ok(())
}

/// The client exits at once but leaves a descendant holding its stdout, the
/// shape of a helper unit that survives its systemd-run client. The bound
/// covers the stream, so the call is refused on time instead of waiting for
/// the holder, and the holder is then killed by its recorded pid.
#[test]
fn bounded_output_refuses_a_stdout_held_open_after_the_client_exits() -> Result<(), Box<dyn Error>>
{
    let holder_pid = std::env::temp_dir().join(format!(
        "grapt-bounded-output-holder-{}-{}",
        std::process::id(),
        NEXT_HELPER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut client = Command::new("/bin/sh");
    client
        .arg("-c")
        .arg("/usr/bin/sleep 5 & echo $! > \"$1\"; echo client-exited")
        .arg("sh")
        .arg(&holder_pid);
    let bound = Duration::from_millis(300);
    let started = tokio::time::Instant::now();
    let held = bounded_output(client, bound, OUTPUT_BYTES);
    let elapsed = started.elapsed();
    let holder = std::fs::read_to_string(&holder_pid)?;
    std::fs::remove_file(&holder_pid)?;
    let mut kill = Command::new("/usr/bin/kill");
    kill.args(["-KILL", holder.trim()]);
    let killed = bounded_output(kill, QUERY_BOUND, OUTPUT_BYTES)?;
    let refusal = held
        .err()
        .ok_or("a stdout held past the bound was admitted")?
        .to_string();
    assert!(
        elapsed < bound + Duration::from_millis(700),
        "the held stdout outlived the bound: {elapsed:?}"
    );
    assert!(
        refusal.contains("exited exit status: 0 with its stdout still held open")
            && refusal.contains("was killed and reaped"),
        "{refusal}"
    );
    assert!(killed.status.success(), "the stdout holder was not killed");
    Ok(())
}

/// A reap that stays pending is abandoned at its deadline with the pid
/// named, and a kill followed by the bounded reap collects the client.
#[test]
fn a_pending_reap_is_bounded_and_named() -> Result<(), Box<dyn Error>> {
    let mut client = Command::new("/usr/bin/sleep");
    client.arg("30");
    let mut child = client.stdin(Stdio::null()).spawn()?;
    let bound = Duration::from_millis(200);
    let started = tokio::time::Instant::now();
    let pending = wait_until(&mut child, deadline_after(bound)?);
    let elapsed = started.elapsed();
    let reaped = kill_and_reap(&mut child);
    assert_eq!(
        pending,
        Err(format!("pid {} did not exit", child.id())),
        "a pending reap was not named"
    );
    assert!(elapsed < bound + Duration::from_millis(700), "{elapsed:?}");
    assert!(reaped.contains("was killed and reaped"), "{reaped}");
    assert!(
        child.try_wait()?.is_some(),
        "the killed client was not reaped"
    );
    Ok(())
}

/// The real helper reads this test process's own executable; a helper
/// whose target has gone fails the read; and a client that overruns its
/// bound is killed and reaped. On every path the helper unit, never the
/// inspected process, is stopped and proven absent.
#[test]
fn the_readlink_helper_reads_a_live_pid_and_always_leaves_no_unit() -> Result<(), Box<dyn Error>> {
    let expected = std::fs::canonicalize(std::env::current_exe()?)?;
    let helper = next_helper_unit()?;
    let read = run_readlink_helper(&helper, std::process::id())?;
    assert_eq!(read?, expected);

    // A client bound no systemd-run can meet: the read fails as an overrun,
    // and the outer Ok is the helper unit's proven absence.
    let helper = next_helper_unit()?;
    let overrun =
        run_readlink_helper_within(&helper, std::process::id(), Duration::from_millis(1))?
            .err()
            .ok_or("the helper met a 1 ms client bound")?;
    assert!(
        overrun.contains("outlived its bound") && overrun.contains("was killed and reaped"),
        "{overrun}"
    );

    let mut ended = Command::new("/usr/bin/true").spawn()?;
    let gone = ended.id();
    ended.wait()?;
    if confined_start_time(gone)?.is_some() {
        return Err(format!("pid {gone} was reused before the control; the run is void").into());
    }
    let helper = next_helper_unit()?;
    assert!(
        run_readlink_helper(&helper, gone)?.is_err(),
        "the helper read an executable for an ended pid"
    );
    Ok(())
}
