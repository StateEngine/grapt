//! Permission asks, replies and the reply vocabulary.
//!
//! Split from `wire.rs` by SUBJECT when that file passed this workspace's
//! module ceiling. Every encode and decode still goes through the real codecs
//! under the real limits; nothing here was rewritten on the way out.

use super::*;
const MSYS_SESSION_RESOURCE: &str = "windows-session-command-trees";

#[derive(Debug, Clone, PartialEq)]
pub struct PermissionRow {
    pub id: PermissionWireId,
    pub session_id: SessionWireId,
    pub title: String,
    /// "tool {action}" plus what the ask will do, from the row's
    /// consequences. The title names the tool only by position; this line
    /// says it by name and states the effect the preset classified.
    pub subtitle: String,
    pub live: bool,
    pub allow_always: bool,
    /// The tool call waiting on this ask, when a tool asked.
    pub call: Option<ToolCallWireId>,
    /// Display context from the committed ask; approval still uses its ID.
    pub preview: Option<grapt_api::u3a::PermissionPreviewDto>,
}

/// The complete closed reply vocabulary carried by the permission wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionReplyChoice {
    Once,
    Session,
    Always,
    Reject,
}

impl PermissionReplyChoice {
    fn suffix(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Session => "session",
            Self::Always => "always",
            Self::Reject => "reject",
        }
    }
}

use grapt_api::u3a::OpenObject;

pub fn decode_permissions(bytes: &[u8]) -> Result<Vec<PermissionRow>, WireError> {
    use grapt_api::u3a::PermissionGlobalListResponseDto;
    let dto: PermissionGlobalListResponseDto = decode_page(bytes, limits()?)?;
    Ok(permission_rows(dto.data().get().as_slice()))
}

/// The one projection of typed pending permissions, whichever response
/// carried them: the global list or a session snapshot.
pub fn permission_rows(rows: &[grapt_api::u3a::PermissionRequestStateDto]) -> Vec<PermissionRow> {
    use grapt_api::u3a::PermissionRequestStateDto;
    rows.iter()
        .map(|row| {
            macro_rules! project {
                ($request:expr, $live:expr) => {{
                    let resources = $request
                        .resources()
                        .get()
                        .as_slice()
                        .iter()
                        .map(|resource| match resource.as_str() {
                            MSYS_SESSION_RESOURCE => "Windows shell command trees",
                            value => value,
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    let action = $request.action().get().as_str();
                    let session_msys = action == "sandbox.msys_unconfined"
                        && $request
                            .resources()
                            .get()
                            .as_slice()
                            .iter()
                            .any(|resource| resource.as_str() == MSYS_SESSION_RESOURCE);
                    let title = if action == "sandbox.msys_unconfined" {
                        format!("Runs without OS containment (including all children): {resources}")
                    } else if resources.is_empty() {
                        action.to_owned()
                    } else {
                        format!("{action}: {resources}")
                    };
                    let subtitle =
                        permission_subtitle(action, $request.metadata().as_present(), session_msys);
                    PermissionRow {
                        id: $request.id().get().clone(),
                        session_id: $request.session_id().get().clone(),
                        title,
                        subtitle,
                        live: $live,
                        allow_always: !session_msys,
                        call: $request
                            .source()
                            .as_present()
                            .map(|source| source.call_id().get().clone()),
                        preview: $request.preview().as_present().cloned(),
                    }
                }};
            }
            match row {
                PermissionRequestStateDto::Pending(request) => project!(request, true),
                PermissionRequestStateDto::Suspended(request) => project!(request, false),
            }
        })
        .collect()
}

/// "tool {action}" plus the effect in plain words, one clause per
/// classified consequence. The row's `action` is the tool name, but only
/// the metadata's `consequences` say what will happen — "patch" alone does
/// not say it will delete. A row without consequence metadata still names
/// its tool; a consequence that is not a {action, resource} object is
/// skipped rather than misrendered.
fn permission_subtitle(action: &str, metadata: Option<&OpenObject>, session_msys: bool) -> String {
    if session_msys {
        return "Once: this command and its children run without OS containment. Session: all Windows shell commands and their children run without OS filesystem or network restrictions until this Session ends.".into();
    }
    if action == "sandbox.write" {
        return "Grant a requested write target and retry inside the sandbox. Earlier effects may repeat. Session and Always remember only the target; each command keeps its ordinary permission checks.".into();
    }
    let parts = metadata
        .and_then(|metadata| metadata.get("consequences"))
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|consequence| {
            let verb = consequence.entry("action")?.as_str()?;
            let resource = consequence.entry("resource")?.as_str()?;
            Some(format!("{} {resource}", consequence_verb(verb)))
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        format!("tool {action}")
    } else {
        format!("tool {action} · {}", parts.join(", "))
    }
}

fn consequence_verb(action: &str) -> &str {
    match action {
        "sandbox.msys_unconfined" => "runs without OS containment (including all children):",
        "fs.create" => "creates",
        "fs.modify" | "fs.overwrite" => "modifies",
        "fs.delete" => "deletes",
        "fs.move" => "moves",
        other => other,
    }
}

pub fn encode_permission_reply(
    request_id: &PermissionWireId,
    choice: PermissionReplyChoice,
) -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::{PermissionReplyDto, PermissionReplyRequestDto, WireId};
    let reply = match choice {
        PermissionReplyChoice::Once => PermissionReplyDto::Once,
        PermissionReplyChoice::Session => PermissionReplyDto::Session,
        PermissionReplyChoice::Always => PermissionReplyDto::Always,
        PermissionReplyChoice::Reject => PermissionReplyDto::Reject,
    };
    let suffix = choice.suffix();
    let request = WireId::<grapt_types::RequestId>::parse(format!(
        "req_{}_{}",
        request_id.as_inner().as_str(),
        suffix
    ))
    .ok_or_else(|| WireError::Invalid {
        what: "permission reply request identity",
        value: request_id.as_inner().as_str().to_owned(),
    })?;
    let nonce = WireId::<grapt_types::DecisionNonce>::parse(format!(
        "nonce_{}_{}",
        request_id.as_inner().as_str(),
        suffix
    ))
    .ok_or_else(|| WireError::Invalid {
        what: "permission reply nonce",
        value: request_id.as_inner().as_str().to_owned(),
    })?;
    let dto = PermissionReplyRequestDto::new(
        Required::new(reply),
        Optional::present(request),
        Optional::present(nonce),
        Optional::absent(),
    );
    Ok(encode_page(&dto, limits()?)?)
}

pub fn encode_permission_create(action: &str, resources: &[&str]) -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::PermissionCreateRequestDto;
    let bytes = serde_json::json!({"action": action, "resources": resources})
        .to_string()
        .into_bytes();
    let _validated: PermissionCreateRequestDto = decode_request(&bytes, limits()?)?;
    Ok(bytes)
}

