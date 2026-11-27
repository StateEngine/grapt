//! The legacy deletion carries a complete SessionInfo, unlike v2's sessionID.

use grapt_types::{ProjectId, SessionId, WorkspaceId};
use serde_json::Value;

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(crate) fn decode_v1(data: &Value) -> Result<SessionId, String> {
    let session: SessionId = string(data, "sessionID")?
        .parse()
        .map_err(|error| format!("session.deleted.1 sessionID: {error}"))?;
    let info = data
        .get("info")
        .filter(|info| info.is_object())
        .ok_or("session.deleted.1 requires full info object")?;
    if string(info, "id")? != session.as_str() {
        return Err("session.deleted.1 info.id differs from sessionID".into());
    }
    for field in ["slug", "title", "version"] {
        string(info, field)?;
    }
    ProjectId::new(string(info, "projectID")?)
        .ok_or("session.deleted.1 info.projectID is empty")?;
    let workspace = optional_string(info, "workspaceID")?
        .map(|raw| WorkspaceId::new(raw).ok_or("session.deleted.1 workspaceID is empty"))
        .transpose()?;
    crate::list::location_of(string(info, "directory")?, workspace.as_ref())
        .map_err(|error| format!("session.deleted.1 info location: {error}"))?;
    if let Some(parent) = optional_string(info, "parentID")? {
        parent
            .parse::<SessionId>()
            .map_err(|error| format!("session.deleted.1 info.parentID: {error}"))?;
    }
    for field in ["path", "agent"] {
        optional_string(info, field)?;
    }
    let time = info
        .get("time")
        .filter(|time| time.is_object())
        .ok_or("session.deleted.1 info.time is missing or not an object")?;
    for field in ["created", "updated"] {
        nonnegative(time, field)?;
    }
    if time.get("compacting").is_some() {
        nonnegative(time, "compacting")?;
    }
    for (object, field) in [(info, "cost"), (time, "archived")] {
        if object.get(field).is_some_and(|value| !value.is_number()) {
            return Err(format!("session.deleted.1 {field} is not a number"));
        }
    }
    // Optional legacy fields retain their own shape; absent never means null.
    for field in ["summary", "tokens", "share", "model", "metadata", "revert"] {
        if info.get(field).is_some_and(|value| !value.is_object()) {
            return Err(format!("session.deleted.1 info.{field} is not an object"));
        }
    }
    if info
        .get("permission")
        .is_some_and(|value| !value.is_array())
    {
        return Err("session.deleted.1 info.permission is not an array".into());
    }
    if let Some(summary) = info.get("summary") {
        for field in ["additions", "deletions", "files"] {
            number(summary, field)?;
        }
        if let Some(diffs) = summary.get("diffs") {
            for diff in diffs
                .as_array()
                .ok_or("session.deleted.1 summary.diffs is not an array")?
            {
                number(diff, "additions")?;
                number(diff, "deletions")?;
                optional_string(diff, "file")?;
                optional_string(diff, "patch")?;
                if let Some(status) = optional_string(diff, "status")? {
                    if !matches!(status, "added" | "deleted" | "modified") {
                        return Err("session.deleted.1 summary diff status is invalid".into());
                    }
                }
            }
        }
    }
    if let Some(tokens) = info.get("tokens") {
        for field in ["input", "output", "reasoning"] {
            number(tokens, field)?;
        }
        let cache = tokens
            .get("cache")
            .ok_or("session.deleted.1 tokens.cache is missing")?;
        number(cache, "read")?;
        number(cache, "write")?;
    }
    if let Some(share) = info.get("share") {
        string(share, "url")?;
    }
    if let Some(model) = info.get("model") {
        string(model, "id")?;
        string(model, "providerID")?;
        optional_string(model, "variant")?;
    }
    if let Some(revert) = info.get("revert") {
        if !string(revert, "messageID")?.starts_with("msg") {
            return Err("session.deleted.1 revert.messageID is invalid".into());
        }
        if optional_string(revert, "partID")?.is_some_and(|id| !id.starts_with("prt")) {
            return Err("session.deleted.1 revert.partID is invalid".into());
        }
        for field in ["snapshot", "diff"] {
            optional_string(revert, field)?;
        }
    }
    if let Some(rules) = info.get("permission").and_then(Value::as_array) {
        for rule in rules {
            string(rule, "permission")?;
            string(rule, "pattern")?;
            if !matches!(string(rule, "action")?, "allow" | "deny" | "ask") {
                return Err("session.deleted.1 permission action is invalid".into());
            }
        }
    }
    Ok(session)
}

