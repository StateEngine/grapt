//! .NET Framework PowerShell must start through both real product adapters.
use super::*;

fn startup(tty: bool, label: &str) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root(label);
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 120_000);
    unattended(&root);
    let provider = ProviderFixture::start(Vec::new());
    let mut service = process_fixture::spawn_controlled_with(
        &root,
        provider.address,
        &[("GRAPT_TEST_SECRET_SENTINEL", "must-not-reach-shell")],
    );
    let address = await_ready(&root, &mut service);
    let session = create(address);
    let system = std::env::var_os("SystemRoot").ok_or("Windows SystemRoot is absent")?;
    let shell = PathBuf::from(system).join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let command = "if(Test-Path Env:GRAPT_TEST_SECRET_SENTINEL){throw 'shell inherited the fixture sentinel'}; [Console]::WriteLine('POWERSHELL-STARTUP-OK')";
    let raw = tool(
        &root,
        &provider,
        address,
        &session,
        "call_ps_start",
        "exec_command",
        serde_json::json!({"cmd":command,"shell":shell.to_str().ok_or("PowerShell path is not UTF-8")?,
            "login":false,"tty":tty,"yield_time_ms":5000,"timeout_ms":30000,"max_output_tokens":4000}),
    )?;
    let mut result: serde_json::Value = serde_json::from_str(&raw)?;
    let mut output = result["output"]
        .as_str()
        .ok_or("PowerShell output absent")?
        .to_owned();
    for call in ["call_ps_poll_1", "call_ps_poll_2"] {
        if result["done"] == true {
            break;
        }
        let handle = result["session_id"]
            .as_str()
            .ok_or("PowerShell handle absent")?
            .to_owned();
        let raw = tool(
            &root,
            &provider,
            address,
            &session,
            call,
            "write_stdin",
            stdin(&handle, "", None),
        )?;
        result = serde_json::from_str(&raw)?;
        output.push_str(
            result["output"]
                .as_str()
                .ok_or("PowerShell poll output absent")?,
        );
    }
    assert_eq!(result["done"], true, "{result}; output: {output}");
    assert_eq!(result["exit_code"], 0, "{result}; output: {output}");
    assert_eq!(result["cleanup"], "complete", "{result}");
    assert_eq!(result["pipe"], "drained", "{result}");
    assert!(output.contains("POWERSHELL-STARTUP-OK"), "{output}");
    // The older shell tool also receives the canonical workspace as cmd cwd.
    let directory = tool(
        &root,
        &provider,
        address,
        &session,
        "call_ps_cwd",
        "shell",
        serde_json::json!({"command":"cd","timeout_ms":5000}),
    )?;
    let shown = directory
        .lines()
        .find(|line| Path::new(line.trim()).is_absolute())
        .ok_or_else(|| format!("cmd did not report its directory: {directory}"))?;
    assert_eq!(fs::canonicalize(shown.trim())?, fs::canonicalize(&root)?);
    service.stop_after_witness(&root);
    drop(service);
    process_fixture::cleanup(&root);
    Ok(())
}

#[test]
fn powershell_startup_works_in_pipe_mode() -> Result<(), Box<dyn std::error::Error>> {
    startup(false, "powershell-pipe-startup")
}

#[test]
fn powershell_startup_works_in_pty_mode() -> Result<(), Box<dyn std::error::Error>> {
    startup(true, "powershell-pty-startup")
}
