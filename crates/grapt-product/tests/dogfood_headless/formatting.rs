//! Formatting through actual provider calls, followed by file undo and restart.
use super::*;

fn configure(service: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let (program, arguments) = if cfg!(windows) {
        let script = service.join("formatter.cmd");
        fs::write(&script, "@echo off\r\n> \"%~1\" echo %FORMAT_VALUE%\r\n>>format-count <nul set /p =x\r\nif \"%~x1\"==\".fail\" exit /b 7\r\n")?;
        (
            std::env::var_os("COMSPEC").ok_or("command processor missing")?,
            vec![
                "/d".into(),
                "/c".into(),
                script.to_string_lossy().into_owned(),
                "$FILE".into(),
            ],
        )
    } else {
        ("/bin/sh".into(), vec!["-c".into(), "printf '%s\\n' \"$FORMAT_VALUE\" > \"$1\"; printf x >> format-count; case \"$1\" in *.fail) exit 7;; esac".into(), "formatter".into(), "$FILE".into()])
    };
    let mut command = vec![program
        .to_str()
        .ok_or("formatter executable encoding")?
        .to_owned()];
    command.extend(arguments);
    let path = service.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    config["formatter"] = serde_json::json!({"fixture":{"command":command,"environment":{"FORMAT_VALUE":"formatted"},"extensions":[".d.fmt",".fail"]}});
    fs::write(path, serde_json::to_vec(&config)?)?;
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn formatter_runs_after_edit_write_patch_and_preserves_failed_edits_and_undo(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let service = root("formatter-service");
    prepare(&service, 200_000);
    configure(&service)?;
    let workspace = root("formatter-workspace");
    process_fixture::cleanup(&workspace);
    fs::create_dir_all(&workspace)?;
    let input = |call, tool, arguments| Script::ToolInput {
        call,
        tool,
        arguments,
    };
    let provider = ProviderFixture::start(vec![
        input(
            "call_format_write",
            "write",
            serde_json::json!({"path":"notes.d.fmt","content":"unformatted\n"}),
        ),
        input(
            "call_format_edit",
            "edit",
            serde_json::json!({"path":"notes.d.fmt","old_text":"formatted","new_text":"edited"}),
        ),
        input(
            "call_format_patch",
            "apply_patch",
            serde_json::json!({"input":"*** Begin Patch\n*** Add File: patch.d.fmt\n+raw\n*** End Patch\n"}),
        ),
        input(
            "call_format_failure",
            "write",
            serde_json::json!({"path":"notes.fail","content":"retained\n"}),
        ),
        Script::Success,
    ]);
    let mut child = spawn(&service, provider.address);
    let address = await_ready(&service, &mut child);
    let session = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({"location":{"directory":workspace}}).to_string(),
    )["data"]["id"]
        .as_str()
        .ok_or("session identity")?
        .to_owned();
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"change files"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    for path in ["notes.d.fmt", "patch.d.fmt"] {
        assert_eq!(
            fs::read_to_string(workspace.join(path))?.replace("\r\n", "\n"),
            "formatted\n"
        );
    }
    assert_eq!(
        fs::read_to_string(workspace.join("notes.fail"))?,
        "retained\n"
    );
    assert_eq!(fs::read(workspace.join("format-count"))?, b"xxxx");
    for call in ["call_format_write", "call_format_edit", "call_format_patch"] {
        let output = permission_boundaries::output(&provider, call);
        assert!(
            output.contains("formatted") && output.contains("formatting"),
            "{output}"
        );
        assert!(!output.contains("undoUnavailable"), "{output}");
        let result: serde_json::Value = serde_json::from_str(&output)?;
        let files = result["files"]
            .as_array()
            .ok_or("final file receipts absent")?;
        assert_eq!(files.len(), 1, "{output}");
        let diff = &files[0]["diff"];
        assert_eq!(
            diff["newText"].as_str().map(|s| s.replace("\r\n", "\n")),
            Some("formatted\n".into())
        );
        if call == "call_format_write" {
            assert_eq!(
                result["bytes"],
                fs::metadata(workspace.join("notes.d.fmt"))?.len()
            );
            assert_eq!(diff["oldText"], "");
        } else if call == "call_format_edit" {
            assert_eq!(
                result["afterBytes"],
                fs::metadata(workspace.join("notes.d.fmt"))?.len()
            );
            assert!(
                !diff["newText"]
                    .as_str()
                    .is_some_and(|text| text.contains("edited")),
                "{output}"
            );
        }
    }
    let output = permission_boundaries::output(&provider, "call_format_failure");
    assert!(
        output.contains("code 7") && !output.contains("Tool failed"),
        "{output}"
    );
    child.stop_after_witness(&service);
    drop(child);
    let mut child = spawn(&service, provider.address);
    let address = await_ready(&service, &mut child);
    let preview = json(
        address,
        "POST",
        &format!("/api/session/{session}/undo/preview"),
        "{}",
    );
    let undo = json(address, "POST", &format!("/api/session/{session}/undo"), &serde_json::json!({"inputID":preview["data"]["inputID"],"target":preview["data"]["target"]}).to_string());
    assert_eq!(undo["data"]["status"], "applied", "{undo}");
    for path in ["notes.d.fmt", "patch.d.fmt", "notes.fail"] {
        assert!(!workspace.join(path).exists());
    }
    child.stop_after_witness(&service);
    drop(child);
    process_fixture::cleanup(&service);
    process_fixture::cleanup(&workspace);
    Ok(())
}

