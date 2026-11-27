//! The launch window as a 30 s deadline, on the paused tokio clock. The
//! real-clock edges (liveness overrun, probe tail, service-owner reap,
//! drop-kill, detach) are in `launch_window_real`.

use super::*;

/// A startup whose ready receipt lands at 10 s (well past the old 128-poll
/// count) completes inside the window.
#[tokio::test(start_paused = true)]
async fn a_delayed_legitimate_startup_completes_inside_the_window() -> TestResult {
    let root = Scratch::new("window-delayed-startup")?;
    let pid = std::process::id();
    let build = "grapt-cli-public-test";
    let health = format!(
        "{{\"schema\":\"secode.api.health.v1\",\"pid\":{pid},\"build\":\"{build}\",\"environment\":\"test\",\"service_instance\":\"instance-live\"}}"
    );
    let (endpoint, server) =
        spawn_server(vec![Script::json("GET", "/api/health", 200, &health)]).await?;
    write_discovery_fixture(root.path(), pid, &endpoint, build)?;
    let ready_path = root.path().join("serve-ready.json");
    let pending_path = root.path().join(".serve-ready.pending");
    let ready = std::fs::read(&ready_path)?;
    std::fs::remove_file(&ready_path)?;
    let publisher = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        std::fs::write(&pending_path, ready)?;
        std::fs::rename(&pending_path, &ready_path)?;
        // Everything after publication is real loopback I/O: the health
        // probe and the scripted server's one answer. On the paused clock an
        // idle runtime jumps straight to the probe's allowance, so a reply the
        // kernel delivers a moment late under load lost the race, used up the
        // server's only script, and the startup was refused. The paused clock
        // has done its job (the 10 s publication); let real time run.
        tokio::time::resume();
        Ok::<(), std::io::Error>(())
    });
    let discovery = ServiceDiscovery::try_new(root.path(), env!("CARGO_PKG_VERSION"))?;
    let discovered = discovery
        .await_child_readiness(pid, || ChildLiveness::Alive)
        .await;
    publisher.await??;
    if let Err(wait) = discovered {
        return Err(
            format!("a legitimate startup that published at 10 s was refused: {wait:?}").into(),
        );
    }
    assert_eq!(server.await??, ["GET /api/health"]);
    Ok(())
}

/// A registration owned by a foreign pid ends the window naming the mismatch.
#[tokio::test(start_paused = true)]
async fn a_foreign_readiness_squatting_ends_the_window_named() -> TestResult {
    let root = Scratch::new("window-foreign-squat")?;
    let pid = std::process::id();
    let foreign = pid.saturating_add(1);
    write_discovery_fixture(
        root.path(),
        foreign,
        "http://127.0.0.1:9",
        "grapt-cli-public-test",
    )?;
    let discovery = ServiceDiscovery::try_new(root.path(), env!("CARGO_PKG_VERSION"))?;
    let wait = discovery
        .await_child_readiness(pid, || ChildLiveness::Alive)
        .await
        .err()
        .ok_or("a foreign-pid registration is not readiness")?;
    let observation = match wait {
        grapt_cli::ReadinessWait::Window(observation) => observation,
        other => {
            return Err(format!("the foreign squat ended as {other:?}, not the window").into())
        }
    };
    assert!(
        observation.polls() > 0,
        "the window ended without a completed probe"
    );
    match observation.last_pending() {
        Some(DiscoveryError::ChildOwnerMismatch { expected, found }) => {
            assert_eq!(*expected, pid);
            assert_eq!(*found, foreign);
        }
        other => return Err(format!("last probe was {other:?}, not the owner mismatch").into()),
    }
    Ok(())
}

