/// Query-seam witnesses for the Linux containment watchdog, split from
/// `tests.rs` so both files stay under the 1000-line ceiling the
/// no_god_files gate enforces (PLR-R10). The split is mechanical: every
/// witness below moved byte-for-byte, the shared helpers and constants
/// in `tests.rs` gained `pub(super)`, and nothing else changed. The
/// stub-based seams stand in for the production systemctl path, which
/// hardcodes /usr/bin/systemctl - a PATH stub cannot intercept it - so
/// the bounds and the fail-closed classifications are witnessed
/// through the injectable-path seams, the same seam law (PLR111).
use super::tests::{
    linux_test_limits, spawn_linux_test_process, wait_for_unit_absent, wait_for_unit_exited,
    TEST_MEMORY_MAX, UNAVAILABLE_REASON,
};
use super::*;
use std::error::Error;

/// The PLR111 witness: the bound lives in the ControlGroup query itself,
/// not only in the attach loop that consumes its verdict (PLR108's seam).
/// The production query hardcodes /usr/bin/systemctl, which a PATH stub
/// cannot intercept, so the witness runs the injectable-path seam against
/// a stalled stub systemctl: green is Err(TimedOut) at the stated bound
/// plus scheduling slack, and the .output() mutant blocks the stub's full
/// sleep and answers Ok(None), which this witness reds on the result.
#[test]
fn a_stalled_control_group_query_is_killed_at_the_stated_bound() -> Result<(), Box<dyn Error>> {
    let stub = std::env::temp_dir().join(format!(
        "secode-plugin-plr111-systemctl-{}",
        std::process::id()
    ));
    let _removal = StubRemoval(stub.clone());
    write_executable_stub(&stub, "#!/bin/sh\nexec /bin/sleep 5\n")?;
    let wait = Duration::from_millis(500);
    let started = tokio::time::Instant::now();
    let result = bounded_show_control_group_at(
        stub.to_str().ok_or("the stub path is not valid UTF-8")?,
        "plr111-witness",
        wait,
    );
    let elapsed = started.elapsed();
    assert!(
        matches!(result, Err(ref error) if error.kind() == std::io::ErrorKind::TimedOut),
        "a stalled ControlGroup query must be killed at the stated bound and fail closed as TimedOut, but the query returned {result:?}"
    );
    assert!(
        elapsed < wait + Duration::from_millis(300),
        "the bounded ControlGroup query outlived the stated bound plus scheduling slack: {elapsed:?} for a {wait:?} bound"
    );
    Ok(())
}

/// Removes its stub on every exit path, including a panic in the witness.
struct StubRemoval(PathBuf);

/// Writes an executable stub through a short-lived `/bin/sh` writer. Exec
/// of a stub this multi-threaded test process wrote itself failed once with
/// ETXTBSY ("Text file busy (os error 26)", light 20260929091311). A
/// standalone probe reproduced that failure only while other threads were
/// forking (19 of 600 execs with four forking threads, 0 of 600 with none)
/// and never with a separate writer (0 of 600 under heavier forking),
/// consistent with a concurrently forked child inheriting the write
/// descriptor; the witnesses' own failure is not otherwise attributed.
fn write_executable_stub(stub: &Path, script: &str) -> Result<(), Box<dyn Error>> {
    let mut writer = std::process::Command::new("/bin/sh");
    writer
        .args([
            "-c",
            "printf '%s' \"$1\" > \"$2\" && chmod 0755 \"$2\"",
            "sh",
            script,
        ])
        .arg(stub);
    run_stub_writer(writer, STUB_WRITER_BOUND)
        .map_err(|failure| format!("the stub writer for {} {failure}", stub.display()).into())
}

/// Bound for the stub writer, far above a short shell write.
const STUB_WRITER_BOUND: Duration = Duration::from_secs(5);
/// Bound for reaping a killed stub writer.
const STUB_WRITER_REAP_BOUND: Duration = Duration::from_secs(2);

