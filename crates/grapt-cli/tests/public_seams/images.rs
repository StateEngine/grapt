use super::*;
use grapt_cli::{CliCommand, PromptSource};

#[tokio::test]
async fn repeated_images_reach_prompt_admission_in_order_and_stay_bounded() -> TestResult {
    let parsed = CliCommand::parse([
        "run",
        "--service",
        "root",
        "--image",
        "one.png",
        "-i",
        "two.png",
    ])?;
    assert!(
        matches!(parsed, CliCommand::Run { images, prompt: PromptSource::Argument(text), .. }
        if images == [PathBuf::from("one.png"),PathBuf::from("two.png")] && text.is_empty())
    );
    assert!(
        CliCommand::parse(["attach", "--service", "root", "--new", "--image", "one.png"]).is_err()
    );
    assert!(CliCommand::parse([
        "run",
        "--service",
        "root",
        "--image",
        "1",
        "--image",
        "2",
        "--image",
        "3",
        "--image",
        "4",
        "--image",
        "5"
    ])
    .is_err());
    let image = grapt_image::prepare_rgba(2, 2, vec![255; 16])?;
    let values: Vec<_> = ["one.png", "two.png"]
        .into_iter()
        .map(|name| serde_json::json!({"uri":image.data_url(),"name":name}))
        .collect();
    let files: Vec<grapt_api::u3a::PromptInputFileAttachmentDto> = values
        .iter()
        .cloned()
        .map(serde_json::from_value)
        .collect::<Result<_, _>>()?;
    let accepted = r#"{"data":{"admittedSeq":1,"id":"msg_user","sessionID":"ses_a","timeCreated":1.0,"type":"user","data":{"text":"describe"},"delivery":"queue"}}"#;
    let (endpoint, server) = spawn_server(vec![Script::json(
        "POST",
        "/api/session/ses_a/prompt",
        200,
        accepted,
    )
    .expect_json(serde_json::json!({"text":"describe","files":values}))])
    .await?;
    let client = client(&endpoint)?;
    let session: grapt_types::SessionId = "ses_a".parse()?;
    client
        .prompt_with_files(&session, "describe".into(), &files)
        .await?;
    server.await??;
    let too_many = vec![files[0].clone(); 5];
    assert!(matches!(
        client
            .prompt_with_files(&session, "kept".into(), &too_many)
            .await,
        Err(grapt_cli::ApiClientError::PromptBound)
    ));
    Ok(())
}