/// A child that exits mid-window ends the wait on the next poll.
#[tokio::test(start_paused = true)]
async fn a_child_that_exits_mid_window_ends_the_wait_at_once() -> TestResult {
    let root = Scratch::new("window-child-exit")?;
    let pid = std::process::id();
    write_discovery_fixture(
        root.path(),
        pid,
        "http://127.0.0.1:9",
        "grapt-cli-public-test",
    )?;
    std::fs::remove_file(root.path().join("serve-ready.json"))?;
    let discovery = ServiceDiscovery::try_new(root.path(), env!("CARGO_PKG_VERSION"))?;
    let mut alive_calls = 0u32;
    let wait = discovery
        .await_child_readiness(pid, || {
            alive_calls += 1;
            if alive_calls > 3 {
                ChildLiveness::Exited("exited mid-window: signal: none, code 1".to_owned())
            } else {
                ChildLiveness::Alive
            }
        })
        .await;
    match wait {
        Err(grapt_cli::ReadinessWait::Child(message)) => {
            assert_eq!(message, "exited mid-window: signal: none, code 1");
        }
        other => {
            return Err(format!("the mid-window exit ended as {other:?}, not the Child arm").into())
        }
    }
    assert_eq!(
        alive_calls, 4,
        "the exit must be seen on the very next poll"
    );
    Ok(())
}

/// A child that dies between its last polled sample and the deadline's final
/// liveness check is reported as its exit, not as the window: the refusal
/// never calls a dead child alive.
#[tokio::test(start_paused = true)]
async fn a_child_that_dies_at_the_deadline_is_reported_as_its_exit_not_the_window() -> TestResult {
    let root = Scratch::new("window-death-at-deadline")?;
    let pid = std::process::id();
    write_discovery_fixture(
        root.path(),
        pid,
        "http://127.0.0.1:9",
        "grapt-cli-public-test",
    )?;
    std::fs::remove_file(root.path().join("serve-ready.json"))?;
    let discovery = ServiceDiscovery::try_new(root.path(), env!("CARGO_PKG_VERSION"))?;
    let mut alive_calls = 0u32;
    let wait = discovery
        .await_child_readiness(pid, || {
            alive_calls += 1;
            if alive_calls >= 1201 {
                ChildLiveness::Exited("exited at the deadline".to_owned())
            } else {
                ChildLiveness::Alive
            }
        })
        .await;
    match wait {
        Err(grapt_cli::ReadinessWait::Child(message)) => {
            assert_eq!(message, "exited at the deadline");
        }
        other => {
            return Err(format!(
                "the death in the last allowance ended as {other:?}, not the Child arm"
            )
            .into())
        }
    }
    assert_eq!(
        alive_calls, 1201,
        "the final liveness sample must be the one that saw the exit"
    );
    Ok(())
}

/// A child that never publishes ends at exactly 30 s, naming its last probe.
#[tokio::test(start_paused = true)]
async fn a_never_ready_child_ends_at_the_wall_clock_bound() -> TestResult {
    let root = Scratch::new("window-never-ready")?;
    let pid = std::process::id();
    write_discovery_fixture(
        root.path(),
        pid,
        "http://127.0.0.1:9",
        "grapt-cli-public-test",
    )?;
    std::fs::remove_file(root.path().join("serve-ready.json"))?;
    let discovery = ServiceDiscovery::try_new(root.path(), env!("CARGO_PKG_VERSION"))?;
    let started = tokio::time::Instant::now();
    let wait = discovery
        .await_child_readiness(pid, || ChildLiveness::Alive)
        .await;
    let elapsed = tokio::time::Instant::now() - started;
    let observation = match wait {
        Err(grapt_cli::ReadinessWait::Window(observation)) => observation,
        other => {
            return Err(format!("the never-ready child ended as {other:?}, not the window").into())
        }
    };
    assert!(
        elapsed >= std::time::Duration::from_secs(30),
        "the window ended at {elapsed:?}, before the 30 s bound"
    );
    assert!(
        elapsed <= std::time::Duration::from_secs(30) + std::time::Duration::from_millis(50),
        "the window ended at {elapsed:?}, past the 30 s bound"
    );
    assert_eq!(
        observation.polls(),
        1200,
        "30 s at the 25 ms pending backoff completes exactly 1200 probes"
    );
    match observation.last_pending() {
        Some(DiscoveryError::ReadyAbsent) => {}
        other => return Err(format!("last probe was {other:?}, not ReadyAbsent").into()),
    }
    Ok(())
}

