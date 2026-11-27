use super::*;
use std::error::Error;

/// The test ceiling: the 256 MiB value the bridge RSS witness
/// configures (RSS_TEST_THRESHOLD_BYTES with the headroom collapsed),
/// so OOM_REASON is the exact string the production boundary path
/// emits for that ceiling (PLR103).
pub(super) const TEST_MEMORY_MAX: u64 = 268435456;
const OOM_REASON: &str = "plugin bridge RSS reached the 268435456-byte hard cgroup memory boundary";
pub(super) const UNAVAILABLE_REASON: &str = "plugin containment OOM evidence was unavailable";

#[test]
fn stop_absence_requires_complete_exact_inactive_evidence() {
    let evidence = "Id=owned.service\nLoadState=not-found\nActiveState=inactive\nSubState=dead\nMainPID=0\nControlPID=0\n";
    assert!(absent_unit_evidence(
        "owned.service",
        Ok((true, evidence.as_bytes()))
    ));
    for altered in [
        evidence.replace("owned.service", "foreign.service"),
        evidence.replace("not-found", "loaded"),
        evidence.replace("inactive", "active"),
        evidence.replace("inactive", "failed"),
        evidence.replace("dead", "running"),
        evidence.replace("MainPID=0", "MainPID=123"),
        evidence.replace("ControlPID=0", "ControlPID=123"),
        evidence.replace("MainPID=0\n", ""),
        format!("{evidence}MainPID=0\n"),
        format!("{evidence}{}", " ".repeat(4096)),
    ] {
        assert!(
            !absent_unit_evidence("owned.service", Ok((true, altered.as_bytes()))),
            "nonterminal or ambiguous evidence accepted: {altered}"
        );
    }
    assert!(!absent_unit_evidence("owned.service", Err(())));
    assert!(!absent_unit_evidence(
        "owned.service",
        Ok((false, evidence.as_bytes()))
    ));
    assert!(!absent_unit_evidence("owned.service", Ok((true, &[0xff]))));
}

#[test]
fn stop_command_output_and_duration_are_bounded() -> Result<(), Box<dyn Error>> {
    let mut oversized = Command::new("/usr/bin/python3");
    oversized.args(["-c", "import sys; sys.stdout.write('x'*4097)"]);
    let error = bounded_command_output(oversized)
        .err()
        .ok_or("oversized output accepted")?;
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    let mut stalled = Command::new("/usr/bin/python3");
    stalled.args(["-c", "import time; time.sleep(30)"]);
    let error = bounded_command_output(stalled)
        .err()
        .ok_or("stalled command accepted")?;
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    Ok(())
}

#[test]
fn already_removed_owned_unit_stops_idempotently() -> Result<(), Box<dyn Error>> {
    let limits = linux_test_limits(100);
    let (control, unit) = spawn_linux_test_process("exec /usr/bin/sleep 30", &[], limits, false)?;
    let bound = Duration::from_millis(limits.startup_millis);
    let started = locate_cgroup(&control, &unit, Duration::from_millis(50), bound).is_some();
    let cleanup = control.terminate();
    assert!(
        started,
        "owned containment must start before repeated-stop witness"
    );
    cleanup?;
    // Reaping the launcher does not wait for the manager to unload its unit.
    // Establish exact absence before exercising an already-removed unit.
    wait_for_unit_absent(&unit, Duration::from_secs(5))?;
    stop_unit(&unit).unwrap_or_else(|error| {
        panic!("already removed owned unit must stop idempotently: {error}")
    });
    assert!(control.has_exited()?);
    Ok(())
}

