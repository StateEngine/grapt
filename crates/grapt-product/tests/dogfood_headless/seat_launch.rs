//! Shipped provisioning + packet continuity against a disposable real product.
use super::*;
use sha2::{Digest, Sha256};

const SESSION: &str = "ses_native_seat_launch";
const PACKET: &[u8] = b"packet_id: native_seat_packet\nSEAT_PACKET_TASK: continue admitted work.\n";

fn provision_command(root: &Path, pointer: &serde_json::Value) -> Command {
    let identity = serde_json::json!({"scope":"project","project_id":"native-fixture","agent_id":"agt_native_fixture","mail_transport_protocol":"remote-seat-mail-transport/v2"});
    let mut command = Command::new(super::cli_continuity::cli_binary());
    command
        .current_dir(root)
        .args(["session", "provision", "--service"])
        .arg(root)
        .args([
            "--session",
            SESSION,
            "--agent",
            "agt_live_headless1",
            "--model",
            "openai/gpt-live",
            "--location",
        ])
        .arg(root)
        .arg("--identity-entry")
        .arg(identity.to_string())
        .arg("--packet-entry")
        .arg(pointer.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn provision(root: &Path, pointer: &serde_json::Value) -> std::process::Output {
    let mut cli = provision_command(root, pointer)
        .spawn()
        .unwrap_or_else(|error| panic!("provision CLI: {error}"));
    let mut stdin = cli
        .stdin
        .take()
        .unwrap_or_else(|| panic!("provision stdin absent"));
    stdin
        .write_all(b"NATIVE_FIXTURE_CAPABILITY\n")
        .unwrap_or_else(|error| panic!("capability fixture stdin: {error}"));
    drop(stdin);
    cli.wait_with_output()
        .unwrap_or_else(|error| panic!("provision CLI result: {error}"))
}

fn run(root: &Path) -> super::cli_continuity::CliRun {
    super::cli_continuity::cli(
        root,
        &[
            "run",
            "--service",
            &root.to_string_lossy(),
            "--session",
            SESSION,
            "--prompt",
            "Continue the admitted fixture task.",
        ],
    )
}

#[test]
#[cfg_attr(not(windows), expect(clippy::cognitive_complexity, reason = "legacy"))]
#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
fn shipped_seat_provision_packet_and_restart_keep_failures_before_provider() {
    let _journey = process_journey_custody();
    for mode in ["ready", "seat-required-reads", "packet-bad-ack", "corrupt"] {
        let successful = matches!(mode, "ready" | "seat-required-reads");
        let root = root(&format!("seat-launch-{mode}"));
        prepare(&root, 120_000);
        super::stateengine_native::configure(&root, if mode == "corrupt" { "ready" } else { mode });
        let path = root.join("packet.md");
        fs::write(&path, PACKET).unwrap_or_else(|error| panic!("packet fixture: {error}"));
        let digest: String = Sha256::digest(PACKET)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let pointer =
            serde_json::json!({"state":"written","path":path,"bytes":PACKET.len(),"sha256":digest});
        if mode == "corrupt" {
            let mut corrupt = PACKET.to_vec();
            corrupt[PACKET.len() - 2] = b'!';
            fs::write(&path, corrupt).unwrap_or_else(|error| panic!("corrupt fixture: {error}"));
        }
        let provider = ProviderFixture::start(if successful {
            vec![Script::Success, Script::Success]
        } else {
            vec![]
        });
        let stopped = provision_command(&root, &pointer)
            .stdin(Stdio::null())
            .output()
            .unwrap_or_else(|error| panic!("stopped service probe: {error}"));
        assert!(
            !stopped.status.success(),
            "provision silently started a missing service"
        );
        let diagnostic = String::from_utf8_lossy(&stopped.stderr);
        assert!(
            diagnostic.contains("unavailable")
                && diagnostic.contains("start the configured service")
        );
        let mut child = spawn(&root, provider.address);
        let address = await_ready(&root, &mut child);
        let provisioned = provision(&root, &pointer);
        assert!(
            provisioned.status.success(),
            "provision failed: {}",
            String::from_utf8_lossy(&provisioned.stderr)
        );
        let selected = json(address, "GET", &format!("/api/session/{SESSION}"), "");
        assert_eq!(selected["data"]["id"], SESSION);
        assert_eq!(selected["data"]["agent"], "agt_live_headless1");
        assert_eq!(selected["data"]["model"]["id"], "gpt-live");
        let entries = json(
            address,
            "GET",
            &format!("/api/session/{SESSION}/instructions/entries"),
            "",
        );
        assert!(entries.to_string().contains("stateengine-packet"));
        assert!(!entries.to_string().contains("NATIVE_FIXTURE_CAPABILITY"));
        if successful {
            let mut changed = pointer.clone();
            changed["sha256"] = serde_json::json!("0".repeat(64));
            let duplicate = provision(&root, &changed);
            assert!(
                !duplicate.status.success(),
                "duplicate launch adopted a Session"
            );
            assert!(String::from_utf8_lossy(&duplicate.stderr).contains("already claimed"));
            assert_eq!(
                json(
                    address,
                    "GET",
                    &format!("/api/session/{SESSION}/instructions/entries"),
                    ""
                ),
                entries,
                "duplicate launch changed the existing binding"
            );
            assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        }
        let first = run(&root);
        if successful {
            assert_eq!(first.code, Some(0), "seat run: {}", first.stderr);
            fs::remove_file(&path).unwrap_or_else(|error| panic!("packet removal: {error}"));
            let second = run(&root);
            assert_eq!(
                second.code,
                Some(0),
                "cached packet reread: {}",
                second.stderr
            );
            assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
            let bodies = provider
                .bodies
                .lock()
                .unwrap_or_else(|_| panic!("request evidence poisoned"));
            assert_eq!(bodies.len(), 2);
            for body in bodies.iter() {
                assert!(body.contains("SEAT_PACKET_TASK"));
                assert!(!body.contains("NATIVE_FIXTURE_CAPABILITY"));
                assert!(!body.contains("NATIVE_FIXTURE_PRIVATE_CURSOR"));
            }
            drop(bodies);
        } else {
            assert_ne!(first.code, Some(0), "invalid packet admitted execution");
            assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        }
        child.stop_after_witness(&root);
        drop(child);
        if successful {
            let mut restarted = spawn(&root, provider.address);
            let address = await_ready(&root, &mut restarted);
            assert_ne!(
                run(&root).code,
                Some(0),
                "restart retained a process capability"
            );
            let (status, _) = request(
                address,
                "PUT",
                &format!("/api/session/{SESSION}/stateengine/capability"),
                r#"{"capability":"NATIVE_FIXTURE_CAPABILITY"}"#,
            );
            assert_eq!(status, 204);
            assert_ne!(
                run(&root).code,
                Some(0),
                "rebind resurrected absent packet bytes"
            );
            assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
            let history = json(
                address,
                "GET",
                &format!("/api/session/{SESSION}/message?order=asc&limit=50"),
                "",
            );
            assert!(history.to_string().contains("launch packet is absent"));
            assert!(!history.to_string().contains("NATIVE_FIXTURE_CAPABILITY"));
            restarted.stop_after_witness(&root);
            drop(restarted);
        }
        process_fixture::cleanup(&root);
    }
}

#[test]
fn a_transport_outage_before_launch_packet_delivery_stops_before_provider() {
    let _journey = process_journey_custody();
    let root = root("seat-packet-transport");
    prepare(&root, 120_000);
    super::stateengine_native::configure(&root, "transport");
    let path = root.join("packet.md");
    fs::write(&path, PACKET).unwrap_or_else(|error| panic!("packet fixture: {error}"));
    let digest: String = Sha256::digest(PACKET)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let pointer = serde_json::json!({
        "state":"written", "path":path, "bytes":PACKET.len(), "sha256":digest
    });
    let provider = ProviderFixture::start(Vec::new());
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let provisioned = provision(&root, &pointer);
    assert!(
        provisioned.status.success(),
        "provision failed: {}",
        String::from_utf8_lossy(&provisioned.stderr)
    );
    let first = run(&root);
    assert_ne!(first.code, Some(0), "undelivered packet admitted execution");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    let history = json(
        address,
        "GET",
        &format!("/api/session/{SESSION}/message?order=asc&limit=50"),
        "",
    );
    assert!(history
        .to_string()
        .contains(&grapt_session::execution::ContinuityFailure::Transport.message()));
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}