/// Runs `writer` to a successful exit within `bound`. An unsuccessful exit
/// is refused; an overrun or unreadable status kills the writer and bounds
/// its reap, naming a failed kill or an unreaped pid. The refusal never
/// carries a witness's diagnostic, so it cannot score as an intended red.
fn run_stub_writer(mut writer: std::process::Command, bound: Duration) -> Result<(), String> {
    let mut child = writer
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("did not start ({error})"))?;
    let failure = match wait_for_writer(&mut child, bound) {
        Ok(Some(status)) if status.success() => return Ok(()),
        Ok(Some(status)) => return Err(format!("exited {status}")),
        Ok(None) => format!("outlived its {bound:?} bound"),
        Err(error) => format!("had an unreadable status ({error})"),
    };
    let kill = child.kill().err().map_or(String::new(), |error| {
        format!("; its kill failed ({error})")
    });
    let reap = match wait_for_writer(&mut child, STUB_WRITER_REAP_BOUND) {
        Ok(Some(status)) => format!("pid {} was killed and reaped ({status})", child.id()),
        Ok(None) => format!(
            "residue: pid {} was not reaped within {STUB_WRITER_REAP_BOUND:?}",
            child.id()
        ),
        Err(error) => format!(
            "residue: pid {} had an unreadable status ({error})",
            child.id()
        ),
    };
    Err(format!("{failure}; {reap}{kill}"))
}