#[test]
fn linux_oom_evidence_refuses_unknown_as_zero() -> Result<(), Box<dyn Error>> {
    for evidence in [
        Err(()),
        Ok((false, b"".as_slice())),
        Ok((true, b"Result=success\n".as_slice())),
        Ok((true, b"OOMKills=not-a-number\nResult=success\n".as_slice())),
        Ok((true, b"OOMKills=0\n".as_slice())),
        // The pre-C11 production shape, now refused: the query contract
        // includes LoadState, so a response without it is unavailable.
        Ok((true, b"OOMKills=0\nResult=success\n".as_slice())),
        Ok((true, &[0xff_u8][..])),
    ] {
        let error = classify_systemd_oom_evidence(evidence, TEST_MEMORY_MAX)
            .err()
            .ok_or("unreadable OOM evidence was accepted as zero")?;
        assert!(error.contains(UNAVAILABLE_REASON), "wrong refusal: {error}");
    }

    assert_eq!(
        classify_systemd_oom_evidence(
            Ok((
                true,
                b"LoadState=loaded\nOOMKills=0\nResult=success\n".as_slice(),
            )),
            TEST_MEMORY_MAX
        )?,
        None,
        "ordinary successful evidence was classified as OOM"
    );
    assert_eq!(
        classify_systemd_oom_evidence(
            Ok((
                true,
                b"LoadState=loaded\nOOMKills=1\nResult=success\n".as_slice(),
            )),
            TEST_MEMORY_MAX
        )?
        .as_deref(),
        Some(OOM_REASON),
        "retained OOMKills did not override systemd's successful SIGKILL status"
    );
    Ok(())
}

/// The PLR107 witness: the OOMKills sentinel is absence under any
/// LoadState, never OOM evidence and never corrupt evidence.
/// systemctl --user show answers a not-found unit with
/// OOMKills=18446744073709551615 (UINT64_MAX), Result=success and exit 0
/// (probed 2026-09-26 on the box; the 48-wide side-by-side journal
/// carried zero OOM lines while the is_running hot path read that shape
/// as the bridge's own hard-boundary OOM). The counter is a monotonic
/// kill count that can never physically reach u64::MAX, yet the
/// shape-capture probe caught it under LoadState=loaded once, in the
/// startup window before the counter is populated (1387
/// production-shape queries, 3 distinct shapes). A poll racing a start
/// or stop window must read the sentinel as absence, never as OOM and
/// never as corruption that fail-closes the generation.
#[test]
fn linux_oom_evidence_reads_the_not_found_sentinel_as_absent() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        classify_systemd_oom_evidence(
            Ok((
                true,
                b"LoadState=not-found\nOOMKills=18446744073709551615\nResult=success\n".as_slice(),
            )),
            TEST_MEMORY_MAX
        )?,
        None,
        "the not-found sentinel was classified as OOM evidence"
    );
    assert_eq!(
        classify_systemd_oom_evidence(
            Ok((
                true,
                b"LoadState=loaded\nOOMKills=18446744073709551615\nResult=success\n".as_slice(),
            )),
            TEST_MEMORY_MAX
        )?,
        None,
        "the loaded+sentinel startup shape was not read as absence"
    );
    Ok(())
}

/// The PLR109 witness (R2): the explicit systemd verdict
/// Result=oom-kill is positive OOM evidence under any LoadState and
/// whether or not the OOMKills counter is populated. The pre-fix C12
/// guard returned Ok(None) before reading Result, so a poll racing a
/// not-loaded state or an unpopulated sentinel suppressed a real OOM
/// verdict; and a non-loaded state with no positive evidence is
/// absence, never a blanket UNAVAILABLE - the shape probe (1387
/// production-shape queries, 3 distinct shapes) found zero OOM
/// evidence in any non-loaded state: not-found and the startup stub
/// read as absence on their own evidence, while every unmodelled state
/// fails closed naming itself (the PLR110 witness).
#[test]
fn linux_oom_evidence_evaluates_positive_result_before_load_state() -> Result<(), Box<dyn Error>> {
    for evidence in [
        b"LoadState=not-found\nOOMKills=0\nResult=oom-kill\n".as_slice(),
        b"LoadState=not-found\nOOMKills=18446744073709551615\nResult=oom-kill\n".as_slice(),
        b"LoadState=loaded\nOOMKills=18446744073709551615\nResult=oom-kill\n".as_slice(),
    ] {
        assert_eq!(
            classify_systemd_oom_evidence(Ok((true, evidence)), TEST_MEMORY_MAX)?,
            Some(OOM_REASON.to_owned()),
            "an explicit oom-kill verdict was suppressed by the unit state"
        );
    }
    assert_eq!(
        classify_systemd_oom_evidence(
            Ok((
                true,
                b"LoadState=stub\nOOMKills=0\nResult=success\n".as_slice(),
            )),
            TEST_MEMORY_MAX
        )?,
        None,
        "a valid startup LoadState was fail-closed instead of read as absence"
    );
    Ok(())
}

