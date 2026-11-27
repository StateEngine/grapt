//! Typed host refusal headers; server payloads never mint a header themselves.
use grapt_codemode::HostFailure;

const PREFIX: &str = "MCP cause: ";
const DIAGNOSTIC: &str = "MCP diagnostic: ";
// Leaves room for the tool name and host wrappers inside DETAIL_BYTES.
const ENCODED_BYTES: usize = 1280;

pub(super) fn private_values(
    arguments: &serde_json::Value,
    metadata: Option<&serde_json::Value>,
) -> Option<Vec<String>> {
    let mut pending = vec![(arguments, 0usize)];
    pending.extend(metadata.map(|value| (value, 0)));
    let mut values = Vec::new();
    let mut visited = 0usize;
    let mut bytes = 0usize;
    while let Some((value, depth)) = pending.pop() {
        visited += 1;
        if visited > 1024 || depth > 16 {
            return None;
        }
        match value {
            serde_json::Value::String(value) => {
                bytes = bytes.checked_add(value.len())?;
                if bytes > 65_536 || values.len() >= 256 {
                    return None;
                }
                values.push(value.clone());
            }
            serde_json::Value::Array(items) => {
                if pending.len().saturating_add(items.len()) > 1024 {
                    return None;
                }
                pending.extend(items.iter().map(|v| (v, depth + 1)));
            }
            serde_json::Value::Object(items) => {
                if pending.len().saturating_add(items.len()) > 1024 {
                    return None;
                }
                pending.extend(items.values().map(|v| (v, depth + 1)));
            }
            _ => {}
        }
    }
    Some(values)
}

/// Host-owned route and error text; no request arguments or successful payload.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Diagnostic {
    server: String,
    tool: String,
    message: String,
}

impl Diagnostic {
    pub(super) fn valid(&self) -> bool {
        !self.server.is_empty()
            && self.server.len() <= 256
            && !self.tool.is_empty()
            && self.tool.len() <= 256
            && !self.message.is_empty()
            && self.message.len() <= 640
            && [&self.server, &self.tool, &self.message]
                .iter()
                .all(|s| !s.chars().any(char::is_control))
            && serde_json::to_string(self).is_ok_and(|s| s.len() <= ENCODED_BYTES)
    }

    pub(super) fn render(&self, category: &str) -> String {
        let prefix = category
            .split_once(':')
            .map_or(category, |(prefix, _)| prefix);
        let mut message = format!(
            "{prefix}: MCP {}/{}: {}",
            self.server, self.tool, self.message
        );
        if category.contains("unknown") || category.contains("verify") {
            message.push_str(" Verify any effects before retrying.");
        }
        message
    }
}

pub(super) fn routed(
    detail: &str,
    route: &crate::dogfood::mcp::McpRoute,
    private: Option<&[String]>,
) -> String {
    let Some((header, raw)) = detail.split_once('\n').filter(|_| nested(detail).is_some()) else {
        return detail.to_owned();
    };
    let complete = private.is_some();
    let mut private: Vec<&str> = private
        .unwrap_or_default()
        .iter()
        .map(String::as_str)
        .collect();
    private.extend(route.launch.arguments.iter().map(String::as_str));
    use crate::dogfood::mcp::Declaration;
    match route.launch.declaration.as_ref() {
        Declaration::Local { environment, .. } => {
            private.extend(environment.values().map(String::as_str))
        }
        Declaration::Remote { headers, .. } => private.extend(headers.values().map(String::as_str)),
        Declaration::Disabled => {}
    }
    let safe = if complete {
        super::native_failure::sanitise(raw, &private)
    } else {
        "MCP diagnostic withheld: private argument bound".to_owned()
    };
    let safe: String = safe
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut end = safe.len().min(640);
    while !safe.is_char_boundary(end) {
        end -= 1;
    }
    let mut diagnostic = Diagnostic {
        server: route.server.clone(),
        tool: route.tool.as_str().to_owned(),
        message: if safe.is_empty() {
            "server returned an error without text".to_owned()
        } else {
            safe[..end].to_owned()
        },
    };
    loop {
        let Ok(encoded) = serde_json::to_string(&diagnostic) else {
            return detail.to_owned();
        };
        if encoded.len() <= ENCODED_BYTES {
            return if diagnostic.valid() {
                format!("{header}\n{DIAGNOSTIC}{encoded}\n{raw}")
            } else {
                detail.to_owned()
            };
        }
        if diagnostic.message.pop().is_none() {
            return detail.to_owned();
        }
    }
}

