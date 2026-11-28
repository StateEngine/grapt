//! Positions (B3 S2c). Every stream must open with a positioned
//! `server.connected`; once it has, the client reads one coherent snapshot at
//! a committed position P: its rows and cursor install through the tail path,
//! and P becomes the durable baseline, so a durable frame at or before P is a
//! duplicate and one after it applies once. A service without positions or
//! without the snapshot is refused as too old, naming the upgrade.
use super::*;
use grapt_api::u3a::{decode_page, SessionSnapshotResponseDto};

/// What the one live stream has proved about itself.
#[derive(Debug, Default)]
pub(crate) struct Positions {
    /// Whether this stream's first frame, a positioned `server.connected`,
    /// has arrived. Cleared when the stream ends.
    pub(super) proven: bool,
    /// The committed position the next tail install adopts as its baseline.
    pub(super) baseline: Option<u64>,
    /// When the client began waiting for a stream's first frame, on the
    /// `pump_at` clock. Only a proof clears it, so the wait is bounded in total
    /// across stream ends and selection changes.
    pub(super) handshake_since: Option<std::time::Duration>,
    /// The session whose snapshot installed last. It outlives the stream:
    /// what the snapshot told about that session stays known until a
    /// selection clears what it installed.
    installed: Option<SessionWireId>,
}

impl Positions {
    /// A selection cleared the session-scoped state a snapshot installed, so
    /// no session is ready until its own snapshot installs again.
    pub(super) fn forget_installed(&mut self) {
        self.installed = None;
    }

    /// A stream ended: its proof and baseline go. The handshake deadline
    /// survives until the next stream's first frame.
    pub(super) fn reset(&mut self) {
        self.proven = false;
        self.baseline = None;
    }
}

/// Which of a snapshot's open-state sections an install takes. A staged
/// attachment reads its forms after its snapshot, and may accept a newer
/// permission refresh meanwhile; it leaves those sections to the newer reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Sections {
    pub(super) permissions: bool,
    pub(super) forms: bool,
}

impl Sections {
    pub(super) const ALL: Self = Self {
        permissions: true,
        forms: true,
    };
}

/// A decoded snapshot: the page the tail path installs, its position, and
/// the live blocks every subscriber had been offered.
pub(super) struct Snapshot {
    pub(super) page: crate::wire::MessagePage,
    pub(super) page_bytes: usize,
    pub(super) seq: u64,
    pub(super) provisional: Vec<Provisional>,
    /// The session's pending permissions and forms at P, through the same
    /// typed projections the list reads use.
    permissions: Vec<crate::wire::PermissionRow>,
    forms: Vec<crate::wire::FormRow>,
    /// The session's execution state at P, typed.
    execution: super::observation::Observation,
}

/// One live block a snapshot carries.
pub(super) struct Provisional {
    kind: crate::projection::SliceKind,
    identity: (crate::wire::MessageWireId, u64),
    text: String,
}

/// The message page shape, borrowed from the snapshot's typed rows and cursor.
#[derive(serde::Serialize)]
struct Page<'a, R, C> {
    data: &'a R,
    cursor: &'a C,
}

/// Decode through the closed snapshot decoder, then hand its rows and cursor
/// to the same page decoder older-history pages use, so both install one
/// shape.
pub(super) fn decode(bytes: &[u8]) -> Result<Snapshot, ClientError> {
    let limits = crate::wire::limits()?;
    let snapshot: SessionSnapshotResponseDto =
        decode_page(bytes, limits).map_err(crate::wire::WireError::from)?;
    let seq = serde_json::to_value(snapshot.revision().get().seq().get())
        .ok()
        .and_then(|seq| seq.as_u64())
        .ok_or_else(|| crate::wire::WireError::Invalid {
            what: "snapshot revision",
            value: String::new(),
        })?;
    // Re-encode from the typed values, never through `serde_json::Value`: a
    // value map sorts its keys, and the row unions read their tag first.
    let page = serde_json::to_vec(&Page {
        data: snapshot.rows().get(),
        cursor: snapshot.cursor().get(),
    })
    .map_err(|error| crate::wire::WireError::Codec {
        reason: error.to_string(),
    })?;
    Ok(Snapshot {
        page: crate::wire::decode_messages(&page)?,
        page_bytes: page.len(),
        seq,
        provisional: provisional(snapshot.provisional().get())?,
        execution: execution(snapshot.execution().get(), seq)?,
        permissions: crate::wire::permission_rows(snapshot.permissions().get().as_slice()),
        forms: crate::wire::form_rows(snapshot.forms().get().as_slice())?,
    })
}

impl<C: Connection> Client<C> {
    /// Whether the selected session's snapshot has installed, so its open
    /// permissions, forms and execution state are known. Selecting another
    /// session waits for that session's own snapshot.
    pub fn selected_snapshot_installed(&self) -> bool {
        self.selected.is_some() && self.positions.installed == self.selected
    }