/// The PLR110 witness (R5): the LoadState match is exhaustive. The known
/// non-loaded states are absence on their own evidence - not-found has no
/// OOM evidence to attribute, and stub is a valid transient startup state
/// whose counter is not yet meaningful - while every unmodelled state
/// (error, bad-setting, masked, merged, or an unrecognised value) fails
/// closed UNAVAILABLE and names the state. Restoring the blanket
/// non-loaded -> absence rule (PLR110) reads those states as absence by
/// default and this witness reds.
#[test]
fn linux_oom_evidence_fail_closes_every_unmodelled_load_state() -> Result<(), Box<dyn Error>> {
    for state in [
        "error",
        "bad-setting",
        "masked",
        "merged",
        "not-a-systemd-state",
    ] {
        let evidence = format!("LoadState={state}\nOOMKills=0\nResult=success\n");
        let error = classify_systemd_oom_evidence(Ok((true, evidence.as_bytes())), TEST_MEMORY_MAX)
            .err()
            .ok_or("an unmodelled LoadState was read as absence by default")?;
        assert!(
            error.contains(&format!("LoadState is {state}")),
            "the refusal did not name the state: {error}"
        );
    }
    assert_eq!(
        classify_systemd_oom_evidence(
            Ok((
                true,
                b"LoadState=not-found\nOOMKills=18446744073709551615\nResult=success\n".as_slice(),
            )),
            TEST_MEMORY_MAX
        )?,
        None,
        "a not-found unit carries no OOM evidence to attribute"
    );
    assert_eq!(
        classify_systemd_oom_evidence(
            Ok((
                true,
                b"LoadState=stub\nOOMKills=0\nResult=success\n".as_slice(),
            )),
            TEST_MEMORY_MAX
        )?,
        None,
        "a stub startup state is absence on its own evidence"
    );
    Ok(())
}

#[test]
fn linux_main_oom_is_attributed_and_cleaned() -> Result<(), Box<dyn Error>> {
    let limits = linux_test_limits(100);
    let (control, unit) = spawn_linux_test_process(
        "exec /usr/bin/python3 -c 'held=[]\nwhile True: held.append(bytearray(8*1024*1024))'",
        &[],
        limits,
        true,
    )?;
    let outcome = (|| -> Result<String, Box<dyn Error>> {
        wait_for_exit(&control, &unit)?;
        control
            .violation()
            .ok_or_else(|| "main-process OOM was accepted as successful work".into())
    })();
    let cleanup = control.terminate();
    let reason = outcome?;
    cleanup?;
    assert!(reason.contains("RSS"), "wrong main OOM reason: {reason}");
    assert_unit_inactive(&unit)?;
    Ok(())
}

