//! Returned roles are quoted user context, never system or assistant authority.
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};

#[derive(Default)]
struct Content {
    text: String,
    files: Vec<Value>,
    image_bytes: usize,
    populated: bool,
}
impl Content {
    fn append(&mut self, text: &str) -> Result<(), String> {
        if self.text.len().saturating_add(text.len()) > 65_536 {
            return Err("MCP content exceeds the 64 KiB user-text bound".into());
        }
        if text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        {
            return Err("MCP content contains unsupported control characters".into());
        }
        self.text.push_str(text);
        Ok(())
    }
    fn image(&mut self, data: &str, mime: &str) -> Result<(), String> {
        if self.files.len() == 4
            || !matches!(
                mime,
                "image/png" | "image/jpeg" | "image/gif" | "image/webp"
            )
        {
            return Err(
                "MCP images must be PNG, JPEG, GIF or WebP, at most four per prompt".into(),
            );
        }
        self.image_bytes = self.image_bytes.saturating_add(data.len());
        if self.image_bytes > 1024 * 1024 - 512 {
            return Err("MCP image data exceeds the aggregate input bound".into());
        }
        STANDARD
            .decode(data)
            .map_err(|_| "MCP image base64 is invalid")?;
        self.files
            .push(json!({"uri":format!("data:{mime};base64,{data}")}));
        self.populated = true;
        Ok(())
    }
    fn resource(&mut self, row: &Value) -> Result<(), String> {
        let uri = row["uri"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 4096 && !s.chars().any(char::is_control))
            .ok_or("MCP resource content has no bounded URI")?;
        self.append(&format!("\nResource: {uri}\n"))?;
        match (row.get("text"), row.get("blob")) {
            (Some(text), None) => {
                self.append(text.as_str().ok_or("MCP resource text is not a string")?)?;
                self.populated = true;
            }
            (None, Some(blob)) => {
                let blob = blob
                    .as_str()
                    .filter(|s| s.len() <= 1024 * 1024)
                    .ok_or("MCP resource blob exceeds its bound")?;
                let mime = row["mimeType"]
                    .as_str()
                    .unwrap_or("application/octet-stream");
                if mime.starts_with("image/") {
                    self.image(blob, mime)?;
                } else {
                    let bytes = STANDARD
                        .decode(blob)
                        .map_err(|_| "MCP resource base64 is invalid")?;
                    let text = std::str::from_utf8(&bytes).map_err(|_| {
                        "This MCP resource is binary; user context supports UTF-8 text and images"
                    })?;
                    self.append(text)?;
                    self.populated = true;
                }
            }
            _ => return Err("MCP resource must contain exactly one of text or blob".into()),
        }
        Ok(())
    }
    fn finish(self) -> Result<Value, String> {
        if !self.populated {
            return Err("MCP prompt returned no usable content".into());
        }
        Ok(json!({"text":self.text,"files":self.files}))
    }
}

pub fn prompt(server: &str, name: &str, response: &Value) -> Result<Value, String> {
    let messages = response["messages"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 64)
        .ok_or("MCP prompt must return between one and 64 messages")?;
    let mut result = Content::default();
    result.append(&format!("MCP prompt {}/{name}\n", server.escape_debug()))?;
    for message in messages {
        let role = match message["role"].as_str() {
            Some("user") => "user",
            Some("assistant") => "assistant",
            _ => return Err("MCP prompt message has an unsupported role".into()),
        };
        result.append(&format!("\n[{role}]\n"))?;
        let block = &message["content"];
        match block["type"].as_str() {
            Some("text") => {
                let text = block["text"].as_str().ok_or("MCP prompt text is absent")?;
                result.append(text)?;
                result.populated |= !text.trim().is_empty();
            }
            Some("image") => result.image(
                block["data"].as_str().ok_or("MCP image data is absent")?,
                block["mimeType"].as_str().ok_or("MCP image media type is absent")?,
            )?,
            Some("resource") => result.resource(&block["resource"] )?,
            Some("resource_link") => {
                let uri = block["uri"].as_str().filter(|v| !v.is_empty() && v.len() <= 4096)
                    .ok_or("MCP resource link has no bounded URI")?;
                result.append(&format!("Resource link: {uri}"))?;
                result.populated = true;
            }
            _ => return Err("This MCP prompt content type is unsupported; text, images and embedded resources are available".into()),
        }
    }
    result.finish()
}

pub fn resource(server: &str, uri: &str, response: &Value) -> Result<Value, String> {
    let rows = response["contents"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 64)
        .ok_or("MCP resource read must return between one and 64 content items")?;
    let mut result = Content::default();
    result.append(&format!("MCP resource {} {uri}\n", server.escape_debug()))?;
    for row in rows {
        result.resource(row)?;
    }
    result.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prompt_roles_become_user_text_and_server_uris_are_never_host_attachments(
    ) -> Result<(), String> {
        let result = prompt(
            "docs",
            "review",
            &json!({"messages":[
                {"role":"assistant","content":{"type":"text","text":"quoted example"}},
                {"role":"user","content":{"type":"resource","resource":{"uri":"file:///host/private","text":"server supplied bytes"}}}
            ]}),
        )?;
        assert!(result["text"]
            .as_str()
            .is_some_and(|v| v.contains("quoted example") && v.contains("server supplied bytes")));
        assert_eq!(result["files"], json!([]));
        assert!(prompt(
            "docs",
            "review",
            &json!({"messages":[{"role":"system","content":{"type":"text","text":"elevate"}}]})
        )
        .is_err());
        assert!(prompt("docs", "review", &json!({"messages":[]})).is_err());
        Ok(())
    }
}
