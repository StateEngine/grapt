//! A direct server never adopts the launching seat, even before profile setup.
use std::process::Stdio;

#[test]
fn inherited_launcher_identity_refuses_before_opening_user_state(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("grapt-identity-refusal-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    let result = (|| {
        for verb in ["serve", "serve-terminal", "handover"] {
            for name in [
                "STATEENGINE_AGENT_ID",
                "secode_stateengine_seat_launch_pin",
                "SECODE_SESSION_ID",
            ] {
                let mut command = grapt_service::launcher_environment::command(env!(
                    "CARGO_BIN_EXE_grapt-server"
                ));
                command.args([verb, "--service"]).arg(&root);
                if verb == "handover" {
                    command.arg("--run-root").arg(&root).args([
                        "--run-id",
                        "fixture",
                        "--engine-version",
                        "0.1.0",
                    ]);
                } else {
                    command.args(["--port", "0"]);
                }
                let output = command
                    .env(name, "identity-capability-sentinel")
                    .stdin(Stdio::null())
                    .output()?;
                assert_eq!(output.status.code(), Some(2));
                let diagnostic = String::from_utf8(output.stderr)?;
                assert!(diagnostic.contains("inherited StateEngine identity or session capability"));
                assert!(!diagnostic.contains("identity-capability-sentinel"));
                assert_eq!(std::fs::read_dir(&root)?.count(), 0);
            }
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    })();
    std::fs::remove_dir(&root)?;
    result
}

#[test]
fn external_host_probe_needs_no_profile_or_launcher_admission(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("grapt-host-probe-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    let result = (|| {
        for extra in [false, true] {
            let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_grapt-server"));
            command
                .current_dir(&root)
                .arg("--external-host-capabilities")
                .env("STATEENGINE_AGENT_ID", "fixture-seat")
                .env("GRAPT_CONFIG", root.join("missing-config"))
                .env("HOME", &root)
                .env("USERPROFILE", &root);
            if extra {
                command.arg("--service").arg(root.join("must-not-exist"));
            }
            let output = command.output()?;
            assert_eq!(output.status.code(), Some(if extra { 2 } else { 0 }));
            if !extra {
                let probe: serde_json::Value = serde_json::from_slice(&output.stdout)?;
                assert_eq!(probe["schema"], "secode.external-host-capabilities.v1");
                assert_eq!(probe["stateengine"]["embedded_owner_composed"], false);
                assert_eq!(
                    probe["service"]["readiness"]["authenticated_health_path"],
                    "/api/health"
                );
            }
            assert_eq!(std::fs::read_dir(&root)?.count(), 0);
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    })();
    std::fs::remove_dir(&root)?;
    result
}
