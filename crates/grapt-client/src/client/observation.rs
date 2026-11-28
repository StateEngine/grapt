//! Typed, revisioned execution observations on a positions stream (B3
//! S2c-1c).
//!
//! A session's state revision is its committed sequence, the same scale as a
//! snapshot's position P and a durable frame's `seq`. Each listed session keeps
//! one pair: the newest admitted presentation fact and its committed position.
//! A fact changes what the window shows only when it is strictly newer than the
//! pair, and then replaces both halves together, so the pair always describes
//! one fact. Everything shown (the view's idle, execution and feedback, and the
//! picker's activity) is derived from that pair alone.
use super::*;
use crate::contract::EventMember;
use grapt_api::u3a::{SessionStateDto, SessionStateKind as Kind, SessionStateReason as Reason};

/// One typed observation of a session's execution state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    Known {
        kind: Kind,
        execution: Option<crate::wire::ExecutionWireId>,
        reason: Option<Reason>,
    },
    /// The service could not read the state (its typed error code).
    Unreadable(grapt_api::u3a::SessionStateErrorCode),
    Absent,
}

/// The newest admitted presentation fact and its committed position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Presented {
    observation: Observation,
    anchor: u64,
}

impl Observation {
    pub(super) fn of(state: &SessionStateDto) -> Result<(Self, u64), ClientError> {
        let revision = serde_json::to_value(state.revision().get())
            .ok()
            .and_then(|value| value.as_u64())
            .ok_or(crate::wire::WireError::Invalid {
                what: "session state revision",
                value: String::new(),
            })?;
        let observation = Self::Known {
            kind: *state.kind().get(),
            execution: state.execution_id().as_present().cloned(),
            reason: state.reason().as_present().copied(),
        };
        Ok((observation, revision))
    }

    /// The observation a durable lifecycle frame establishes.
    fn of_lifecycle(member: EventMember, execution: crate::wire::ExecutionWireId) -> Option<Self> {
        let kind = match member {
            EventMember::ExecutionStarted => Kind::Running,
            EventMember::ExecutionSucceeded => Kind::Completed,
            EventMember::ExecutionFailed => Kind::Failed,
            EventMember::ExecutionInterrupted => Kind::Interrupted,
            _ => return None,
        };
        Some(Self::Known {
            kind,
            execution: Some(execution),
            reason: None,
        })
    }
}

/// A lifecycle frame's place against the pair, decided before any effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Lifecycle {
    /// Not a positions stream, no pair, or not a lifecycle frame.
    NotOwned,
    /// Older than the pair, or unpositioned: its history is kept as evidence,
    /// but nothing shown changes.
    Superseded,
    /// Missing its execution identity, or contradicting the pair (a terminal
    /// for another execution than the running one, or a second terminal):
    /// refused, never treated as superseded.
    Refused,
    /// Newer than the pair at this committed position.
    Newer(u64),
}

/// What a frame did on a positions stream.
pub(super) enum Admission {
    /// Not a presentation fact this module owns: apply it as today.
    Ordinary,
    /// Applied (or deliberately not): its frame needs no further fold.
    Handled(bool),
}

impl<C: Connection> Client<C> {
    /// Judge one frame on a positions stream before any effect. A typed state
    /// strictly newer than the pair replaces it and is presented; an older or
    /// equal one, or a state-less status while a pair exists, changes nothing.
    pub(super) fn admit_presentation(
        &mut self,
        envelope: &EventEnvelope,
    ) -> Result<Admission, ClientError> {
        let Some(session) = envelope.session_id.clone() else {
            return Ok(Admission::Ordinary);
        };
        if !self.tracks(&session) {
            return Ok(Admission::Ordinary);
        }
        // An unpositioned idle cannot outrank the pair.
        if envelope.member == Some(EventMember::SessionIdle) {
            return Ok(
                Admission::Handled(false).or_ordinary(self.observations.contains_key(&session))
            );
        }
        if envelope.member != Some(EventMember::SessionStatus) {
            return Ok(Admission::Ordinary);
        }
        let Some(state) = envelope.data.get("state") else {
            return Ok(
                Admission::Handled(false).or_ordinary(self.observations.contains_key(&session))
            );
        };
        let state: SessionStateDto = grapt_api::u3a::decode_page(
            &serde_json::to_vec(state).map_err(|_| projection_error())?,
            crate::wire::limits()?,
        )
        .map_err(|_| projection_error())?;
        let (observation, revision) = Observation::of(&state)?;
        if self
            .anchor(&session)
            .is_some_and(|anchor| revision <= anchor)
        {
            return Ok(Admission::Handled(false));
        }
        let feedback = matches!(
            observation,
            Observation::Known {
                kind: Kind::Waiting,
                ..
            }
        );
        self.observations.insert(
            session.clone(),
            Presented {
                observation,
                anchor: revision,
            },
        );
        // A waiting state's retry feedback travels on the same frame.
        if feedback && self.selected.as_ref() == Some(&session) {
            if let Some(view) = self.views.get_mut(&session) {
                let bounds = self.bounds;
                let _ = view.apply(envelope, bounds);
            }
        }
        Ok(Admission::Handled(self.present(&session)))
    }

