//! CLI-COMPATIBILITY-MATRIX-01 witnesses: launch readiness.
//! Split out of `compatibility.rs` by HYGIENE-FOLLOWUPS-01; no test was renamed or dropped.

use super::*;

/// The product publishes its registration and then its ready receipt, each
/// durably. A launch that polls between the two keeps waiting, inside the same
/// finite window, and succeeds once the receipt lands. On a slow disk that gap
/// is seconds wide; here it is four polls, deterministically.
#[tokio::test]
async fn a_launch_waits_through_the_registration_to_ready_gap() -> TestResult {
    let root = Scratch::new("ready-gap")?;
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
    // Published atomically a few polls later, as `ready::publish` does.
    let publisher = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        std::fs::write(&pending_path, ready)?;
        std::fs::rename(&pending_path, &ready_path)
    });
    let discovery = ServiceDiscovery::try_new(root.path(), env!("CARGO_PKG_VERSION"))?;
    let discovered = discovery
        .await_child_readiness(pid, || ChildLiveness::Alive)
        .await;
    publisher.await??;
    if let Err(wait) = discovered {
        return Err(format!(
            "CCM115 a launch polling between the registration and its ready receipt was refused: {wait:?}"
        )
        .into());
    }
    assert_eq!(server.await??, ["GET /api/health"]);
    Ok(())
}

/// A child that never publishes its ready receipt still ends at the finite
/// launch window - the 30 s wall-clock bound, which this paused clock makes
/// virtual - and a child that exits ends the wait at once.
#[tokio::test(start_paused = true)]
async fn a_launch_without_readiness_ends_at_the_window_or_the_child_exit() -> TestResult {
    let root = Scratch::new("ready-never")?;
    let pid = std::process::id();
    write_discovery_fixture(
        root.path(),
        pid,
        "http://127.0.0.1:9",
        "grapt-cli-public-test",
    )?;
    std::fs::remove_file(root.path().join("serve-ready.json"))?;
    let discovery = ServiceDiscovery::try_new(root.path(), env!("CARGO_PKG_VERSION"))?;
    assert!(matches!(
        discovery
            .await_child_readiness(pid, || ChildLiveness::Alive)
            .await,
        Err(grapt_cli::ReadinessWait::Window(_))
    ));
    assert!(matches!(
        discovery
            .await_child_readiness(pid, || ChildLiveness::Exited("exited".to_owned()))
            .await,
        Err(grapt_cli::ReadinessWait::Child(message)) if message == "exited"
    ));
    Ok(())
}
