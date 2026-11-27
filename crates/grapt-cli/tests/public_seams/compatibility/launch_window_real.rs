//! Launch-window edges a paused clock cannot show: a real liveness overrun,
//! and the real process table around failed, aborted and successful launches.

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

/// A liveness check that overruns the deadline (900 ms of real time, starting
/// 700 ms before it) ends the window without a refunded probe: the overrun
/// call plus the deadline's final liveness sample, and no probe after.
#[tokio::test]
async fn a_liveness_segment_that_overruns_the_deadline_ends_the_window_without_refunding_the_probe(
) -> TestResult {
    let root = Scratch::new("window-liveness-overrun")?;
    let pid = std::process::id();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let answer =
        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let server = tokio::spawn(five_hundred_server(listener, answer.to_owned()));
    write_discovery_fixture(root.path(), pid, &endpoint, "grapt-cli-public-test")?;
    let discovery = ServiceDiscovery::try_new(root.path(), env!("CARGO_PKG_VERSION"))?;
    let alive_calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let counted = alive_calls.clone();
    let started = tokio::time::Instant::now();
    let deadline = started + std::time::Duration::from_secs(30);
    let trigger = deadline - std::time::Duration::from_millis(700);
    let wait = discovery
        .await_child_readiness(pid, move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if tokio::time::Instant::now() >= trigger {
                std::thread::sleep(std::time::Duration::from_millis(900));
            }
            ChildLiveness::Alive
        })
        .await;
    server.abort();
    let elapsed = tokio::time::Instant::now() - started;
    let observation = match wait {
        Err(grapt_cli::ReadinessWait::Window(observation)) => observation,
        other => {
            return Err(format!("the overrun liveness ended as {other:?}, not the window").into())
        }
    };
    assert!(
        elapsed >= std::time::Duration::from_secs(30),
        "the window ended at {elapsed:?}, before the 30 s bound"
    );
    assert!(
        elapsed <= std::time::Duration::from_secs(30) + std::time::Duration::from_millis(2_000),
        "the window ended at {elapsed:?}, past the bound plus both controlled overruns"
    );
    let alive_calls = alive_calls.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        alive_calls,
        observation.polls() as u32 + 2,
        "a probe after the overrun liveness would only complete on a refunded budget: {alive_calls} liveness calls (the overrun plus the final check) for {} polls",
        observation.polls()
    );
    match observation.last_pending() {
        Some(DiscoveryError::Stale(reason)) => {
            assert_eq!(
                reason, "the probe returned status 500",
                "the answer must be the refused 500: {reason}"
            );
        }
        other => return Err(format!("last probe was {other:?}, not the refused 500").into()),
    }
    Ok(())
}

/// Answers every connection with a 500.
async fn five_hundred_server(listener: tokio::net::TcpListener, answer: String) {
    while let Ok((mut stream, _)) = listener.accept().await {
        let answer = answer.clone();
        tokio::spawn(async move {
            let _ = read_probe_request(&mut stream).await;
            let _ = stream.write_all(answer.as_bytes()).await;
        });
    }
}

/// Reads one probe request up to its blank line; false when the peer closed.
async fn read_probe_request(stream: &mut tokio::net::TcpStream) -> std::io::Result<bool> {
    const MAX_REQUEST: usize = 8_192;
    let mut bytes: Vec<u8> = Vec::new();
    let mut chunk = [0_u8; 1_024];
    loop {
        let amount = stream.read(&mut chunk).await?;
        if amount == 0 {
            return Ok(false);
        }
        if bytes.len().saturating_add(amount) > MAX_REQUEST {
            return Ok(true);
        }
        bytes.extend_from_slice(&chunk[..amount]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(true);
        }
    }
}

