//! Live, payload-free inbox attention. This never reports Session quiescence.
use super::*;

/// One bounded local page. An incomplete page is a lower bound, even at zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct MailPendingCount(u8);
impl MailPendingCount {
    pub fn new(value: u8) -> Option<Self> {
        (value <= 64).then_some(Self(value))
    }
    pub fn is_zero(&self) -> bool {
        self.0 == 0
    }
    pub fn get(self) -> u8 {
        self.0
    }
}
impl TryFrom<u8> for MailPendingCount {
    type Error = &'static str;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value).ok_or("mail attention count exceeds its page bound")
    }
}
impl From<MailPendingCount> for u8 {
    fn from(value: MailPendingCount) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", from = "MailAttentionWire")]
pub enum MailAttentionInfo {
    Disabled,
    Ready {
        pending: MailPendingCount,
        incomplete: bool,
        #[serde(default, skip_serializing_if = "MailPendingCount::is_zero")]
        decode_failures: MailPendingCount,
    },
    Unavailable,
    Retrying,
    #[serde(rename = "rebind_required")]
    RebindRequired,
    /// A refusal that will not clear by reconnecting; someone needs to act.
    Stopped,
}

// Struct variants also reject unknown fields for the empty states. Serde's
// internally tagged unit variants otherwise ignore those fields.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
enum MailAttentionWire {
    Disabled {},
    Ready {
        pending: MailPendingCount,
        incomplete: bool,
        #[serde(default)]
        decode_failures: MailPendingCount,
    },
    Unavailable {},
    Retrying {},
    #[serde(rename = "rebind_required")]
    RebindRequired {},
    Stopped {},
}
impl From<MailAttentionWire> for MailAttentionInfo {
    fn from(value: MailAttentionWire) -> Self {
        match value {
            MailAttentionWire::Disabled {} => Self::Disabled,
            MailAttentionWire::Ready {
                pending,
                incomplete,
                decode_failures,
            } => Self::Ready {
                pending,
                incomplete,
                decode_failures,
            },
            MailAttentionWire::Unavailable {} => Self::Unavailable,
            MailAttentionWire::Retrying {} => Self::Retrying,
            MailAttentionWire::RebindRequired {} => Self::RebindRequired,
            MailAttentionWire::Stopped {} => Self::Stopped,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MailAttentionData {
    #[serde(rename = "sessionID")]
    session_id: WireId<grapt_types::SessionId>,
    status: MailAttentionInfo,
    #[serde(rename = "staleSince", skip_serializing_if = "Option::is_none")]
    stale_since: Option<FiniteF64>,
    #[serde(skip)]
    location: grapt_types::LocationRef,
}
impl MailAttentionData {
    pub fn with_stale_since(mut self, since: Option<FiniteF64>) -> Self {
        self.stale_since = since;
        self
    }
    pub fn location(&self) -> &grapt_types::LocationRef {
        &self.location
    }
    pub fn new(
        session_id: WireId<grapt_types::SessionId>,
        status: MailAttentionInfo,
        location: grapt_types::LocationRef,
    ) -> Self {
        Self {
            session_id,
            status,
            stale_since: None,
            location,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn attention_is_bounded_closed_and_preserves_unknown_vs_empty(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let empty: MailAttentionInfo =
            serde_json::from_str(r#"{"type":"ready","pending":0,"incomplete":false}"#)?;
        let partial: MailAttentionInfo =
            serde_json::from_str(r#"{"type":"ready","pending":0,"incomplete":true}"#)?;
        let unavailable: MailAttentionInfo = serde_json::from_str(r#"{"type":"unavailable"}"#)?;
        assert_ne!(empty, partial);
        assert_ne!(empty, unavailable);
        let retrying: MailAttentionInfo = serde_json::from_str(r#"{"type":"retrying"}"#)?;
        assert_eq!(retrying, MailAttentionInfo::Retrying);
        assert_ne!(retrying, unavailable);
        let rebind: MailAttentionInfo = serde_json::from_str(r#"{"type":"rebind_required"}"#)?;
        assert_eq!(rebind, MailAttentionInfo::RebindRequired);
        assert_ne!(rebind, retrying);
        assert_eq!(
            serde_json::to_string(&rebind)?,
            r#"{"type":"rebind_required"}"#
        );
        assert!(serde_json::from_str::<MailAttentionInfo>(
            r#"{"type":"rebind_required","capability":"must-not-appear"}"#
        )
        .is_err());
        assert_eq!(serde_json::to_string(&retrying)?, r#"{"type":"retrying"}"#);
        for invalid in [
            r#"{"type":"ready","pending":65,"incomplete":false}"#,
            r#"{"type":"ready","pending":0}"#,
            r#"{"type":"ready","pending":0,"incomplete":false,"body":"secret"}"#,
            r#"{"type":"disabled","pending":0}"#,
            r#"{"type":"retrying","detail":"secret"}"#,
            r#"{"type":"idle"}"#,
            r#"{"type":"ready","pending":0,"pending":1,"incomplete":false}"#,
        ] {
            assert!(
                serde_json::from_str::<MailAttentionInfo>(invalid).is_err(),
                "{invalid}"
            );
        }
        assert_eq!(
            serde_json::to_string(&unavailable)?,
            r#"{"type":"unavailable"}"#
        );
        Ok(())
    }
    #[test]
    fn local_decode_count_is_optional_bounded_and_nonblocking(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let legacy = r#"{"type":"ready","pending":0,"incomplete":false}"#;
        let ready: MailAttentionInfo = serde_json::from_str(legacy)?;
        assert_eq!(serde_json::to_string(&ready)?, legacy);
        let warned: MailAttentionInfo = serde_json::from_str(
            r#"{"type":"ready","pending":2,"incomplete":false,"decode_failures":1}"#,
        )?;
        assert!(
            matches!(warned,MailAttentionInfo::Ready{pending,decode_failures,..}
            if pending.get()==2 && decode_failures.get()==1)
        );
        assert!(serde_json::from_str::<MailAttentionInfo>(
            r#"{"type":"ready","pending":0,"incomplete":false,"decode_failures":65}"#
        )
        .is_err());
        Ok(())
    }
}
