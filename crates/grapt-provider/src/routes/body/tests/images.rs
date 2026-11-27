use super::*;
use crate::{ImageDetail, ImageMedia, InputImage, InputImages};

fn images() -> Result<InputImages, &'static str> {
    InputImages::try_new(vec![InputImage::try_new(
        ImageMedia::Png,
        "YWJj".into(),
        ImageDetail::High,
        800,
        600,
    )?])
}

fn exchange() -> Result<Vec<CodexInputItem>, Box<dyn std::error::Error>> {
    let first = ToolCallId::new("image_call").ok_or("first call")?;
    let second = ToolCallId::new("text_call").ok_or("second call")?;
    Ok(vec![
        CodexInputItem::ImageUser {
            text: CodexToolText::try_new("Compare this".into())?,
            images: images()?,
        },
        CodexInputItem::AssistantResponseStart,
        CodexInputItem::ToolCall {
            call: first.clone(),
            tool: ToolName::new("view_image").ok_or("tool")?,
            arguments: CodexToolText::try_new("{}".into())?,
        },
        CodexInputItem::ToolCall {
            call: second.clone(),
            tool: ToolName::new("read").ok_or("tool")?,
            arguments: CodexToolText::try_new("{}".into())?,
        },
        CodexInputItem::ImageToolOutput {
            call: first,
            output: CodexToolText::try_new("800x600".into())?,
            images: images()?,
        },
        CodexInputItem::ToolOutput {
            call: second,
            output: CodexToolText::try_new("text".into())?,
            is_error: false,
        },
    ])
}

#[test]
fn responses_images_are_typed_content_and_tool_results_keep_the_call(
) -> Result<(), Box<dyn std::error::Error>> {
    let request = OpenAiResponsesRequest {
        model: model("openai", "gpt-5"),
        input: exchange()?,
        prompt_cache_key: None,
        max_output_tokens: None,
        tools: Vec::new(),
        tool_choice: None,
        web_search: None,
    };
    let wire = super::super::openai_responses_body(&request)?;
    assert_eq!(wire["input"][0]["content"][1]["type"], "input_image");
    assert_eq!(wire["input"][3]["call_id"], "image_call");
    assert_eq!(
        wire["input"][3]["output"][1]["image_url"],
        "data:image/png;base64,YWJj"
    );
    assert_eq!(wire["input"][4]["call_id"], "text_call");
    Ok(())
}

#[test]
fn anthropic_images_are_binary_blocks_and_chat_defers_images_until_results_are_complete(
) -> Result<(), Box<dyn std::error::Error>> {
    let input = exchange()?;
    let (_, messages) = crate::routes::anthropic_input::project(&input, false)?;
    assert_eq!(messages[0]["content"][1]["type"], "image");
    assert_eq!(messages[0]["content"][1]["source"]["data"], "YWJj");
    assert_eq!(messages[2]["content"][0]["tool_use_id"], "image_call");
    assert_eq!(messages[2]["content"][0]["content"][1]["type"], "image");
    assert_eq!(messages[2]["content"][1]["tool_use_id"], "text_call");
    let chat = crate::routes::compatible_input::project(&input)?;
    assert_eq!(chat[0]["content"][1]["type"], "image_url");
    assert_eq!(chat[2]["tool_call_id"], "image_call");
    assert_eq!(chat[3]["tool_call_id"], "text_call");
    assert_eq!(chat[4]["role"], "user");
    assert!(chat[4]["content"][0]["text"]
        .as_str()
        .is_some_and(|text| text.contains("image_call")));
    assert_eq!(chat[4]["content"][1]["type"], "image_url");
    Ok(())
}