#[test]
fn repository_formatter_command_cannot_launch_and_does_not_fail_the_edit(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let service = root("formatter-project-service");
    prepare(&service, 120_000);
    let workspace = root("formatter-project-workspace");
    process_fixture::cleanup(&workspace);
    fs::create_dir_all(&workspace)?;
    fs::write(
        workspace.join("grapt.json"),
        r#"{"formatter":{"untrusted":{"command":["must-not-launch"],"extensions":[".fmt"]}}}"#,
    )?;
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_project_formatter",
            tool: "write",
            arguments: serde_json::json!({"path":"notes.d.fmt","content":"retained"}),
        },
        Script::Success,
    ]);
    let mut child = spawn(&service, provider.address);
    let address = await_ready(&service, &mut child);
    let session = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({"location":{"directory":workspace}}).to_string(),
    )["data"]["id"]
        .as_str()
        .ok_or("session identity")?
        .to_owned();
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"write notes"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(
        fs::read_to_string(workspace.join("notes.d.fmt"))?,
        "retained"
    );
    let output = permission_boundaries::output(&provider, "call_project_formatter");
    assert!(
        output.contains("project formatter configuration") && !output.contains("Tool failed"),
        "{output}"
    );
    child.stop_after_witness(&service);
    drop(child);
    process_fixture::cleanup(&service);
    process_fixture::cleanup(&workspace);
    Ok(())
}

#[test]
fn builtin_rustfmt_keeps_referenced_modules_unchanged_and_reports_localized_final_diff(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let service = root("formatter-rust-service");
    prepare(&service, 120_000);
    let config_path = service.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&config_path)?)?;
    config["formatter"] = true.into();
    fs::write(config_path, serde_json::to_vec(&config)?)?;
    let workspace = root("formatter-rust-workspace");
    process_fixture::cleanup(&workspace);
    fs::create_dir_all(&workspace)?;
    let external = root("formatter-external.rs");
    let unformatted = "pub fn untouched(  ){ }\n";
    fs::write(workspace.join("child.rs"), unformatted)?;
    fs::write(&external, unformatted)?;
    let source = format!(
        "mod child;\n#[path = {external:?}]\nmod external;\n{}fn main() {{}}\n",
        "// preserved context\n".repeat(600)
    );
    fs::write(workspace.join("main.rs"), &source)?;
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_rust_format",
            tool: "edit",
            arguments: serde_json::json!({"path":"main.rs","old_text":"fn main() {}","new_text":"fn main( ){ let value=1; }"}),
        },
        Script::Success,
    ]);
    let mut child = spawn(&service, provider.address);
    let address = await_ready(&service, &mut child);
    let session = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({"location":{"directory":workspace}}).to_string(),
    )["data"]["id"]
        .as_str()
        .ok_or("session identity")?
        .to_owned();
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"edit source"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(fs::read_to_string(workspace.join("child.rs"))?, unformatted);
    assert_eq!(fs::read_to_string(&external)?, unformatted);
    let result: serde_json::Value = serde_json::from_str(&permission_boundaries::output(
        &provider,
        "call_rust_format",
    ))?;
    assert!(
        fs::read_to_string(workspace.join("main.rs"))?.contains("let value = 1;"),
        "{result}"
    );
    let files = result["files"].as_array().ok_or("final receipts absent")?;
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["diff"]["omittedOldLines"], 0);
    assert_eq!(files[0]["diff"]["omittedNewLines"], 0);
    assert!(files[0]["diff"]["newText"]
        .as_str()
        .is_some_and(|text| text.contains("let value = 1;")));
    let preview = json(
        address,
        "POST",
        &format!("/api/session/{session}/undo/preview"),
        "{}",
    );
    let undo = json(address, "POST", &format!("/api/session/{session}/undo"), &serde_json::json!({"inputID":preview["data"]["inputID"],"target":preview["data"]["target"]}).to_string());
    assert_eq!(undo["data"]["status"], "applied", "{undo}");
    assert_eq!(fs::read_to_string(workspace.join("main.rs"))?, source);
    child.stop_after_witness(&service);
    drop(child);
    process_fixture::cleanup(&service);
    process_fixture::cleanup(&workspace);
    fs::remove_file(external)?;
    Ok(())
}
