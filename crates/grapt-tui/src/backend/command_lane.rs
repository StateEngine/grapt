//! Permission replies and Session opens run off the protocol loop, on the
//! read worker's command slot, one at a time. Commands that arrive behind
//! one wait for it in order, so none acts on a Session an open is leaving.
use super::{reads::Reads, setup, Command, CommandId};
use crate::bridge::Bridge;
use grapt_client::{
    contract::Operation,
    wire::{PermissionReplyChoice, SessionWireId},
    Client, ClientError, PermissionAsk, PreparedOpen, PreparedReply,
};
use std::collections::VecDeque;

#[derive(Debug)]
enum InFlight {
    Reply(PreparedReply),
    /// Boxed: an open carries the Session read across its steps.
    Open(CommandId, Box<PreparedOpen>),
}

#[derive(Debug, Default)]
pub(super) struct Lane {
    in_flight: Option<InFlight>,
    /// The rest of a command batch that arrived behind the one in flight.
    held: VecDeque<(CommandId, Command)>,
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
        !self.busy() && !self.held.is_empty()
    }

    /// Commands held behind an earlier one, oldest first, once it settled.
    pub(super) fn take_held(&mut self) -> Vec<(CommandId, Command)> {
        if self.busy() {
            return Vec::new();
        }
        self.held.drain(..).collect()
    }

    /// The commands to run now: up to and including the first that goes to
    /// the lane. The rest wait for it.
    pub(super) fn admit(
        &mut self,
        mut batch: Vec<(CommandId, Command)>,
    ) -> Vec<(CommandId, Command)> {
        let first = batch
            .iter()
            .position(|(_, command)| matches!(command, Command::Reply(..) | Command::Select(_)));
        if let Some(first) = first {
            self.held.extend(batch.split_off(first + 1));
        }
        batch
    }

    pub(super) fn reply(
        &mut self,
        client: &mut Client<Bridge>,
        reads: &mut Reads,
        ask: &PermissionAsk,
        choice: PermissionReplyChoice,
    ) -> Result<(), ClientError> {
        let prepared = client.prepare_permission_reply(&ask.session_id, &ask.request_id, choice)?;
        reads.command(client, prepared.request.clone())?;
        self.in_flight = Some(InFlight::Reply(prepared));
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
        reads.command(client, prepared.request.clone())?;
        self.in_flight = Some(InFlight::Open(id, Box::new(prepared)));
        Ok(())
    }

    /// Apply the command's answer once it arrives; an open sends its next
    /// step until the Session is installed.
    pub(super) fn settle(&mut self, client: &mut Client<Bridge>, reads: &mut Reads) -> Answered {
        let Some(response) = reads.commanded() else {
            return Answered::default();
        };
        match self.in_flight.take() {
            None => Answered::default(),
            Some(InFlight::Reply(prepared)) => Answered {
                status: client
                    .accept_permission_reply(prepared, response)
                    .err()
                    .map(|error| format!("Reply not confirmed: {error}")),
                ..Answered::default()
            },
            Some(InFlight::Open(id, prepared)) => {
                let session = prepared.session().clone();
                let next = client.accept_open(*prepared, response).and_then(|next| {
                    next.map(|next| {
                        reads.command(client, next.request.clone())?;
                        self.in_flight = Some(InFlight::Open(id, Box::new(next)));
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