/// `grapt service start` with a product that never publishes is refused at
/// the real 30 s window, and its exact child is reaped before the CLI exits.
#[tokio::test]
#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
async fn a_windowed_service_owner_is_refused_named_and_reaped() -> TestResult {
    let root = Scratch::new("window-service-owner")?;
    let service = root.path().join("service");
    std::fs::create_dir_all(&service)?;
    let owned_pid = root.path().join("owned.pid");
    let stub = root.path().join("stub-product");
    write_executable(
        &stub,
        format!(
            "#!/bin/sh\necho $$ > {}\nexec sleep 120\n",
            owned_pid.display()
        ),
    )?;

    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"))
        .args(["service", "start", "--service"])
        .arg(&service)
        .arg("--product")
        .arg(&stub)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let started = tokio::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if tokio::time::Instant::now() - started > std::time::Duration::from_secs(60) {
            child.kill().await?;
            return Err(
                "the launch window is not finite: the refusal did not arrive within 60 s".into(),
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    };
    let elapsed = tokio::time::Instant::now() - started;
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        pipe.read_to_string(&mut stderr).await?;
    }
    let mut stdout_drain = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        pipe.read_to_end(&mut stdout_drain).await?;
    }

    assert!(
        elapsed >= std::time::Duration::from_millis(29_500)
            && elapsed <= std::time::Duration::from_secs(40),
        "the service-owner window ended at {elapsed:?}, not at the 30 s real-time bound"
    );
    assert_eq!(
        status.code(),
        Some(i32::from(ExitClass::Child.code())),
        "the refusal must carry the child exit class; stderr: {stderr}"
    );
    let line = stderr
        .lines()
        .find(|line| line.starts_with("grapt: "))
        .ok_or_else(|| format!("the refusal line is missing from stderr: {stderr}"))?;
    let log_suffix = format!(
        "; server log: {}",
        service.join("logs/server.log").display()
    );
    let line = line
        .strip_suffix(&log_suffix)
        .ok_or("failure omitted server log path")?;
    let alive_prefix = "grapt: the service is still starting (slow disk?): waited 30s; process ";
    let alive_mid = " alive; child did not publish its own attributed readiness within the finite launch window (";
    let tail = " polls; last probe: service registration is absent)";
    let Some(rest) = line.strip_prefix(alive_prefix) else {
        return Err(format!("the refusal is not the honest window refusal: {line}").into());
    };
    let Some((pid_text, rest)) = rest.split_once(alive_mid) else {
        return Err(format!("the refusal carries no window evidence: {line}").into());
    };
    let reported_pid: u32 = pid_text
        .parse()
        .ok()
        .ok_or_else(|| format!("the refusal names no child pid: {line}"))?;
    let Some(rest) = rest.strip_suffix(tail) else {
        return Err(format!("the refusal carries no window evidence: {line}").into());
    };
    let polls: u32 = rest
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
        .ok_or_else(|| format!("the poll count is not numeric in: {line}"))?;
    assert!(
        (400..=1260).contains(&polls),
        "the window ran {polls} probes, not the 25 ms pending pacing"
    );
    let child_pid: u32 = std::fs::read_to_string(&owned_pid)?
        .trim()
        .parse()
        .ok()
        .ok_or("the stub never wrote its own pid")?;
    assert_eq!(
        reported_pid, child_pid,
        "the refusal must name the still-alive child it reaps"
    );
    let mut reaped = false;
    for _ in 0..40 {
        if pid_state_of(child_pid)?.is_none() {
            reaped = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        reaped,
        "the failed launch left its exact child alive or as a zombie (pid {child_pid})"
    );
    Ok(())
}

