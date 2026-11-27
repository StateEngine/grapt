//! The positions subscriber class, served the way the product serves it.
//!
//! A positioned double stamps every frame with `id: <epoch>-<seq>`, places
//! each live delta at its block's byte offset (`from`), reports each session's
//! execution state as a revisioned `state` on its status frames, and answers
//! the coherent session snapshot. Every double serves this class unless a
//! witness asks for [`ContractDouble::legacy`], an older service that keeps
//! its frames byte for byte and has no snapshot.

use super::*;

/// The positions class of the double's one subscription.
#[derive(Debug)]
pub(super) struct Positions {
    enabled: bool,
    /// The serving process's epoch: a restart starts a new one.
    epoch: u64,
    /// The last position stamped in this epoch, shared by every stream of it.
    seq: u64,
    /// Bytes already sent per live block, keyed by its message and ordinal.
    offsets: BTreeMap<(String, u64), u64>,
}

/// One session's execution state, at the committed position that reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExecutionState {
    kind: &'static str,
    revision: u64,
    execution: Option<String>,
}

impl ExecutionState {
    pub(super) fn idle(revision: u64) -> Self {
        Self {
            kind: "idle",
            revision,
            execution: None,
        }
    }

    fn dto(&self) -> serde_json::Value {
        let mut state = serde_json::json!({"type": self.kind, "revision": self.revision});
        if let Some(execution) = &self.execution {
            state["executionID"] = serde_json::json!(execution);
        }
        state
    }
}

impl Positions {
    /// The class a current service serves.
    pub(super) fn served() -> Self {
        Self {
            enabled: true,
            epoch: 0,
            seq: 0,
            offsets: BTreeMap::new(),
        }
    }

    /// Wrap one frame for the subscription: positioned when this class is on.
    pub(super) fn stamp(&mut self, frame: String) -> FramePoll {
        if !self.enabled {
            return FramePoll::Frame(frame);
        }
        self.seq = self.seq.saturating_add(1);
        FramePoll::Positioned {
            id: format!("double{}-{}", self.epoch, self.seq),
            data: frame,
        }
    }

    /// The process restarted: positions start again in a new epoch, and no
    /// live block survives it.
    pub(super) fn restart(&mut self) {
        self.epoch = self.epoch.saturating_add(1);
        self.seq = 0;
        self.offsets.clear();
    }

    /// The session-less frame a positions stream ends a reconcile with; a
    /// legacy stream just ends.
    pub(super) fn resync(&mut self) -> Option<FramePoll> {
        self.enabled.then(|| {
            let frame = serde_json::json!({
                "type": "stream.resync",
                "data": {"epoch": format!("double{}", self.epoch)},
            });
            self.stamp(frame.to_string())
        })
    }

    /// Place one delta at its block's offset, as the product's positioned
    /// frame carries it. A legacy double leaves the delta as it was.
    fn place(&mut self, data: &mut serde_json::Value) {
        if !self.enabled {
            return;
        }
        let key = (
            data["assistantMessageID"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            data["ordinal"].as_u64().unwrap_or_default(),
        );
        let bytes = data["delta"].as_str().map_or(0, str::len) as u64;
        let offset = self.offsets.entry(key).or_insert(0);
        data["from"] = serde_json::json!(*offset);
        *offset = offset.saturating_add(bytes);
    }
}

impl ContractDouble {
    /// An older service: legacy frames only and no session snapshot.
    pub fn legacy(mut self) -> Self {
        self.positions.enabled = false;
        self
    }

    /// Serve one frame a witness wrote as this double serves its own: on a
    /// positioned double it takes the next position, and a live delta that
    /// names no offset is placed at its block's. A legacy double passes it
    /// through.
    pub fn serve(&mut self, frame: FramePoll) -> FramePoll {
        let FramePoll::Frame(text) = frame else {
            return frame;
        };
        if !self.positions.enabled {
            return FramePoll::Frame(text);
        }
        let text = match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(mut value)
                if value["data"]["delta"].is_string() && value["data"].get("from").is_none() =>
            {
                self.positions.place(&mut value["data"]);
                value.to_string()
            }
            _ => text,
        };
        self.positions.stamp(text)
    }

    pub(super) fn positions_enabled(&self) -> bool {
        self.positions.enabled
    }