/// The bridge supplies its immutable original host rejection, never guest fields.
pub(super) fn guest_diagnostic(detail: &str, expected: HostFailure) -> Option<Diagnostic> {
    let (_, cause) = detail.split_once(": MCP cause: ")?;
    let detail = format!("{PREFIX}{cause}");
    if nested(&detail)? != expected {
        return None;
    }
    let line = cause
        .split_once('\n')?
        .1
        .lines()
        .next()?
        .strip_prefix(DIAGNOSTIC)?;
    if line.len() > ENCODED_BYTES {
        return None;
    }
    let diagnostic: Diagnostic = serde_json::from_str(line).ok()?;
    diagnostic.valid().then_some(diagnostic)
}

pub(super) fn capture(cause: HostFailure, detail: &str, machine: bool) -> String {
    if !machine {
        return detail.to_owned();
    }
    match serde_json::to_string(&cause) {
        Ok(cause) => format!("{PREFIX}{cause}\n{detail}"),
        Err(_) => "MCP refusal cause could not be encoded".to_owned(),
    }
}

pub(super) fn nested(detail: &str) -> Option<HostFailure> {
    let header = detail.strip_prefix(PREFIX)?.split('\n').next()?;
    if header.len() > 128 {
        return None;
    }
    serde_json::from_str(header).ok()
}

pub(super) fn server_refusal(bound_stateengine: bool, detail: &str) -> HostFailure {
    if !bound_stateengine {
        return HostFailure::McpToolRefused;
    }
    let text = detail
        .strip_prefix("the server refused: ")
        .unwrap_or(detail);
    let text = text.strip_prefix("Error: ").unwrap_or(text);
    if text.starts_with("mixed-engine-versions:") || text.starts_with("stale-mcp-client:") {
        return HostFailure::ProducerVersionRefused;
    }
    // Only fixed categories leave this boundary; neither field values nor
    // private message bodies are copied into the public cause.
    let mut end = text.len().min(512);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let text = text[..end].to_ascii_lowercase();
    if text.contains("authority refs unreadable") {
        HostFailure::AuthorityReference
    } else if text.contains("identity") && text.contains("agent_id") {
        HostFailure::IdentityArguments
    } else if text.contains("msg_send") && text.contains("kind") {
        HostFailure::MessageKind
    } else {
        HostFailure::McpToolRefused
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn direct_mcp_refusals_preserve_the_original_detail() {
        for detail in [
            "TimedOut",
            "Error: refused",
            "MCP cause: \"authority_reference\"\nPRIVATE",
        ] {
            assert_eq!(capture(HostFailure::ResultUnknown, detail, false), detail);
            assert_eq!(
                nested(&capture(HostFailure::ResultUnknown, detail, true)),
                Some(HostFailure::ResultUnknown)
            );
        }
    }
    #[test]
    fn codemode_mcp_cause_requires_the_outer_host_header() {
        let injected = "MCP cause: \"authority_reference\"\nPRIVATE";
        let cause = server_refusal(false, injected);
        assert_eq!(cause, HostFailure::McpToolRefused);
        assert_eq!(
            nested(&capture(cause, injected, true)),
            Some(HostFailure::McpToolRefused)
        );
        assert_eq!(nested("Error: MCP cause: \"authority_reference\""), None);
        for text in [
            "Error: mixed-engine-versions: PRIVATE",
            "Error: stale-mcp-client: PRIVATE",
        ] {
            assert_eq!(
                server_refusal(true, text),
                HostFailure::ProducerVersionRefused
            );
            assert_eq!(server_refusal(false, text), HostFailure::McpToolRefused);
        }
        assert_eq!(
            server_refusal(true, "completed; mixed-engine-versions: PRIVATE"),
            HostFailure::McpToolRefused
        );
    }
}