#[test]
fn linux_child_oom_is_attributed_and_all_descendants_are_cleaned() -> Result<(), Box<dyn Error>> {
    let identity = NEXT_CONTAINMENT_ID.load(Ordering::Relaxed);
    let marker = std::env::temp_dir().join(format!(
        "secode-plugin-child-{}-{identity}.txt",
        std::process::id()
    ));
    let victim = std::env::temp_dir().join(format!(
        "secode-plugin-child-victim-{}-{identity}.txt",
        std::process::id()
    ));
    let pending_marker = PathBuf::from(format!("{}.pending", marker.to_string_lossy()));
    let marker_arg = marker.to_string_lossy().into_owned();
    let victim_arg = victim.to_string_lossy().into_owned();
    let script = r#"
/usr/bin/python3 -c 'held=[]
while True: held.append(bytearray(8*1024*1024))' &
child=$!
printf '%s %s' "$BASHPID" "$child" > "$1.pending"
mv "$1.pending" "$1"
if wait "$child"; then exit 70; fi
printf 'child-oom' > "$2"
sleep 300
"#;
    let limits = linux_test_limits(500);
    let (control, unit) = spawn_linux_test_process(
        script,
        &[marker_arg.as_str(), victim_arg.as_str()],
        limits,
        false,
    )?;
    let outcome = (|| -> Result<(u32, u32, String), Box<dyn Error>> {
        let pids = wait_for_file(&marker, &unit)?;
        let mut fields = pids.split_whitespace();
        let main = fields
            .next()
            .ok_or("main pid was not recorded")?
            .parse()
            .map_err(|error| format!("main pid receipt {pids:?} was invalid: {error}"))?;
        let child = fields
            .next()
            .ok_or("child pid was not recorded")?
            .parse()
            .map_err(|error| format!("child pid receipt {pids:?} was invalid: {error}"))?;
        let victim_receipt = wait_for_file(&victim, &unit)?;
        if victim_receipt.trim() != "child-oom" {
            return Err("child OOM receipt was malformed".into());
        }
        // Delay attachment until the victim receipt proves the child OOM already
        // happened. The watchdog must retain that first event instead of adopting it
        // as an innocent baseline.
        start_resource_watchdog(control.clone(), 0, limits)?;
        wait_for_exit(&control, &unit)
            .map_err(|_| "early child OOM was missed before the watchdog baseline")?;
        let reason = control
            .violation()
            .ok_or("child-process OOM was accepted as successful work")?;
        Ok((main, child, reason))
    })();
    let cleanup = control.terminate();
    let _ = std::fs::remove_file(&marker);
    let _ = std::fs::remove_file(&pending_marker);
    let _ = std::fs::remove_file(&victim);
    let (main, child, reason) = outcome?;
    cleanup?;
    assert!(reason.contains("RSS"), "wrong child OOM reason: {reason}");
    assert!(
        !Path::new("/proc").join(main.to_string()).exists(),
        "main process survived containment cleanup"
    );
    assert!(
        !Path::new("/proc").join(child.to_string()).exists(),
        "OOM victim survived containment cleanup"
    );
    assert_unit_inactive(&unit)?;
    Ok(())
}

/// An ordinary SIGKILL of the main process is never promoted to OOM or
/// to success. PLR-R8 reshaped this witness: the pre-fix body asserted
/// the watchdog's own telemetry fail-close as the classification owner,
/// but the ruled fix defers a settled unit exit to the stream-end path
/// (the same as the dead-child path), so the witness now asserts the
/// contract the stream end enforces - no limit (Resource) violation is
/// ever recorded for an ordinary SIGKILL, and the stream-end
/// classification of the observed exit is the canonical exit reason,
/// never the memory-boundary or the telemetry string.
#[test]
fn linux_ordinary_sigkill_is_not_promoted_to_oom_or_success() -> Result<(), Box<dyn Error>> {
    let limits = linux_test_limits(100);
    let (control, unit) = spawn_linux_test_process("exec /usr/bin/sleep 300", &[], limits, true)?;
    let cgroup = locate_cgroup(
        &control,
        &unit,
        Duration::from_millis(50),
        Duration::from_millis(limits.startup_millis),
    )
    .ok_or("ordinary SIGKILL unit did not publish its cgroup")?;
    let procs = std::fs::read_to_string(cgroup.join("cgroup.procs"))
        .map_err(|error| format!("the SIGKILL witness cgroup receipt was unreadable: {error}"))?;
    let pid = procs
        .lines()
        .find(|line| !line.trim().is_empty())
        .ok_or("the SIGKILL witness unit published no main process; the run is void")?
        .trim()
        .parse::<u32>()
        .map_err(|error| {
            format!("the SIGKILL witness cgroup receipt {procs:?} was invalid: {error}")
        })?;
    let mut kill = Command::new("/usr/bin/kill")
        .arg("-9")
        .arg(pid.to_string())
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn_with_required_environment()?;
    if !kill.wait()?.success() {
        return Err("ordinary SIGKILL injection failed".into());
    }
    let outcome = (|| -> Result<(Option<Violation>, StreamEnd), Box<dyn Error>> {
        wait_for_unit_exited(&unit, Duration::from_secs(10))?;
        let cleanup = control.terminate();
        wait_for_exit(&control, &unit)?;
        let recorded = control.recorded_violation();
        let settled = settle_stream_end_classification(
            recorded.as_ref(),
            &mut || control.has_exited().unwrap_or(false),
            Duration::from_millis(500),
            None,
        );
        cleanup?;
        Ok((recorded, settled))
    })();
    let (recorded, settled) = outcome?;
    assert!(
        recorded.as_ref().map(|violation| violation.kind) != Some(ViolationKind::Resource),
        "an ordinary SIGKILL was promoted to a limit violation: {recorded:?}"
    );
    let StreamEnd::Exited(reason) = settled else {
        panic!("the stream end did not classify an observed exit: {settled:?}");
    };
    assert_eq!(
        reason, "plugin bridge exited without replying",
        "the stream end misclassified an ordinary SIGKILL: {reason}"
    );
    assert_unit_inactive(&unit)?;
    Ok(())
}