fn number(data: &Value, field: &str) -> Result<(), String> {
    if data.get(field).is_some_and(Value::is_number) {
        Ok(())
    } else {
        Err(format!(
            "session.deleted.1 {field} is missing or not a number"
        ))
    }
}

fn string<'a>(data: &'a Value, field: &str) -> Result<&'a str, String> {
    data.get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("session.deleted.1 {field} is missing or not a string"))
}

fn optional_string<'a>(data: &'a Value, field: &str) -> Result<Option<&'a str>, String> {
    data.get(field).map(|_| string(data, field)).transpose()
}

fn nonnegative(data: &Value, field: &str) -> Result<(), String> {
    if data
        .get(field)
        .and_then(Value::as_i64)
        .is_some_and(|value| value >= 0)
    {
        Ok(())
    } else {
        Err(format!(
            "session.deleted.1 time.{field} is missing or not a nonnegative integer"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_full_info_accepts_optional_fields_and_zero_stamps() -> Result<(), String> {
        let data = legacy();
        assert_eq!(decode_v1(&data)?.as_str(), "ses_legacy");
        let mut windows = data;
        windows["info"]["directory"] = json!("E:\\legacy\\work");
        assert_eq!(decode_v1(&windows)?.as_str(), "ses_legacy");
        Ok(())
    }

    #[test]
    fn incompatible_and_corrupt_legacy_fields_refuse() -> Result<(), String> {
        assert!(
            decode_v1(&json!({"sessionID":"ses_legacy"})).is_err(),
            "v2 payload decoded as v1"
        );
        for (path, value) in [
            ("/info/id", json!("ses_other")),
            ("/info/slug", Value::Null),
            ("/info/time/updated", json!(-1)),
            ("/info/time/created", json!(0.5)),
            ("/info/model", Value::Null),
            ("/info/model/id", json!(42)),
            ("/info/summary/files", json!("wrong")),
            ("/info/summary/diffs/0/status", json!("unknown")),
            ("/info/tokens/cache/read", Value::Null),
            ("/info/share/url", json!(false)),
            ("/info/revert/messageID", json!(null)),
            ("/info/revert/messageID", json!("invalid")),
            ("/info/revert/partID", json!("invalid")),
            ("/info/permission/0/action", json!("unknown")),
        ] {
            let mut data = legacy();
            *data
                .pointer_mut(path)
                .ok_or_else(|| format!("missing fixture field {path}"))? = value;
            assert!(decode_v1(&data).is_err(), "corrupt {path} decoded");
        }
        Ok(())
    }

    fn legacy() -> Value {
        json!({"sessionID":"ses_legacy","info":{
            "id":"ses_legacy","slug":"old","projectID":"global","directory":"/work",
            "title":"legacy","version":"1","time":{"created":0,"updated":0},
            "model":{"id":"model","providerID":"provider","variant":"high"},
            "summary":{"additions":0,"deletions":0,"files":1,
                "diffs":[{"additions":0,"deletions":0,"status":"modified"}]},
            "tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},
            "share":{"url":"https://example.invalid/legacy"},
            "metadata":{"opaque":null},"cost":0,
            "revert":{"messageID":"msg_old","partID":"prt_old"},
            "permission":[{"permission":"read","pattern":"*","action":"ask"}]}})
    }
}
