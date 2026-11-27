use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use grapt_service::RegistrationRecord;
use grapt_types::service::{
    Endpoint, EnvironmentReceipt, OwnerReceipt, ProcessId, ServiceGeneration, ServiceInstanceId,
    ServiceRegistrationVersion, ServiceSlotId,
};
use grapt_types::Timestamp;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Profile(PathBuf);

impl Profile {
    fn new() -> TestResult<Self> {
        let root = std::env::temp_dir().join(format!(
            "grapt-terminal-attach-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("service.conf"), b"fixture")?;
        std::fs::write(root.join("basic.credential"), b"test:test\n")?;
        Ok(Self(root))
    }

    fn command(&self) -> crate::command::terminal::TerminalCommand {
        crate::command::terminal::TerminalCommand {
            profile: self.0.clone(),
            profile_source: crate::command::terminal::ProfileSource::Explicit,
            user_profile: None,
            profile_trust: Default::default(),
            repository: self.0.clone(),
            setup: None,
            preset: Some("inspect"),
            reasoning_effort: Some("high".into()),
            selection: Default::default(),
        }
    }

    fn terminal(&self) -> TestResult<PathBuf> {
        let terminal = self.0.join("terminal");
        crate::testfs::write_executable(
            &terminal,
            b"#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$2/ui-args\"\nexit 7\n",
        )?;
        Ok(terminal)
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        let _cleanup = std::fs::remove_dir_all(&self.0);
    }
}

fn publish(root: &Path, endpoint: &str) -> TestResult {
    let pid = std::process::id();
    let registration = RegistrationRecord::new(
        OwnerReceipt::new(
            ServiceSlotId::new("grapt").ok_or("slot")?,
            ServiceInstanceId::new("instance-attach").ok_or("instance")?,
            EnvironmentReceipt::new("test").ok_or("environment")?,
            ServiceGeneration::new(7),
            ProcessId::new(pid).ok_or("pid")?,
            Endpoint::new(endpoint).ok_or("endpoint")?,
        ),
        ServiceRegistrationVersion::new(env!("CARGO_PKG_VERSION")).ok_or("version")?,
        Timestamp::from_millis(1),
    )?;
    std::fs::write(root.join("registration"), registration.encode())?;
    std::fs::write(
        root.join("serve-ready.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema":"secode.product.ready.v2", "operation":"serve", "service_slot":"grapt",
            "service_instance":"instance-attach", "environment":"test", "service_generation":7,
            "pid":pid, "observed_endpoint":endpoint, "serve_window_ms":1, "build":"attach-test"
        }))?,
    )?;
    Ok(())
}

#[tokio::test]
async fn live_service_is_reused_and_remains_available_after_terminal_exit() -> TestResult {
    tokio::time::timeout(Duration::from_secs(10), async {
        prove_reuse(false).await?;
        prove_reuse(true).await
    })
    .await?
}

async fn prove_reuse(selected: bool) -> TestResult {
    let profile = Profile::new()?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    publish(&profile.0, &format!("http://{}", listener.local_addr()?))?;
    let registration = std::fs::read(profile.0.join("registration"))?;
    let ready = std::fs::read(profile.0.join("serve-ready.json"))?;
    // Probe before attaching and after UI exit; selection adds one atomic create.
    let server = tokio::spawn(async move {
        for request_index in 0..(2 + usize::from(selected)) {
            let (mut stream, _) = listener.accept().await?;
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") && request.len() < 8_192 {
                let byte = stream.read_u8().await?;
                request.push(byte);
            }
            let request = std::str::from_utf8(&request)?;
            let creating = selected && request_index == 1;
            assert!(request.starts_with(if creating {
                "POST /api/session HTTP/1.1\r\n"
            } else {
                "GET /api/health HTTP/1.1\r\n"
            }));
            assert!(request
                .to_ascii_lowercase()
                .contains("authorization: basic dgvzddp0zxn0\r\n"));
            let body = if creating {
                let length: usize = request
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(str::to_owned)
                    })
                    .ok_or("content length missing")?
                    .parse()?;
                assert!(length < 8192);
                let mut body = vec![0; length];
                stream.read_exact(&mut body).await?;
                let chosen: serde_json::Value = serde_json::from_slice(&body)?;
                assert_eq!(chosen["agent"], "reviewer");
                assert_eq!(
                    chosen["model"],
                    serde_json::json!({"providerID":"p","id":"m"})
                );
                assert!(chosen["location"]["directory"].is_string());
                r#"{"data":{"id":"ses_selected","projectID":"prj_a","cost":0.0,"tokens":{"input":0.0,"output":0.0,"reasoning":0.0,"cache":{"read":0.0,"write":0.0}},"time":{"created":1.0,"updated":1.0},"title":"t","location":{"directory":"/w"}}}"#.to_owned()
            } else {
                serde_json::json!({
                "schema":"secode.api.health.v1", "pid":std::process::id(),
                "build":"attach-test", "environment":"test", "service_instance":"instance-attach"
            })
            .to_string()
            };
            stream.write_all(format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ).as_bytes()).await?;
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });
    let mut command = profile.command();
    if selected {
        command.selection.set_model("p/m")?;
        command.selection.set_agent("reviewer")?;
        command.reasoning_effort = None;
    }
    // A missing product binary makes any accidental second launch fail.
    let outcome = super::super::run_with_binaries_inner(
        command,
        &profile.0.join("must-not-launch-product"),
        &profile.terminal()?,
    )
    .await;
    assert_eq!(outcome?, 1, "the existing service hid the UI failure");
    let args = std::fs::read_to_string(profile.0.join("ui-args"))?;
    assert!(args.contains("\n--expect-health\n"), "UI has no health pin");
    if selected {
        assert!(args.contains("\n--session\nses_selected\n"));
        assert!(!args.contains("--directory"));
    } else {
        assert!(args.ends_with("--inspect\n--reasoning-effort\nhigh\n"));
    }
    let discovery = crate::ServiceDiscovery::try_new(&profile.0, env!("CARGO_PKG_VERSION"))?;
    let health = discovery.discover().await?.product_health()?.encoded()?;
    assert!(
        args.contains(&format!("\n{health}\n")),
        "UI used a different identity"
    );
    assert_eq!(std::fs::read(profile.0.join("registration"))?, registration);
    assert_eq!(std::fs::read(profile.0.join("serve-ready.json"))?, ready);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn incomplete_or_malformed_service_is_refused_before_launching_any_process() -> TestResult {
    for case in ["registration", "ready-absent", "ready-mismatch"] {
        let profile = Profile::new()?;
        publish(&profile.0, "http://127.0.0.1:9")?;
        let expected = match case {
            "registration" => {
                std::fs::write(profile.0.join("registration"), b"malformed")?;
                "registration is malformed"
            }
            "ready-absent" => {
                std::fs::remove_file(profile.0.join("serve-ready.json"))?;
                "ready receipt is absent"
            }
            _ => {
                std::fs::write(profile.0.join("serve-ready.json"), b"{}")?;
                "ready receipt is malformed"
            }
        };
        let registration = std::fs::read(profile.0.join("registration"))?;
        let outcome = super::super::run_with_binaries_inner(
            profile.command(),
            &profile.0.join("must-not-launch-product"),
            &profile.terminal()?,
        )
        .await;
        let error = outcome.err().ok_or("incomplete service was accepted")?;
        assert!(error.to_string().contains(expected), "{case}: {error}");
        assert!(!profile.0.join("ui-args").exists());
        assert_eq!(std::fs::read(profile.0.join("registration"))?, registration);
    }
    Ok(())
}
