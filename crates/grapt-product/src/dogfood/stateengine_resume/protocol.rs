//! Versioned continuity identities and the authenticated context-read boundary.
use super::{bounded_string, verify, ContinuityUnavailable as E};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Principal {
    NodeFingerprint(String),
    RustSession(EngineSession),
}

/// The remote session is distinct from this harness's local Session identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct EngineSession(String);

impl Principal {
    pub(super) fn from_fingerprint(value: &str) -> Self {
        Self::NodeFingerprint(value.to_owned())
    }

    pub(super) fn matches_fingerprint(&self, expected: &str) -> bool {
        matches!(self, Self::NodeFingerprint(actual) if actual == expected)
    }

    pub(super) fn is_rust(&self) -> bool {
        matches!(self, Self::RustSession(_))
    }

    pub(super) fn read(value: &Value, project: &str, agent: &str) -> Result<Self, E> {
        match value.get("schema_version").and_then(Value::as_u64) {
            Some(2) => {
                verify(value, project, agent, None)?;
                Ok(Self::from_fingerprint(bounded_string(
                    value.get("principal").ok_or(E::Principal)?,
                    "identity_fingerprint",
                    128,
                )?))
            }
            Some(1) => {
                let principal = value.get("principal").ok_or(E::Principal)?;
                if principal.pointer("/project/id").and_then(Value::as_str) != Some(project)
                    || principal.pointer("/agent/id").and_then(Value::as_str) != Some(agent)
                    || principal.get("preview") != Some(&Value::Bool(false))
                {
                    return Err(E::Principal);
                }
                // The approved MCP launch and Session capability authenticate
                // these coordinates; display names grant no authority.
                let session = bounded_string(principal, "session_id", 512)?;
                if !session.bytes().all(|byte| byte.is_ascii_graphic()) {
                    return Err(E::Principal);
                }
                Ok(Self::RustSession(EngineSession(session.to_owned())))
            }
            _ => Err(E::Contract),
        }
    }
}

/// The Rust schema represents an optional Cursor as a local reference or a
/// nullable string. Only bounded local schema traversal is supported.
pub(super) fn accepts_since(schema: &Value) -> bool {
    fn string(root: &Value, value: &Value, depth: usize) -> bool {
        if depth > 4 {
            return false;
        }
        if value.get("type").and_then(Value::as_str) == Some("string")
            || value
                .get("type")
                .and_then(Value::as_array)
                .is_some_and(|types| types.iter().any(|kind| kind.as_str() == Some("string")))
        {
            return true;
        }
        if let Some(reference) = value.get("$ref").and_then(Value::as_str) {
            return reference
                .strip_prefix("#/$defs/")
                .filter(|name| !name.contains('/'))
                .and_then(|name| root.get("$defs")?.get(name))
                .is_some_and(|definition| string(root, definition, depth + 1));
        }
        value
            .get("anyOf")
            .and_then(Value::as_array)
            .filter(|variants| variants.len() <= 4)
            .is_some_and(|variants| {
                variants
                    .iter()
                    .any(|variant| string(root, variant, depth + 1))
            })
    }
    schema
        .pointer("/properties/since")
        .is_some_and(|value| string(schema, value, 0))
}

/// Pending reads belong to the bound agent. Preserve their commands as context;
/// never acknowledge previews or execute recovery commands in the harness.
pub(super) fn admit_rust(reply: &Value) -> Result<(), E> {
    let reads = reply
        .get("required_reads")
        .and_then(Value::as_array)
        .ok_or(E::Incomplete)?;
    let omissions = reply
        .get("omissions")
        .and_then(Value::as_array)
        .ok_or(E::Incomplete)?;
    for key in ["working_state", "transcript_events", "last_compaction"] {
        if !matches!(
            reply
                .get(key)
                .and_then(|value| value.get("state"))
                .and_then(Value::as_str),
            Some("present" | "absent")
        ) {
            return Err(E::Incomplete);
        }
    }
    if matches!(
        reply
            .pointer("/unread/window/omitted/reason")
            .and_then(Value::as_str),
        Some("unreadable")
    ) {
        return Err(E::Incomplete);
    }
    for read in reads {
        if read.get("kind").and_then(Value::as_str) != Some("message") {
            return Err(E::Contract);
        }
        let id = bounded_string(read, "id", 512)?;
        if reply
            .pointer("/unread/items")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("id").and_then(Value::as_str) == Some(id)
                        && item.get("unreadable").is_some_and(|value| !value.is_null())
                })
            })
        {
            return Err(E::Incomplete);
        }
        bounded_string(read, "why", super::MAX_REPLY)?;
    }
    let shared_recovery = omissions.iter().any(|omission| {
        omission.get("section").and_then(Value::as_str) == Some("recovery_commands")
            && omission.get("tool").and_then(Value::as_str) == Some("stateengine_resume")
            && omission
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|text| !text.is_empty())
            && omission
                .get("argv")
                .and_then(Value::as_array)
                .is_some_and(|argv| !argv.is_empty())
    });
    for omission in omissions {
        bounded_string(omission, "section", 128)?;
        bounded_string(omission, "reason", super::MAX_REPLY)?;
        if matches!(
            omission.get("reason").and_then(Value::as_str),
            Some("source unreadable")
        ) {
            return Err(E::Incomplete);
        }
        bounded_string(omission, "tool", 128)?;
        let command = omission
            .get("command")
            .and_then(Value::as_str)
            .ok_or(E::Contract)?;
        let argv = omission
            .get("argv")
            .and_then(Value::as_array)
            .ok_or(E::Contract)?;
        if argv.iter().any(|arg| !arg.is_string())
            || command.is_empty() != argv.is_empty()
            || (command.is_empty() && !shared_recovery)
        {
            return Err(E::Contract);
        }
    }
    let complete = reply
        .get("complete")
        .and_then(Value::as_bool)
        .ok_or(E::Incomplete)?;
    // An optional code attachment can reduce completeness independently of
    // the required continuity sources. Preserve that status for the agent.
    let optional_incomplete = reply.pointer("/code_status/complete") == Some(&Value::Bool(false));
    match reply.get("resume_status").and_then(Value::as_str) {
        Some("ready")
            if reads.is_empty()
                && omissions.is_empty()
                && reply.pointer("/unread/window/complete") == Some(&Value::Bool(true))
                && (complete || optional_incomplete) =>
        {
            Ok(())
        }
        Some("needs_read") if !reads.is_empty() || !omissions.is_empty() => Ok(()),
        _ => Err(E::Incomplete),
    }
}

pub(super) const READ_GUIDANCE: &str = concat!(
    "Before task work, finish every required read and omission below through the bound ",
    "authenticated StateEngine tools and read their full results. Context previews do not ",
    "acknowledge messages. This packet retains its reported status and completeness.\n"
);
