use super::*;
#[test]
fn larger_prompt_bodies_require_explicit_bounded_inline_images(
) -> Result<(), Box<dyn std::error::Error>> {
    let text = serde_json::to_vec(&serde_json::json!({"text":"x".repeat(65_536)}))?;
    assert!(
        decode_image_prompt(&text).is_err(),
        "ordinary requests keep their host limit"
    );
    let image = serde_json::to_vec(
        &serde_json::json!({"text":"describe","files":[{"uri":format!("data:image/png;base64,{}","a".repeat(300_000))}]}),
    )?;
    assert!(
        decode_request::<super::super::PromptRequestDto>(&image, U3aLimits::production()?).is_err()
    );
    assert!(decode_image_prompt(&image).is_ok());
    let huge = serde_json::to_vec(
        &serde_json::json!({"text":"describe","files":[{"uri":format!("data:image/png;base64,{}","a".repeat(1_048_576))}]}),
    )?;
    assert!(decode_image_prompt(&huge).is_err());
    assert!(decode_image_prompt(&vec![b' '; 4 * 1024 * 1024 + 1]).is_err());
    Ok(())
}