/// `embedded_client` with a child that never publishes is refused at the
/// window and the child is killed. The cmdline scan cannot tell a zombie from
/// a reaped child; `launch_window_real` witnesses the reap itself.
#[tokio::test(start_paused = true)]
async fn a_windowed_embedded_child_is_refused_named_and_cleaned_up() -> TestResult {
    let root = Scratch::new("window-embedded-child")?;
    let service = root.path().join("service");
    std::fs::create_dir_all(&service)?;
    let stub = root.path().join("stub-product");
    write_executable(
        &stub,
        "#!/usr/bin/env python3\nimport time\ntime.sleep(300)\n",
    )?;
    let stub_path = stub.to_string_lossy().into_owned();

    let result = grapt_cli::embedded_client(
        std::path::PathBuf::from(stub_path.clone()),
        std::path::PathBuf::from(service.to_string_lossy().into_owned()),
        CliLimits::production(),
    )
    .await;
    let (class, message) = match result {
        Err((class, message)) => (class, message),
        Ok(_) => return Err("a child that never publishes cannot be readiness".into()),
    };
    assert_eq!(class, ExitClass::Child);
    // Only the window part is asserted: on a paused clock the virtual 10 s
    // cleanup bound can fire before the real SIGKILL death is observable.
    let log_suffix = format!(
        "; server log: {}",
        service.join("logs/server.log").display()
    );
    let message = message
        .strip_suffix(&log_suffix)
        .ok_or("failure omitted server log path")?;
    let rest = message
        .strip_prefix("the service is still starting (slow disk?): waited 30s; process ")
        .ok_or_else(|| format!("the window refusal lost its honest lead: {message}"))?;
    let (pid_text, rest) = rest
        .split_once(" alive; embedded child did not publish its own attributed readiness within the finite launch window (")
        .ok_or_else(|| format!("the refusal carries no window evidence: {message}"))?;
    let _reported_pid: u32 = pid_text
        .parse()
        .ok()
        .ok_or_else(|| format!("the refusal names no child pid: {message}"))?;
    assert!(
        rest.starts_with("1200 polls; last probe: service registration is absent)"),
        "the window refusal changed shape: {message}"
    );

    // Observed in real time: the /proc scan is real work each pass.
    let mut cleaned = false;
    for _ in 0..200 {
        match pid_state_with_command_line(&stub_path)? {
            None => {
                cleaned = true;
                break;
            }
            Some(_) => {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }
    }
    assert!(
        cleaned,
        "the windowed embedded child never disappeared from the cmdline scan after the failure path"
    );
    Ok(())
}

/// A probe slower than the remaining budget is cut in flight at the deadline
/// and not counted. Against a listener that never answers, each probe uses
/// one to three seconds of virtual time across its independent allowances.
#[tokio::test(start_paused = true)]
async fn a_slow_probe_is_cut_in_flight_at_the_deadline() -> TestResult {
    let root = Scratch::new("window-slow-probe")?;
    let pid = std::process::id();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let accepted = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let counted = accepted.clone();
    let drain = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let _ = tokio::io::copy(&mut stream, &mut tokio::io::sink()).await;
        }
    });
    write_discovery_fixture(root.path(), pid, &endpoint, "grapt-cli-public-test")?;
    let discovery = ServiceDiscovery::try_new(root.path(), env!("CARGO_PKG_VERSION"))?;
    let started = tokio::time::Instant::now();
    let wait = discovery
        .await_child_readiness(pid, || ChildLiveness::Alive)
        .await;
    let elapsed = tokio::time::Instant::now() - started;
    let observation = match wait {
        Err(grapt_cli::ReadinessWait::Window(observation)) => observation,
        other => {
            drain.abort();
            return Err(format!("the slow probe ended as {other:?}, not the window").into());
        }
    };
    // Let the probes' accepts land in the drain's count. The window above ran
    // on the paused clock, but the drain accepts on real sockets, and paused
    // sleeps auto-advance without letting any real time pass: under load the
    // count was read before the drain had caught up ("14 connections were
    // accepted but only 15 probes were counted"). Resume the real clock and
    // allow it a bounded two seconds.
    tokio::time::resume();
    for _ in 0..200 {
        if accepted.load(std::sync::atomic::Ordering::Relaxed) > observation.polls() as u32 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let accepted = accepted.load(std::sync::atomic::Ordering::Relaxed);
    drain.abort();

    assert!(
        elapsed >= std::time::Duration::from_secs(30)
            && elapsed <= std::time::Duration::from_secs(30) + std::time::Duration::from_millis(50),
        "the window ended at {elapsed:?}, not at the 30 s deadline"
    );
    // A connect or response can time out after 1 s; all three allowances total 3 s.
    assert!(
        (9..=29).contains(&observation.polls()),
        "a slow probe must burn most of the window: {polls} polls in {elapsed:?}",
        polls = observation.polls()
    );
    match observation.last_pending() {
        Some(DiscoveryError::Stale(reason)) => {
            assert!(
                reason == "the probe response could not be read within its allowance"
                    || reason == "the connect allowance elapsed",
                "the last completed probe gave out somewhere else: {reason}"
            );
        }
        other => return Err(format!("last probe was {other:?}, not a probe allowance").into()),
    }
    // One connection per probe; only the cut probe goes uncounted.
    assert!(
        accepted <= observation.polls() as u32 + 1 && accepted >= observation.polls() as u32,
        "{accepted} connections were accepted but only {polls} probes were counted",
        polls = observation.polls()
    );
    Ok(())
}

