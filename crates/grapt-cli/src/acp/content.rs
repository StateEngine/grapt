//! ACP content projection without network fetching or ambient file reads.

use serde_json::{json, Value};

use super::RpcFault;

const MAX_CONTENT_BLOCKS: usize = 256;
const MAX_FILES: usize = 64;

#[derive(Debug)]
pub(crate) struct PromptContent {
    pub(crate) text: String,
    pub(crate) files: Vec<Value>,
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(crate) fn prompt_content(params: &Value, maximum: usize) -> Result<PromptContent, RpcFault> {
    let prompt = params
        .get("prompt")
        .and_then(Value::as_array)
        .ok_or_else(|| RpcFault::invalid_params("prompt must be an array"))?;
    if prompt.len() > MAX_CONTENT_BLOCKS {
        return Err(RpcFault::invalid_params(
            "prompt carries too many content blocks",
        ));
    }
    let mut parts = Vec::new();
    let mut files = Vec::new();
    for block in prompt {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if audience_is(block, "user") {
                    continue;
                }
                let text = required_string(block, "text")?;
                if !text.is_empty() {
                    if audience_is(block, "assistant") {
                        parts.push(format!("[ACP assistant context]\n{text}"));
                    } else {
                        parts.push(text.to_owned());
                    }
                }
            }
            Some("resource_link") => {
                let uri = required_string(block, "uri")?;
                if uri.starts_with("file://") {
                    push_file(&mut files, uri, file_name(block, uri))?;
                } else {
                    parts.push(uri.to_owned());
                }
            }
            Some("resource") => {
                let resource = block
                    .get("resource")
                    .and_then(Value::as_object)
                    .ok_or_else(|| RpcFault::invalid_params("embedded resource is malformed"))?;
                let uri = resource
                    .get("uri")
                    .and_then(Value::as_str)
                    .ok_or_else(|| RpcFault::invalid_params("embedded resource uri is absent"))?;
                if let Some(text) = resource.get("text").and_then(Value::as_str) {
                    parts.push(format!("[{uri}]\n{text}"));
                } else {
                    let blob = resource
                        .get("blob")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            RpcFault::invalid_params("embedded resource content is absent")
                        })?;
                    let mime = resource
                        .get("mimeType")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            RpcFault::invalid_params("binary resource mimeType is absent")
                        })?;
                    let file_uri = if uri.starts_with("data:") {
                        uri.to_owned()
                    } else {
                        format!("data:{mime};base64,{blob}")
                    };
                    push_file(&mut files, &file_uri, name_from_uri(uri, "file"))?;
                }
            }
            Some("image") => {
                let mime = required_string(block, "mimeType")?;
                let uri = match block.get("data").and_then(Value::as_str) {
                    Some(data) if !data.is_empty() => format!("data:{mime};base64,{data}"),
                    _ => block
                        .get("uri")
                        .and_then(Value::as_str)
                        .filter(|uri| {
                            uri.starts_with("data:")
                                || uri.starts_with("http://")
                                || uri.starts_with("https://")
                        })
                        .map(str::to_owned)
                        .ok_or_else(|| RpcFault::invalid_params("image content is absent"))?,
                };
                let name = block
                    .get("uri")
                    .and_then(Value::as_str)
                    .map(|uri| name_from_uri(uri, "image"))
                    .unwrap_or_else(|| "image".to_owned());
                push_file(&mut files, &uri, name)?;
            }
            Some("audio") => {
                return Err(RpcFault::invalid_params(
                    "audio content is not an advertised prompt capability",
                ));
            }
            _ => return Err(RpcFault::invalid_params("unknown prompt content type")),
        }
    }
    let text = parts.join("\n");
    let observed = files.iter().fold(text.len(), |total, file| {
        total.saturating_add(file.to_string().len())
    });
    if observed > maximum {
        return Err(RpcFault::invalid_params(format!(
            "projected prompt exceeds its finite {maximum}-byte bound"
        )));
    }
    Ok(PromptContent { text, files })
}

fn push_file(files: &mut Vec<Value>, uri: &str, name: String) -> Result<(), RpcFault> {
    if files.len() >= MAX_FILES {
        return Err(RpcFault::invalid_params(
            "prompt carries too many file or image parts",
        ));
    }
    files.push(json!({"uri":uri, "name":name}));
    Ok(())
}

fn file_name(block: &Value, uri: &str) -> String {
    block
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| name_from_uri(uri, "file"))
}

fn name_from_uri(uri: &str, fallback: &str) -> String {
    uri.split(['/', '\\'])
        .next_back()
        .and_then(|name| name.split(['?', '#']).next())
        .filter(|name| !name.is_empty() && !name.starts_with("data:"))
        .unwrap_or(fallback)
        .to_owned()
}

fn audience_is(block: &Value, expected: &str) -> bool {
    block
        .get("annotations")
        .and_then(|value| value.get("audience"))
        .and_then(Value::as_array)
        .is_some_and(|audience| audience.len() == 1 && audience[0].as_str() == Some(expected))
}

fn required_string<'a>(value: &'a Value, key: &'static str) -> Result<&'a str, RpcFault> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| RpcFault::invalid_params(format!("prompt content {key} is absent")))
}

#[cfg(test)]
mod tests {
    use super::prompt_content;
    use serde_json::json;

    #[test]
    fn content_preserves_text_context_images_files_and_audience() {
        let result = prompt_content(
            &json!({"prompt":[
                {"type":"text","text":"visible"},
                {"type":"text","text":"context","annotations":{"audience":["assistant"]}},
                {"type":"text","text":"ignored","annotations":{"audience":["user"]}},
                {"type":"resource_link","uri":"file:///work/readme.md","name":"readme.md"},
                {"type":"image","mimeType":"image/png","data":"AA==","uri":"file:///image.png"},
                {"type":"resource","resource":{"uri":"memo://one","mimeType":"text/plain","text":"memo"}}
            ]}),
            8_192,
        );
        let Ok(content) = result else {
            panic!("valid ACP content was refused: {result:?}");
        };
        assert!(content.text.contains("visible"));
        assert!(content.text.contains("[ACP assistant context]\ncontext"));
        assert!(
            !content.text.contains("ignored"),
            "user-audience content leaked into the projected prompt"
        );
        assert!(content.text.contains("[memo://one]\nmemo"));
        assert_eq!(content.files.len(), 2);
        assert_eq!(content.files[0]["uri"], "file:///work/readme.md");
        assert_eq!(content.files[1]["uri"], "data:image/png;base64,AA==");
    }
}
