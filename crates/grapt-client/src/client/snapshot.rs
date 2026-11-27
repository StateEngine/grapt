//! Positions mode (B3 S2c). A connection whose stream proved positions reads
//! one coherent snapshot at a committed position P in place of the legacy
//! history read: its rows and cursor install through the existing tail path,
//! and P becomes the durable baseline, so a durable frame at or before P is a
//! duplicate and one after it applies once. A legacy connection never asks.
use super::*;
use grapt_api::u3a::{decode_page, SessionSnapshotResponseDto};

/// What the one live connection proved about stream positions.
#[derive(Debug, Default)]
pub(crate) struct Positions {
    /// How this connection's stream carries live blocks, decided once by its
    /// first frame: a `server.connected` with an `id:` proves positions, any
    /// other first frame is legacy. Cleared when the stream ends.
    pub(super) mode: Option<crate::projection::StreamMode>,
    /// The service had no snapshot route (404): the connection is turning
    /// legacy, and the messages read waits for the legacy stream's first frame.
    pub(super) awaiting_legacy: bool,
    /// The committed position the next tail install adopts as its baseline.
    pub(super) baseline: Option<u64>,
    /// The last mode any stream on this connection proved. After a cut it says
    /// whether the next stream must prove its mode before the messages read.
    pub(super) proven: Option<crate::projection::StreamMode>,
    /// A legacy messages read installed while no stream had proved a mode. A
    /// later positions proof re-reads a snapshot before any delta applies.
    pub(super) legacy_unproven: bool,
    /// When the client began waiting for a stream to prove its mode, on the
    /// `pump_at` clock. Only a proof clears it, so the wait is bounded in total
    /// across stream ends and selection changes.
    pub(super) handshake_since: Option<std::time::Duration>,
}

impl Positions {
    /// A stream ended: its mode and baseline go. What the connection learned
    /// (a 404's wait, the last proof, an unproven baseline, the handshake
    /// deadline) survives until the next stream's first frame.
    pub(super) fn reset(&mut self) {
        self.mode = None;
        self.baseline = None;
    }
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

/// The legacy page shape, borrowed from the snapshot's typed rows and cursor.
#[derive(serde::Serialize)]
struct Page<'a, R, C> {
    data: &'a R,
    cursor: &'a C,
}

