//! Permission replies and Session opens run off the protocol loop, on the
//! read worker's command slot, one at a time. Commands that arrive behind
//! one wait for it in order, so none acts on a Session an open is leaving;
//! only a Quit is never held. Each command is fenced to the attachment it
//! was sent on: an answer from a revoked attachment is never applied.
use super::{reads::Reads, setup, Command, CommandId, Handoff};
use crate::bridge::Bridge;
use grapt_client::{
    contract::Operation,
    wire::{PermissionReplyChoice, SessionWireId},
    Client, ClientError, PermissionAsk, PreparedOpen, PreparedReply, TransportError, WireResponse,
};
use std::collections::VecDeque;
use std::sync::{atomic::AtomicBool, mpsc::Receiver, Arc};

/// The most commands read from the intake and not yet run.
const HELD: usize = 16;

#[derive(Debug)]
enum InFlight {
    Reply(PreparedReply),
    /// Boxed: an open carries the Session read across its steps.
    Open(CommandId, Box<PreparedOpen>),
}

/// Commands read from the intake and not yet run, oldest first. They still
/// count in the intake depth until they run.
#[derive(Debug, Default)]
struct Held(VecDeque<(CommandId, Command)>);

impl Held {
    /// Read the intake up to the bound, keeping only the newest of adjacent
    /// Selects. True when the bound stopped the read.
    fn fill(&mut self, commands: &Receiver<(CommandId, Command)>, updates: &Handoff) -> bool {
        while self.0.len() < HELD {
            let Ok(command) = commands.try_recv() else {
                return false;
            };
            if matches!(&command.1, Command::Select(_))
                && matches!(self.0.back(), Some((_, Command::Select(_))))
            {
                self.0.pop_back();
                updates.received();
            }
            self.0.push_back(command);
        }
        true
    }

    /// The commands to run now. While the lane is busy only a Quit runs;
    /// otherwise everything up to and including the next command for it.
    fn next(&mut self, busy: bool, updates: &Handoff) -> Vec<(CommandId, Command)> {
        let ready: Vec<_> = if busy {
            let mut quits = Vec::new();
            for held in std::mem::take(&mut self.0) {
                if matches!(held.1, Command::Quit(_)) {
                    quits.push(held);
                } else {
                    self.0.push_back(held);
                }
            }
            quits
        } else {
            let end = self
                .0
                .iter()
                .position(|(_, command)| matches!(command, Command::Reply(..) | Command::Select(_)))
                .map_or(self.0.len(), |first| first + 1);
            self.0.drain(..end).collect()
        };
        for _ in &ready {
            updates.received();
        }
        ready
    }
}

#[derive(Debug, Default)]
pub(super) struct Lane {
    /// The command in flight and the attachment it is fenced to: an open's
    /// first step pins it for every later step.
    in_flight: Option<(InFlight, Arc<AtomicBool>)>,
    held: Held,
}

/// What a settled command leaves for the loop.
#[derive(Debug, Default)]
pub(super) struct Answered {
    /// The Select a view now answers, opened or refused.
    pub(super) selection: Option<CommandId>,
    pub(super) status: Option<String>,
    pub(super) unavailable: Option<(CommandId, crate::sessions::Message)>,
}

impl Answered {
    /// Hand the outcome to the loop's holders; whether to publish.
    pub(super) fn apply(
        self,
        messages: &mut Vec<(CommandId, crate::sessions::Message)>,
        selection: &mut Option<CommandId>,
        status: &mut String,
    ) -> bool {
        messages.extend(self.unavailable);
        *selection = self.selection.or(selection.take());
        let Some(notice) = self.status else {
            return false;
        };
        *status = notice;
        true
    }
}

impl Lane {
    pub(super) fn busy(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Whether held commands can run now.
    pub(super) fn ready(&self) -> bool {
        !self.busy() && !self.held.0.is_empty()
    }

    /// Read the intake up to the bound. True when more may be waiting that
    /// could run now.
    pub(super) fn fill(
        &mut self,
        commands: &Receiver<(CommandId, Command)>,
        updates: &Handoff,
    ) -> bool {
        self.held.fill(commands, updates) && !self.busy()
    }

    /// The commands to run now, in order.
    pub(super) fn next(&mut self, updates: &Handoff) -> Vec<(CommandId, Command)> {
        self.held.next(self.busy(), updates)
    }

    pub(super) fn reply(
        &mut self,
        client: &mut Client<Bridge>,
        reads: &mut Reads,
        ask: &PermissionAsk,
        choice: PermissionReplyChoice,
    ) -> Result<(), ClientError> {
        let prepared = client.prepare_permission_reply(&ask.session_id, &ask.request_id, choice)?;
        let attachment = reads.command(client, prepared.request.clone())?;
        self.in_flight = Some((InFlight::Reply(prepared), attachment));
        Ok(())
    }

    pub(super) fn open(
        &mut self,
        (client, reads): (&mut Client<Bridge>, &mut Reads),
        directory: &mut Option<setup::Directory>,
        id: CommandId,
        session: &SessionWireId,
    ) -> Result<(), ClientError> {
        setup::leave_directory(client, directory);
        let prepared = client.prepare_open(session)?;
        let attachment = reads.command(client, prepared.request.clone())?;
        self.in_flight = Some((InFlight::Open(id, Box::new(prepared)), attachment));
        Ok(())
    }

    /// Apply the command's answer once it arrives.
    pub(super) fn settle(&mut self, client: &mut Client<Bridge>, reads: &mut Reads) -> Answered {
        reads
            .commanded()
            .map(|response| self.answer(client, reads, response))
            .unwrap_or_default()
    }

    /// An answer whose attachment was revoked is dropped whole; an open's
    /// next step is sent only while its pinned attachment holds.
    fn answer(
        &mut self,
        client: &mut Client<Bridge>,
        reads: &mut Reads,
        response: Result<WireResponse, TransportError>,
    ) -> Answered {
        let Some((in_flight, attachment)) = self.in_flight.take() else {
            return Answered::default();
        };
        let current = client.peer_mut().attachment_current(&attachment);
        match in_flight {
            InFlight::Reply(_) if !current => Answered {
                status: Some(
                    "Reply not confirmed: the service reconnected; answer the request again".into(),
                ),
                ..Answered::default()
            },
            InFlight::Reply(prepared) => Answered {
                status: client
                    .accept_permission_reply(prepared, response)
                    .err()
                    .map(|error| format!("Reply not confirmed: {error}")),
                ..Answered::default()
            },
            InFlight::Open(id, _) if !current => Answered {
                selection: Some(id),
                status: Some("Session not opened: the service reconnected; open it again".into()),
                unavailable: None,
            },
            InFlight::Open(id, prepared) => {
                let session = prepared.session().clone();
                let next = client.accept_open(*prepared, response).and_then(|next| {
                    next.map(|next| {
                        reads.command(client, next.request.clone())?;
                        let step = InFlight::Open(id, Box::new(next));
                        self.in_flight = Some((step, attachment));
                        Ok(())
                    })
                    .transpose()
                });
                match next {
                    Ok(Some(())) => Answered::default(),
                    Ok(None) => Answered {
                        selection: Some(id),
                        status: Some("Connected".into()),
                        unavailable: None,
                    },
                    Err(error) => Answered {
                        selection: Some(id),
                        unavailable:
                            matches!(&error, ClientError::Status { operation, status: 404 }
                            if *operation == Operation::SessionGet.operation_id())
                            .then(|| (id, crate::sessions::Message::Unavailable { id: session })),
                        status: Some(error.to_string()),
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