pub fn decode_permission_create(bytes: &[u8]) -> Result<String, WireError> {
    use grapt_api::u3a::PermissionCreateRequestDto;
    let dto: PermissionCreateRequestDto = decode_request(bytes, limits()?)?;
    Ok(dto.action().get().as_str().to_owned())
}

pub fn decode_permission_created(bytes: &[u8]) -> Result<PermissionWireId, WireError> {
    use grapt_api::u3a::PermissionCreateResponseDto;
    let dto: PermissionCreateResponseDto = decode_page(bytes, limits()?)?;
    Ok(dto.data().get().id().get().clone())
}

pub fn decode_permission_reply(bytes: &[u8]) -> Result<PermissionReplyChoice, WireError> {
    use grapt_api::u3a::{PermissionReplyDto, PermissionReplyRequestDto};
    let dto: PermissionReplyRequestDto = decode_request(bytes, limits()?)?;
    match dto.reply().get() {
        PermissionReplyDto::Once => Ok(PermissionReplyChoice::Once),
        PermissionReplyDto::Session => Ok(PermissionReplyChoice::Session),
        PermissionReplyDto::Always => Ok(PermissionReplyChoice::Always),
        PermissionReplyDto::Reject => Ok(PermissionReplyChoice::Reject),
    }
}

#[cfg(test)]
mod preview_tests {
    use super::*;
    #[test]
    fn polling_retains_typed_preview_and_accepts_legacy_rows() -> Result<(), WireError> {
        let mut row = serde_json::json!({"id":"per_preview", "sessionID":"ses_preview",
            "action":"shell", "resources":["pwd"], "status":"pending"});
        for preview in [
            serde_json::Value::Null,
            serde_json::json!({"type":"shell",
            "command":"pwd", "cwd":"/repo", "shell":"/bin/sh", "truncated":false}),
        ] {
            if !preview.is_null() {
                row["preview"] = preview.clone();
            }
            let page = serde_json::json!({"location":{"directory":"/repo", "project":{
                "id":"prj_preview", "directory":"/repo"}}, "data":[row.clone()], "hasMore":false});
            let rows = decode_permissions(page.to_string().as_bytes())?;
            assert_eq!(rows.len(), 1);
            match rows[0].preview.as_ref() {
                None => assert!(preview.is_null()),
                Some(grapt_api::u3a::PermissionPreviewDto::Shell(shell)) => {
                    assert_eq!(shell.command().get().as_str(), "pwd");
                    assert_eq!(shell.cwd().get().as_str(), "/repo");
                    assert_eq!(shell.shell().get().as_str(), "/bin/sh");
                }
                _ => panic!("shell preview changed variant"),
            }
        }
        Ok(())
    }
}