/// The writer's exit status once it exits, or None when `bound` passes.
fn wait_for_writer(
    child: &mut std::process::Child,
    bound: Duration,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    let deadline = tokio::time::Instant::now() + bound;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// A stalled stub writer is killed and reaped at its bound and a failing
/// one is refused, so neither can hang a witness or let it score.
#[test]
fn a_stalled_or_failing_stub_writer_is_refused_within_its_bound() {
    let mut stalled = std::process::Command::new("/bin/sleep");
    stalled.arg("30");
    let bound = Duration::from_millis(200);
    let started = tokio::time::Instant::now();
    let overrun = run_stub_writer(stalled, bound);
    let elapsed = started.elapsed();
    assert!(
        overrun.as_ref().is_err_and(
            |failure| failure.contains("outlived") && failure.contains("killed and reaped")
        ),
        "a stalled stub writer was not killed and reaped: {overrun:?}"
    );
    assert!(
        elapsed < bound + Duration::from_millis(700),
        "a stalled stub writer held the witness for {elapsed:?}"
    );
    // A failing writer is refused by its exit status within the writer's own
    // bound; it exits after the stall bound so the two branches stay distinct.
    let mut failing_late = std::process::Command::new("/bin/sh");
    failing_late.args(["-c", "sleep 0.5; exit 3"]);
    let failing = run_stub_writer(failing_late, STUB_WRITER_BOUND);
    assert!(
        failing
            .as_ref()
            .is_err_and(|failure| failure.starts_with("exited") && failure.contains('3')),
        "a failing stub writer was accepted: {failing:?}"
    );
}

impl Drop for StubRemoval {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The PLR-R8 deferring-branch seam witness: a settled terminal
/// SubState under a readable LoadState is read as an exited main
/// process. `failed` is the settled shape of a non-zero exit outside
/// SuccessExitStatus - the process-loss shape whose exit the lifecycle
/// red hid under a telemetry loss (shape probe on this box, exact
/// production unit shape: SubState=exited by 100 ms for the SIGKILL
/// shape, SubState=failed by 290 ms for the exit-71 shape) - and
/// `dead` covers the stopped or reset unit.
#[test]
fn main_process_exit_evidence_defers_on_settled_terminal_states() {
    for evidence in [
        b"LoadState=loaded\nSubState=exited\n".as_slice(),
        b"LoadState=loaded\nSubState=failed\n".as_slice(),
        b"LoadState=not-found\nSubState=dead\n".as_slice(),
        // A response without the final newline is still complete evidence.
        b"LoadState=loaded\nSubState=exited".as_slice(),
    ] {
        assert!(
            main_process_exited_evidence(Ok((true, evidence))),
            "a settled terminal unit state was not read as an exited main process"
        );
    }
}

/// The PLR-R8 fail-closed-branch seam witness: every answer that is not
/// a settled terminal state - an unsettled SubState, an unrecognised
/// LoadState, a missing property, overlong or non-UTF-8 stdout, a
/// failed query, and the transport error - keeps the fail-close. An
/// unknown state may never defer.
#[test]
fn main_process_exit_evidence_fails_closed_on_every_unsettled_state() {
    for evidence in [
        Err(()),
        Ok((false, b"LoadState=loaded\nSubState=exited\n".as_slice())),
        Ok((true, b"LoadState=loaded\nSubState=running\n".as_slice())),
        Ok((true, b"LoadState=loaded\nSubState=activating\n".as_slice())),
        Ok((true, b"LoadState=loaded\n".as_slice())),
        Ok((true, b"SubState=exited\n".as_slice())),
        Ok((true, b"LoadState=bad-setting\nSubState=exited\n".as_slice())),
        Ok((
            true,
            b"LoadState=not-a-systemd-state\nSubState=exited\n".as_slice(),
        )),
        Ok((true, &[0xff_u8][..])),
        Ok((true, b"".as_slice())),
    ] {
        assert!(
            !main_process_exited_evidence(evidence),
            "an unsettled or unreadable unit state deferred the fail-close"
        );
    }
}

/// The PLR-R8 bound witness: the unit-exit query itself is the bounded
/// subprocess - polled, killed and reaped at the stated allowance -
/// the same seam law as the ControlGroup query (PLR111). Green is
/// Err(TimedOut) at the allowance plus scheduling slack; a mutation
/// that swaps the bounded stack for a timeout-less .output() blocks
/// the stub's full sleep and reds on both the result and the elapsed
/// bound.
#[test]
fn a_stalled_unit_exit_query_is_killed_at_the_stated_bound() -> Result<(), Box<dyn Error>> {
    let stub = std::env::temp_dir().join(format!(
        "secode-plugin-plr112-systemctl-{}",
        std::process::id()
    ));
    let _removal = StubRemoval(stub.clone());
    write_executable_stub(&stub, "#!/bin/sh\nexec /bin/sleep 30\n")?;
    let started = tokio::time::Instant::now();
    let result = unit_main_state_query_at(
        stub.to_str().ok_or("the stub path is not valid UTF-8")?,
        "plr112-witness.service",
    );
    let elapsed = started.elapsed();
    assert!(
        matches!(result, Err(ref error) if error.kind() == std::io::ErrorKind::TimedOut),
        "a stalled unit-exit query must be killed at the stated bound and fail closed as TimedOut, but the query returned {result:?}"
    );
    assert!(
        elapsed < UNIT_MAIN_STATE_ALLOWANCE + Duration::from_millis(300),
        "the bounded unit-exit query outlived the stated allowance plus scheduling slack: {elapsed:?} for a {UNIT_MAIN_STATE_ALLOWANCE:?} allowance"
    );
    Ok(())
}

/// The PLR-R9 bound witness (the disclosed correction to the
/// pre-review bound formula): the containment (OOM-verdict) query is the
/// bounded subprocess - polled, killed and reaped at the stated
/// allowance - the same seam law as the ControlGroup query (PLR111) and
/// the unit-exit query (PLR112). Green is the UNAVAILABLE fail-closed
/// classification at the allowance plus scheduling slack; the .output()
/// mutant blocks the stub's full sleep and reds on the elapsed bound.
#[test]
fn a_stalled_containment_query_is_killed_at_the_stated_bound() -> Result<(), Box<dyn Error>> {
    let stub = std::env::temp_dir().join(format!(
        "secode-plugin-plr114-systemctl-{}",
        std::process::id()
    ));
    let _removal = StubRemoval(stub.clone());
    write_executable_stub(&stub, "#!/bin/sh\nexec /bin/sleep 30\n")?;
    let started = tokio::time::Instant::now();
    let result = containment_violation_evidence_at(
        stub.to_str().ok_or("the stub path is not valid UTF-8")?,
        "plr114-witness",
        TEST_MEMORY_MAX,
    );
    let elapsed = started.elapsed();
    assert!(
        matches!(result, Err(ref error) if error.contains(UNAVAILABLE_REASON)),
        "a stalled containment query must fail closed as UNAVAILABLE, but the query returned {result:?}"
    );
    assert!(
        elapsed < UNIT_MAIN_STATE_ALLOWANCE + Duration::from_millis(300),
        "the bounded containment query outlived the stated allowance plus scheduling slack: {elapsed:?} for a {UNIT_MAIN_STATE_ALLOWANCE:?} allowance"
    );
    Ok(())
}

/// One PLR112 scenario instruction for the watchdog's next sample.
enum SamplerStep {
    /// Read the attached cgroup exactly as production does.
    Actual,
    /// Report this decision's sample as missing at the sampler boundary.
    Absent,
}

/// What the PLR112 scenario observes at the sampler boundary.
enum SamplerEvent {
    /// The watchdog asked for a sample from the cgroup it attached to and
    /// is now held until the scenario answers.
    Called(PathBuf),
    /// An `Actual` step completed; true when every field was readable.
    Read(bool),
}

/// Bound on every PLR112 handshake. It covers one decision's bounded
/// systemctl queries (containment and settled state, 10 s each) plus
/// the sample interval, so a timeout means a lost window, never a slow
/// decision.
const PLR112_HANDSHAKE: Duration = Duration::from_secs(25);

/// Missing decisions after which the scenario stops feeding absence;
/// the fail-close under the mutant lands at the third.
const PLR112_MAX_DECISIONS: u32 = 8;

/// The PLR112 witness: the watchdog's missing-telemetry branch must
/// defer a settled unit exit instead of fail-closing over it (PLR-R8).
/// The window is held at the sampler seam, not raced: a real unit and
/// its real systemd-run launcher are started, the watchdog attaches to
/// the unit's cgroup and one actual sample is acknowledged, then the
/// watchdog is held at its next sample while the unit's published main
/// process is SIGKILLed and its terminal state is awaited through the
/// production bounded query. Only then is absence injected at the
/// sampler boundary - the physical cgroup is never removed by the
/// witness (the retained removal probes are mechanism history). Green
/// ends the watchdog at the FIRST missing decision with nothing
/// recorded while the launcher is still live. Deleting the settled-exit
/// check (PLR112) makes the watchdog ask again; each further decision
/// is fed absence only while the launcher is still live, and the third
/// records the telemetry loss this witness reds on. A launcher that
/// exits before a decision, or a handshake that times out, is a lost
/// window and voids the run.
#[test]
fn linux_watchdog_defers_a_settled_unit_exit_instead_of_fail_closing() -> Result<(), Box<dyn Error>>
{
    let limits = linux_test_limits(50);
    let (control, unit) = spawn_linux_test_process("exec /usr/bin/sleep 300", &[], limits, false)?;
    let (event_sender, events) = std::sync::mpsc::sync_channel(2);
    let (steps, step_receiver) = std::sync::mpsc::sync_channel(1);
    let started = start_cgroup_watchdog_sampling(
        control.clone(),
        Arc::from(unit.as_str()),
        limits,
        move |cgroup: &Path| {
            if event_sender
                .try_send(SamplerEvent::Called(cgroup.to_path_buf()))
                .is_err()
            {
                return None;
            }
            match step_receiver.recv_timeout(PLR112_HANDSHAKE) {
                Ok(SamplerStep::Actual) => {
                    let sample = read_cgroup_sample(cgroup);
                    let _ = event_sender.try_send(SamplerEvent::Read(sample.is_some()));
                    sample
                }
                Ok(SamplerStep::Absent) | Err(_) => None,
            }
        },
    );
    let outcome = started
        .map_err(Box::<dyn Error>::from)
        .and_then(|()| plr112_scenario(&control, &unit, limits, &events, &steps));
    let cleanup = control.terminate();
    // Releases a watchdog still held at the seam; it then observes the
    // reaped launcher and ends.
    drop(steps);
    let absent = wait_for_unit_absent(&unit, Duration::from_secs(30));
    judge_plr112(outcome, cleanup, absent)
}

/// Judges the scenario only once its cleanup is proven. A failed launcher
/// reap or an unproven unit absence voids the run before the outcome is
/// read, so neither the intended diagnostic nor a pass can score over it.
fn judge_plr112(
    outcome: Result<Plr112Outcome, Box<dyn Error>>,
    cleanup: Result<(), PluginError>,
    absent: Result<(), Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    if cleanup.is_err() || absent.is_err() {
        return Err(format!(
            "the PLR112 cleanup was not proven, so the run is void: launcher reap \
             {cleanup:?}, unit absence {absent:?}"
        )
        .into());
    }
    let Plr112Outcome {
        recorded,
        decisions,
        launcher_live,
        still_settled,
    } = outcome?;
    assert!(
        recorded.is_none(),
        "the watchdog fail-closed over a known exit after {decisions} missing decisions: {recorded:?}"
    );
    if !launcher_live {
        return Err("the launcher exited before the watchdog ended; the deferral is unattributed and the run is void".into());
    }
    assert_eq!(
        decisions, 1,
        "the watchdog did not defer at the first missing sample"
    );
    assert!(
        still_settled,
        "the watchdog stopped a unit whose main process had already exited"
    );
    Ok(())
}

/// The outcome the PLR112 deletion mutant produces, marked as a control.
fn plr112_intended_red() -> Result<Plr112Outcome, Box<dyn Error>> {
    Ok(Plr112Outcome {
        recorded: Some(Violation {
            kind: ViolationKind::Other,
            reason: "control: intended red with failed cleanup".to_owned(),
        }),
        decisions: 3,
        launcher_live: true,
        still_settled: true,
    })
}

/// A failed launcher reap or an unproven unit absence voids the run even
/// when the scenario carries the intended red, and even when it passed.
#[test]
fn a_plr112_cleanup_failure_voids_the_intended_red_and_a_pass() {
    let passed = || {
        Ok(Plr112Outcome {
            recorded: None,
            decisions: 1,
            launcher_live: true,
            still_settled: true,
        })
    };
    for outcome in [plr112_intended_red as fn() -> _, passed] {
        for (reap_failed, absence_failed) in [(true, false), (false, true), (true, true)] {
            let cleanup = if reap_failed {
                Err(PluginError::Poisoned)
            } else {
                Ok(())
            };
            let absent = if absence_failed {
                Err("owned unit absence unproven".into())
            } else {
                Ok(())
            };
            let judged = judge_plr112(outcome(), cleanup, absent)
                .err()
                .map(|error| error.to_string());
            assert!(
                judged
                    .as_deref()
                    .is_some_and(|message| message.contains("cleanup was not proven")),
                "a PLR112 run was scored over unproven cleanup: {judged:?}"
            );
        }
    }
}

/// With cleanup proven, the intended red still fires unchanged.
#[test]
#[should_panic(expected = "the watchdog fail-closed over a known exit after 3 missing decisions")]
fn a_proven_plr112_cleanup_lets_the_intended_red_fire() {
    let _ = judge_plr112(plr112_intended_red(), Ok(()), Ok(()));
}

/// What the held-window scenario observed once the watchdog ended.
struct Plr112Outcome {
    recorded: Option<Violation>,
    /// Missing decisions fed while the launcher was live.
    decisions: u32,
    /// Whether the launcher was still live when the watchdog ended.
    launcher_live: bool,
    /// Whether the unit still read as a settled exit then.
    still_settled: bool,
}

/// The held-window scenario.
fn plr112_scenario(
    control: &ProcessControl,
    unit: &str,
    limits: BridgeLimits,
    events: &std::sync::mpsc::Receiver<SamplerEvent>,
    steps: &std::sync::mpsc::SyncSender<SamplerStep>,
) -> Result<Plr112Outcome, Box<dyn Error>> {
    let attach = Duration::from_millis(limits.startup_millis)
        .checked_add(PLR112_HANDSHAKE)
        .ok_or("the PLR112 attach bound overflowed")?;
    let Ok(SamplerEvent::Called(cgroup)) = events.recv_timeout(attach) else {
        return Err("the watchdog never asked for a first sample; the run is void".into());
    };
    steps
        .try_send(SamplerStep::Actual)
        .map_err(|_| "the PLR112 actual-sample handshake was refused")?;
    let Ok(SamplerEvent::Read(true)) = events.recv_timeout(PLR112_HANDSHAKE) else {
        return Err("no actual cgroup sample was acknowledged; the run is void".into());
    };
    // The watchdog is now held at its next sample.
    match events.recv_timeout(PLR112_HANDSHAKE) {
        Ok(SamplerEvent::Called(next)) if next == cgroup => {}
        _ => {
            return Err(
                "the watchdog did not return to its attached cgroup; the run is void".into(),
            )
        }
    }
    let main_pid = published_main_pid(unit)?;
    let procs = std::fs::read_to_string(cgroup.join("cgroup.procs"))
        .map_err(|error| format!("the PLR112 cgroup receipt was unreadable: {error}"))?;
    let pids = procs
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            line.trim().parse::<u32>().map_err(|error| {
                format!("the PLR112 cgroup receipt {procs:?} was invalid: {error}")
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if !pids.contains(&main_pid) {
        return Err(format!(
            "the published MainPID {main_pid} is not in the attached cgroup {pids:?}; the run is void"
        )
        .into());
    }
    for pid in pids {
        let mut kill = Command::new("/usr/bin/kill")
            .arg("-9")
            .arg(pid.to_string())
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn_with_required_environment()?;
        if !kill.wait()?.success() {
            return Err(format!("the PLR112 SIGKILL injection failed for pid {pid}").into());
        }
    }
    wait_for_unit_exited(unit, Duration::from_secs(10))?;
    let mut decisions = 0u32;
    loop {
        if control.has_exited()? {
            return Err("the launcher exited inside the decision window; the run is void".into());
        }
        if decisions == PLR112_MAX_DECISIONS {
            break;
        }
        steps
            .try_send(SamplerStep::Absent)
            .map_err(|_| "the PLR112 absence handshake was refused")?;
        decisions += 1;
        match events.recv_timeout(PLR112_HANDSHAKE) {
            Ok(SamplerEvent::Called(_)) => {}
            Ok(SamplerEvent::Read(_)) => {
                return Err("the watchdog read a sample it was told was absent".into());
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                return Err(
                    "the watchdog neither ended nor sampled again in its bound; the run is void"
                        .into(),
                );
            }
        }
    }
    let recorded = control.recorded_violation();
    let launcher_live = !control.has_exited()?;
    let still_settled = match unit_main_state_query(unit) {
        Ok(output) => main_process_exited_evidence(Ok((output.status.success(), &output.stdout))),
        Err(_) => false,
    };
    Ok(Plr112Outcome {
        recorded,
        decisions,
        launcher_live,
        still_settled,
    })
}

/// The unit's published main process, read through the bounded query.
fn published_main_pid(unit: &str) -> Result<u32, Box<dyn Error>> {
    let service = format!("{unit}.service");
    let output = bounded_systemctl(&["show", &service, "--property=MainPID"])?;
    if !output.status.success() {
        return Err(format!("the MainPID query for {service} failed").into());
    }
    let text = std::str::from_utf8(&output.stdout)?;
    let pid = text
        .lines()
        .find_map(|line| line.strip_prefix("MainPID="))
        .ok_or("the MainPID property is missing")?
        .parse::<u32>()?;
    if pid == 0 {
        return Err(format!("{service} published no main process; the run is void").into());
    }
    Ok(pid)
}

/// The PLR108 witness: the attach budget is the generation's own
/// startup bound, not a fixed attempt count. The cgroup query reports
/// "not published yet" for 40 polls - past the retired fixed 30 x sample
/// window, then answers with the running unit's real cgroup - so the
/// bounded attach must still find it. Restoring the fixed count (PLR108)
/// gives up first and reds here. The seam controls query availability
/// rather than the clock: `locate_cgroup_with` still reads a real
/// `Instant` and sleeps `sample.min(remaining)` between polls, so this
/// spends about 400 ms of real time (40 x the 10 ms sample) inside the
/// unchanged 10 s bound instead of the old 6 s wall-clock sleep. It
/// replaces a fixture that consumed most of the margin, not every
/// dependence on scheduling; a host stalled past the remaining bound
/// can still end the witness.
#[test]
fn linux_attach_survives_a_delayed_cgroup_within_the_startup_bound() -> Result<(), Box<dyn Error>> {
    let limits = linux_test_limits(10);
    let (control, unit) = spawn_linux_test_process("exec /usr/bin/sleep 300", &[], limits, false)?;
    // Past the retired fixed window's 30 polls, far inside the 10 s bound.
    const DELAYED_POLLS: u32 = 40;
    let polls = std::cell::Cell::new(0u32);
    let delayed = |_unit: &str, wait: Duration| {
        polls.set(polls.get() + 1);
        if polls.get() <= DELAYED_POLLS {
            return Ok(None);
        }
        bounded_show_control_group(&unit, wait)
    };
    let sample = Duration::from_millis(limits.sample_millis);
    let bound = Duration::from_millis(limits.startup_millis);
    // Judge existence while the unit is still running: stopping it retires the
    // cgroup, so a post-cleanup check would only ever report absence.
    let located = (|| -> Result<PathBuf, Box<dyn Error>> {
        let cgroup = locate_cgroup_with(&control, &unit, sample, bound, &delayed)
            .ok_or("attach gave up before the delayed cgroup appeared within the startup bound")?;
        if !cgroup.exists() {
            return Err(format!("located cgroup vanished: {cgroup:?}").into());
        }
        Ok(cgroup)
    })();
    let cleanup = control.terminate();
    cleanup?;
    located?;
    Ok(())
}

/// The R6 stalled-query witness: a query that stalls beyond its per-call
/// bound still ends the attach within the stated bound. The seam stands
/// in for the bounded subprocess - production kills and reaps the stalled
/// query at `wait` - by reporting the same overrun outcome after the full
/// wait elapses; the attach must return None at the deadline, never
/// later. A resurrection that drops the per-call bound (PLR108's fixed
/// attempt count) runs the whole fixed loop past the deadline and reds
/// on the elapsed bound.
#[test]
fn linux_attach_ends_within_the_bound_when_the_query_stalls() -> Result<(), Box<dyn Error>> {
    let limits = linux_test_limits(100);
    let (control, unit) = spawn_linux_test_process("exec /usr/bin/sleep 300", &[], limits, false)?;
    let bound = Duration::from_millis(2000);
    let sample = Duration::from_millis(50);
    let stalled = |_unit: &str, wait: Duration| {
        // The production bound kills and reaps the stalled subprocess at
        // `wait`; the seam reports the same overrun outcome.
        thread::sleep(wait);
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "query outlived its bound",
        ))
    };
    // Precondition: the attach must start while the process is alive. If
    // the environment took it before the attach (a unit that failed to
    // start under load), candidate and mutant both return None at once
    // and this row's red is no longer the law it guards: void the run
    // loudly instead of scoring a misleading red.
    if control.has_exited()? {
        return Err(format!(
            "the witness process for {unit} exited before the attach started; the run is void"
        )
        .into());
    }
    let started = tokio::time::Instant::now();
    let cgroup = locate_cgroup_with(&control, &unit, sample, bound, &stalled);
    let elapsed = started.elapsed();
    // Stop only after the attach settles: stopping first races the unit
    // activation and voids the witness on a healthy manager. A busy user
    // manager can lose the stop job after the kill lands, or answer the
    // evidence query before the unit has left: retry the stop once, then
    // require proven absence either way - a surviving unit still fails
    // the test loudly.
    let cleanup = control.terminate();
    if let Err(first) = cleanup {
        match control.terminate() {
            Ok(()) => {}
            Err(retry) => {
                wait_for_unit_absent(&unit, Duration::from_secs(30)).map_err(|_| {
                    format!(
                        "containment unit {unit} survived the stop: first {first}, retry {retry}"
                    )
                })?;
            }
        }
    }
    assert!(
        cgroup.is_none(),
        "a stalled query was read as an attached cgroup"
    );
    assert!(
        elapsed <= bound + Duration::from_millis(500),
        "the stalled attach outlived the stated bound plus slack: {elapsed:?}"
    );
    assert!(
        elapsed >= bound - Duration::from_millis(100),
        "the stalled attach gave up before the stated bound: {elapsed:?}"
    );
    Ok(())
}

/// The PLR115 witness (R11, reshaped from the end-to-end stopped-unit
/// construction): the attach-failure branch's decision seam over a
/// unit's main-process exit evidence. The R10 shared predicate
/// accepted LoadState not-found as a settled exit, and a never-created
/// unit reads not-found/dead with rc 0 on this carrier (review,
/// verified on-box) - so the attach branch deferred the not-yet-created
/// state and returned silently, leaving a unit activating just after
/// the attach bound unwatched (finding
/// PLR-R11-72933DEA-ATTACH-NOTFOUND-FAILS-OPEN, the RETURN on
/// 80f3c50e). R11 splits the decision: the sampling branch keeps
/// not-found (its unit was attached, so a unit gone there is a settled
/// exit), and the attach predicate defers ONLY a settled terminal
/// SubState under LoadState loaded - not-found first, then every
/// unsettled, unrecognised, or unreadable state, fails the attach
/// fail-close closed. The end-to-end half of this law is the PLR116
/// control witness. The stopped-then-absent construction this witness
/// replaced assembled exactly the not-found shape that must now fail
/// closed, which is why it is retired with the stated reason and the
/// law is witnessed at the decision level, the same class as the
/// PLR113 seam row.
#[test]
fn attach_process_exit_evidence_defers_only_a_settled_loaded_terminal_state() {
    for evidence in [
        b"LoadState=loaded\nSubState=exited\n".as_slice(),
        b"LoadState=loaded\nSubState=failed\n".as_slice(),
        b"LoadState=loaded\nSubState=dead\n".as_slice(),
        // A response without the final newline is still complete evidence.
        b"LoadState=loaded\nSubState=exited".as_slice(),
    ] {
        assert!(
            attach_process_exited_evidence(Ok((true, evidence))),
            "a settled loaded terminal unit state was not read as an exited main process"
        );
    }
    for evidence in [
        Err(()),
        Ok((false, b"LoadState=loaded\nSubState=exited\n".as_slice())),
        // The R11 cell: the never-created unit reads not-found/dead with rc 0.
        Ok((true, b"LoadState=not-found\nSubState=dead\n".as_slice())),
        Ok((true, b"LoadState=not-found\nSubState=exited\n".as_slice())),
        Ok((true, b"LoadState=not-found\nSubState=failed\n".as_slice())),
        Ok((true, b"LoadState=loaded\nSubState=running\n".as_slice())),
        Ok((true, b"LoadState=loaded\nSubState=activating\n".as_slice())),
        Ok((true, b"LoadState=loaded\n".as_slice())),
        Ok((true, b"SubState=exited\n".as_slice())),
        Ok((true, b"LoadState=bad-setting\nSubState=exited\n".as_slice())),
        Ok((true, b"LoadState=not-a-systemd-state\nSubState=exited\n")),
        Ok((true, &[0xff_u8][..])),
        Ok((true, &[0x20_u8; 4097][..])),
        Ok((true, b"".as_slice())),
    ] {
        assert!(
            !attach_process_exited_evidence(evidence),
            "an unsettled, not-found, or unreadable unit state deferred the attach fail-close"
        );
    }
}

/// The PLR116 witness: the attach-failure branch fails closed over a
/// never-created unit - the R11 control for
/// PLR-R11-72933DEA-ATTACH-NOTFOUND-FAILS-OPEN. The never-created unit
/// name is the exact shape review verified on-box:
/// LoadState=not-found, SubState=dead, OOMKills=UINT64_MAX,
/// Result=success, rc 0. The unit is never started - no systemd-run,
/// no stop, no receipt beyond its constructed absence - so the shape
/// cannot be explained by a timing race, and its containment id is
/// consumed from the process-global counter, so no future real unit
/// can take the name. The watchdog's control carries a real live child
/// (an owned sleep), so the branch's dead-child check passes and the
/// OOM verdict classifies the not-found unit as absence by
/// construction; the attach predicate is the one decision that must
/// NOT defer, and the fail-close verdict is the witness. Under the R10
/// shared predicate the branch deferred this exact shape and returned
/// silently - a unit activating just after the attach bound would then
/// run unwatched, with no RSS or CPU policy.
#[test]
fn linux_attach_failure_fails_closed_over_a_never_created_unit() -> Result<(), Box<dyn Error>> {
    let limits = BridgeLimits {
        startup_millis: 2_000,
        ..linux_test_limits(50)
    };
    let unit =
        next_containment_unit(PluginGenerationId::new(9).ok_or("test generation was invalid")?)?;
    let child = Command::new("/usr/bin/sleep")
        .arg("300")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn_with_required_environment()?;
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
    let outcome = (|| -> Result<(), Box<dyn Error>> {
        if control.has_exited()? {
            return Err(
                "the PLR116 stand-in child exited before the watchdog started; the run is void"
                    .into(),
            );
        }
        start_resource_watchdog(control.clone(), 0, limits)?;
        // The 2 s attach bound (a never-created unit answers Ok(None) on
        // every poll, so the deadline is the whole bound) plus the two
        // bounded branch queries and scheduling slack: the verdict stands
        // well inside this sleep.
        thread::sleep(Duration::from_millis(4_000));
        let recorded = control.recorded_violation();
        assert!(
            matches!(
                recorded.as_ref(),
                Some(violation)
                    if violation.kind == ViolationKind::Other
                        && violation.reason
                            == "plugin cgroup telemetry was unavailable; the owned bridge was terminated fail-closed"
            ),
            "the attach-failure branch did not fail closed over a never-created unit: {recorded:?}"
        );
        Ok(())
    })();
    let cleanup = control.terminate();
    outcome?;
    cleanup?;
    Ok(())
}