/// Synchronous work in `child_alive` is neither cut by the deadline nor
/// counted against it.
#[tokio::test(start_paused = true)]
async fn synchronous_segments_run_to_completion_outside_the_window_clock() -> TestResult {
    let root = Scratch::new("window-non-yielding")?;
    let pid = std::process::id();
    write_discovery_fixture(
        root.path(),
        pid,
        "http://127.0.0.1:9",
        "grapt-cli-public-test",
    )?;
    std::fs::remove_file(root.path().join("serve-ready.json"))?;
    let discovery = ServiceDiscovery::try_new(root.path(), env!("CARGO_PKG_VERSION"))?;
    let segments = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let work = segments.clone();
    let started = tokio::time::Instant::now();
    let wait = discovery
        .await_child_readiness(pid, move || {
            let mut churn = 0u64;
            for _ in 0..100_000 {
                churn = churn.wrapping_add(31);
            }
            std::hint::black_box(churn);
            work.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            ChildLiveness::Alive
        })
        .await;
    let elapsed = tokio::time::Instant::now() - started;
    let observation = match wait {
        Err(grapt_cli::ReadinessWait::Window(observation)) => observation,
        other => {
            return Err(format!("the never-ready child ended as {other:?}, not the window").into())
        }
    };
    assert!(
        elapsed >= std::time::Duration::from_secs(30)
            && elapsed <= std::time::Duration::from_secs(30) + std::time::Duration::from_millis(50),
        "synchronous segments must not consume window time: the window ended at {elapsed:?}"
    );
    assert_eq!(observation.polls(), 1200, "the window clock is unchanged");
    assert_eq!(
        segments.load(std::sync::atomic::Ordering::Relaxed),
        observation.polls() as u32 + 1,
        "every synchronous segment must complete; the deadline cuts none, and the deadline's final liveness sample adds exactly one"
    );
    Ok(())
}

