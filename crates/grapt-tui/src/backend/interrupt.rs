//! Bounded stop ownership keeps slow cleanup off the protocol loop.
use super::Command;
use crate::bridge::{Bridge, StopPeer};
use grapt_client::{
    wire::{ExecutionWireId, SessionWireId, Spelled},
    Bounds, Client, ClientError, Connection,
};
use std::sync::mpsc::{self, Receiver, SyncSender};

const MAX_STOPS: usize = 4;
/// The service's refusal when a fenced stop's execution is no longer running.
const ENDED: &str = "the active execution identity changed";
type Reply = SyncSender<Result<(), String>>;

pub(super) fn submission_refused() -> ClientError {
    ClientError::InvalidInput(
        "Session stop is awaiting confirmation; draft kept. Wait for its result before sending.",
    )
}

struct Request {
    session: SessionWireId,
    resume: bool,
    observed: Option<ExecutionWireId>,
}

struct Pending {
    session: SessionWireId,
    /// The stop named the execution it was aimed at.
    fenced: bool,
    result: Receiver<Result<(), String>>,
    reply: Option<Reply>,
}

struct WaitingQuit {
    peer: StopPeer,
    request: Request,
    reply: Reply,
    wake: SyncSender<()>,
}

/// Wakes even if a worker unwinds, so a lost completion is a visible failure.
struct CompletionWake(SyncSender<()>);
impl Drop for CompletionWake {
    fn drop(&mut self) {
        let _ = self.0.try_send(());
    }
}

#[derive(Default)]
pub(super) struct Stops {
    pending: Vec<Pending>,
    quit: Option<WaitingQuit>,
}

impl Stops {
    pub(super) fn pending_for(&self, session: Option<&SessionWireId>) -> bool {
        self.pending
            .iter()
            .any(|pending| Some(&pending.session) == session)
            || self
                .quit
                .as_ref()
                .is_some_and(|quit| Some(&quit.request.session) == session)
    }

    pub(super) fn dispatch(
        &mut self,
        client: &mut Client<Bridge>,
        command: Command,
        reply: Option<Reply>,
        wake: &SyncSender<()>,
    ) -> String {
        let request = Request::new(client, command);
        let result = request.and_then(|request| {
            if self
                .quit
                .as_ref()
                .is_some_and(|quit| quit.request.session == request.session)
                || self
                    .pending
                    .iter()
                    .any(|pending| pending.session == request.session && pending.reply.is_some())
            {
                return Err(ClientError::InvalidInput(
                    "Exit stop is awaiting confirmation; wait for its result before retrying.",
                ));
            }
            let peer = client.peer_mut().stop_peer()?;
            if let Some(reply) = reply.as_ref() {
                if self.pending_for(Some(&request.session)) {
                    if self.quit.is_some() {
                        return Err(ClientError::InvalidInput(
                            "Another exit stop is waiting; wait for its result before retrying.",
                        ));
                    }
                    self.quit = Some(WaitingQuit {
                        peer,
                        request,
                        reply: reply.clone(),
                        wake: wake.clone(),
                    });
                    return Ok(());
                }
            }
            self.start(peer, request, reply.clone(), wake.clone())
        });
        match result {
            Ok(()) => "Stopping session…".into(),
            Err(error) => {
                let error = error.to_string();
                if let Some(reply) = reply {
                    let _ = reply.try_send(Err(error.clone()));
                    format!("Cannot stop session; terminal kept open: {error}")
                } else {
                    error
                }
            }
        }
    }