    /// Queue one live delta, placed at its offset on a positioned double.
    pub(super) fn delta(&mut self, member: EventMember, mut data: serde_json::Value) {
        self.positions.place(&mut data);
        self.live(member, data);
    }

    /// Record a session's execution state at its committed position. A
    /// positioned double also reports it on a status frame; `busy` is the
    /// status a legacy reader of the same frame folds.
    pub(super) fn observe_state(
        &mut self,
        session: &SessionWireId,
        kind: &'static str,
        execution: Option<String>,
    ) {
        let Some(entry) = self.sessions.get_mut(session) else {
            return;
        };
        let state = ExecutionState {
            kind,
            revision: entry.next_seq,
            execution,
        };
        entry.state = Some(state.clone());
        if self.positions.enabled {
            let status = if kind == "running" { "busy" } else { "idle" };
            self.live(
                EventMember::SessionStatus,
                serde_json::json!({
                    "sessionID": session.spelled(),
                    "status": {"type": status},
                    "state": state.dto(),
                }),
            );
        }
    }

    /// The coherent snapshot: the newest page, the open asks and forms, and
    /// the execution state, all at the session's committed position. Every
    /// section is decoded by the real decoder before it is assembled, so a
    /// snapshot the product could not send never leaves the double.
    pub(super) fn coherent_snapshot(
        &self,
        session: &SessionWireId,
    ) -> Result<Option<Vec<u8>>, crate::wire::WireError> {
        use grapt_api::u3a::{
            decode_page, encode_page, BoundedVec, PermissionListResponseDto,
            SessionFormListResponseDto, SessionMessagesResponseDto, SessionSnapshotExecutionDto,
            SessionSnapshotResponseDto, SessionSnapshotRevisionDto,
        };
        use grapt_schema::Required;
        let Some(entry) = self.sessions.get(session) else {
            return Ok(None);
        };
        let limits = crate::wire::limits()?;
        let window = WireRequest::new(Operation::MessageList, Some(session), None)
            .map_err(|_| invalid("snapshot window"))?
            .with_query("limit", "200")
            .with_query("order", "desc");
        let page: SessionMessagesResponseDto =
            decode_page(&self.message_page(session, &window)?, limits)?;
        let asks: Vec<_> = self
            .permissions
            .get(session)
            .into_iter()
            .flatten()
            .filter(|ask| ask.settled.is_none())
            .map(|ask| ask.row(session))
            .collect();
        let asks: PermissionListResponseDto = decode_page(
            &json_bytes(&serde_json::json!({"data": asks, "hasMore": false}))?,
            limits,
        )?;
        let forms: Vec<crate::wire::FormRow> = self
            .forms
            .iter()
            .filter(|form| form.row.owner.session() == Some(session) && form.state.is_open())
            .map(|form| form.row.clone())
            .collect();
        let forms: SessionFormListResponseDto =
            decode_page(&crate::wire::encode_session_forms(&forms)?, limits)?;
        let mut execution = serde_json::json!({});
        if let Some(state) = &entry.state {
            execution["state"] = state.dto();
        }
        let execution: SessionSnapshotExecutionDto = decode_page(&json_bytes(&execution)?, limits)?;
        let aggregate = grapt_api::u3a::WireId::parse(session.spelled().to_owned())
            .ok_or_else(|| invalid("session identity"))?;
        let snapshot = SessionSnapshotResponseDto::new(
            Required::new(SessionSnapshotRevisionDto::new(
                Required::new(aggregate),
                Required::new(entry.next_seq.into()),
            )),
            Required::new(page.data().get().clone()),
            Required::new(page.cursor().get().clone()),
            Required::new(asks.data().get().clone()),
            Required::new(forms.data().get().clone()),
            Required::new(execution),
            Required::new(BoundedVec::try_new(Vec::new()).map_err(invalid)?),
        );
        Ok(Some(encode_page(&snapshot, limits)?))
    }
}

fn json_bytes(value: &serde_json::Value) -> Result<Vec<u8>, crate::wire::WireError> {
    serde_json::to_vec(value).map_err(|error| invalid(&error.to_string()))
}

fn invalid(value: &str) -> crate::wire::WireError {
    crate::wire::WireError::Invalid {
        what: "double session snapshot",
        value: value.to_owned(),
    }
}
