//! Actual prepare/permission/effect failures survive the public history fold.
use super::*;

#[derive(Clone, Copy)]
enum Case {
    Corrupt,
    Unsupported,
    SourceBound,
    OriginalBound,
    Denied,
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn journey(case: Case, label: &str) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root(label);
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 200_000);
    let config_path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&config_path)?)?;
    if matches!(case, Case::Denied) {
        config["permission"] = serde_json::json!({"read":"deny"});
    }
    fs::write(config_path, serde_json::to_vec(&config)?)?;
    let catalog_path = root.join("model-catalog.json");
    let mut catalog: serde_json::Value = serde_json::from_slice(&fs::read(&catalog_path)?)?;
    catalog["openai"]["models"]["gpt-live"]["modalities"]["input"] =
        serde_json::json!(["text", "image"]);
    fs::write(catalog_path, serde_json::to_vec(&catalog)?)?;
    let account_path = root.join("account-models.json");
    let mut account: serde_json::Value = serde_json::from_slice(&fs::read(&account_path)?)?;
    account["models"][0]["supports_image_detail_original"] = true.into();
    fs::write(account_path, serde_json::to_vec(&account)?)?;
    let path = root.join("image.png");
    let width = if matches!(case, Case::OriginalBound) {
        4097
    } else {
        2
    };
    image::DynamicImage::new_rgb8(width, 1).save(&path)?;
    match case {
        Case::Corrupt => {
            let mut bytes = fs::read(&path)?;
            bytes.truncate(bytes.len() - 12);
            fs::write(&path, bytes)?;
        }
        Case::Unsupported => {
            // A valid portable pixmap needs no optional encoder feature.
            fs::write(&path, b"P6\n2 1\n255\n\xff\0\0\0\0\xff")?;
        }
        Case::SourceBound => fs::File::create(&path)?.set_len(16 * 1024 * 1024 + 1)?,
        _ => {}
    }
    let source_len = fs::metadata(&path)?.len();
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_image_refusal",
            tool: "view_image",
            arguments: serde_json::json!({"path":"image.png","detail":"original"}),
        },
        Script::Success,
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    assert_eq!(
        request(
            address,
            "POST",
            &format!("/api/session/{session}/preset"),
            r#"{"preset":"inspect"}"#
        )
        .0,
        204
    );
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_image_refusal","text":"view image.png"}"#,
    );
    manual_compaction::assert_carried(&root, "msg_image_refusal", "session.execution.succeeded.1");
    permission_residuals::wait_session_idle(address, &session);
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    );
    let tool = history["data"]
        .as_array()
        .ok_or("history absent")?
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .find(|item| item["type"] == "tool" && item["id"] == "call_image_refusal")
        .ok_or("image tool absent from actual history")?;
    assert_eq!(tool["state"]["status"], "error");
    assert_eq!(tool["state"]["metadata"]["publicBoundBytes"], 2048);
    let detail = tool["state"]["content"]
        .as_array()
        .ok_or_else(|| format!("image cause omitted: {tool}"))?
        .iter()
        .find_map(|item| item["text"].as_str())
        .ok_or("public image cause absent")?;
    let expected = match case {
        Case::Corrupt => "truncated or corrupt",
        Case::Unsupported => "supported image formats",
        Case::SourceBound => "16 MiB",
        Case::OriginalBound => "use high detail",
        Case::Denied => "denied by this Session's permission policy",
    };
    assert!(detail.contains(expected), "{detail}");
    assert!(detail.len() <= 2048);
    assert!(!history.to_string().contains("secode-image-v1"));
    let bodies = locked(&provider.bodies);
    assert_eq!(bodies.len(), 2);
    let request: serde_json::Value = serde_json::from_str(&bodies[1])?;
    let output = request["input"]
        .as_array()
        .ok_or("provider input")?
        .iter()
        .find(|item| {
            item["type"] == "function_call_output" && item["call_id"] == "call_image_refusal"
        })
        .and_then(|item| item["output"].as_str())
        .ok_or("provider refusal absent")?;
    assert!(
        output.contains(if matches!(case, Case::Denied) {
            "denied"
        } else {
            "EffectFailed"
        }),
        "{output}"
    );
    assert!(
        !request["input"].to_string().contains("input_image"),
        "refusal leaked pixels"
    );
    drop(bodies);
    let db = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let asks: i64 = db.query_row(
        "SELECT count(*) FROM event WHERE type='permission.asked.2'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        asks, 0,
        "fixture did not cover calls without permission records"
    );
    assert_eq!(fs::metadata(path)?.len(), source_len);
    child.stop_after_witness(&root);
    Ok(())
}

#[test]
fn corrupt_image_has_a_public_cause_without_a_permission_record(
) -> Result<(), Box<dyn std::error::Error>> {
    journey(Case::Corrupt, "image-detail-corrupt")
}
#[test]
fn unsupported_image_has_a_public_cause_without_a_permission_record(
) -> Result<(), Box<dyn std::error::Error>> {
    journey(Case::Unsupported, "image-detail-unsupported")
}
#[test]
fn over_bound_image_has_a_public_cause_without_a_permission_record(
) -> Result<(), Box<dyn std::error::Error>> {
    journey(Case::SourceBound, "image-detail-source-bound")?;
    journey(Case::OriginalBound, "image-detail-original-bound")
}
#[test]
fn denied_image_has_a_public_policy_cause_without_a_permission_record(
) -> Result<(), Box<dyn std::error::Error>> {
    journey(Case::Denied, "image-detail-denied")
}
