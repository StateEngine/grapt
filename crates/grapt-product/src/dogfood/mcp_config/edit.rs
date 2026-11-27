//! Change one MCP declaration while retaining other values and JSONC comments.
use grapt_config::{ConfigValue, Spanned};
use serde_json::Value;

use crate::dogfood::catalog_config::{bounds, DOCUMENT_BYTES};

pub(crate) fn approval(
    bytes: &[u8],
    location: &str,
    name: &str,
    value: Option<&Value>,
) -> Result<Vec<u8>, String> {
    let document = grapt_config::decode(bytes, bounds()?).map_err(|error| error.to_string())?;
    let mut output =
        String::from_utf8(bytes.to_vec()).map_err(|_| "MCP configuration is not UTF-8")?;
    let encoded = value
        .map(serde_json::to_string)
        .transpose()
        .map_err(|_| "MCP approval cannot be encoded")?;
    change(
        &mut output,
        &document,
        &["mcpProjectApprovals", location, name],
        encoded.as_deref(),
    )?;
    if output.len() > DOCUMENT_BYTES {
        return Err("MCP configuration exceeds its document bound".into());
    }
    let updated =
        grapt_config::decode(output.as_bytes(), bounds()?).map_err(|error| error.to_string())?;
    if let Some(projects) = updated.value().get("mcpProjectApprovals") {
        let projects = projects
            .value()
            .as_object()
            .ok_or("MCP project approvals must be an object")?;
        if projects.len() > 256
            || projects.iter().any(|(_, servers)| {
                servers
                    .value()
                    .as_object()
                    .is_none_or(|servers| servers.len() > 256)
            })
        {
            return Err("MCP project approvals exceed their project/server bounds".into());
        }
    }
    Ok(output.into_bytes())
}

pub(crate) fn enabled(bytes: &[u8], name: &str, enabled: bool) -> Result<Vec<u8>, String> {
    let document = grapt_config::decode(bytes, bounds()?).map_err(|error| error.to_string())?;
    let mcp = document
        .value()
        .get("mcp")
        .ok_or("MCP declaration does not exist in user config")?;
    let flat = mcp.value().get("servers").is_none() && mcp.value().get("timeout").is_none();
    let servers = if flat {
        mcp
    } else {
        mcp.value()
            .get("servers")
            .ok_or("MCP declaration does not exist in user config")?
    };
    let declaration = servers
        .value()
        .get(name)
        .ok_or("MCP declaration does not exist in user config")?;
    if declaration.value().as_object().is_none() {
        return Err("MCP declaration must be an object".into());
    }
    let legacy = declaration.value().get("enabled").is_some();
    let mut path = if flat {
        vec!["mcp", name]
    } else {
        vec!["mcp", "servers", name]
    };
    path.push("disabled");
    let mut output =
        String::from_utf8(bytes.to_vec()).map_err(|_| "MCP configuration is not UTF-8")?;
    change(
        &mut output,
        &document,
        &path,
        Some(if enabled { "false" } else { "true" }),
    )?;
    if legacy {
        let document = grapt_config::decode(output.as_bytes(), bounds()?)
            .map_err(|error| error.to_string())?;
        path.pop();
        path.push("enabled");
        change(
            &mut output,
            &document,
            &path,
            Some(if enabled { "true" } else { "false" }),
        )?;
    }
    if output.len() > DOCUMENT_BYTES {
        return Err("MCP configuration exceeds its document bound".into());
    }
    Ok(output.into_bytes())
}

