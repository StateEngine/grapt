//! Commands that reach the service run off the protocol loop, on the read
//! worker's command slot, one at a time and a step at a time. Commands that
//! arrive behind one wait for it in order, so none acts on state an earlier
//! one is still changing. A Quit arrives apart from them and is never held;
//! every command still waiting when it arrives is discarded, so none takes
//! effect after it. Each command is fenced to the attachment its first step
//! was sent on: an answer from a revoked attachment is never applied.
use super::{
    closing::Holders, commands, forms, reads::Reads, sessions, setup, step::Turn, Command,
    CommandId, Handoff, Outcome,
};
use crate::bridge::Bridge;
use grapt_client::{
    client::HistoryMove,
    contract::Operation,
    wire::{AgentRow, ModelRow, SessionWireId},
    Accepted, Client, ClientError, Done, PreparedCall, TransportError, WireResponse,
};
use std::collections::VecDeque;
use std::sync::{atomic::AtomicBool, mpsc::Receiver, Arc};

/// The most commands read from the intake and not yet run, and the most
/// read in one pass.
const HELD: usize = 16;

/// Whether `command` runs on the lane.
fn runs_on_lane(command: &Command) -> bool {
    matches!(
        command,
        Command::Reply(..)
            | Command::Select(_)
            | Command::Browse(_)
            | Command::ChooseAgent(_)
            | Command::Model(_)
            | Command::SessionSearch(_)
            | Command::ManageSession(_)
            | Command::Form(_)
            | Command::RevokeGrant(_)
    )
}

/// A command on the lane, and where its steps settle.
#[derive(Debug)]
enum Job {
    Reply,
    Open {
        id: CommandId,
        session: SessionWireId,
    },
    Browse(CommandId, HistoryMove),
    Agent,
    Model(CommandId),
    Search(CommandId, String),
    Manage(CommandId, sessions::Manage),
    Form(CommandId, Box<forms::Flow>),
    Grant(CommandId, commands::Withdrawal),
}

/// The step in flight, and what the command's later steps are held to.
#[derive(Debug)]
struct InFlight {
    job: Job,
    call: PreparedCall,
    fence: Fence,
}

/// What a command's later steps are held to.
#[derive(Debug, Default)]
struct Fence {
    /// The attachment its first step was sent on: every later answer must
    /// come back on it.
    attachment: Option<Arc<AtomicBool>>,
    /// That attachment was revoked: no later step is sent, and each fails.
    revoked: bool,
    /// A quit was handed out while it ran: it sends no further write.
    closing: bool,
}

/// Commands read from the intake and not yet run, oldest first. They still
/// count in the intake depth until they run.
#[derive(Debug, Default)]
struct Held(VecDeque<(CommandId, Command)>);

