//! Commands a host may send off its state loop: a permission reply and a
//! Session open. The client builds each request from its state, the host
//! sends it on another connection, and the client applies the answer here.
//! An open takes several requests; the current Session stays selected until
//! all of them have validated, and installing the new one starts no request.
use super::*;
use crate::wire::{ModelRow, PermissionReplyChoice, SessionAttachment};

/// A permission reply built from the client's state.
#[derive(Debug)]
pub struct PreparedReply {
    pub request: WireRequest,
    session: SessionWireId,
    ask: PermissionWireId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Session,
    Project,
    Subagents,
}

impl Step {
    fn operation(self) -> Operation {
        match self {
            Self::Session => Operation::SessionGet,
            Self::Project => Operation::ProjectCurrent,
            Self::Subagents => Operation::SessionSubagentModelGet,
        }
    }
}

/// The Session read, validated: its saved choices, its row and the working
/// directory its catalog is bound to.
#[derive(Debug)]
struct Opened {
    saved: SessionAttachment,
    row: Vec<u8>,
    directory: String,
}

/// One step of a Session open. Send its request, then hand the answer to
/// [`Client::accept_open`].
#[derive(Debug)]
pub struct PreparedOpen {
    pub request: WireRequest,
    step: Step,
    session: SessionWireId,
    opened: Option<Opened>,
}

impl PreparedOpen {
    pub fn session(&self) -> &SessionWireId {
        &self.session
    }
}

impl<C: Connection> Client<C> {
    /// Build a permission reply. Nothing changes until its answer is accepted.
    pub fn prepare_permission_reply(
        &mut self,
        session: &SessionWireId,
        ask: &PermissionWireId,
        choice: PermissionReplyChoice,
    ) -> Result<PreparedReply, ClientError> {
        self.refused()?;
        let body = crate::wire::encode_permission_reply(ask, choice)?;
        let request = self.wire(
            Operation::SessionPermissionReply,
            Some(session),
            Some(ask),
            Some(body),
            None,
            None,
        )?;
        // Host-started I/O never coexists with a pending resume.
        self.abandon_resume();
        Ok(PreparedReply {
            request,
            session: session.clone(),
            ask: ask.clone(),
        })
    }

    /// Apply a reply's answer. An accepted reply settles its ask, fences
    /// older permission reads, and asks the host to re-read the open asks.
    pub fn accept_permission_reply(
        &mut self,
        prepared: PreparedReply,
        response: Result<WireResponse, TransportError>,
    ) -> Result<(), ClientError> {
        self.accept_response(Operation::SessionPermissionReply, response?)?;
        self.acknowledged = self.acknowledged.wrapping_add(1);
        self.permissions
            .retain(|ask| !(ask.session_id == prepared.session && ask.request_id == prepared.ask));
        self.permissions_stale = true;
        Ok(())
    }

    /// Build the first request of opening `session`.
    pub fn prepare_open(&mut self, session: &SessionWireId) -> Result<PreparedOpen, ClientError> {
        self.refused()?;
        let request = self.wire(Operation::SessionGet, Some(session), None, None, None, None)?;
        self.abandon_resume();
        Ok(PreparedOpen {
            request,
            step: Step::Session,
            session: session.clone(),
            opened: None,
        })
    }

    /// Apply one step's answer: the next step, or `None` once the Session is
    /// installed. A Session the service does not have is forgotten; one whose
    /// saved project no longer matches is refused.
    pub fn accept_open(
        &mut self,
        mut prepared: PreparedOpen,
        response: Result<WireResponse, TransportError>,
    ) -> Result<Option<PreparedOpen>, ClientError> {
        self.refused()?;
        let operation = prepared.step.operation();
        let accepted = response
            .map_err(ClientError::from)
            .and_then(|response| self.accept_response(operation, response));
        if prepared.step == Step::Session
            && matches!(&accepted, Err(ClientError::Status { status: 404, .. }))
        {
            self.forget_session(&prepared.session);
        }
        let bytes = accepted?;
        let session = prepared.session.clone();
        match (prepared.step, prepared.opened.take()) {
            (Step::Session, _) => {
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
                let next = if resolved {
                    self.open_step(Step::Subagents, &session, None)?
                } else {
                    let worktree = opened.saved.project.worktree.clone();
                    self.open_step(Step::Project, &session, Some(&worktree))?
                };
                Ok(Some(PreparedOpen {
                    opened: Some(opened),
                    ..next
                }))
            }
            (Step::Project, Some(mut opened)) => {
                let (id, worktree) = crate::wire::decode_project_current(&bytes)?;
                if id != opened.saved.project.id {
                    return Err(ClientError::NotOffered {
                        what: "session in saved project",
                    });
                }
                opened.saved.project.worktree = worktree;
                let next = self.open_step(Step::Subagents, &session, None)?;
                Ok(Some(PreparedOpen {
                    opened: Some(opened),
                    ..next
                }))
            }
            (Step::Subagents, Some(opened)) => {
                let subagents = crate::wire::decode_subagent_model(&bytes)?;
                // As in an interactive resume, a launch effort bound to a
                // Session is not reapplied.
                let effort = self
                    .launch_effort
                    .take()
                    .filter(|effort| effort.session.is_none());
                let installed = self.install_open(&session, opened, subagents);
                self.launch_effort = effort;
                installed.map(|()| None)
            }
            (Step::Project | Step::Subagents, None) => Err(ClientError::NotOffered {
                what: "requested session",
            }),
        }
    }

    fn open_step(
        &self,
        step: Step,
        session: &SessionWireId,
        location: Option<&str>,
    ) -> Result<PreparedOpen, ClientError> {
        let request = self.wire(step.operation(), Some(session), None, None, location, None)?;
        Ok(PreparedOpen {
            request,
            step,
            session: session.clone(),
            opened: None,
        })
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