pub(crate) fn server(
    bytes: &[u8],
    name: &str,
    replacement: Option<&Value>,
) -> Result<Vec<u8>, String> {
    let document = grapt_config::decode(bytes, bounds()?).map_err(|error| error.to_string())?;
    let mcp = document.value().get("mcp");
    // Keep the legacy flat map readable; new documents use the documented shape.
    let flat = mcp.is_some_and(|value| {
        value.value().get("servers").is_none() && value.value().get("timeout").is_none()
    });
    let path = if flat {
        vec!["mcp", name]
    } else {
        vec!["mcp", "servers", name]
    };
    let replacement = replacement
        .map(serde_json::to_string_pretty)
        .transpose()
        .map_err(|_| "MCP declaration cannot be encoded")?;
    let mut output =
        String::from_utf8(bytes.to_vec()).map_err(|_| "MCP configuration is not UTF-8")?;
    change(&mut output, &document, &path, replacement.as_deref())?;
    if output.len() > DOCUMENT_BYTES {
        return Err("MCP configuration exceeds its document bound".into());
    }
    let updated =
        grapt_config::decode(output.as_bytes(), bounds()?).map_err(|error| error.to_string())?;
    if let Some(mcp) = updated.value().get("mcp") {
        let servers = if flat {
            Some(mcp)
        } else {
            mcp.value().get("servers")
        };
        if servers
            .and_then(|value| value.value().as_object())
            .is_some_and(|servers| servers.len() > 256)
        {
            return Err("MCP catalog exceeds 256 servers".into());
        }
    }
    Ok(output.into_bytes())
}

pub(crate) fn change(
    text: &mut String,
    object: &Spanned<ConfigValue>,
    path: &[&str],
    replacement: Option<&str>,
) -> Result<(), String> {
    let members = object
        .value()
        .as_object()
        .ok_or("MCP configuration path must be an object")?;
    let Some((key, remaining)) = path.split_first() else {
        return Err("MCP configuration edit has no key".into());
    };
    let index = members
        .iter()
        .position(|(name, _)| name.value().as_str() == *key);
    match index {
        Some(index) if !remaining.is_empty() => {
            change(text, &members[index].1, remaining, replacement)
        }
        Some(index) => {
            let (name, value) = &members[index];
            if let Some(replacement) = replacement {
                text.replace_range(value.span().start()..value.span().end(), replacement);
            } else {
                let end = members
                    .get(index + 1)
                    .map_or(object.span().end() - 1, |(key, _)| key.span().start());
                let after = comma(text, value.span().end(), end);
                let before = if after.is_none() && index > 0 {
                    comma(text, members[index - 1].1.span().end(), name.span().start())
                } else {
                    None
                };
                // Remove a trailing/following separator first so the value's
                // original byte offsets remain valid. Comments are left intact.
                if let Some(comma) = after {
                    text.remove(comma);
                }
                text.replace_range(name.span().start()..value.span().end(), "");
                if let Some(comma) = before {
                    text.remove(comma);
                }
            }
            Ok(())
        }
        None => {
            let Some(mut value) = replacement.map(str::to_owned) else {
                return Ok(());
            };
            for key in remaining.iter().rev() {
                value = format!("{{{}:{value}}}", encode_key(key)?);
            }
            let close = object.span().end() - 1;
            text.insert_str(close, &format!("\n  {}: {value}\n", encode_key(key)?));
            if let Some((_, last)) = members.last() {
                if comma(text, last.span().end(), close).is_none() {
                    text.insert(last.span().end(), ',');
                }
            }
            Ok(())
        }
    }
}

fn encode_key(key: &str) -> Result<String, String> {
    serde_json::to_string(key).map_err(|_| "MCP configuration key cannot be encoded".into())
}

/// Only trivia separates a parsed value from the next key/closing brace.
fn comma(text: &str, mut index: usize, end: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    while index < end {
        match bytes.get(index..index + 2) {
            Some(b"//") => {
                index += 2;
                while index < end && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            Some(b"/*") => {
                index += 2;
                while index + 1 < end && &bytes[index..index + 2] != b"*/" {
                    index += 1;
                }
                index += 2;
            }
            _ if bytes[index] == b',' => return Some(index),
            _ => index += 1,
        }
    }
    None
}