    /// Classify a lifecycle frame on a positions stream before any effect.
    /// One newer than the pair is folded by the reducer (keeping its duplicate
    /// and attribution checks) and only then replaces the pair; an older or
    /// unpositioned one was superseded and changes nothing shown.
    pub(super) fn lifecycle(&self, envelope: &EventEnvelope) -> Lifecycle {
        let (Some(session), Some(member)) = (envelope.session_id.as_ref(), envelope.member) else {
            return Lifecycle::NotOwned;
        };
        if !matches!(
            member,
            EventMember::ExecutionStarted
                | EventMember::ExecutionSucceeded
                | EventMember::ExecutionFailed
                | EventMember::ExecutionInterrupted
        ) {
            return Lifecycle::NotOwned;
        }
        let Some(anchor) = self.anchor(session) else {
            return Lifecycle::NotOwned;
        };
        // Identity first: a missing execution is malformed, not old.
        let Some(execution) = envelope.execution_id.as_ref() else {
            return Lifecycle::Refused;
        };
        match envelope.durable.as_ref().map(|durable| durable.seq) {
            Some(seq) if seq > anchor => {
                if self.attributable(session, member, execution) {
                    Lifecycle::Newer(seq)
                } else {
                    Lifecycle::Refused
                }
            }
            _ => Lifecycle::Superseded,
        }
    }

    /// The reducer's ownership checks, against the pair, so a session without
    /// a transcript keeps them: a terminal must belong to the running
    /// execution, and an execution settles once.
    fn attributable(
        &self,
        session: &SessionWireId,
        member: EventMember,
        execution: &crate::wire::ExecutionWireId,
    ) -> bool {
        let terminal = matches!(
            member,
            EventMember::ExecutionSucceeded
                | EventMember::ExecutionFailed
                | EventMember::ExecutionInterrupted
        );
        let Some(Observation::Known {
            kind,
            execution: Some(current),
            ..
        }) = self.observations.get(session).map(|held| &held.observation)
        else {
            return true;
        };
        match kind {
            Kind::Running | Kind::Waiting => !terminal || current == execution,
            Kind::Completed | Kind::Failed | Kind::Interrupted => !terminal || current != execution,
            _ => true,
        }
    }

    /// Keep a superseded lifecycle frame's history: an interruption it records
    /// stays observable, without touching what is shown. No transcript is
    /// allocated for it.
    pub(super) fn record_superseded(&mut self, envelope: &EventEnvelope) {
        if envelope.member != Some(EventMember::ExecutionInterrupted) {
            return;
        }
        let (Some(session), Some(execution)) =
            (envelope.session_id.as_ref(), envelope.execution_id.clone())
        else {
            return;
        };
        if let Some(view) = self.views.get_mut(session) {
            view.interrupted_observed = Some(execution);
        }
    }

    /// Commit a newer lifecycle frame's pair, after the reducer accepted it.
    pub(super) fn commit_lifecycle(&mut self, envelope: &EventEnvelope, seq: u64) -> bool {
        let (Some(session), Some(member), Some(execution)) = (
            envelope.session_id.clone(),
            envelope.member,
            envelope.execution_id.clone(),
        ) else {
            return false;
        };
        let Some(observation) = Observation::of_lifecycle(member, execution) else {
            return false;
        };
        self.observations.insert(
            session.clone(),
            Presented {
                observation,
                anchor: seq,
            },
        );
        self.present(&session)
    }

    /// The pair a snapshot at `p` leaves: its section, unless a held pair is
    /// strictly newer than P.
    pub(super) fn observed_at(
        &self,
        session: &SessionWireId,
        p: u64,
        at_p: Observation,
    ) -> Presented {
        match self.observations.get(session) {
            Some(held) if held.anchor > p => held.clone(),
            _ => Presented {
                observation: at_p,
                anchor: p,
            },
        }
    }

    /// Publish a snapshot's pair with its install (infallible).
    pub(super) fn commit_presented(&mut self, session: &SessionWireId, presented: Presented) {
        self.observations.insert(session.clone(), presented);
        self.present(session);
    }

    fn anchor(&self, session: &SessionWireId) -> Option<u64> {
        self.observations.get(session).map(|held| held.anchor)
    }