    /// Check one frame against this stream's contract. Its first frame must
    /// be a positioned `server.connected`; an unpositioned one comes from a
    /// service that predates positions, which is refused. False when the
    /// stream breaks its own contract: a positioned first frame that is not
    /// the connect, or a later frame without a position.
    pub(super) fn latch_mode(
        &mut self,
        payload: &str,
        positioned: bool,
    ) -> Result<bool, ClientError> {
        if self.positions.proven {
            return Ok(positioned);
        }
        if !positioned {
            // No later stream from this service can prove positions.
            return Err(self.refuse_too_old("stream positions"));
        }
        let connected = EventEnvelope::decode(payload)
            .ok()
            .and_then(|envelope| envelope.member)
            == Some(crate::contract::EventMember::ServerConnected);
        if connected {
            self.positions.proven = true;
            self.positions.handshake_since = None;
        }
        Ok(connected)
    }

    /// Whether this stream's first frame has arrived, so the snapshot may be
    /// read. The first wait starts the handshake deadline; a stream still
    /// silent at [`super::recovery::RECONNECT_RETRY_MAX`] is retired, so
    /// recovery subscribes afresh, and the deadline starts again.
    pub(super) fn first_frame_seen(&mut self, now: std::time::Duration) -> bool {
        if self.positions.proven {
            return true;
        }
        let since = *self.positions.handshake_since.get_or_insert(now);
        if now.saturating_sub(since) >= super::recovery::RECONNECT_RETRY_MAX {
            self.connection.retire_stream();
            self.cursor.stream_closed();
            self.positions.handshake_since = Some(now);
        }
        false
    }

    /// Install one snapshot, baselined at P, as one atomic swap: every section
    /// is built and checked on a detached candidate, then published with plain
    /// assignments that cannot fail. An error leaves the client untouched.
    pub(super) fn install_snapshot(
        &mut self,
        session: &SessionWireId,
        bytes: &[u8],
    ) -> Result<(), ClientError> {
        self.install_snapshot_sections(session, bytes, Sections::ALL)
    }

    /// Install a snapshot, leaving out the sections the caller already holds
    /// newer than it.
    pub(super) fn install_snapshot_sections(
        &mut self,
        session: &SessionWireId,
        bytes: &[u8],
        sections: Sections,
    ) -> Result<(), ClientError> {
        let snapshot = decode(bytes)?;
        // The durable tail, and the live blocks opened on it; durable truth wins.
        let mut tail = self.prepare_tail(session, &snapshot.page, snapshot.page_bytes)?;
        for block in snapshot.provisional {
            tail.view
                .open_provisional(block.kind, block.identity, &block.text, self.bounds)?;
        }
        // Permissions are one list across sessions: this session's asks are
        // P's, every other session's are kept.
        let kept = self
            .permissions
            .iter()
            .filter(|ask| &ask.session_id != session)
            .cloned()
            .collect();
        let permissions = if sections.permissions {
            Some(self.permission_candidate(snapshot.permissions, kept))
        } else {
            None
        };
        // The execution state at P, unless this client already holds a newer
        // presentation fact; what the install shows is derived from it alone.
        let presented = self.observed_at(session, snapshot.seq, snapshot.execution);
        super::observation::derive_view(&mut tail.view, &presented);
        // Never a quiet shrink: a block the window rendered must hold at least
        // as many bytes under the same identity after the install.
        let shrunk = self.views.get(session).is_some_and(|before| {
            before.live_blocks().iter().any(|(kind, identity, shown)| {
                tail.view
                    .live_bytes(*kind, identity)
                    .or_else(|| tail.view.durable_bytes(&identity.0, *kind, identity.1))
                    .is_none_or(|now| now < *shown)
            })
        });
        // Commit: nothing below can fail.
        self.positions.baseline = Some(snapshot.seq);
        self.commit_tail(session, &snapshot.page, snapshot.page_bytes, tail);
        self.positions.baseline = None;
        self.positions.installed = Some(session.clone());
        self.commit_presented(session, presented);
        if let Some(permissions) = permissions {
            self.commit_permissions(permissions);
        }
        if sections.forms && self.selected.as_ref() == Some(session) {
            self.forms = snapshot
                .forms
                .into_iter()
                .take(self.bounds.max_retained_permissions)
                .collect();
            self.session_forms_dirty = false;
        }
        if shrunk {
            // At least the visible resync line at that block.
            self.gap_recovery_pending = true;
            self.diagnostics.gaps_observed = self.diagnostics.gaps_observed.saturating_add(1);
        }
        Ok(())
    }

