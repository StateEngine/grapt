//! Image preparation metadata follows the selected model, never its name.
//! Copyright 2025 OpenAI.
//! Apache-2.0; Modified by Grapt: typed request items and explicit account metadata.
use grapt_provider::{CodexInputItem, ToolDefinition};

pub(super) fn filter_tools(tools: &mut Vec<ToolDefinition>, supports_images: bool) {
    if !supports_images {
        tools.retain(|tool| tool.name != "view_image");
    }
}

pub(super) fn normalise(
    items: &mut [CodexInputItem],
    supports_images: bool,
    original_supported: bool,
) -> Result<(), String> {
    use grapt_provider::{CodexToolText, InputMessage, MessageRole};
    const OMITTED: &str =
        "[Image input omitted: the selected model does not advertise image input.]";
    for item in items {
        match item {
            CodexInputItem::ImageUser { text, .. } if !supports_images => {
                *item = CodexInputItem::Message(InputMessage {
                    role: MessageRole::User,
                    content: format!("{}\n{OMITTED}", text.as_str()),
                });
            }
            CodexInputItem::ImageToolOutput { call, output, .. } if !supports_images => {
                *item = CodexInputItem::ToolOutput {
                    call: call.clone(),
                    output: CodexToolText::try_new(format!("{}\n{OMITTED}", output.as_str()))
                        .map_err(|error| error.to_string())?,
                    is_error: false,
                };
            }
            CodexInputItem::ImageUser { images, .. }
            | CodexInputItem::ImageToolOutput { images, .. } => {
                images.normalise_detail(original_supported);
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_provider::{CodexToolText, ImageDetail, ImageMedia, InputImage, InputImages};

    #[test]
    fn image_detail_fallback_preserves_pixels_and_supported_original() -> Result<(), String> {
        for supported in [false, true] {
            let image =
                InputImage::try_new(ImageMedia::Png, "YWJj".into(), ImageDetail::Original, 8, 6)?;
            let mut input = vec![CodexInputItem::ImageUser {
                text: CodexToolText::try_new("look".into()).map_err(|error| error.to_string())?,
                images: InputImages::try_new(vec![image])?,
            }];
            normalise(&mut input, true, supported)?;
            let CodexInputItem::ImageUser { text, images } = &input[0] else {
                return Err("image user item changed kind".into());
            };
            assert_eq!(text.as_str(), "look");
            assert_eq!(images.items()[0].encoded(), "YWJj");
            assert_eq!(images.items()[0].dimensions(), (8, 6));
            assert_eq!(
                images.items()[0].detail(),
                if supported {
                    ImageDetail::Original
                } else {
                    ImageDetail::High
                }
            );
        }
        Ok(())
    }

    #[test]
    fn switching_to_text_preserves_the_tool_result_and_marks_omitted_pixels() -> Result<(), String>
    {
        let call = grapt_types::ToolCallId::new("call_image").ok_or("call identity")?;
        let image =
            InputImage::try_new(ImageMedia::Png, "YWJj".into(), ImageDetail::Original, 8, 6)?;
        let mut input = vec![CodexInputItem::ImageToolOutput {
            call: call.clone(),
            output: CodexToolText::try_new("Viewed 8x6 image/png image.".into())
                .map_err(|error| error.to_string())?,
            images: InputImages::try_new(vec![image])?,
        }];
        normalise(&mut input, false, false)?;
        let CodexInputItem::ToolOutput {
            call: restored,
            output,
            is_error,
        } = &input[0]
        else {
            return Err("nonvision model retained image input".into());
        };
        assert_eq!(restored, &call);
        assert!(!*is_error);
        assert!(output.as_str().contains("Viewed 8x6 image/png image."));
        assert!(output
            .as_str()
            .contains("selected model does not advertise image input"));
        assert!(!output.as_str().contains("YWJj"));
        Ok(())
    }

    #[test]
    fn nonvision_models_keep_ordinary_tools_and_do_not_offer_view_image() {
        let original = crate::dogfood::tools::definitions();
        let mut tools = original.clone();
        filter_tools(&mut tools, false);
        assert!(!tools.iter().any(|tool| tool.name == "view_image"));
        assert!(tools.iter().any(|tool| tool.name == "read"));
        filter_tools(&mut tools, true);
        assert!(!tools.iter().any(|tool| tool.name == "view_image"));
        let mut supported = original;
        filter_tools(&mut supported, true);
        assert!(supported.iter().any(|tool| tool.name == "view_image"));
    }
}
