// Copyright 2025 OpenAI. Licensed under Apache-2.0.
//! Recovery metadata travels in the ordinary captured tool result, not a job store.
//! Process-handle retention.
//! Grapt retains observations for recovery, never process liveness or authority.
use grapt_types::{OperationId, ToolCallId};
use serde::{Deserialize, Serialize};

const MAX_HOST: usize = 128;
const MAX_ID: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::dogfood) struct Origin {
    started_ms: i64,
    host: String,
}

impl Origin {
    pub(in crate::dogfood::tools) fn local(started_ms: i64) -> Self {
        let name = std::env::var(if cfg!(windows) {
            "COMPUTERNAME"
        } else {
            "HOSTNAME"
        })
        .ok();
        Self::at(started_ms, name.as_deref())
    }

    fn at(started_ms: i64, hostname: Option<&str>) -> Self {
        let name = hostname
            .filter(|name| {
                !name.is_empty() && name.len() <= MAX_HOST && !name.chars().any(char::is_control)
            })
            .unwrap_or("hostname unavailable");
        Self {
            started_ms,
            host: format!("local/{}/{name}", std::env::consts::OS),
        }
    }
}

#[derive(Debug, Clone)]
pub(in crate::dogfood) struct Record {
    pub operation: OperationId,
    pub call: ToolCallId,
    origin: Origin,
    status: String,
    done: bool,
}

impl Record {
    /// Only the native terminal owner may interpret this marker. Its caller
    /// must verify native tool provenance before passing untrusted output.
    pub(in crate::dogfood) fn decode(
        call: &ToolCallId,
        output: &str,
    ) -> Result<Option<Self>, String> {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
            return Ok(None);
        };
        let Some(origin) = value.get("recovery") else {
            return Ok(None);
        };
        let origin: Origin = serde_json::from_value(origin.clone())
            .map_err(|_| "terminal recovery origin is corrupt")?;
        if origin.host.is_empty()
            || origin.host.len() > MAX_HOST + 64
            || origin.host.chars().any(char::is_control)
        {
            return Err("terminal recovery host exceeds its bound or contains controls".into());
        }
        let id = value
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .ok_or("terminal recovery command identity is absent")?;
        if id.len() > MAX_ID || id.chars().any(char::is_control) {
            return Err("terminal recovery command identity exceeds its bound".into());
        }
        let operation =
            OperationId::new(id).ok_or("terminal recovery command identity is invalid")?;
        let status = value
            .get("status")
            .and_then(serde_json::Value::as_str)
            .filter(|status| {
                matches!(
                    *status,
                    "running" | "exited" | "timed_out" | "cancelled" | "failed"
                )
            })
            .ok_or("terminal recovery observation is invalid")?;
        let done = value
            .get("done")
            .and_then(serde_json::Value::as_bool)
            .ok_or("terminal recovery settlement is absent")?;
        Ok(Some(Self {
            operation,
            call: call.clone(),
            origin,
            status: status.into(),
            done,
        }))
    }

    pub(in crate::dogfood) fn pending(&self) -> bool {
        !self.done
    }

    pub(in crate::dogfood) fn notice(&self) -> String {
        let encoded = serde_json::json!({"call": self.call.as_str()}).to_string();
        let result = if encoded.len() <= MAX_ID {
            format!("history({encoded})")
        } else {
            "history({}) to list this session's retained results".into()
        };
        format!("Command {:?} previously started on {:?} at Unix ms {}; status unknown; last observed {:?}; see {result}. Never replay it to recover output.", self.operation.as_str(), self.origin.host, self.origin.started_ms, self.status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostname_is_bounded_and_never_takes_control_of_the_notice() {
        for name in [None, Some("bad\nhost"), Some(""), Some("\u{1b}[31m")] {
            let origin = Origin::at(123, name);
            assert!(origin.host.ends_with("hostname unavailable"));
            assert!(!origin.host.chars().any(char::is_control));
        }
        assert!(Origin::at(123, Some(&"h".repeat(MAX_HOST + 1)))
            .host
            .ends_with("hostname unavailable"));
        assert!(Origin::at(123, Some("fixture-host"))
            .host
            .ends_with("fixture-host"));
    }

    #[test]
    fn invalid_marked_records_refuse_instead_of_becoming_no_previous_work() -> Result<(), String> {
        let call = ToolCallId::new("recover-fixture").ok_or("call")?;
        assert!(Record::decode(&call, "ordinary historical output")?.is_none());
        for value in [
            serde_json::json!({"recovery": {}}),
            serde_json::json!({"recovery":{"host":"bad\nhost", "started_ms": 7}}),
            serde_json::json!({"recovery":{"host":"host", "started_ms":7}, "session_id":"op_fixture", "done":false, "status":"alive"}),
        ] {
            assert!(Record::decode(&call, &value.to_string()).is_err());
        }
        Ok(())
    }
}
