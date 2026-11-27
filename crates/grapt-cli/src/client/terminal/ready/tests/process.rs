use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::super::super::{control, drain};
use super::super::start;
use grapt_service::RegistrationRecord;
use grapt_types::service::{
    Endpoint, EnvironmentReceipt, OwnerReceipt, ProcessId, ServiceGeneration, ServiceInstanceId,
    ServiceRegistrationVersion, ServiceSlotId,
};
use grapt_types::Timestamp;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Profile(PathBuf);

impl Profile {
    fn new() -> TestResult<Self> {
        let root = std::env::temp_dir().join(format!(
            "grapt-notification-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("basic.credential"), b"test:test\n")?;
        Ok(Self(root))
    }

    fn discovery(&self) -> TestResult<crate::ServiceDiscovery> {
        Ok(crate::ServiceDiscovery::try_new(
            &self.0,
            env!("CARGO_PKG_VERSION"),
        )?)
    }

    fn publish(&self, pid: u32, endpoint: &str) -> TestResult {
        let registration = RegistrationRecord::new(
            OwnerReceipt::new(
                ServiceSlotId::new("grapt").ok_or("slot")?,
                ServiceInstanceId::new("notification-test").ok_or("instance")?,
                EnvironmentReceipt::new("test").ok_or("environment")?,
                ServiceGeneration::new(7),
                ProcessId::new(pid).ok_or("pid")?,
                Endpoint::new(endpoint).ok_or("endpoint")?,
            ),
            ServiceRegistrationVersion::new(env!("CARGO_PKG_VERSION")).ok_or("version")?,
            Timestamp::from_millis(1),
        )?;
        std::fs::write(self.0.join("registration"), registration.encode())?;
        std::fs::write(
            self.0.join("serve-ready.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema":"secode.product.ready.v2", "operation":"serve", "service_slot":"grapt",
                "service_instance":"notification-test", "environment":"test", "service_generation":7,
                "pid":pid, "observed_endpoint":endpoint, "serve_window_ms":30_000, "build":"test"
            }))?,
        )?;
        Ok(())
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        let _cleanup = std::fs::remove_dir_all(&self.0);
    }
}

fn child() -> TestResult<tokio::process::Child> {
    Ok(Command::new("/bin/sh")
        .args([
            "-c",
            r"printf '\036'; read control; printf 'EOF_SETTLED'; exit 0",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?)
}

#[tokio::test]
async fn notification_still_requires_a_publication_owned_by_the_exact_child() -> TestResult {
    tokio::time::timeout(Duration::from_secs(10), async {
        for wrong_owner in [false, true] {
            let profile = Profile::new()?;
            let mut child = child()?;
            let pid = child.id().ok_or("child pid")?;
            if wrong_owner {
                profile.publish(std::process::id(), "http://127.0.0.1:9")?;
            }
            let error = start(&mut child, &profile.discovery()?, pid)
                .await
                .err()
                .ok_or("notification bypassed discovery")?;
            assert!(
                error.contains(if wrong_owner {
                    "does not own"
                } else {
                    "absent"
                }),
                "{error}"
            );
            assert!(child.try_wait()?.is_some(), "refused child was not reaped");
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await?
}

#[tokio::test]
async fn attributed_notification_retains_private_control_until_explicit_drain() -> TestResult {
    tokio::time::timeout(Duration::from_secs(10), prove_control()).await?
}

async fn prove_control() -> TestResult {
    let profile = Profile::new()?;
    let mut child = child()?;
    let pid = child.id().ok_or("child pid")?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    profile.publish(pid, &format!("http://{}", listener.local_addr()?))?;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") && request.len() < 8_192 {
            request.push(stream.read_u8().await?);
        }
        let request = std::str::from_utf8(&request)?;
        assert!(request.starts_with("GET /api/health HTTP/1.1\r\n"));
        assert!(request
            .to_ascii_lowercase()
            .contains("authorization: basic dgvzddp0zxn0\r\n"));
        let body = serde_json::json!({
            "schema":"secode.api.health.v1", "pid":pid,
            "build":"test", "environment":"test", "service_instance":"notification-test"
        })
        .to_string();
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });
    let service = start(&mut child, &profile.discovery()?, pid).await?;
    assert_eq!(service.registration().owner().pid().value(), pid);
    assert!(child.stdin.is_some() && child.stdout.is_some());
    let control = control::retain(&mut child)?;
    let observation = tokio::time::timeout(
        Duration::from_millis(25),
        control::observe(&mut child, control),
    )
    .await;
    assert!(
        observation.is_err(),
        "readiness observation sent premature EOF"
    );
    assert!(drain(&mut child).await?.success());
    let mut receipt = Vec::new();
    child
        .stdout
        .take()
        .ok_or("returned stdout")?
        .take(64)
        .read_to_end(&mut receipt)
        .await?;
    assert_eq!(receipt, b"EOF_SETTLED");
    server.await??;
    Ok(())
}
