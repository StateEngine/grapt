// SPDX-License-Identifier: MIT OR Apache-2.0
//! Turn state for an external seat: `busy` while its runner works a turn,
//! `idle` after. While a seat is busy the host holds its new mail.
use grapt_stateengine_api::{AgentId, ProjectId, SessionId};
use serde::{Deserialize, Serialize};

/// The host capability that offers [`Activity`] reports.
pub const ACTIVITY_CAPABILITY: &str = "session.activity";

/// A seat's turn state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    Busy,
    Idle,
}

/// The external-seat requests this client sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExternalRequest {
    /// An owner connection names the seat's agent; `project` narrows it.
    SessionActivity {
        #[serde(default, with = "crate::ids::option")]
        project: Option<ProjectId>,
        #[serde(default, with = "crate::ids::option")]
        agent: Option<AgentId>,
        state: Activity,
    },
}

/// The host's record of the reported state. Fields beside these are not
/// part of the receipt and are not kept.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ActivityReceipt {
    #[serde(with = "crate::ids")]
    pub project_id: ProjectId,
    #[serde(with = "crate::ids")]
    pub agent_id: AgentId,
    #[serde(with = "crate::ids")]
    pub session_id: SessionId,
    pub state: Activity,
    /// Whether this report ended a busy turn.
    pub turn_ended: bool,
}

pub(crate) const RECEIPT_SCHEMA: &str = "se.session-activity/v1";
