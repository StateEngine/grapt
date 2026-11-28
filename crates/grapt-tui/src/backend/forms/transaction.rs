//! One form transaction, run on the command lane a request at a time: read
//! the form's state, check the question is unchanged, send the decision,
//! then read what became of it. A failed write or read after a committed
//! one is reported, never replayed.
use super::super::command_lane::Turn;
use grapt_client::{
    wire::FormState, Accepted, Client, ClientError, Connection, Done, FormAt, FormCall,
};
use grapt_tui_forms::{Decision, Reply, Request, Target};

/// What a transaction settles with: the state it read, its note, and
/// whether a matching validation receipt refused the decision.
type Settled = Result<(Option<FormState>, String, bool), String>;

#[derive(Debug)]
pub(in crate::backend) struct Flow {
    request: Request,
    stage: Stage,
}

/// The request whose answer the transaction waits for.
#[derive(Debug)]
enum Stage {
    Before,
    Unchanged(FormState),
    Decided(FormState),
    Confirming {
        before: FormState,
        sent: Result<(), ClientError>,
    },
    Refreshing {
        state: FormState,
        note: &'static str,
    },
}

/// Start a transaction: refused at once for a Session or location no
/// longer current, or a question whose owner does not match its address.
pub(in crate::backend) fn begin<C: Connection>(
    client: &mut Client<C>,
    request: Request,
) -> Turn<Flow, Reply> {
    if !request
        .target
        .active(client.selected(), client.form_location().as_ref())
    {
        let note = match request.target {
            Target::Session(_) => "Session changed; no form response sent",
            Target::Location(_) => "Location changed; no form response sent",
        };
        return Turn::Settle(reply(&request, Err(note.into())));
    }
    if !request.target.owner_matches(&request.form) {
        let note = "Question owner does not match its address; no response sent";
        return Turn::Settle(reply(&request, Err(note.into())));
    }
    send(client, request, FormCall::State, Stage::Before)
}

impl Flow {
    pub(in crate::backend) fn advance<C: Connection>(
        self,
        client: &mut Client<C>,
        done: Result<Done, ClientError>,
    ) -> Turn<Self, Reply> {
        let Self { request, stage } = self;
        match stage {
            Stage::Before => match state(done) {
                Err(error) => {
                    let note = format!("State unreadable; no new response sent: {error}");
                    Turn::Settle(reply(&request, Err(note)))
                }
                Ok(before) if !before.is_open() => {
                    let note = "Question already settled; no response sent";
                    Turn::Settle(reply(&request, Ok((Some(before), note.into(), false))))
                }
                Ok(before) if request.decision.is_none() => {
                    let note =
                        "Question is open. Draft kept; only an explicit decision sends a reply";
                    Turn::Settle(reply(&request, Ok((Some(before), note.into(), false))))
                }
                Ok(before) => {
                    let check = match request.target {
                        Target::Session(_) => FormCall::List,
                        Target::Location(_) => FormCall::Get,
                    };
                    send(client, request, check, Stage::Unchanged(before))
                }
            },
            Stage::Unchanged(before) => {
                let unchanged = match (&request.target, done) {
                    (Target::Session(_), Ok(_)) => Ok(client.forms().contains(&request.form)),
                    (Target::Location(_), Ok(Done::Form(row))) => Ok(row == request.form),
                    (Target::Location(_), Ok(_)) => Ok(false),
                    (Target::Session(_), Err(error)) => Err(format!(
                        "Question list unreadable; no response sent: {error}"
                    )),
                    (Target::Location(_), Err(error)) => {
                        Err(format!("Question unreadable; no response sent: {error}"))
                    }
                };
                match unchanged {
                    Err(note) => Turn::Settle(reply(&request, Err(note))),
                    Ok(false) => {
                        let note = "Question changed or left the list; no response sent. Ctrl+O checks state";
                        Turn::Settle(reply(&request, Err(note.into())))
                    }
                    Ok(true) => decide(client, request, before),
                }
            }
            Stage::Decided(before) => {
                let sent = done.map(|_| ());
                send(
                    client,
                    request,
                    FormCall::State,
                    Stage::Confirming { before, sent },
                )
            }
            Stage::Confirming { before, sent } => confirm(client, request, before, sent, done),
            Stage::Refreshing { state, note } => {
                let note = match done {
                    Ok(_) => note.into(),
                    Err(error) => format!("{note}; question list unavailable: {error}"),
                };
                Turn::Settle(reply(&request, Ok((Some(state), note, false))))
            }
        }
    }
}