    fn start(
        &mut self,
        peer: StopPeer,
        request: Request,
        reply: Option<Reply>,
        wake: SyncSender<()>,
    ) -> Result<(), ClientError> {
        if self.pending.len() >= MAX_STOPS {
            return Err(ClientError::InvalidInput(
                "Four session stops are awaiting confirmation; wait for a result before retrying.",
            ));
        }
        let (sender, result) = mpsc::sync_channel(1);
        let session = request.session.clone();
        let fenced = request.observed.is_some();
        let _worker = std::thread::Builder::new()
            .name("tui-session-stop".into())
            .spawn(move || {
                let _wake = CompletionWake(wake);
                // Close the result channel before waking on an unwind.
                let result_sender = sender;
                let result = peer.verify().map_err(ClientError::from).and_then(|peer| {
                    let mut client = Client::new(peer, Bounds::PRODUCTION);
                    match (request.resume, request.observed.as_ref()) {
                        (true, observed) => {
                            client.interrupt_session_and_resume(&request.session, observed)
                        }
                        (false, Some(execution)) => {
                            client.interrupt_execution(&request.session, execution)
                        }
                        (false, None) => client.interrupt_session(&request.session),
                    }
                });
                let _ = result_sender.try_send(result.map_err(|error| error.to_string()));
            })
            .map_err(|_| {
                ClientError::InvalidInput("Session stop worker could not start; retry stop.")
            })?;
        self.pending.push(Pending {
            session,
            fenced,
            result,
            reply,
        });
        Ok(())
    }

    pub(super) fn advance(&mut self) -> Option<String> {
        let mut notices = Vec::new();
        let mut index = 0;
        while index < self.pending.len() {
            let result = match self.pending[index].result.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => {
                    index += 1;
                    continue;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    Err("Session stop worker ended without confirmation; retry stop.".into())
                }
            };
            let pending = self.pending.swap_remove(index);
            if let Err(error) = &result {
                if self
                    .quit
                    .as_ref()
                    .is_some_and(|quit| quit.request.session == pending.session)
                {
                    if let Some(quit) = self.quit.take() {
                        let _ = quit.reply.try_send(Err(format!(
                            "Earlier stop was not confirmed; retry quit after status recovers: {error}"
                        )));
                    }
                }
            }
            let notice = match &result {
                Ok(()) => format!("Session {} stop confirmed", pending.session.spelled()),
                Err(error) if pending.reply.is_some() => {
                    format!("Cannot stop session; terminal kept open: {error}")
                }
                // Only a stop that named its execution can learn that it ended.
                Err(error) if pending.fenced && error.contains(ENDED) => format!(
                    "Session {} had already finished; nothing was stopped",
                    pending.session.spelled()
                ),
                Err(error) => format!("Session {} stop failed: {error}", pending.session.spelled()),
            };
            if let Some(reply) = pending.reply {
                let _ = reply.try_send(result);
            }
            notices.push(notice);
        }
        if self.quit.as_ref().is_some_and(|quit| {
            !self
                .pending
                .iter()
                .any(|pending| pending.session == quit.request.session)
        }) {
            if let Some(quit) = self.quit.take() {
                // A resume stop may have admitted queued input before its reply.
                // Only this subsequent, non-resuming stop can confirm quit.
                if let Err(error) =
                    self.start(quit.peer, quit.request, Some(quit.reply.clone()), quit.wake)
                {
                    let error = error.to_string();
                    let _ = quit.reply.try_send(Err(error.clone()));
                    notices.push(format!("Cannot stop session; terminal kept open: {error}"));
                }
            }
        }
        (!notices.is_empty()).then(|| notices.join("; "))
    }
}

impl Request {
    fn new<C: Connection>(client: &Client<C>, command: Command) -> Result<Self, ClientError> {
        let selected = || client.selected().cloned().ok_or(ClientError::NoSelection);
        let (session, resume, observed) = match command {
            Command::InterruptSession(session) => (session, false, None),
            Command::Interrupt(aimed) => (selected()?, false, aimed),
            Command::InterruptAndResume(aimed) => (selected()?, true, aimed),
            _ => return Err(ClientError::InvalidInput("Not a session stop command")),
        };
        Ok(Self {
            session,
            resume,
            observed,
        })
    }
}

#[cfg(test)]
#[path = "interrupt_tests.rs"]
mod tests;
