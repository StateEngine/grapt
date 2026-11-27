use super::super::*;

pub(in crate::dogfood::continuation) fn permission_calls(
    rows: Vec<(u64, String)>,
) -> Result<Vec<PermissionCall>, String> {
    let mut calls: Vec<PermissionCall> = Vec::new();
    for (_, data) in rows {
        let value = serde_json::from_str(&data)
            .map_err(|error| format!("permission ask is not JSON: {error}"))?;
        let event = PermissionEvent::decode("permission.asked", 2, &value)
            .map_err(|error| format!("permission ask is corrupt: {error}"))?
            .ok_or_else(|| "permission.asked.2 was not decoded".to_owned())?;
        let PermissionEvent::Asked {
            action,
            coordinates,
            ..
        } = event
        else {
            continue;
        };
        let Some(coordinates) = coordinates else {
            continue;
        };
        let Some(source) = coordinates.source else {
            continue;
        };
        let Some((message, call)) = tool_source(&source)? else {
            continue;
        };
        if calls.iter().any(|existing| existing.call == call) {
            return Err(format!("tool {call} has duplicate permission asks"));
        }
        let tool = tool_name(&action, &call)?;
        calls.push(PermissionCall {
            input: message,
            call,
            tool,
        });
    }
    Ok(calls)
}

pub(in crate::dogfood::continuation) fn tool_identities(
    session: &SessionId,
    rows: Vec<(u64, String)>,
    version: u32,
) -> Result<Vec<ToolIdentity>, String> {
    let mut identities: Vec<ToolIdentity> = Vec::new();
    for (seq, raw) in rows {
        let value = serde_json::from_str(&raw)
            .map_err(|error| format!("tool identity is not JSON: {error}"))?;
        let data = grapt_event::session_tool_identity::decode(version, &value)
            .map_err(|error| error.to_string())?;
        if data.session != *session {
            return Err("tool identity belongs to another Session".into());
        }
        if identities.iter().any(|held| held.call == data.call) {
            return Err("duplicate recorded tool identity".into());
        }
        identities.push(ToolIdentity {
            seq,
            message: data.message,
            call: data.call,
            tool: ToolName::new(data.tool.as_str()).ok_or("recorded tool name is invalid")?,
            execution: data.execution,
            explicit_execution: version == 2,
        });
    }
    Ok(identities)
}

pub(in crate::dogfood::continuation) fn merge_identities(
    mut local: Vec<ToolIdentity>,
    hosted: Vec<ToolIdentity>,
    maximum: usize,
) -> Result<Vec<ToolIdentity>, String> {
    if local.len().saturating_add(hosted.len()) > maximum {
        return Err("tool identities exceed the history row bound".into());
    }
    for identity in hosted {
        if local.iter().any(|held| held.call == identity.call) {
            return Err("duplicate recorded tool identity across versions".into());
        }
        local.push(identity);
    }
    Ok(local)
}