    /// Listed sessions and the selected one may hold a pair; no other.
    fn tracks(&self, session: &SessionWireId) -> bool {
        self.selected.as_ref() == Some(session)
            || self.sessions.iter().any(|row| &row.id == session)
    }

    /// Derive everything shown for `session` from its pair.
    pub(super) fn present(&mut self, session: &SessionWireId) -> bool {
        let Some(presented) = self.observations.get(session).cloned() else {
            return false;
        };
        let observation = &presented.observation;
        let activity = activity_of(observation);
        let before = self.activity.get(session).map(|row| row.state);
        match activity {
            Some(state) => {
                let created = self.activity.get(session).map_or(0, |row| row.created);
                let stamp = crate::projection::FactStamp {
                    seq: Some(presented.anchor),
                    created: None,
                };
                self.activity.insert(
                    session.clone(),
                    activity::Observation {
                        state,
                        created,
                        stamp,
                    },
                );
            }
            None => {
                self.activity.remove(session);
            }
        }
        let mut changed = before != activity;
        let baselined = self.cursor.state().is_live();
        if let Some(view) = self.views.get_mut(session) {
            changed |= derive_view(view, &presented);
            // A running execution presented after an installed baseline counts
            // live output from zero; before one, the estimate stays unknown.
            if baselined {
                view.arm_live_output();
            }
        }
        changed
    }
}

impl Admission {
    /// A state-less status is owned here only while a pair exists.
    fn or_ordinary(self, held: bool) -> Self {
        if held {
            self
        } else {
            Self::Ordinary
        }
    }
}

fn projection_error() -> ClientError {
    crate::projection::ProjectionError::SessionStatusMalformed.into()
}

/// The picker's activity for one observation; `None` is unreported.
fn activity_of(observation: &Observation) -> Option<SessionActivity> {
    let Observation::Known { kind, reason, .. } = observation else {
        return None;
    };
    Some(match kind {
        Kind::Idle => SessionActivity::Idle,
        Kind::Running if *reason == Some(Reason::Compacting) => SessionActivity::Compacting,
        Kind::Running => SessionActivity::Running,
        Kind::Waiting if *reason == Some(Reason::ProviderSlot) => {
            SessionActivity::WaitingForProviderSlot { position: None }
        }
        Kind::Waiting if *reason == Some(Reason::ProviderRetry) => SessionActivity::Retrying,
        Kind::Waiting => SessionActivity::Running,
        Kind::Completed => SessionActivity::Succeeded,
        Kind::Failed => SessionActivity::Failed,
        Kind::Interrupted => SessionActivity::Interrupted,
        Kind::Paused | Kind::Disconnected => return None,
    })
}

/// The view's idle, execution and feedback for one observation. History's
/// execution stands where the observation names none; a previous Running never
/// survives an observation that is not running. The pair's position orders
/// what it sets against the live facts.
pub(super) fn derive_view(view: &mut SessionView, presented: &Presented) -> bool {
    let observation = &presented.observation;
    use crate::projection::{ExecutionState, Outcome};
    let before = (view.idle, view.execution.clone(), view.feedback.clone());
    let historical = match &view.execution {
        ExecutionState::Running { .. } => ExecutionState::Idle,
        other => other.clone(),
    };
    let (idle, execution) = match observation {
        Observation::Known {
            kind, execution, ..
        } => {
            let settled = |outcome| match execution {
                Some(id) => ExecutionState::Settled {
                    execution_id: Some(id.clone()),
                    outcome,
                },
                None => historical.clone(),
            };
            match kind {
                Kind::Running | Kind::Waiting => (
                    Some(false),
                    execution
                        .clone()
                        .map_or(historical.clone(), |execution_id| ExecutionState::Running {
                            execution_id,
                        }),
                ),
                Kind::Idle | Kind::Paused => (Some(true), historical.clone()),
                Kind::Completed => (Some(true), settled(Outcome::Succeeded)),
                Kind::Failed => (Some(true), settled(Outcome::Failed)),
                Kind::Interrupted => (Some(true), settled(Outcome::Interrupted)),
                Kind::Disconnected => (None, historical.clone()),
            }
        }
        Observation::Unreadable(_) | Observation::Absent => (None, historical),
    };
    if !matches!(
        observation,
        Observation::Known {
            kind: Kind::Waiting,
            ..
        }
    ) {
        view.feedback = None;
    }
    if idle.is_some() {
        view.idle_seq = Some(presented.anchor);
    }
    if matches!(
        observation,
        Observation::Known {
            kind: Kind::Completed | Kind::Failed | Kind::Interrupted,
            execution: Some(_),
            ..
        }
    ) {
        view.terminal_seq = Some(presented.anchor);
    }
    view.idle = idle;
    view.execution = execution;
    before != (view.idle, view.execution.clone(), view.feedback.clone())
}
