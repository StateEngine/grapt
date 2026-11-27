use grapt_types::SessionId;

/// A session's runtime lifecycle. `Paused` keeps an interrupted turn that
/// can resume; `Idle` has no turn at all, so the two never merge.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SessionRuntimeState {
    Idle,
    Running,
    Paused,
    WaitingTool,
    Compacting,
    Failed,
}

impl SessionRuntimeState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::WaitingTool => "waiting_tool",
            Self::Compacting => "compacting",
            Self::Failed => "failed",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        [
            Self::Idle,
            Self::Running,
            Self::Paused,
            Self::WaitingTool,
            Self::Compacting,
            Self::Failed,
        ]
        .into_iter()
        .find(|state| state.as_str() == text)
    }
}

/// A requested lifecycle step. The actor accepts it only from the states
/// listed in [`Event::apply`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Event {
    StartTurn,
    FinishTurn,
    Pause,
    Resume,
    AwaitTool,
    ToolReturned,
    StartCompaction,
    FinishCompaction,
    Cancel,
    Fail,
    Recover,
}

impl Event {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StartTurn => "start_turn",
            Self::FinishTurn => "finish_turn",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::AwaitTool => "await_tool",
            Self::ToolReturned => "tool_returned",
            Self::StartCompaction => "start_compaction",
            Self::FinishCompaction => "finish_compaction",
            Self::Cancel => "cancel",
            Self::Fail => "fail",
            Self::Recover => "recover",
        }
    }

    /// The state this event moves `from` into, or a refusal naming both.
    pub fn apply(
        self,
        from: SessionRuntimeState,
    ) -> Result<SessionRuntimeState, InvalidTransition> {
        use Event as E;
        use SessionRuntimeState as S;
        let to = match (self, from) {
            (E::StartTurn, S::Idle)
            | (E::Resume, S::Paused)
            | (E::ToolReturned, S::WaitingTool)
            | (E::FinishCompaction, S::Compacting) => S::Running,
            (E::FinishTurn, S::Running)
            | (E::Cancel, S::Running | S::Paused | S::WaitingTool | S::Compacting)
            | (E::Recover, S::Failed) => S::Idle,
            (E::Pause, S::Running) => S::Paused,
            (E::AwaitTool, S::Running) => S::WaitingTool,
            (E::StartCompaction, S::Running) => S::Compacting,
            (E::Fail, from) if from != S::Failed => S::Failed,
            _ => return Err(InvalidTransition { from, event: self }),
        };
        Ok(to)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{} is not allowed while the session is {}", event.as_str(), from.as_str())]
pub struct InvalidTransition {
    pub from: SessionRuntimeState,
    pub event: Event,
}

/// Monotonic per-session transition counter; zero means no transition has
/// been committed yet. It never exceeds the store's integer range.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StateSeq(u64);

impl StateSeq {
    pub const INITIAL: Self = Self(0);

    pub fn get(self) -> u64 {
        self.0
    }

    pub(crate) fn from_stored(value: i64) -> Option<Self> {
        u64::try_from(value).ok().map(Self)
    }

    pub(crate) fn stored(self) -> i64 {
        i64::try_from(self.0).unwrap_or(i64::MAX)
    }

    pub(crate) fn next(self) -> Option<Self> {
        let next = self.stored().checked_add(1)?;
        Self::from_stored(next)
    }
}

/// A committed submit. `duplicate` is true when an earlier submit with the
/// same key and event already made this transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Applied {
    pub state: SessionRuntimeState,
    pub state_seq: StateSeq,
    pub duplicate: bool,
}

/// The committed state of one session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSnapshot {
    pub session: SessionId,
    pub state: SessionRuntimeState,
    pub state_seq: StateSeq,
}

/// One committed transition, published after its commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateChange {
    pub session: SessionId,
    pub event: Event,
    pub from: SessionRuntimeState,
    pub to: SessionRuntimeState,
    pub state_seq: StateSeq,
}

#[cfg(test)]
mod tests {
    use super::{Event, SessionRuntimeState as S};

    #[test]
    fn paused_resumes_its_turn_while_idle_has_none_to_resume() {
        assert_eq!(Event::Resume.apply(S::Paused), Ok(S::Running));
        assert!(Event::Resume.apply(S::Idle).is_err());
        assert!(Event::Pause.apply(S::Idle).is_err());
        assert!(Event::Pause.apply(S::WaitingTool).is_err());
        assert_eq!(Event::Cancel.apply(S::Paused), Ok(S::Idle));
        assert!(Event::Fail.apply(S::Failed).is_err());
        assert_eq!(Event::Recover.apply(S::Failed), Ok(S::Idle));
        for state in [
            S::Idle,
            S::Running,
            S::Paused,
            S::WaitingTool,
            S::Compacting,
            S::Failed,
        ] {
            assert_eq!(S::parse(state.as_str()), Some(state));
        }
    }
}
