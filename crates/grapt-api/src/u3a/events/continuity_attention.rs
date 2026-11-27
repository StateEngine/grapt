//! Live StateEngine continuity health for a Session. Every frame carries the
//! whole current state, never a change, so a missed frame is healed by the
//! next one or by hydration. Its shape is `SessionState.continuity`.
use super::*;

/// Why continuity is degraded: the outages a turn proceeds without. A closed
/// vocabulary, never remote text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuityDegradedReason {
    Transport,
    Capability,
    ServerUnavailable,
    Incomplete,
    Rebound,
    TimedOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "lowercase",
    from = "ContinuityAttentionWire"
)]
pub enum ContinuityAttentionInfo {
    Ok,
    Degraded { reason: ContinuityDegradedReason },
}

// Struct variants also reject unknown fields for the empty state.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
enum ContinuityAttentionWire {
    Ok {},
    Degraded { reason: ContinuityDegradedReason },
}
impl From<ContinuityAttentionWire> for ContinuityAttentionInfo {
    fn from(value: ContinuityAttentionWire) -> Self {
        match value {
            ContinuityAttentionWire::Ok {} => Self::Ok,
            ContinuityAttentionWire::Degraded { reason } => Self::Degraded { reason },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContinuityAttentionData {
    #[serde(rename = "sessionID")]
    session_id: WireId<grapt_types::SessionId>,
    status: ContinuityAttentionInfo,
    #[serde(skip)]
    location: grapt_types::LocationRef,
}
impl ContinuityAttentionData {
    pub fn new(
        session_id: WireId<grapt_types::SessionId>,
        status: ContinuityAttentionInfo,
        location: grapt_types::LocationRef,
    ) -> Self {
        Self {
            session_id,
            status,
            location,
        }
    }
    pub fn location(&self) -> &grapt_types::LocationRef {
        &self.location
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuity_attention_is_a_closed_level() -> Result<(), Box<dyn std::error::Error>> {
        let degraded = ContinuityAttentionInfo::Degraded {
            reason: ContinuityDegradedReason::ServerUnavailable,
        };
        let wire = serde_json::to_value(degraded)?;
        assert_eq!(
            wire,
            serde_json::json!({"type": "degraded", "reason": "server_unavailable"})
        );
        assert_eq!(
            serde_json::from_value::<ContinuityAttentionInfo>(wire)?,
            degraded
        );
        assert_eq!(
            serde_json::from_value::<ContinuityAttentionInfo>(serde_json::json!({"type": "ok"}))?,
            ContinuityAttentionInfo::Ok
        );
        for refused in [
            serde_json::json!({"type": "ok", "reason": "transport"}),
            serde_json::json!({"type": "degraded"}),
            serde_json::json!({"type": "degraded", "reason": "remote text"}),
            serde_json::json!({"type": "unknown"}),
        ] {
            assert!(
                serde_json::from_value::<ContinuityAttentionInfo>(refused.clone()).is_err(),
                "{refused}"
            );
        }
        Ok(())
    }
}
