//! Native startup advice binds real OS process absence and exact custody bytes.
//! This isolates advice attribution from the separate full HTTP crash journey.
use grapt_service::RegistrationRecord;
use grapt_types::{
    authority::Timestamp,
    service::{
        Endpoint, EnvironmentReceipt, OwnerReceipt, ProcessId, ServiceGeneration,
        ServiceInstanceId, ServiceRegistrationVersion, ServiceSlotId,
    },
};
use std::{
    fs,
    path::Path,
    process::{Child, Command, Output, Stdio},
};

/// A witness process the test owns until drop: killed and reaped in
/// `Drop`, so a failing assertion cannot orphan it. Spawned directly
/// (no shell wrapper) so the killed pid is the one the registration
/// names — on Windows a `cmd /c` wrapper would die and leave its
/// `ping` child alive.
struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn inspect(root: &Path) -> Output {
    grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
        .args(["startup", "--service"])
        .arg(root)
        .args([
            "--slot",
            "product",
            "--environment",
            "internal-alpha",
            "--instance",
            "desktop-new",
        ])
        .output()
        .unwrap_or_else(|error| panic!("inspection failed to launch: {error}"))
}

fn publish(root: &Path, pid: u32) -> (Vec<u8>, Vec<u8>) {
    let owner = OwnerReceipt::new(
        ServiceSlotId::new("product").unwrap_or_else(|| panic!("slot")),
        ServiceInstanceId::new("predecessor").unwrap_or_else(|| panic!("instance")),
        EnvironmentReceipt::new("internal-alpha").unwrap_or_else(|| panic!("environment")),
        ServiceGeneration::new(7),
        ProcessId::new(pid).unwrap_or_else(|| panic!("pid")),
        Endpoint::new("http://127.0.0.1:49151").unwrap_or_else(|| panic!("endpoint")),
    );
    let registration = RegistrationRecord::new(
        owner,
        ServiceRegistrationVersion::new("0.1.0").unwrap_or_else(|| panic!("version")),
        Timestamp::from_millis(0),
    )
    .unwrap_or_else(|error| panic!("{error}"))
    .encode();
    let ready = serde_json::to_vec(&serde_json::json!({
        "schema":"secode.product.ready.v2", "operation":"serve", "service_slot":"product",
        "service_instance":"predecessor", "environment":"internal-alpha", "service_generation":7,
        "pid":pid, "observed_endpoint":"http://127.0.0.1:49151", "serve_window_ms":1000, "build":"fixture",
    })).unwrap_or_else(|error| panic!("{error}"));
    fs::write(root.join("registration"), &registration).unwrap_or_else(|error| panic!("{error}"));
    fs::write(root.join("serve-ready.json"), &ready).unwrap_or_else(|error| panic!("{error}"));
    (registration, ready)
}

#[test]
fn product_version_distinguishes_the_loopback_fixture_build(
) -> Result<(), Box<dyn std::error::Error>> {
    let output = grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
        .arg("--version")
        .output()?;
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout)?;
    if cfg!(feature = "loopback-provider-testkit") {
        assert_eq!(
            text.trim(),
            concat!(
                env!("CARGO_PKG_VERSION"),
                " (testkit/fixture: loopback-provider-testkit)"
            )
        );
    } else {
        assert_eq!(text.trim(), env!("CARGO_PKG_VERSION"));
    }
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn native_startup_advice_proves_dead_owner_without_retiring_custody() {
    let root = std::env::temp_dir().join(format!("grapt-startup-advice-{}", std::process::id()));
    fs::create_dir(&root).unwrap_or_else(|error| panic!("fresh fixture required: {error}"));
    let mut predecessor =
        grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|error| panic!("predecessor launch failed: {error}"));
    let dead_pid = predecessor.id();
    assert!(predecessor
        .wait()
        .unwrap_or_else(|error| panic!("predecessor reap failed: {error}"))
        .success());
    let (registration, ready) = publish(&root, dead_pid);
    let result = inspect(&root);
    assert!(
        result.status.success(),
        "dead predecessor inspection refused: {}",
        String::from_utf8_lossy(&result.stdout)
    );
    let advice: serde_json::Value =
        serde_json::from_slice(&result.stdout).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        advice["generation"], 8,
        "startup advice reset recovery generation"
    );
    assert_eq!(
        advice["instance"], "desktop-new-restart-8",
        "startup advice lost recovery instance"
    );
    assert_eq!(
        fs::read(root.join("registration")).unwrap_or_else(|error| panic!("{error}")),
        registration
    );
    assert_eq!(
        fs::read(root.join("serve-ready.json")).unwrap_or_else(|error| panic!("{error}")),
        ready
    );
    let mut foreign: serde_json::Value =
        serde_json::from_slice(&ready).unwrap_or_else(|error| panic!("{error}"));
    foreign["service_generation"] = serde_json::json!(9);
    fs::write(root.join("serve-ready.json"), foreign.to_string())
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(
        !inspect(&root).status.success(),
        "inspection accepted mismatched predecessor custody"
    );
    {
        // Witness 1: the registration names a live pid under a different
        // process but holds no election lock. The predecessor is dead — the
        // lock, not the process table, decides — so the advice must start.
        let mut command = Command::new(if cfg!(windows) { "ping" } else { "sleep" });
        if cfg!(windows) {
            command.args(["-n", "120", "127.0.0.1"]);
        } else {
            command.arg("120");
        }
        let unrelated = OwnedChild(
            command
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap_or_else(|error| panic!("unrelated process spawn failed: {error}")),
        );
        let stale = publish(&root, unrelated.0.id());
        let result = inspect(&root);
        assert!(
            result.status.success(),
            "lock-free stale predecessor must be admitted: {}",
            String::from_utf8_lossy(&result.stdout)
        );
        // Read-only advice: the custody bytes stand untouched.
        assert_eq!(
            fs::read(root.join("registration")).unwrap_or_else(|error| panic!("{error}")),
            stale.0
        );
        assert_eq!(
            fs::read(root.join("serve-ready.json")).unwrap_or_else(|error| panic!("{error}")),
            stale.1
        );
        // Witness 2: the same registration with the election lock held by
        // this test process is a live predecessor and must be refused.
        // O_CLOEXEC keeps the lock's file description out of the inspect
        // children this test spawns while it holds the lock; Windows handles
        // are not inherited by default, so no flag is needed there.
        let mut options = std::fs::OpenOptions::new();
        options.create(true).read(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(0o2000000);
        }
        let holder = options
            .open(root.join("registration.elected"))
            .unwrap_or_else(|error| panic!("election lock open failed: {error}"));
        holder
            .try_lock()
            .unwrap_or_else(|error| panic!("election lock hold failed: {error:?}"));
        let result = inspect(&root);
        assert!(
            !result.status.success(),
            "inspection admitted a lock-holding predecessor: {}",
            String::from_utf8_lossy(&result.stdout)
        );
        // The refusal is the lock's: releasing it admits the same
        // registration.
        drop(holder);
        let result = inspect(&root);
        assert!(
            result.status.success(),
            "lock release must admit the stale predecessor: {}",
            String::from_utf8_lossy(&result.stdout)
        );
        drop(unrelated);
    }
    fs::remove_dir_all(root).unwrap_or_else(|error| panic!("{error}"));
}
