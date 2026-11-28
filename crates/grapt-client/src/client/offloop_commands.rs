//! Commands a host may send off its state loop: a permission reply and a
//! Session open. Each runs as prepared steps (see [`super::prepared`]). An
//! open takes several requests; the current Session stays selected until
//! all of them have validated, and installing the new one starts no request.
use super::prepared::{Accepted, Done, Open, OpenStep, Opened, PreparedCall, Reply, Route, Step};
use super::*;
use crate::wire::{ModelRow, PermissionReplyChoice};

impl<C: Connection> Client<C> {
    /// Build a permission reply. Nothing changes until its answer is accepted.
    pub fn prepare_permission_reply(
        &mut self,
        session: &SessionWireId,
        ask: &PermissionWireId,
        choice: PermissionReplyChoice,
    ) -> Result<PreparedCall, ClientError> {
        let body = crate::wire::encode_permission_reply(ask, choice)?;
        let step = Step::Reply(Reply {
            session: session.clone(),
            ask: ask.clone(),
        });
        let route = Route {
            request: Some(ask),
            body: Some(body),
            ..Route::new(Operation::SessionPermissionReply, Some(session))
        };
        self.prepare_call(step, route)
    }

    /// An accepted reply settles its ask, fences older permission reads, and
    /// asks the host to re-read the open asks.
    pub(super) fn accept_reply(
        &mut self,
        reply: Reply,
        answer: Result<Vec<u8>, ClientError>,
    ) -> Result<Accepted, ClientError> {
        answer?;
        self.permissions
            .retain(|ask| !(ask.session_id == reply.session && ask.request_id == reply.ask));
        self.permissions_stale = true;
        Ok(Accepted::Done(Done::Applied))
    }

    /// Build the first request of opening `session`.
    pub fn prepare_open(&mut self, session: &SessionWireId) -> Result<PreparedCall, ClientError> {
        self.open_step(OpenStep::Session, session, None, None)
    }

    /// Apply one step's answer: the next step, or `Done` once the Session is
    /// installed. A Session the service does not have is forgotten; one whose
    /// saved project no longer matches is refused.
    pub(super) fn accept_open(
        &mut self,
        mut open: Open,
        answer: Result<Vec<u8>, ClientError>,
    ) -> Result<Accepted, ClientError> {
        if open.step == OpenStep::Session
            && matches!(&answer, Err(ClientError::Status { status: 404, .. }))
        {
            self.forget_session(&open.session);
        }
        let bytes = answer?;
        let session = open.session.clone();
        let next = match (open.step, open.opened.take()) {
            (OpenStep::Session, _) => {
                let saved = crate::wire::decode_session_attachment(&bytes)?;
                if saved.id != session {
                    return Err(ClientError::NotOffered {
                        what: "requested session",
                    });
                }
                let opened = Opened {
                    directory: saved.project.worktree.clone(),
                    saved,
                    row: bytes,
                };
                // A saved Session keeps its working directory, which may be
                // nested inside its repository: compare resolved checkout roots.
                let resolved = self.launch_project.as_ref().is_some_and(|project| {
                    project.id == opened.saved.project.id
                        && project.worktree == opened.saved.project.worktree
                });
                if resolved {
                    self.open_step(OpenStep::Subagents, &session, None, Some(opened))?
                } else {
                    let worktree = opened.saved.project.worktree.clone();
                    self.open_step(OpenStep::Project, &session, Some(&worktree), Some(opened))?
                }
            }
            (OpenStep::Project, Some(mut opened)) => {
                let (id, worktree) = crate::wire::decode_project_current(&bytes)?;
                if id != opened.saved.project.id {
                    return Err(ClientError::NotOffered {
                        what: "session in saved project",
                    });
                }
                opened.saved.project.worktree = worktree;
                self.open_step(OpenStep::Subagents, &session, None, Some(opened))?
            }
            (OpenStep::Subagents, Some(opened)) => {
                let subagents = crate::wire::decode_subagent_model(&bytes)?;
                // As in an interactive resume, a launch effort bound to a
                // Session is not reapplied.
                let effort = self
                    .launch_effort
                    .take()
                    .filter(|effort| effort.session.is_none());
                let installed = self.install_open(&session, opened, subagents);
                self.launch_effort = effort;
                return installed.map(|()| Accepted::Done(Done::Applied));
            }
            (OpenStep::Project | OpenStep::Subagents, None) => {
                return Err(ClientError::NotOffered {
                    what: "requested session",
                })
            }
        };
        Ok(Accepted::Next(next))
    }

    fn open_step(
        &mut self,
        step: OpenStep,
        session: &SessionWireId,
        location: Option<&str>,
        opened: Option<Opened>,
    ) -> Result<PreparedCall, ClientError> {
        let open = Open {
            step,
            session: session.clone(),
            opened,
        };
        let route = Route {
            location,
            ..Route::new(step.operation(), Some(session))
        };
        self.prepare_call(Step::Open(Box::new(open)), route)
    }

    /// Install a validated open without starting a request: the snapshot
    /// goes to the recovery read, the forms to the host's form read.
    fn install_open(
        &mut self,
        session: &SessionWireId,
        opened: Opened,
        subagents: Option<ModelRow>,
    ) -> Result<(), ClientError> {
        let choice = self.adopt_saved(session, opened.saved, &opened.row, opened.directory)?;
        if let Err(error) = self.enter_session(session) {
            self.model = None;
            return Err(error);
        }
        self.request_history();
        if self.rewind_uncertain.as_ref() == Some(session) {
            self.rewind_uncertain = None;
        }
        self.subagent_model = Some(subagents);
        self.restore_saved_model(choice);
        Ok(())
    }
}
