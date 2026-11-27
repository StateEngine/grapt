//! Periodic reads a host may run off its state loop. The client builds each
//! request from its state; the host sends it on another connection and hands
//! the answer back, and the client applies it here only if the scope it was
//! asked for still holds.
use super::*;

/// The reads a host repeats in the background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodicRead {
    Permissions,
    Sessions,
    SessionForms,
    LocationForms,
}

/// A read built from the client's state, with the scope its answer must
/// still match when it comes back.
#[derive(Debug)]
pub struct PreparedRead {
    pub read: PeriodicRead,
    pub request: WireRequest,
    operation: Operation,
    session: Option<SessionWireId>,
    location: Option<crate::wire::FormLocation>,
    /// The client's acknowledgment revision when the read was issued.
    acknowledged: u64,
    /// The session-form hint stamp when the read was issued.
    hints: u64,
}

/// The operations whose acknowledgment an older snapshot must not undo.
pub(super) fn acknowledges(operation: Operation) -> bool {
    matches!(
        operation,
        Operation::SessionCreate
            | Operation::SessionDelete
            | Operation::SessionRename
            | Operation::SessionPermissionReply
            | Operation::SessionFormReply
            | Operation::SessionFormCancel
            | Operation::GlobalFormReply
            | Operation::GlobalFormCancel
    )
}

impl<C: Connection> Client<C> {
    /// Build `read`'s request. `None` when there is nothing to ask, after
    /// applying what that means, exactly as the synchronous read would.
    pub fn prepare_read(
        &mut self,
        read: PeriodicRead,
    ) -> Result<Option<PreparedRead>, ClientError> {
        let (operation, session, location) = match read {
            PeriodicRead::Permissions => (Operation::PermissionRequestList, None, None),
            PeriodicRead::Sessions => (Operation::SessionList, None, None),
            PeriodicRead::SessionForms => {
                let Some(session) = self.selected.clone() else {
                    self.forms.clear();
                    self.session_forms_dirty = false;
                    return Ok(None);
                };
                (Operation::SessionFormList, Some(session), None)
            }
            PeriodicRead::LocationForms => {
                let Some(location) = self.form_location() else {
                    self.location_forms.clear();
                    return Ok(None);
                };
                (Operation::FormRequestList, None, Some(location))
            }
        };
        let directory = location.as_ref().map(|at| at.directory.as_str());
        let request = self.wire(operation, session.as_ref(), None, None, directory, None)?;
        // Host-started I/O, like `ingest`: it never coexists with a resume.
        self.abandon_resume();
        Ok(Some(PreparedRead {
            read,
            request,
            operation,
            session,
            location,
            acknowledged: self.acknowledged,
            hints: self.session_form_hints,
        }))
    }

    /// Apply the answer to a prepared read. Scope comes before status:
    /// `Ok(false)`, with nothing applied and nothing reported, when the
    /// Session or location it was asked for is no longer current, or the
    /// client has since acknowledged a close, rename, create, reply or cancel
    /// the snapshot cannot know about. The host should ask again.
    pub fn accept_read(
        &mut self,
        prepared: PreparedRead,
        response: Result<WireResponse, TransportError>,
    ) -> Result<bool, ClientError> {
        let in_scope = prepared.acknowledged == self.acknowledged
            && match prepared.read {
                PeriodicRead::Permissions | PeriodicRead::Sessions => true,
                PeriodicRead::SessionForms => self.selected == prepared.session,
                PeriodicRead::LocationForms => {
                    prepared.location.is_some() && self.form_location() == prepared.location
                }
            };
        if !in_scope {
            return Ok(false);
        }
        let bytes = self.accept_response(prepared.operation, response?)?;
        match (prepared.read, prepared.location) {
            (PeriodicRead::Permissions, _) => self.accept_permissions(bytes)?,
            (PeriodicRead::Sessions, _) => self.accept_sessions(&bytes)?,
            (PeriodicRead::SessionForms, _) => {
                // Installing clears the hint; one raised after this read was
                // issued is newer than the answer and stays pending.
                let newer = self.session_form_hints != prepared.hints;
                self.install_session_forms(&bytes)?;
                self.session_forms_dirty |= newer;
            }
            (PeriodicRead::LocationForms, Some(location)) => {
                self.accept_location_forms(location, &bytes)?;
            }
            (PeriodicRead::LocationForms, None) => return Ok(false),
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod live_tests;