/// Send `call` for the transaction, which waits for its answer in `stage`.
fn send<C: Connection>(
    client: &mut Client<C>,
    request: Request,
    call: FormCall,
    stage: Stage,
) -> Turn<Flow, Reply> {
    let prepared = client
        .prepare_form(&at(&request.target), &request.form.id, call)
        .map(Accepted::Next);
    Turn::Step(Flow { request, stage }, prepared)
}

/// Validate and send the decision.
fn decide<C: Connection>(
    client: &mut Client<C>,
    request: Request,
    before: FormState,
) -> Turn<Flow, Reply> {
    let call = match &request.decision {
        Some(Decision::Answer(answer)) => {
            if let Err(note) = grapt_tui_forms::validate(&request.form, answer) {
                return Turn::Settle(reply(&request, Err(note)));
            }
            FormCall::Reply(answer.clone())
        }
        Some(Decision::Cancel) => FormCall::Cancel,
        None => {
            let note = "Question is open. Draft kept; only an explicit decision sends a reply";
            return Turn::Settle(reply(&request, Ok((Some(before), note.into(), false))));
        }
    };
    send(client, request, call, Stage::Decided(before))
}

/// Report the state read after the decision; a settled question is listed
/// again before the transaction settles.
fn confirm<C: Connection>(
    client: &mut Client<C>,
    request: Request,
    before: FormState,
    sent: Result<(), ClientError>,
    done: Result<Done, ClientError>,
) -> Turn<Flow, Reply> {
    let invalid = match &sent {
        Err(ClientError::FormInvalidAnswer {
            operation,
            form,
            message,
        }) if *form == request.form.id
            && *operation
                == match request.target {
                    Target::Session(_) => "v2.session.form.reply",
                    Target::Location(_) => "grapt.form.reply",
                } =>
        {
            Some(message.clone())
        }
        _ => None,
    };
    let settled: Settled = match (state(done), invalid) {
        (Ok(state), _) if !state.is_open() => {
            let note = settled_note(&request, &before, &state);
            return send(client, request, FormCall::List, Stage::Refreshing { state, note });
        }
        (Ok(state), Some(message)) => Ok((
            Some(state),
            format!("Answer refused: {message}. Edit the draft, then Ctrl+S answers"),
            true,
        )),
        (Ok(state), None) => Ok((
            Some(state),
            format!(
                "Decision not confirmed; draft kept. Ctrl+O reads; explicit retry sends the same decision. {}",
                sent.err().map(|error| error.to_string()).unwrap_or_default()
            ),
            false,
        )),
        (Err(error), Some(message)) => Ok((
            None,
            format!("Answer refused: {message}. Draft editable; state unreadable: {error}"),
            true,
        )),
        (Err(error), None) => Err(format!(
            "Decision unconfirmed; draft kept. Ctrl+O reads; no automatic retry: {error}"
        )),
    };
    Turn::Settle(reply(&request, settled))
}

fn settled_note(request: &Request, before: &FormState, state: &FormState) -> &'static str {
    let same = match (state, &request.decision) {
        (FormState::Answered(actual), Some(Decision::Answer(expected))) => actual == expected,
        (FormState::Cancelled, Some(Decision::Cancel)) => true,
        _ => false,
    };
    if !same {
        "Question settled elsewhere; inspect the reported state"
    } else if matches!(request.target, Target::Location(_)) {
        if matches!(before, FormState::Suspended(_)) {
            "Decision recorded; suspended MCP wait was not resumed"
        } else {
            "Decision recorded; live MCP waiter delivery is unconfirmed"
        }
    } else if matches!(request.decision, Some(Decision::Answer(_))) {
        "Answer confirmed"
    } else {
        "Cancellation confirmed"
    }
}

fn state(done: Result<Done, ClientError>) -> Result<FormState, ClientError> {
    match done? {
        Done::FormState(state) => Ok(state),
        _ => Err(ClientError::NotOffered { what: "form state" }),
    }
}

fn at(target: &Target) -> FormAt {
    match target {
        Target::Session(session) => FormAt::Session(session.clone()),
        Target::Location(location) => FormAt::Location(location.clone()),
    }
}

fn reply(request: &Request, settled: Settled) -> Reply {
    let mut reply = Reply {
        target: request.target.clone(),
        form: request.form.id.clone(),
        state: None,
        note: String::new(),
        validation_refused: false,
    };
    match settled {
        Ok((state, note, validation_refused)) => {
            reply.state = state;
            reply.validation_refused = validation_refused;
            reply.note = note;
        }
        Err(note) => reply.note = note,
    }
    reply
}
