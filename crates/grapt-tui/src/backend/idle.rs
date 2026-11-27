//! The protocol loop's wait between passes.
use super::Handoff;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::Duration;

/// One wait between passes. The loop's liveness reads as waiting for exactly
/// its length; a command backlog does not sleep at all; and every start of a
/// pass wakes the UI once, so a UI that saw the loop waiting arms its lag
/// deadline instead of sleeping through a long pass.
pub(super) fn pause<const N: usize>(
    (updates, ui_wake, ready): (&Handoff, &SyncSender<()>, &Receiver<()>),
    backlog: bool,
    deadlines: [Option<Duration>; N],
) -> Result<(), String> {
    updates.waiting();
    let waited = if backlog {
        Ok(())
    } else {
        wait(ready, deadlines)
    };
    updates.working();
    let _ = ui_wake.try_send(());
    waited
}

/// The earliest outstanding timer becomes the loop's wake deadline. A quiet
/// client leaves every slot empty; a steady `Some(0)` deadline would spin the
/// protocol thread instead of letting the receive sleep.
pub(super) fn wake_delay<const N: usize>(
    deadlines: [Option<std::time::Duration>; N],
) -> Option<std::time::Duration> {
    deadlines.into_iter().flatten().min()
}

pub(super) fn wait<const N: usize>(
    ready: &Receiver<()>,
    deadlines: [Option<std::time::Duration>; N],
) -> Result<(), String> {
    // An outstanding snapshot has one frame deadline. Socket/command wakes can
    // advance the model meanwhile; idle sessions retain an untimed receive.
    match wake_delay(deadlines) {
        None => ready.recv().map_err(|e| e.to_string()),
        Some(delay) => match ready.recv_timeout(delay) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => Ok(()),
            Err(error) => Err(error.to_string()),
        },
    }
}