impl Held {
    /// Read at most `HELD` commands from the intake, up to the bound, keeping
    /// only the newest of adjacent Selects. True when a limit stopped the read.
    fn fill(&mut self, commands: &Receiver<(CommandId, Command)>, updates: &Handoff) -> bool {
        for _ in 0..HELD {
            if self.0.len() >= HELD {
                return true;
            }
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

    /// The commands to run now: none while the lane is busy, otherwise
    /// everything up to and including the next command for it.
    fn next(&mut self, busy: bool, updates: &Handoff) -> Vec<(CommandId, Command)> {
        if busy {
            return Vec::new();
        }
        let end = self
            .0
            .iter()
            .position(|(_, command)| runs_on_lane(command))
            .map_or(self.0.len(), |first| first + 1);
        let ready: Vec<_> = self.0.drain(..end).collect();
        for _ in &ready {
            updates.received();
        }
        ready
    }

    /// Discard every command held or still in the intake: all were sent
    /// before the quit, because the UI takes no input while it quits.
    fn discard(
        &mut self,
        commands: &Receiver<(CommandId, Command)>,
        updates: &Handoff,
    ) -> Vec<(CommandId, Command)> {
        let discarded: Vec<_> = self.0.drain(..).chain(commands.try_iter()).collect();
        for _ in &discarded {
            updates.received();
        }
        discarded
    }
}

#[derive(Debug, Default)]
pub(super) struct Lane {
    in_flight: Option<InFlight>,
    held: Held,
    settled: Settled,
    /// What a quit discarded, told with the next settlement.
    notice: Option<String>,
    /// Whether the last commands handed out were quits.
    quitting: bool,
    /// A managed Session left none selected.
    unselected: bool,
}

/// What settled commands leave for the loop, handed over once a pass.
#[derive(Debug, Default)]
struct Settled {
    outcomes: Vec<(CommandId, Outcome)>,
    moves: Vec<(CommandId, HistoryMove, bool)>,
    /// The newest Select a view now answers, opened or refused.
    selection: Option<CommandId>,
    roster: Option<Vec<AgentRow>>,
    models: Option<Vec<ModelRow>>,
    status: Option<String>,
}

impl Lane {
    pub(super) fn busy(&self) -> bool {
        self.in_flight.is_some()
    }

    /// The page move in flight, which settles when its answer arrives.
    pub(super) fn paging(&self) -> Option<CommandId> {
        match self.in_flight.as_ref().map(|in_flight| &in_flight.job) {
            Some(Job::Browse(id, _)) => Some(*id),
            _ => None,
        }
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

    /// Whether the last commands handed out were quits.
    pub(super) fn quitting(&self) -> bool {
        self.quitting
    }

    /// The commands to run now, in order. A quit runs at once, alone, and
    /// discards every command still waiting, each settled as refused.
    pub(super) fn next(
        &mut self,
        commands: &Receiver<(CommandId, Command)>,
        updates: &Handoff,
        (client, holders): (&Client<Bridge>, Holders<'_>),
    ) -> Vec<(CommandId, Command)> {
        let quits = updates.take_quits();
        self.quitting = !quits.is_empty();
        if quits.is_empty() {
            return self.held.next(self.busy(), updates);
        }
        if let Some(in_flight) = &mut self.in_flight {
            in_flight.fence.closing = true;
        }
        let discarded = self.held.discard(commands, updates);
        if !discarded.is_empty() {
            let count = discarded.len();
            self.notice = Some(format!("Closing: {count} queued commands were not sent"));
            super::closing::refuse(discarded, client, holders);
        }
        quits
            .into_iter()
            .map(|reply| updates.envelope(Command::Quit(reply)))
            .collect()
    }

    /// Start a command that runs on the lane.
    pub(super) fn start(
        &mut self,
        (client, reads): (&mut Client<Bridge>, &mut Reads),
        id: CommandId,
        command: Command,
    ) {
        let (job, first) = match command {
            Command::Reply(ask, choice) => {
                let call =
                    client.prepare_permission_reply(&ask.session_id, &ask.request_id, choice);
                (Job::Reply, call.map(Accepted::Next))
            }
            Command::Browse(movement) => {
                (Job::Browse(id, movement), client.prepare_browse(movement))
            }
            Command::ChooseAgent(agent) => (Job::Agent, client.prepare_agent(&agent)),
            Command::Model(model) => (Job::Model(id), client.prepare_model(&model)),
            Command::SessionSearch(query) => {
                let call = client.prepare_session_search(&query).map(Accepted::Next);
                (Job::Search(id, query), call)
            }
            Command::ManageSession(request) => match sessions::manage(client, request) {
                Turn::Step(manage, call) => (Job::Manage(id, manage), call),
                Turn::Settle(message) => return self.outcome(id, Outcome::Session(message)),
            },
            Command::Form(request) => match forms::begin(client, request) {
                Turn::Step(flow, call) => (Job::Form(id, Box::new(flow)), call),
                Turn::Settle(reply) => return self.form(id, reply),
            },
            Command::RevokeGrant(request) => match commands::revoke_grant(client, request) {
                Turn::Step(withdrawal, call) => (Job::Grant(id, withdrawal), call),
                Turn::Settle(done) => return self.completion(id, done),
            },
            // The protocol loop hands the lane only the commands above.
            _ => return,
        };
        self.drive((client, reads), job, first, Fence::default());
    }

    pub(super) fn open(
        &mut self,
        (client, reads): (&mut Client<Bridge>, &mut Reads),
        directory: &mut Option<super::setup::Directory>,
        id: CommandId,
        session: &SessionWireId,
    ) {
        setup::leave_directory(client, directory);
        let first = client.prepare_open(session).map(Accepted::Next);
        let job = Job::Open {
            id,
            session: session.clone(),
        };
        self.drive((client, reads), job, first, Fence::default());
    }

    /// Apply the step's answer once it arrives. An answer whose attachment
    /// was revoked is never applied; a later step is sent only while the
    /// command's pinned attachment holds.
    pub(super) fn settle(&mut self, client: &mut Client<Bridge>, reads: &mut Reads) {
        if let Some(response) = reads.commanded() {
            self.answer((client, reads), response);
        }
    }

    fn answer(
        &mut self,
        (client, reads): (&mut Client<Bridge>, &mut Reads),
        response: Result<WireResponse, TransportError>,
    ) {
        let Some(InFlight { job, call, fence }) = self.in_flight.take() else {
            return;
        };
        let bridge = client.peer_mut();
        let current = fence
            .attachment
            .as_ref()
            .is_some_and(|attachment| bridge.attachment_current(attachment));
        if current {
            // An answer on the live attachment counts toward its read health,
            // as a synchronous one does; a revoked one speaks for nothing.
            bridge.read_health.call(&call.request, &response);
            let next = client.accept(call, response);
            self.drive((client, reads), job, next, fence);
        } else {
            self.revoked((client, reads), job, fence);
        }
    }

    /// Whether a managed Session left none selected since the last ask.
    pub(super) fn left_unselected(&mut self) -> bool {
        std::mem::take(&mut self.unselected)
    }

    /// Hand what settled to the loop's holders; whether to publish.
    pub(super) fn apply(&mut self, holders: Holders<'_>) -> bool {
        let settled = std::mem::take(&mut self.settled);
        let Holders {
            ledger,
            selection,
            pages,
            roster,
            models,
            status,
        } = holders;
        let mut publish = !settled.outcomes.is_empty()
            || !settled.moves.is_empty()
            || settled.selection.is_some();
        for (id, outcome) in settled.outcomes {
            ledger.settle(id, outcome);
        }
        for (id, movement, changed) in settled.moves {
            pages.moved(id, movement, changed);
        }
        // An older open settling never replaces a newer acknowledgement.
        *selection = selection.take().max(settled.selection);
        if settled.roster.is_some() {
            *roster = settled.roster;
            publish = true;
        }
        if settled.models.is_some() {
            *models = settled.models;
            publish = true;
        }
        if let Some(notice) = settled.status.or_else(|| self.notice.take()) {
            *status = notice;
            publish = true;
        }
        publish
    }

    /// Run a command's steps until one is in flight or it has settled. A
    /// command whose attachment was revoked sends nothing more, and one a
    /// quit overtook sends no further write: each such step fails unsent.
    fn drive(
        &mut self,
        (client, reads): (&mut Client<Bridge>, &mut Reads),
        mut job: Job,
        mut next: Result<Accepted, ClientError>,
        mut fence: Fence,
    ) {
        loop {
            let done = match next {
                Ok(Accepted::Next(_)) if fence.revoked => Err(reconnected()),
                Ok(Accepted::Next(call)) if fence.closing && call.writes() => {
                    if let Job::Form(id, flow) = job {
                        return self.form(id, flow.closed());
                    }
                    Err(ClientError::InvalidInput("Closing; not sent"))
                }
                Ok(Accepted::Next(call)) => match reads.command(client, call.request.clone()) {
                    Ok(sent_on) => {
                        fence.attachment.get_or_insert(sent_on);
                        self.in_flight = Some(InFlight { job, call, fence });
                        return;
                    }
                    Err(error) => Err(error),
                },
                Ok(Accepted::Done(done)) => Ok(done),
                Err(error) => Err(error),
            };
            match self.advance(client, job, done) {
                Some((step, after)) => (job, next) = (step, after),
                None => return,
            }
        }
    }

    /// A step's answer arrived on a revoked attachment. A reply or an open is
    /// asked for again; any other command settles, or goes on, as failed.
    fn revoked(&mut self, lanes: (&mut Client<Bridge>, &mut Reads), job: Job, fence: Fence) {
        match job {
            Job::Reply => {
                self.settled.status = Some(
                    "Reply not confirmed: the service reconnected; answer the request again".into(),
                );
            }
            Job::Open { id, .. } => {
                self.select(id);
                self.settled.status =
                    Some("Session not opened: the service reconnected; open it again".into());
            }
            job => {
                let fence = Fence {
                    revoked: true,
                    ..fence
                };
                self.drive(lanes, job, Err(reconnected()), fence);
            }
        }
    }

    /// Settle a step's result into the command's holder, or return its next
    /// step.
    fn advance(
        &mut self,
        client: &mut Client<Bridge>,
        job: Job,
        done: Result<Done, ClientError>,
    ) -> Option<(Job, Result<Accepted, ClientError>)> {
        match job {
            Job::Reply => {
                if let Err(error) = done {
                    self.settled.status = Some(format!("Reply not confirmed: {error}"));
                }
            }
            Job::Open { id, session } => {
                self.select(id);
                let not_found = matches!(&done, Err(ClientError::Status { operation, status: 404 })
                    if *operation == Operation::SessionGet.operation_id());
                if not_found {
                    let message = crate::sessions::Message::Unavailable { id: session };
                    self.settled.outcomes.push((id, Outcome::Session(message)));
                }
                self.settled.status = Some(match done {
                    Ok(_) => "Connected".into(),
                    Err(error) => error.to_string(),
                });
            }
            Job::Browse(id, movement) => {
                let changed = matches!(done, Ok(Done::Browsed(true)));
                self.settled.moves.push((id, movement, changed));
                self.status(client, &done);
            }
            Job::Agent => {
                match &done {
                    Ok(_) => self.settled.models = Some(client.models().to_vec()),
                    Err(_) => self.settled.roster = setup::roster(client),
                }
                self.status(client, &done);
            }
            Job::Model(id) => {
                self.status(client, &done);
                self.outcome(id, Outcome::Model(done.is_ok()));
            }
            Job::Search(id, query) => {
                self.settled.status = Some("Connected".into());
                let message = sessions::searched(client, query, done);
                self.outcome(id, Outcome::Session(message));
            }
            Job::Manage(id, manage) => match manage.advance(client, done) {
                Turn::Step(manage, next) => return Some((Job::Manage(id, manage), next)),
                Turn::Settle(message) => {
                    self.unselected |= client.selected().is_none();
                    self.settled.status = Some("Connected".into());
                    self.outcome(id, Outcome::Session(message));
                }
            },
            Job::Form(id, flow) => match flow.advance(client, done) {
                Turn::Step(flow, next) => return Some((Job::Form(id, Box::new(flow)), next)),
                Turn::Settle(reply) => self.form(id, reply),
            },
            Job::Grant(id, withdrawal) => match withdrawal.advance(client, done) {
                Turn::Step(withdrawal, next) => return Some((Job::Grant(id, withdrawal), next)),
                Turn::Settle(done) => self.completion(id, done),
            },
        }
        None
    }

    fn select(&mut self, id: CommandId) {
        self.settled.selection = self.settled.selection.max(Some(id));
    }

    /// The status a settled command leaves: a transient failure is the read
    /// health's to report.
    fn status(&mut self, client: &mut Client<Bridge>, done: &Result<Done, ClientError>) {
        self.settled.status = Some(match done {
            Ok(_) => "Connected".into(),
            Err(error) if client.peer_mut().read_health.transient(error) => "Connected".into(),
            Err(error) => error.to_string(),
        });
    }

    fn outcome(&mut self, id: CommandId, outcome: Outcome) {
        self.settled.outcomes.push((id, outcome));
    }

    fn form(&mut self, id: CommandId, reply: grapt_tui_forms::Reply) {
        self.settled.status = Some(reply.note.clone());
        self.outcome(id, Outcome::Form(reply));
    }

    fn completion(&mut self, id: CommandId, done: commands::Completion) {
        self.settled.status = Some(done.notice.clone());
        self.outcome(id, Outcome::Command(done));
    }
}

/// The failure every step of a command on a revoked attachment reports.
fn reconnected() -> ClientError {
    TransportError::Unavailable {
        reason: "the service reconnected before it answered".into(),
    }
    .into()
}

#[cfg(test)]
mod tests;
