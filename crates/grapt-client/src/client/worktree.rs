//! One-shot checkout effects share the independent, finite operator request lane.
use super::*;
use crate::wire::{FormLocation, WorktreeEffect, WorktreeRequest};

#[derive(Debug)]
struct Pending {
    session: Option<SessionWireId>,
    location: FormLocation,
    project_directory: String,
    operation: Operation,
    management: Option<WorktreeRequest>,
}

#[derive(Debug, Default)]
pub(super) struct State {
    pending: Option<Pending>,
    failed: Option<Pending>,
    pub(super) attachment: Option<worktree_attachment::Attachment>,
    created: Option<SessionWireId>,
    uncertain: bool,
    pub(super) creation_uncertain: bool,
}

impl<C: Connection> Client<C> {
    pub fn worktree_pending(&self) -> bool {
        self.worktree.pending.is_some()
    }

    /// Known creation identity survives failed/canceled read-only attachment.
    pub fn worktree_uncertainty(&self) -> String {
        let warning = crate::wire::worktree_uncertainty(self.worktree.created.as_ref());
        let context = self.worktree_request_details();
        if context.is_empty() {
            warning
        } else {
            format!("{warning} {}", context.join("; "))
        }
    }

    /// Original backend coordinates for readable failure/cancellation output.
    pub fn worktree_request_details(&self) -> Vec<String> {
        self.worktree
            .pending
            .as_ref()
            .or(self.worktree.failed.as_ref())
            .and_then(|pending| {
                pending.management.as_ref().map(|request| {
                    crate::wire::worktree_request_context(&pending.location, request)
                })
            })
            .unwrap_or_default()
    }

    pub fn begin_worktree(
        &mut self,
        request: WorktreeRequest,
    ) -> Result<Option<Vec<String>>, ClientError> {
        let body = crate::wire::encode_worktree(&request)?;
        self.begin_worktree_request(Operation::WorkspaceWorktree, body, Some(request))
    }

    pub(super) fn begin_worktree_request(
        &mut self,
        operation: Operation,
        body: Vec<u8>,
        management: Option<WorktreeRequest>,
    ) -> Result<Option<Vec<String>>, ClientError> {
        // Host-started I/O never coexists with a pending resume.
        self.abandon_resume();
        if self.worktree.uncertain {
            return Err(ClientError::Worktree(self.worktree_uncertainty()));
        }
        if self.operator_request_pending() {
            return Err(ClientError::InvalidInput(
                "An operator request is already in progress; draft kept.",
            ));
        }
        if self.has_unconfirmed_prompt() {
            return Err(ClientError::InvalidInput(
                "Resolve the unconfirmed prompt before changing checkouts; draft kept.",
            ));
        }
        let location = self.form_location().ok_or(ClientError::NoProject)?;
        let project_directory = self
            .project
            .as_ref()
            .ok_or(ClientError::NoProject)?
            .worktree
            .clone();
        let mut wire = WireRequest::new(operation, None, None)?.with_body(body);
        if operation == Operation::WorkspaceWorktree {
            wire = crate::wire::with_worktree_location(
                wire,
                &location.directory,
                self.selected.as_ref(),
            );
        }
        self.worktree.pending = Some(Pending {
            session: self.selected.clone(),
            location,
            project_directory,
            operation,
            management,
        });
        if operation == Operation::SessionCreate {
            self.worktree.creation_uncertain = true;
        }
        let result = self
            .connection
            .begin_operator(&wire)
            .map_err(ClientError::from);
        self.worktree_result(result)
    }

    pub fn poll_worktree(&mut self) -> Result<Option<Vec<String>>, ClientError> {
        if !self.worktree_pending() {
            return Ok(None);
        }
        self.check_worktree_source()?;
        if self.worktree.attachment.is_some() {
            // Every error here follows confirmed creation, including typed400
            // refusals. A failed follow-up never permits another creation.
            return match self.advance_worktree_attachment() {
                Ok(Some(lines)) => {
                    self.settle_worktree();
                    Ok(Some(lines))
                }
                Ok(None) => Ok(None),
                Err(error) => Err(self.fail_worktree(error)),
            };
        }
        let result = self.connection.poll_operator().map_err(ClientError::from);
        self.worktree_result(result)
    }