    /// The direct read: the snapshot, installed, or queued with the recovery
    /// owner when it is incomplete for now. A service without the snapshot
    /// is refused as too old where its answer is accepted.
    pub(super) fn read_snapshot(&mut self, session: &SessionWireId) -> Result<(), ClientError> {
        match self.ingest(
            Operation::SessionSnapshot,
            Some(session),
            None,
            None,
            None,
            None,
        ) {
            Ok(bytes) => self.install_snapshot(session, &bytes),
            // Incomplete for now: the recovery read retries it with backoff.
            Err(error) if error.is_transient() => {
                self.request_history();
                Ok(())
            }
            Err(error) => Err(self.latch_too_old(error)),
        }
    }
}

/// The execution section: a state, its typed error, or neither. Both at once
/// is a contradiction, refused before any install.
fn execution(
    section: &grapt_api::u3a::SessionSnapshotExecutionDto,
    p: u64,
) -> Result<super::observation::Observation, ClientError> {
    use super::observation::Observation;
    match (
        section.state().as_present(),
        section.state_error().as_present(),
    ) {
        // A state belongs to the cut at P, so its revision can never pass P.
        (Some(state), None) => match Observation::of(state)? {
            (observation, revision) if revision <= p => Ok(observation),
            _ => Err(crate::wire::WireError::Invalid {
                what: "snapshot state revision",
                value: String::new(),
            }
            .into()),
        },
        (None, Some(error)) => Ok(Observation::Unreadable(*error.code().get())),
        (None, None) => Ok(Observation::Absent),
        (Some(_), Some(_)) => Err(crate::wire::WireError::Invalid {
            what: "snapshot execution",
            value: String::new(),
        }
        .into()),
    }
}

/// The snapshot's live blocks, already bound-checked by the closed decoder.
fn provisional(blocks: &impl serde::Serialize) -> Result<Vec<Provisional>, ClientError> {
    let invalid = |what: &'static str| crate::wire::WireError::Invalid {
        what,
        value: String::new(),
    };
    let blocks = serde_json::to_value(blocks).map_err(|_| invalid("snapshot provisional"))?;
    let mut out = Vec::new();
    for block in blocks
        .as_array()
        .ok_or_else(|| invalid("snapshot provisional"))?
    {
        let field = |name: &str| block.get(name);
        let kind = match field("kind").and_then(serde_json::Value::as_str) {
            Some("text") => crate::projection::SliceKind::Text,
            Some("reasoning") => crate::projection::SliceKind::Reasoning,
            _ => return Err(invalid("provisional kind").into()),
        };
        let message = field("assistantMessageID")
            .and_then(serde_json::Value::as_str)
            .and_then(|raw| crate::wire::MessageWireId::parse(raw.to_owned()))
            .ok_or_else(|| invalid("provisional message"))?;
        let ordinal = field("ordinal")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| invalid("provisional ordinal"))?;
        let text = field("text")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| invalid("provisional text"))?;
        // A complete snapshot carries each block's whole text; a byte count
        // that disagrees with it is refused before it can seed a digest.
        let bytes = field("bytes").and_then(serde_json::Value::as_u64);
        if bytes != u64::try_from(text.len()).ok() {
            return Err(invalid("provisional bytes").into());
        }
        out.push(Provisional {
            kind,
            identity: (message, ordinal),
            text: text.to_owned(),
        });
    }
    Ok(out)
}

/// The session-less `stream.resync` frame a positions stream sends before a
/// reconcile boundary ends it.
pub(super) fn is_stream_resync(payload: &str) -> bool {
    payload.contains("stream.resync")
        && serde_json::from_str::<serde_json::Value>(payload)
            .ok()
            .and_then(|frame| Some(frame.get("type")?.as_str()? == "stream.resync"))
            .unwrap_or(false)
}

/// What a snapshot 404 says, read within `bound`. An unregistered route
/// answers without a typed body: a service too old for snapshots. A typed
/// refusal, a missing session's `SessionNotFoundError` included, keeps its
/// own message rather than collapsing to a status. A body past the bound is
/// not read and keeps the plain status.
pub(super) fn missing(body: &[u8], bound: usize) -> Option<ClientError> {
    if body.len() > bound {
        return None;
    }
    let value = serde_json::from_slice::<serde_json::Value>(body).ok();
    let field = |name: &str| {
        value
            .as_ref()
            .and_then(|value| value.get(name))
            .and_then(serde_json::Value::as_str)
    };
    match field("_tag") {
        None => Some(super::refusal::too_old("session snapshots")),
        Some(tag) => Some(ClientError::Refused {
            operation: Operation::SessionSnapshot.operation_id(),
            status: 404,
            message: field("message").unwrap_or(tag).to_owned(),
        }),
    }
}

#[cfg(test)]
mod rows_tests;