/// The PLR106 witness: an observed launcher exit still removes the
/// unit. The service fails on its own, so systemd-run --wait returns
/// and the exit is fully observed before terminate() runs - the exact
/// precondition under which the C6 shape (early return before
/// stop_containment) skips the teardown. A failed unit stays loaded
/// with its cgroup until stopped and reset, so the C6 shape leaves it
/// standing forever and the witness reds on the stated bound; the
/// fixed shape stops and resets the unit within it.
#[test]
fn linux_terminate_after_observed_exit_still_stops_the_unit() -> Result<(), Box<dyn Error>> {
    let limits = linux_test_limits(100);
    let (control, unit) = spawn_linux_test_process("exit 1", &[], limits, false)?;
    wait_for_exit(&control, &unit)?;
    control.terminate()?;
    wait_for_unit_absent(&unit, Duration::from_millis(5000))?;
    Ok(())
}

/// Bounded wait for the unit's own terminal main-process state through
/// the production bounded query - the PLR-R8 window. The timeout is a
/// loud environment void, never a silent pass.
pub(super) fn wait_for_unit_exited(unit: &str, bound: Duration) -> Result<(), Box<dyn Error>> {
    let deadline = tokio::time::Instant::now()
        .checked_add(bound)
        .ok_or("test bound overflowed")?;
    loop {
        let settled = match unit_main_state_query(unit) {
            Ok(output) => {
                main_process_exited_evidence(Ok((output.status.success(), &output.stdout)))
            }
            Err(_) => false,
        };
        if settled {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "containment unit {unit} did not settle to a terminal main-process state within {bound:?}"
            )
            .into());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

pub(super) fn wait_for_unit_absent(unit: &str, bound: Duration) -> Result<(), Box<dyn Error>> {
    let service = format!("{unit}.service");
    let deadline = tokio::time::Instant::now()
        .checked_add(bound)
        .ok_or("test bound overflowed")?;
    loop {
        let evidence = bounded_systemctl(&[
            "show",
            &service,
            "--property=Id,LoadState,ActiveState,SubState,MainPID,ControlPID",
        ]);
        let absent = evidence
            .as_ref()
            .map(|output| (output.status.success(), output.stdout.as_slice()))
            .map(|(success, stdout)| absent_unit_evidence(&service, Ok((success, stdout))))
            .unwrap_or(false);
        if absent {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "containment unit {unit} was not absent within {bound:?} of an observed exit"
            )
            .into());
        }
        thread::sleep(Duration::from_millis(50));
    }
}

pub(super) fn linux_test_limits(sample_millis: u64) -> BridgeLimits {
    BridgeLimits {
        rss_bytes: 64 * 1024 * 1024,
        cpu_percent: 100,
        cpu_violation_samples: u16::MAX,
        sample_millis,
        tasks: 16,
        ..crate::protocol::DEFAULT_BRIDGE_LIMITS
    }
}