/// Decode through the closed snapshot decoder, then hand its rows and cursor
/// to the same page decoder the legacy read uses, so both paths install one
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
    /// Snapshot reads only on a connection whose stream proved positions and
    /// whose service serves the snapshot; never one without the other.
    pub(super) fn positions_mode(&self) -> bool {
        self.positions.mode == Some(crate::projection::StreamMode::Positions)
    }

    /// Decide this connection's mode from its first frame, once: only a
    /// `server.connected` carrying an `id:` proves positions. False when a
    /// later frame belongs to the other class.
    pub(super) fn latch_mode(&mut self, payload: &str, positioned: bool) -> bool {
        use crate::projection::StreamMode;
        match self.positions.mode {
            None => {
                // The stream a snapshot 404 asked for has arrived.
                self.positions.awaiting_legacy = false;
                let connected = EventEnvelope::decode(payload)
                    .ok()
                    .and_then(|envelope| envelope.member)
                    == Some(crate::contract::EventMember::ServerConnected);
                let mode = if positioned && connected {
                    StreamMode::Positions
                } else {
                    StreamMode::Legacy
                };
                self.positions.mode = Some(mode);
                self.positions.proven = Some(mode);
                self.positions.handshake_since = None;
                // A legacy page read before this proof must not take this
                // stream's deltas: a snapshot replaces it first.
                if std::mem::take(&mut self.positions.legacy_unproven)
                    && mode == StreamMode::Positions
                {
                    self.request_history();
                }
                true
            }
            Some(mode) => (mode == StreamMode::Positions) == positioned,
        }
    }

    /// The one decision both messages reads make: a snapshot on a stream that
    /// proved positions, a legacy page on one that proved legacy. While no
    /// stream has proved a mode, a connection that needs the proof (a snapshot
    /// 404's legacy replacement, or a cut on a positions connection) waits for
    /// the next stream's first frame, bounded in total by
    /// [`super::recovery::RECONNECT_RETRY_MAX`] from `handshake_since`; past
    /// that the page is read over HTTP anyway. `None` waits. A caller without
    /// the `pump_at` clock passes `None` and only ever waits or proceeds as
    /// though the bound had not passed; the queued read applies it.
    pub(super) fn messages_read(
        &self,
        now: Option<std::time::Duration>,
    ) -> Option<super::recovery::MessagesRead> {
        use super::recovery::MessagesRead;
        use crate::projection::StreamMode;
        match self.positions.mode {
            Some(StreamMode::Positions) => Some(MessagesRead::Snapshot),
            Some(StreamMode::Legacy) => Some(MessagesRead::Legacy),
            None => {
                let expired =
                    now.zip(self.positions.handshake_since)
                        .is_some_and(|(now, since)| {
                            now.saturating_sub(since) >= super::recovery::RECONNECT_RETRY_MAX
                        });
                (!self.needs_mode_proof() || expired).then_some(MessagesRead::Legacy)
            }
        }
    }

    /// Whether the next stream must prove its mode before the messages read.
    pub(super) fn needs_mode_proof(&self) -> bool {
        self.positions.mode.is_none()
            && (self.positions.awaiting_legacy
                || self.positions.proven == Some(crate::projection::StreamMode::Positions))
    }

    /// The service has no snapshot (404): the connection turns legacy with a
    /// legacy stream before any legacy read, so a positions stream is never
    /// paired with a legacy history read.
    pub(super) fn refuse_snapshot(&mut self) {
        self.connection.refuse_positions();
        self.drop_resume();
        self.positions.reset();
        self.positions.awaiting_legacy = true;
        self.cursor.stream_closed();
    }

    /// The mode live blocks apply in on this connection.
    pub(super) fn stream_mode(&self) -> crate::projection::StreamMode {
        if self.positions_mode() {
            crate::projection::StreamMode::Positions
        } else {
            crate::projection::StreamMode::Legacy
        }
    }

    /// Install one snapshot, baselined at P, as one atomic swap: every section
    /// is built and checked on a detached candidate, then published with plain
    /// assignments that cannot fail. An error leaves the client untouched.
    pub(super) fn install_snapshot(
        &mut self,
        session: &SessionWireId,
        bytes: &[u8],
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
        let permissions = self.permission_candidate(snapshot.permissions, kept)?;
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
        self.commit_tail(
            session,
            &snapshot.page,
            snapshot.page_bytes,
            tail,
            super::history_install::Carry::Observed,
        );
        self.positions.baseline = None;
        self.positions.legacy_unproven = false;
        self.commit_presented(session, presented);
        self.commit_permissions(permissions);
        if self.selected.as_ref() == Some(session) {
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

    /// The direct read: a snapshot, or `false` when this service has none
    /// and the connection drops to legacy mode.
    pub(super) fn read_snapshot(&mut self, session: &SessionWireId) -> Result<bool, ClientError> {
        match self.ingest(
            Operation::SessionSnapshot,
            Some(session),
            None,
            None,
            None,
            None,
        ) {
            Ok(bytes) => {
                self.install_snapshot(session, &bytes)?;
                Ok(true)
            }
            Err(error) if unsupported(&error) => {
                self.refuse_snapshot();
                self.request_history();
                Ok(true)
            }
            // Incomplete for now: the recovery read retries it with backoff,
            // still in positions mode; the direct read never falls to legacy.
            Err(error) if error.is_transient() => {
                self.request_history();
                Ok(true)
            }
            Err(error) => Err(error),
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

/// Whether a snapshot answer means "this service has no snapshot route": the
/// connection then falls back to its legacy reads. A 503 is not that: the
/// snapshot is incomplete for now (a live block it cannot carry whole) and
/// is retried with the ordinary transient backoff, never mixed with legacy.
pub(super) fn unsupported(error: &ClientError) -> bool {
    matches!(error, ClientError::Status { status: 404, .. })
}

#[cfg(test)]
mod rows_tests;