    /// Discard the socket/result, not the backend effect. Cancellation is not rollback.
    pub fn cancel_worktree(&mut self) {
        self.worktree.attachment = None;
        if let Some(pending) = self.worktree.pending.take() {
            self.worktree.failed = Some(pending);
            self.connection.cancel_operator();
            self.worktree.uncertain = true;
        }
    }

    fn check_worktree_source(&mut self) -> Result<(), ClientError> {
        let pending = self
            .worktree
            .pending
            .as_ref()
            .ok_or(ClientError::NoSelection)?;
        if self.selected != pending.session
            || self.form_location().as_ref() != Some(&pending.location)
            || self.project.as_ref().map(|project| &project.worktree)
                != Some(&pending.project_directory)
        {
            self.cancel_worktree();
            return Err(ClientError::Worktree(format!(
                "Session or source location changed. {}",
                self.worktree_uncertainty()
            )));
        }
        Ok(())
    }

    fn settle_worktree(&mut self) {
        self.worktree.pending = None;
        self.worktree.failed = None;
        self.worktree.attachment = None;
        self.worktree.created = None;
        self.worktree.uncertain = false;
        self.worktree.creation_uncertain = false;
    }

    fn fail_worktree(&mut self, error: ClientError) -> ClientError {
        self.cancel_worktree();
        ClientError::Worktree(format!("{error}. {}", self.worktree_uncertainty()))
    }

    fn worktree_result(
        &mut self,
        result: Result<Option<WireResponse>, ClientError>,
    ) -> Result<Option<Vec<String>>, ClientError> {
        self.check_worktree_source()?;
        if matches!(result, Ok(None)) {
            return Ok(None);
        }
        let operation = self
            .worktree
            .pending
            .as_ref()
            .ok_or(ClientError::NoSelection)?
            .operation;
        self.worktree.uncertain = true;
        let mut not_started = false;
        let completed = (|| {
            let response = result?.ok_or(ClientError::NoSelection)?;
            // HTTP errors, even typed400, may follow a durable commit or a
            // partial filesystem effect. Only a source-validated management
            // receipt can explicitly prove NotStarted.
            let bytes = self.accept_response(operation, response)?;
            if operation == Operation::SessionCreate {
                let id = crate::wire::decode_session(&bytes)?.id;
                self.worktree.created = Some(id.clone());
                if self
                    .worktree
                    .pending
                    .as_ref()
                    .and_then(|pending| pending.session.as_ref())
                    == Some(&id)
                {
                    return Err(ClientError::InvalidInput(
                        "SessionCreate reused an existing Session identity.",
                    ));
                }
                self.worktree.attachment = Some(worktree_attachment::Attachment::new(&bytes)?);
                return Ok(None);
            }
            let reply = crate::wire::decode_worktree(&bytes)?;
            if self
                .worktree
                .pending
                .as_ref()
                .map(|pending| (&pending.location, &pending.project_directory))
                != Some((&reply.location, &reply.project_directory))
            {
                return Err(ClientError::InvalidInput(
                    "Worktree response source location differs from the request.",
                ));
            }
            let request = self
                .worktree
                .pending
                .as_ref()
                .and_then(|pending| pending.management.as_ref())
                .ok_or(ClientError::InvalidInput(
                    "Worktree management request context is missing.",
                ))?;
            match reply.effect {
                WorktreeEffect::Completed => Ok(Some(reply.completed_lines(request)?)),
                WorktreeEffect::NotStarted => {
                    not_started = true;
                    Err(ClientError::Worktree(format!(
                        "Worktree operation not started: {}. {}",
                        reply.message,
                        crate::wire::worktree_request_context(&reply.location, request).join("; ")
                    )))
                }
                WorktreeEffect::MayHaveChanged => Err(ClientError::Worktree(reply.message)),
            }
        })();
        match completed {
            Ok(Some(lines)) => {
                self.settle_worktree();
                Ok(Some(lines))
            }
            Ok(None) => Ok(None),
            Err(error) if not_started => {
                self.settle_worktree();
                Err(error)
            }
            Err(error) => Err(self.fail_worktree(error)),
        }
    }
}
