//! Form requests a host may send off its state loop, one at a time. A host
//! composes them into a form transaction; each is the same route, body and
//! answer check as its synchronous twin. Replies do not re-list: the host's
//! transaction reads the list again when it needs it.
use super::prepared::{Accepted, Done, PreparedCall, Route, Step};
use super::*;
use crate::wire::{FormLocation, FormOwner, FormValue, FormWireId};
use std::collections::BTreeMap;

/// Where a form lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormAt {
    Session(SessionWireId),
    Location(FormLocation),
}

/// One form request.
#[derive(Debug)]
pub enum FormCall {
    /// What became of the form.
    State,
    /// The form itself; a location form only.
    Get,
    /// The outstanding forms at its address.
    List,
    Reply(BTreeMap<String, FormValue>),
    Cancel,
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    State,
    Get,
    List,
    Settle,
}

/// A form request waiting for its answer.
#[derive(Debug)]
pub(super) struct Form {
    at: FormAt,
    form: FormWireId,
    kind: Kind,
}

impl<C: Connection> Client<C> {
    pub fn prepare_form(
        &mut self,
        at: &FormAt,
        form: &FormWireId,
        call: FormCall,
    ) -> Result<PreparedCall, ClientError> {
        let session = match at {
            FormAt::Session(session) => Some(session),
            FormAt::Location(_) => None,
        };
        let location = match at {
            FormAt::Session(_) => None,
            FormAt::Location(location) => Some(location.directory.as_str()),
        };
        let (operation, kind, body) = match (call, session.is_some()) {
            (FormCall::State, true) => (Operation::SessionFormState, Kind::State, None),
            (FormCall::State, false) => (Operation::GlobalFormState, Kind::State, None),
            (FormCall::Get, true) => {
                return Err(ClientError::InvalidInput(
                    "A Session form is read through its list",
                ))
            }
            (FormCall::Get, false) => (Operation::GlobalFormGet, Kind::Get, None),
            (FormCall::List, true) => (Operation::SessionFormList, Kind::List, None),
            (FormCall::List, false) => (Operation::FormRequestList, Kind::List, None),
            (FormCall::Reply(answer), session) => {
                let body = Some(crate::wire::encode_form_reply(&answer)?);
                let operation = if session {
                    Operation::SessionFormReply
                } else {
                    Operation::GlobalFormReply
                };
                (operation, Kind::Settle, body)
            }
            (FormCall::Cancel, true) => (Operation::SessionFormCancel, Kind::Settle, None),
            (FormCall::Cancel, false) => (Operation::GlobalFormCancel, Kind::Settle, None),
        };
        // A list names its address only; every other request names the form.
        let named = !matches!(kind, Kind::List);
        let route = Route {
            request: named.then_some(form as &dyn crate::wire::Spelled),
            body,
            location,
            ..Route::new(operation, session)
        };
        let step = Step::Form(Form {
            at: at.clone(),
            form: form.clone(),
            kind,
        });
        self.prepare_call(step, route)
    }

    /// A list for a Session no longer selected is not installed.
    pub(super) fn accept_form(
        &mut self,
        form: Form,
        answer: Result<Vec<u8>, ClientError>,
    ) -> Result<Accepted, ClientError> {
        let bytes = answer?;
        let done = match (form.kind, form.at) {
            (Kind::State, _) => Done::FormState(crate::wire::decode_form_state(&bytes)?),
            (Kind::Get, _) => {
                let row = crate::wire::decode_form(&bytes)?;
                if row.id != form.form || row.owner != FormOwner::Global {
                    return Err(crate::wire::WireError::Invalid {
                        what: "location form owner",
                        value: "response does not identify the requested global form".into(),
                    }
                    .into());
                }
                Done::Form(row)
            }
            (Kind::List, FormAt::Session(session)) => {
                if self.selected.as_ref() == Some(&session) {
                    self.install_session_forms(&bytes)?;
                }
                Done::Applied
            }
            (Kind::List, FormAt::Location(location)) => {
                self.accept_location_forms(location, &bytes)?;
                Done::Applied
            }
            (Kind::Settle, FormAt::Session(_)) => Done::Applied,
            (Kind::Settle, FormAt::Location(_)) => {
                self.settled_location_form(&form.form);
                Done::Applied
            }
        };
        Ok(Accepted::Done(done))
    }
}