/// The state char of the process whose command line carries `needle` (None
/// when no such process stands), read from /proc on the Linux test host.
fn pid_state_with_command_line(needle: &str) -> TestResult<Option<char>> {
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let name = entry.file_name();
        let digits: String = name
            .to_string_lossy()
            .chars()
            .filter(|c| c.is_ascii_digit())
            .collect();
        let Ok(pid) = digits.parse::<u32>() else {
            continue;
        };
        let Ok(bytes) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        if !String::from_utf8_lossy(&bytes).contains(needle) {
            continue;
        }
        let state = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .as_deref()
            .and_then(|text| text.rsplit(')').next())
            .and_then(|rest| rest.strip_prefix(' '))
            .and_then(|state| state.chars().next());
        return Ok(state);
    }
    Ok(None)
}

/// An embedded child that exits before readiness is refused with its exact
/// status and is already reaped; no kill is needed.
#[tokio::test(start_paused = true)]
async fn an_embedded_child_that_exits_before_readiness_is_reaped_by_the_proven_exit() -> TestResult
{
    let root = Scratch::new("window-child-exit-code")?;
    let service = root.path().join("service");
    std::fs::create_dir_all(&service)?;
    let pidfile = root.path().join("stub.pid");
    let stub = root.path().join("stub-product");
    write_executable(
        &stub,
        format!(
            "#!/bin/sh\necho $$ > {}\necho server-launch-diagnostic >&2\nexit 3\n",
            pidfile.display()
        ),
    )?;
    let stub_path = stub.to_string_lossy().into_owned();

    let result = grapt_cli::embedded_client(
        std::path::PathBuf::from(stub_path),
        std::path::PathBuf::from(service.to_string_lossy().into_owned()),
        CliLimits::production(),
    )
    .await;
    let (class, message) = match result {
        Err((class, message)) => (class, message),
        Ok(_) => return Err("an exited child cannot be readiness".into()),
    };
    assert_eq!(class, ExitClass::Child);
    assert_eq!(
        message,
        format!(
            "embedded child exited before readiness: exit status: 3; server log: {}",
            service.join("logs/server.log").display()
        )
    );
    assert_eq!(
        std::fs::read(service.join("logs/server.log"))?,
        b"server-launch-diagnostic\n"
    );
    let child_pid: u32 = std::fs::read_to_string(&pidfile)?
        .trim()
        .parse()
        .ok()
        .ok_or("the stub never wrote its own pid")?;
    let mut reaped = false;
    for _ in 0..200 {
        if pid_state_of(child_pid)?.is_none() {
            reaped = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(
        reaped,
        "the proven exit left its exact child unreaped (pid {child_pid})"
    );
    Ok(())
}

/// A failed liveness read ends the wait as `StatusFailed`, never `Child`. A
/// real `try_wait` error cannot be forced on a healthy child, so the liveness
/// report is scripted.
#[tokio::test(start_paused = true)]
async fn a_status_read_that_fails_ends_the_wait_as_a_named_status_failure() -> TestResult {
    let root = Scratch::new("window-status-failed")?;
    let pid = std::process::id();
    write_discovery_fixture(
        root.path(),
        pid,
        "http://127.0.0.1:9",
        "grapt-cli-public-test",
    )?;
    std::fs::remove_file(root.path().join("serve-ready.json"))?;
    let discovery = ServiceDiscovery::try_new(root.path(), env!("CARGO_PKG_VERSION"))?;
    let wait = discovery
        .await_child_readiness(pid, || {
            ChildLiveness::StatusFailed("status read failed: ECHILD".to_owned())
        })
        .await;
    match wait {
        Err(grapt_cli::ReadinessWait::StatusFailed(message)) => {
            assert_eq!(message, "status read failed: ECHILD");
        }
        other => {
            return Err(
                format!("the failed status read ended as {other:?}, not StatusFailed").into(),
            )
        }
    }
    Ok(())
}