/// The embedded path on a real clock: the killed child is reaped inside the
/// cleanup bound, so the refusal carries no residue.
#[tokio::test]
async fn a_windowed_embedded_child_is_reaped_before_the_refusal_returns() -> TestResult {
    let root = Scratch::new("window-embedded-real-bound")?;
    let service = root.path().join("service");
    std::fs::create_dir_all(&service)?;
    let owned_pid = root.path().join("owned.pid");
    let stub = root.path().join("stub-product");
    write_executable(
        &stub,
        format!(
            "#!/bin/sh\necho $$ > {}\nexec sleep 120\n",
            owned_pid.display()
        ),
    )?;
    let stub_path = stub.to_string_lossy().into_owned();

    let started = tokio::time::Instant::now();
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
    let elapsed = tokio::time::Instant::now() - started;
    assert_eq!(class, ExitClass::Child);
    assert!(
        elapsed >= std::time::Duration::from_millis(29_500)
            && elapsed <= std::time::Duration::from_secs(40),
        "the embedded window ended at {elapsed:?}, not at the 30 s real-time bound"
    );
    assert!(
        !message.contains("bounded cleanup failed"),
        "the real-clock cleanup left a residue: {message}"
    );
    let log_suffix = format!(
        "; server log: {}",
        service.join("logs/server.log").display()
    );
    let message = message
        .strip_suffix(&log_suffix)
        .ok_or("failure omitted server log path")?;
    let child_pid: u32 = std::fs::read_to_string(&owned_pid)?
        .trim()
        .parse()
        .ok()
        .ok_or("the stub never wrote its own pid")?;
    let alive_prefix = "the service is still starting (slow disk?): waited 30s; process ";
    let alive_mid = " alive; embedded child did not publish its own attributed readiness within the finite launch window (";
    let alive_tail = " polls; last probe: service registration is absent)";
    let rest = message
        .strip_prefix(alive_prefix)
        .ok_or_else(|| format!("the refusal is not the honest window refusal: {message}"))?;
    let (pid_text, rest) = rest
        .split_once(alive_mid)
        .ok_or_else(|| format!("the refusal carries no window evidence: {message}"))?;
    let reported_pid: u32 = pid_text
        .parse()
        .ok()
        .ok_or_else(|| format!("the refusal names no child pid: {message}"))?;
    assert_eq!(
        reported_pid, child_pid,
        "the refusal must name the still-alive child it reaps"
    );
    let rest = rest
        .strip_suffix(alive_tail)
        .ok_or_else(|| format!("the refusal carries no window evidence: {message}"))?;
    let polls: u32 = rest
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
        .ok_or_else(|| format!("the poll count is not numeric in: {message}"))?;
    assert!(
        (400..=1260).contains(&polls),
        "the window ran {polls} probes, not the 25 ms pending pacing"
    );
    let mut reaped = false;
    for _ in 0..40 {
        if pid_state_of(child_pid)?.is_none() {
            reaped = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        reaped,
        "the failed launch left its exact child alive or as a zombie (pid {child_pid})"
    );
    Ok(())
}

/// An embedded launch aborted mid-window still kills its child via
/// `kill_on_drop`.
#[tokio::test]
async fn an_aborted_embedded_launch_delivers_the_drop_kill_and_leaves_no_running_child(
) -> TestResult {
    let root = Scratch::new("window-cancellation")?;
    let service = root.path().join("service");
    std::fs::create_dir_all(&service)?;
    let pidfile = root.path().join("stub.pid");
    let stub = root.path().join("stub-product");
    write_executable(
        &stub,
        format!(
            "#!/bin/sh\necho $$ > {}\nexec sleep 300\n",
            pidfile.display()
        ),
    )?;
    let stub_path = stub.to_string_lossy().into_owned();
    let service_path = service.to_string_lossy().into_owned();

    let mut task = tokio::spawn(grapt_cli::embedded_client(
        std::path::PathBuf::from(stub_path),
        std::path::PathBuf::from(service_path),
        CliLimits::production(),
    ));
    let mut child_pid = None;
    for _ in 0..3000 {
        if let Ok(text) = std::fs::read_to_string(&pidfile) {
            if let Ok(pid) = text.trim().parse::<u32>() {
                if matches!(pid_state_of(pid)?, Some('R') | Some('S')) {
                    child_pid = Some(pid);
                    break;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let early_state = tokio::select! {
        biased;
        joined = &mut task => match joined {
            Ok(Ok(_)) => "task-ended: success".to_owned(),
            Ok(Err((class, message))) => {
                format!("task-ended-err: class={} {message}", class.code())
            }
            Err(error) => format!("task-join-error: {error}"),
        },
        _ = std::future::ready(()) => "task-still-running".to_owned(),
    };
    let child_pid = child_pid.ok_or_else(|| {
        let pidfile_state = match std::fs::read_to_string(&pidfile) {
            Ok(text) => format!("pidfile={:?}", text.trim()),
            Err(error) => format!("pidfile-missing: {error}"),
        };
        format!("the stub never stood as a running child; {pidfile_state}; {early_state}")
    })?;
    task.abort();
    let _ = task.await;
    let mut gone = false;
    for _ in 0..2000 {
        if pid_state_of(child_pid)?.is_none() {
            gone = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(
        gone,
        "the aborted launch left its exact child running or a zombie (pid {child_pid})"
    );
    Ok(())
}

/// A service that reaches readiness outlives the CLI untouched by cleanup.
#[tokio::test]
#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
async fn a_service_owner_that_reaches_readiness_detaches_untouched() -> TestResult {
    let root = Scratch::new("window-detach-untouched")?;
    let service = root.path().join("service");
    std::fs::create_dir_all(&service)?;
    let pidfile = root.path().join("stub.pid");
    let stub = root.path().join("stub-product.py");
    write_executable(
        &stub,
        r#"#!/usr/bin/env python3
import base64, json, os, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

def arg(name):
    return sys.argv[sys.argv.index(name) + 1]

root = arg("--service")
port = int(arg("--port"))
pid = os.getpid()
version = os.environ["GRAPT_STUB_VERSION"]
build = "grapt-cli-public-test"
host = "127.0.0.1"

def publish(name, contents):
    path = os.path.join(root, name)
    pending = path + ".pending"
    with open(pending, "w") as f:
        f.write(contents)
    os.replace(pending, path)

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        expected = "Basic " + base64.b64encode(b"test:test").decode("ascii")
        body = json.dumps({
            "schema": "secode.api.health.v1",
            "pid": pid,
            "build": build,
            "environment": "test",
            "service_instance": "instance-live",
        }).encode("ascii")
        if self.path == "/api/health" and self.headers.get("Authorization") == expected:
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            self.send_response(401)
            self.send_header("Content-Length", "0")
            self.end_headers()

    def log_message(self, *args):
        pass

server = HTTPServer((host, port), Handler)
endpoint = "http://" + host + ":" + str(server.server_address[1])
publish("basic.credential", "test:test\n")
publish(
    "registration",
    "secode-service/registration/1\n"
    "slot=grapt\n"
    "instance=instance-live\n"
    "environment=test\n"
    "generation=7\n"
    "pid=" + str(pid) + "\n"
    "endpoint=" + endpoint + "\n"
    "version=" + version + "\n"
    "registered_at=1\n"
)
publish("serve-ready.json", json.dumps({
        "schema": "secode.product.ready.v2",
        "operation": "serve",
        "service_slot": "grapt",
        "service_instance": "instance-live",
        "environment": "test",
        "service_generation": 7,
        "pid": pid,
        "observed_endpoint": endpoint,
        "serve_window_ms": 1,
        "build": build,
    }))
with open(os.environ["GRAPT_STUB_PIDFILE"], "w") as f:
    f.write(str(pid))
server.serve_forever()
"#,
    )?;

    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"))
        .args(["service", "start", "--service"])
        .arg(&service)
        .arg("--product")
        .arg(&stub)
        .env("GRAPT_STUB_VERSION", env!("CARGO_PKG_VERSION"))
        .env("GRAPT_STUB_PIDFILE", &pidfile)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let status = child.wait().await?;
    let mut stdout = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        pipe.read_to_string(&mut stdout).await?;
    }
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        pipe.read_to_string(&mut stderr).await?;
    }
    let stub_pid = std::fs::read_to_string(&pidfile)
        .map_err(|error| {
            format!(
                "the stub pid was unreadable after CLI exit {:?}: {error}; stdout: {stdout} stderr: {stderr}",
                status.code()
            )
        })?
        .trim()
        .parse::<u32>()
        .map_err(|_| "the stub never wrote its own pid".to_owned())?;
    let mut detached = false;
    for _ in 0..400 {
        if matches!(pid_state_of(stub_pid)?, Some('R') | Some('S')) {
            detached = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    if !detached {
        std::process::Command::new("kill")
            .args(["-TERM", &stub_pid.to_string()])
            .output()
            .ok();
        return Err(format!(
            "the launched service did not survive its CLI (exit {:?}); stdout: {stdout} stderr: {stderr}",
            status.code()
        )
        .into());
    }
    assert_eq!(
        status.code(),
        Some(i32::from(ExitClass::Success.code())),
        "the ready launch must exit as success; stdout: {stdout} stderr: {stderr}"
    );
    assert!(
        stdout.trim_start().starts_with("http://127.0.0.1:"),
        "the service receipt is missing; stdout: {stdout}"
    );
    // Stop the detached service.
    std::process::Command::new("kill")
        .args(["-TERM", &stub_pid.to_string()])
        .output()?;
    for _ in 0..200 {
        if pid_state_of(stub_pid)?.is_none() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    Ok(())
}
