//! The positions subscriber class, served the way the product serves it.
//!
//! A positioned double stamps every frame with `id: <epoch>-<seq>`, places
//! each live delta at its block's byte offset (`from`), reports each session's
//! execution state as a revisioned `state` on its status frames, and answers
//! the coherent session snapshot, as every current service does.

use super::*;

/// The positions class of the double's one subscription.
#[derive(Debug)]
pub(super) struct Positions {
    /// The serving process's epoch: a restart starts a new one.
    epoch: u64,
    /// The last position stamped in this epoch, shared by every stream of it.
    seq: u64,
    /// What each live block has been sent, so a delta is placed at its
    /// offset and an end carries its whole text.
    placer: Placer,
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
            epoch: 0,
            seq: 0,
            placer: Placer::default(),
        }
    }

    /// Wrap one frame for the subscription at the next position.
    pub(super) fn stamp(&mut self, frame: String) -> FramePoll {
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
        self.placer = Placer::default();
    }

    /// The session-less frame a positions stream ends a reconcile with.
    pub(super) fn resync(&mut self) -> FramePoll {
        let frame = serde_json::json!({
            "type": "stream.resync",
            "data": {"epoch": format!("double{}", self.epoch)},
        });
        self.stamp(frame.to_string())
    }
}

impl ContractDouble {
    /// Serve one frame a witness wrote as this double serves its own: it
    /// takes the next position, a live delta that names no offset is placed
    /// at its block's, and an end that names no text carries its block's.
    pub fn serve(&mut self, frame: FramePoll) -> FramePoll {
        let FramePoll::Frame(text) = frame else {
            return frame;
        };
        let text = match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(mut value) if value["data"].is_object() => {
                let member = value["type"].as_str().and_then(EventMember::parse);
                self.positions.placer.place_data(member, &mut value["data"]);
                value.to_string()
            }
            _ => text,
        };
        self.positions.stamp(text)
    }

    /// The positioned `server.connected` this subscription still owes: every
    /// stream opens with it once, before any other frame, including a
    /// witness's own.
    pub fn connect(&mut self) -> Option<FramePoll> {
        if std::mem::replace(&mut self.connected_sent, true) {
            return None;
        }
        let frame = Self::render(
            EventMember::ServerConnected,
            &serde_json::json!({}),
            None,
            self.next_id,
        );
        Some(self.positions.stamp(frame))
    }

    /// Report `session` running `execution` at its committed position, as
    /// the product reports a turn still in flight.
    #[cfg(test)]
    pub(crate) fn report_running(&mut self, session: &SessionWireId, execution: &str) {
        self.observe_state(session, "running", Some(execution.to_owned()));
    }

    /// Append one block's durable end, carrying its whole text and retiring
    /// the block's live offset.
    pub(super) fn end_block(
        &mut self,
        session: &SessionWireId,
        member: EventMember,
        mut data: serde_json::Value,
    ) {
        self.positions.placer.place_data(Some(member), &mut data);
        self.append(session, member, data);
    }

    /// Queue one live delta, placed at its offset.
    pub(super) fn delta(&mut self, member: EventMember, mut data: serde_json::Value) {
        self.positions.placer.place_data(Some(member), &mut data);
        self.live(member, data);
    }

    /// Record a session's execution state at its committed position and
    /// report it on a status frame, beside the `busy`/`idle` status the
    /// product sends with it.
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

/// A witness's own newest message page, answered as the snapshot `request`
/// reads at committed position `seq`: its rows and cursor, no open asks or
/// forms, no live blocks, and no reported execution state.
pub fn page_snapshot(
    request: &WireRequest,
    page: &[u8],
    seq: u64,
) -> Result<Vec<u8>, crate::wire::WireError> {
    use grapt_api::u3a::{
        decode_page, encode_page, BoundedVec, SessionMessagesResponseDto,
        SessionSnapshotExecutionDto, SessionSnapshotResponseDto, SessionSnapshotRevisionDto,
    };
    use grapt_schema::Required;
    let limits = crate::wire::limits()?;
    let page: SessionMessagesResponseDto = decode_page(page, limits)?;
    let execution: SessionSnapshotExecutionDto =
        decode_page(&json_bytes(&serde_json::json!({}))?, limits)?;
    let aggregate = ContractDouble::session_of(request.path())
        .and_then(|session| grapt_api::u3a::WireId::parse(session.spelled().to_owned()))
        .ok_or_else(|| invalid("session identity"))?;
    let snapshot = SessionSnapshotResponseDto::new(
        Required::new(SessionSnapshotRevisionDto::new(
            Required::new(aggregate),
            Required::new(seq.into()),
        )),
        Required::new(page.data().get().clone()),
        Required::new(page.cursor().get().clone()),
        Required::new(BoundedVec::try_new(Vec::new()).map_err(invalid)?),
        Required::new(BoundedVec::try_new(Vec::new()).map_err(invalid)?),
        Required::new(execution),
        Required::new(BoundedVec::try_new(Vec::new()).map_err(invalid)?),
    );
    Ok(encode_page(&snapshot, limits)?)
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
