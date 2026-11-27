//! The public offline command preserves every owner it was not asked to retire.
use std::io::Write;
use std::process::{Output, Stdio};

use grapt_service::{ElectedControllerAuthority, LockAttemptPolicy, RegistrationRecord};
use grapt_types::{
    service::{
        Endpoint, EnvironmentReceipt, OwnerReceipt, ProcessId, ServiceGeneration,
        ServiceInstanceId, ServiceRegistrationVersion, ServiceSlotId,
    },
    Timestamp,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn retire(root: &std::path::Path, credential: &[u8]) -> Result<Output, Box<dyn std::error::Error>> {
    let mut child =
        grapt_service::launcher_environment::command(super::shipped_cli::product_binary())
            .args(["retire-service", "--service"])
            .arg(root)
            .arg("--expected-registration")
            .arg(root.join("captured.registration"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
    child
        .stdin
        .take()
        .ok_or("credential pipe missing")?
        .write_all(credential)?;
    Ok(child.wait_with_output()?)
}

#[test]
fn retirement_authenticates_fences_ownership_and_observes_a_lost_response() -> TestResult {
    let root = super::root("service-retirement");
    super::reset(&root);
    super::write_inputs(&root);
    std::fs::write(root.join("service.conf"), "slot=product\ninstance=predecessor\nenvironment=fixture-environment\nversion=0.1.0\nserve_window_ms=1000\nprovider_mode=production\nprovider_connect_ms=10000\nprovider_response_head_ms=60000\nprovider_active_attempts=4\n")?;
    std::fs::write(
        root.join("basic.credential"),
        b"operator:synthetic-secret\n",
    )?;
    // This PID is alive but is not a product owner: the lifetime election,
    // captured owner tuple and exact bytes decide custody, not a PID lookup.
    let owner = OwnerReceipt::new(
        ServiceSlotId::new("product").ok_or("slot")?,
        ServiceInstanceId::new("predecessor").ok_or("instance")?,
        EnvironmentReceipt::new("fixture-environment").ok_or("environment")?,
        ServiceGeneration::new(7),
        ProcessId::new(std::process::id()).ok_or("pid")?,
        Endpoint::new("http://127.0.0.1:49151").ok_or("endpoint")?,
    );
    let record = RegistrationRecord::new(
        owner.clone(),
        ServiceRegistrationVersion::new("0.1.0").ok_or("version")?,
        Timestamp::from_millis(0),
    )?;
    let bytes = record.encode();
    let ready = serde_json::to_vec(&serde_json::json!({
        "schema":"secode.product.ready.v2", "operation":"serve", "service_slot":"product",
        "service_instance":"predecessor", "environment":"fixture-environment", "service_generation":7,
        "pid":owner.pid().value(), "observed_endpoint":owner.endpoint().as_str(),
        "serve_window_ms":1000, "build":"fixture",
    }))?;
    std::fs::write(root.join("registration"), &bytes)?;
    std::fs::write(root.join("captured.registration"), &bytes)?;
    std::fs::write(root.join("serve-ready.json"), &ready)?;

    let refused = retire(&root, b"operator:incorrect-secret\n")?;
    assert!(!refused.status.success());
    assert!(!String::from_utf8_lossy(&refused.stdout).contains("incorrect-secret"));
    assert_eq!(std::fs::read(root.join("registration"))?, bytes);
    assert_eq!(std::fs::read(root.join("serve-ready.json"))?, ready);
    let election = ElectedControllerAuthority::acquire(
        root.join("registration"),
        LockAttemptPolicy::new(1).ok_or("policy")?,
    )?;
    let refused = retire(&root, b"operator:synthetic-secret\n")?;
    assert!(!refused.status.success(), "live elected owner must refuse");
    assert_eq!(std::fs::read(root.join("registration"))?, bytes);
    assert_eq!(std::fs::read(root.join("serve-ready.json"))?, ready);
    drop(election);

    let successor = RegistrationRecord::new(
        OwnerReceipt::new(
            owner.slot().clone(),
            ServiceInstanceId::new("successor").ok_or("successor")?,
            owner.environment().clone(),
            ServiceGeneration::new(8),
            owner.pid(),
            owner.endpoint().clone(),
        ),
        record.version().clone(),
        Timestamp::from_millis(1),
    )?
    .encode();
    std::fs::write(root.join("registration"), &successor)?;
    assert!(!retire(&root, b"operator:synthetic-secret\n")?
        .status
        .success());
    assert_eq!(std::fs::read(root.join("registration"))?, successor);
    assert_eq!(std::fs::read(root.join("serve-ready.json"))?, ready);
    std::fs::write(root.join("registration"), &bytes)?;
    let mut foreign: serde_json::Value = serde_json::from_slice(&ready)?;
    foreign["service_instance"] = "foreign".into();
    let foreign = serde_json::to_vec(&foreign)?;
    std::fs::write(root.join("serve-ready.json"), &foreign)?;
    assert!(!retire(&root, b"operator:synthetic-secret\n")?
        .status
        .success());
    assert_eq!(std::fs::read(root.join("registration"))?, bytes);
    assert_eq!(std::fs::read(root.join("serve-ready.json"))?, foreign);

    std::fs::write(root.join("serve-ready.json"), &ready)?;
    let released = retire(&root, b"operator:synthetic-secret\n")?;
    assert!(
        released.status.success(),
        "{}",
        String::from_utf8_lossy(&released.stdout)
    );
    let report: serde_json::Value = serde_json::from_slice(&released.stdout)?;
    assert_eq!(report["outcome"], "released");
    assert_eq!(report["registration_released"], true);
    assert!(!root.join("registration").try_exists()?);
    assert!(!root.join("serve-ready.json").try_exists()?);
    // Discard the first reply as a caller with a lost response would do.
    let observed = retire(&root, b"operator:synthetic-secret\n")?;
    assert!(observed.status.success());
    let report: serde_json::Value = serde_json::from_slice(&observed.stdout)?;
    assert_eq!(report["outcome"], "slot_already_empty");
    assert_eq!(report["registration_released"], false);
    std::fs::remove_dir_all(root)?;
    Ok(())
}
