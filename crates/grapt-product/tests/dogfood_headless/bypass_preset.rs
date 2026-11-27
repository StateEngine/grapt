//! Explicit bypass executes an authority-file edit despite repository denials.
use super::*;

#[expect(clippy::too_many_lines, reason = "legacy")]
fn authority_edit(
    label: &str,
    selected: bool,
    profile: bool,
    delegated: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root(label);
    prepare(&root, 200_000);
    let project = root.join("project");
    fs::create_dir_all(&project)?;
    let target = project.join("grapt.json");
    let denied = r#"{"permission_preset":"inspect","permission":"deny"}"#;
    fs::write(&target, denied)?;
    if !selected {
        if profile {
            let path = root.join("service.conf");
            let mut config = fs::read_to_string(&path)?;
            config.push_str("permission_preset=bypass\n");
            fs::write(path, config)?;
        } else {
            fs::create_dir_all(root.join("global-config"))?;
            fs::write(
                root.join("global-config/grapt.json"),
                r#"{"permission_preset":"bypass"}"#,
            )?;
        }
    }
    let mut script = vec![
        Script::ToolInput {
            call: "call_seat02_authority_edit",
            tool: "write",
            arguments: serde_json::json!({"path":target,"content":"{}"}),
        },
        Script::SuccessText("bypass edit completed"),
    ];
    if delegated {
        script.insert(
            0,
            Script::ToolSubagent {
                call: "call_seat02_child",
                task: "edit the authority config",
            },
        );
        script.push(Script::SuccessText("parent completed"));
    }
    let provider = ProviderFixture::start(script);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({"location":{"directory":project}}).to_string(),
    )["data"]["id"]
        .as_str()
        .ok_or("session identity absent")?
        .to_owned();
    if selected {
        assert_eq!(
            request(
                address,
                "POST",
                &format!("/api/session/{session}/preset"),
                r#"{"preset":"bypass"}"#
            )
            .0,
            204
        );
    }
    assert_eq!(
        json(
            address,
            "GET",
            &format!("/api/session/{session}/preset"),
            ""
        )["data"]["effective"],
        "bypass"
    );
    // No approver subscribes: execution must come from the selected preset.
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"edit the project authority config"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(
        fs::read_to_string(&target)?,
        "{}",
        "bypass failed to execute the owner-floor edit"
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        if delegated { 4 } else { 2 }
    );
    if delegated {
        let page = json(address, "GET", "/api/session", "");
        let children = page["data"]
            .as_array()
            .ok_or("session list absent")?
            .iter()
            .filter(|row| row["parentID"] == session)
            .collect::<Vec<_>>();
        assert_eq!(children.len(), 1, "bypass did not delegate");
        let child_id = children[0]["id"].as_str().ok_or("child identity absent")?;
        assert_eq!(
            json(
                address,
                "GET",
                &format!("/api/session/{child_id}/preset"),
                ""
            )["data"]["effective"],
            "bypass"
        );
    }
    assert_eq!(
        json(
            address,
            "GET",
            &format!("/api/session/{session}/permission"),
            ""
        )["data"],
        serde_json::json!([])
    );
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}

#[test]
fn seat02_explicit_session_bypass_skips_config_denial_and_owner_ask(
) -> Result<(), Box<dyn std::error::Error>> {
    authority_edit("seat02-explicit", true, false, false)
}

#[test]
fn seat02_profile_bypass_runs_unattended_despite_repository_inspect(
) -> Result<(), Box<dyn std::error::Error>> {
    authority_edit("seat02-profile", false, true, false)
}

#[test]
fn seat02_global_user_bypass_runs_unattended_despite_repository_inspect(
) -> Result<(), Box<dyn std::error::Error>> {
    authority_edit("seat02-global", false, false, false)
}

#[test]
fn seat02_bypass_child_inherits_the_explicit_parent_selection(
) -> Result<(), Box<dyn std::error::Error>> {
    authority_edit("seat02-child", true, false, true)
}
