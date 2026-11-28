//! The page moves one publication reports. Every command that could move
//! the page settles in it: with the move it made, or as dropped when it
//! ended without one, so no page request waits on it.
use super::{Command, CommandId};
use grapt_client::client::HistoryMove;

#[derive(Debug, Default)]
pub(super) struct Pages {
    moves: Vec<(CommandId, HistoryMove, bool)>,
    /// Commands that could move the page, in the order they ran.
    asked: Vec<CommandId>,
    dropped: Vec<CommandId>,
}

impl Pages {
    /// Whether `command` could move the page.
    pub(super) fn navigates(command: &Command) -> bool {
        matches!(
            command,
            Command::Browse(_)
                | Command::Submit(_)
                | Command::SubmitImages(_)
                | Command::StartupPrompt(_)
                | Command::Retry(_)
        )
    }

    /// Command `id` is about to run.
    pub(super) fn asked(&mut self, id: CommandId, command: &Command) {
        if Self::navigates(command) {
            self.asked.push(id);
        }
    }

    pub(super) fn moved(&mut self, id: CommandId, movement: HistoryMove, changed: bool) {
        self.moves.push((id, movement, changed));
    }

    /// Command `id` never ran.
    pub(super) fn dropped(&mut self, id: CommandId) {
        self.dropped.push(id);
    }

    /// The Session is changing: earlier moves describe the one it leaves,
    /// so the commands that made them settle as dropped.
    pub(super) fn clear(&mut self) {
        self.moves.clear();
    }

    /// The moves made since the last publication, and every command that
    /// could have moved the page but did not. A move still in flight
    /// (`paging`) is neither: it settles in a later publication.
    pub(super) fn take(
        &mut self,
        paging: Option<CommandId>,
    ) -> (Vec<(CommandId, HistoryMove, bool)>, Vec<CommandId>) {
        let moves = std::mem::take(&mut self.moves);
        let mut dropped = std::mem::take(&mut self.dropped);
        let (waiting, ended) = self
            .asked
            .drain(..)
            .filter(|id| !moves.iter().any(|(moved, ..)| moved == id))
            .partition(|id| Some(*id) == paging);
        self.asked = waiting;
        dropped.extend::<Vec<_>>(ended);
        (moves, dropped)
    }
}
