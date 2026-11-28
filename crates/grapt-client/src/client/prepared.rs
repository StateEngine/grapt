//! Commands a host may run off its state loop, one step at a time. The
//! client builds each step's request from its state; the host sends it on
//! another connection and hands the answer to [`Client::accept`], which
//! applies it and returns the command's next step or what it settled.
//! Nothing a step would change is applied before its answer is accepted.
use super::*;

/// One step of a command: send `request`, then hand its answer to
/// [`Client::accept`].
#[derive(Debug)]
pub struct PreparedCall {
    pub request: WireRequest,
    operation: Operation,
    step: Step,
}

/// What a step's answer is applied to.
#[derive(Debug)]
pub(super) enum Step {
    Reply(offloop_commands::Reply),
    Open(Box<offloop_commands::Open>),
    Browse(paged_history::Browse),
    Agent(crate::wire::AgentWireId),
    Model(Box<crate::wire::ModelRow>),
    Search,
    Rename,
    Delete(SessionWireId),
    Sessions,
    Form(offloop_forms::Form),
    Revoke(grants::Revoke),
    Grants(SessionWireId),
}

/// A command's next step, or what it settled.
#[derive(Debug)]
pub enum Accepted {
    Next(PreparedCall),
    Done(Done),
}

/// What a settled command leaves.
#[derive(Debug)]
pub enum Done {
    /// It took effect.
    Applied,
    /// A page move, and whether the page changed.
    Browsed(bool),
    Found(SessionSearch),
    FormState(crate::wire::FormState),
    Form(crate::wire::FormRow),
    /// The service's last word on a revocation.
    Revoked(String),
}

/// How a step addresses the service.
pub(super) struct Route<'a> {
    pub operation: Operation,
    pub session: Option<&'a SessionWireId>,
    pub request: Option<&'a dyn crate::wire::Spelled>,
    pub body: Option<Vec<u8>>,
    pub location: Option<&'a str>,
    pub page: Option<Page<'a>>,
}

impl<'a> Route<'a> {
    pub fn new(operation: Operation, session: Option<&'a SessionWireId>) -> Self {
        Self {
            operation,
            session,
            request: None,
            body: None,
            location: None,
            page: None,
        }
    }
}

impl<C: Connection> Client<C> {
    /// Build one step from the client's state. Host-started I/O, like
    /// `ingest`: it never coexists with a pending resume.
    pub(super) fn prepare_call(
        &mut self,
        step: Step,
        route: Route<'_>,
    ) -> Result<PreparedCall, ClientError> {
        self.refused()?;
        let Route {
            operation,
            session,
            request,
            body,
            location,
            page,
        } = route;
        let request = self.wire(operation, session, request, body, location, page)?;
        self.abandon_resume();
        Ok(PreparedCall {
            request,
            operation,
            step,
        })
    }

    /// Apply a step's answer: the command's next step, or what it settled.
    /// An answer from a service refused since the step was built is not
    /// applied.
    pub fn accept(
        &mut self,
        call: PreparedCall,
        response: Result<WireResponse, TransportError>,
    ) -> Result<Accepted, ClientError> {
        self.refused()?;
        let PreparedCall {
            operation, step, ..
        } = call;
        let answer = response
            .map_err(ClientError::from)
            .and_then(|response| self.accept_response(operation, response));
        if answer.is_ok() && offloop::acknowledges(operation) {
            self.acknowledged = self.acknowledged.wrapping_add(1);
        }
        match step {
            Step::Reply(reply) => self.accept_reply(reply, answer),
            Step::Open(open) => self.accept_open(*open, answer),
            Step::Browse(browse) => self.accept_browse(browse, answer),
            Step::Agent(agent) => answer.map(|_| self.chose_agent(agent)),
            Step::Model(model) => answer.map(|_| self.chose_model(*model)),
            Step::Search => self.accept_search(answer),
            Step::Rename => answer.map(|_| Accepted::Done(Done::Applied)),
            Step::Delete(session) => answer.map(|_| {
                self.forget_session(&session);
                Accepted::Done(Done::Applied)
            }),
            Step::Sessions => self
                .accept_sessions(&answer?)
                .map(|()| Accepted::Done(Done::Applied)),
            Step::Form(form) => self.accept_form(form, answer),
            Step::Revoke(revoke) => self.accept_revoke(revoke, answer),
            Step::Grants(session) => Ok(self.accept_grants(session, answer)),
        }
    }
}