pub(super) fn spawn_linux_test_process(
    script: &str,
    arguments: &[&str],
    limits: BridgeLimits,
    start_watchdog: bool,
) -> Result<(ProcessControl, String), Box<dyn Error>> {
    let generation = PluginGenerationId::new(1).ok_or("test generation was invalid")?;
    let unit = next_containment_unit(generation)?;
    let mut command = systemd_run_command(&unit, limits);
    command
        .arg("/usr/bin/bash")
        .arg("-c")
        .arg(script)
        .arg("secode-plugin-test");
    command.args(arguments);
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn_with_required_environment()?;
    let process_id = child.id();
    let control = ProcessControl {
        child: Arc::new(Mutex::new(Some(child))),
        violation: Arc::new(Mutex::new(None)),
        exit_observed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        containment: ProcessContainment::Systemd {
            unit: Arc::from(unit.as_str()),
        },
        cleanup: Duration::from_millis(limits.cleanup_millis),
        reap: Duration::from_millis(limits.reap_millis),
        memory_max_bytes: memory_max_bytes(limits.rss_bytes),
    };
    if start_watchdog {
        start_resource_watchdog(control.clone(), process_id, limits)?;
    }
    Ok((control, unit))
}

/// The shared cap for real-manager observations. File receipts can fail
/// earlier after their producer settles. A systemd-run launcher can remain
/// alive through containment teardown after its unit's main process settles.
pub(super) const WITNESS_CAP: Duration = Duration::from_secs(120);

/// How long a settled unit's final file receipt may take to be observed.
const SETTLED_GRACE: Duration = Duration::from_secs(10);

/// Poll `condition` while `unit` is live. A systemctl query that times out
/// is no evidence either way; only a loaded unit whose main process reached
/// a terminal state counts as settled (the attach predicate: not-found may
/// still be starting).
pub(super) fn wait_while_live(
    unit: &str,
    what: &str,
    mut condition: impl FnMut() -> Result<bool, Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    let started = tokio::time::Instant::now();
    let mut settled: Option<tokio::time::Instant> = None;
    loop {
        if condition()? {
            return Ok(());
        }
        let now = tokio::time::Instant::now();
        if settled.is_none() && attach_unit_main_process_exited(unit) {
            settled = Some(now);
        }
        if settled.is_some_and(|at| now.duration_since(at) >= SETTLED_GRACE) {
            return Err(format!("{what}: {unit} settled without it").into());
        }
        if now.duration_since(started) >= WITNESS_CAP {
            return Err(format!("{what}: {unit} stayed live past {WITNESS_CAP:?}").into());
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_exit(control: &ProcessControl, unit: &str) -> Result<(), Box<dyn Error>> {
    wait_for_exit_within(control, unit, WITNESS_CAP)
}

fn wait_for_exit_within(
    control: &ProcessControl,
    unit: &str,
    bound: Duration,
) -> Result<(), Box<dyn Error>> {
    let started = tokio::time::Instant::now();
    loop {
        if control.has_exited()? {
            return Ok(());
        }
        let remaining = bound.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(format!("contained launcher {unit} did not exit within {bound:?}").into());
        }
        thread::sleep(Duration::from_millis(50).min(remaining));
    }
}

fn wait_for_file(path: &Path, unit: &str) -> Result<String, Box<dyn Error>> {
    let mut value = None;
    wait_while_live(
        unit,
        &format!("test receipt {} did not appear", path.display()),
        || {
            value = std::fs::read_to_string(path).ok();
            Ok(value.is_some())
        },
    )?;
    value.ok_or_else(|| "the receipt vanished after it appeared".into())
}

pub(super) fn assert_unit_inactive(unit: &str) -> Result<(), Box<dyn Error>> {
    let status = Command::new("/usr/bin/systemctl")
        .args(["--user", "is-active", "--quiet", &format!("{unit}.service")])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn_with_required_environment()?
        .wait()?;
    assert!(
        !status.success(),
        "containment unit remained active: {unit}"
    );
    Ok(())
}

mod wait;
