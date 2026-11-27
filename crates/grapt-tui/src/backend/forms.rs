//! Only canonical Client form reads/writes; no inferred settlement or automatic replay.
use grapt_client::{wire::FormState, Client, ClientError, Connection, PeriodicRead};
use grapt_tui_forms::{Decision, Reply, Request, Target};
use std::time::Duration;
use tokio::time::Instant;
const INTERVAL: Duration = Duration::from_secs(2);

/// Each form read's due time, and a hint that arrived while that read was
/// in flight: no past-due deadline is kept while a read runs.
#[derive(Debug)]
pub struct Poll {
    session_due: Option<Instant>,
    location_due: Option<Instant>,
    session_hint: bool,
    location_hint: bool,
}
impl Poll {
    pub fn new(now: Instant) -> Self {
        Self {
            session_due: Some(now),
            location_due: Some(now),
            session_hint: false,
            location_hint: false,
        }
    }
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        self.session_due
            .into_iter()
            .chain(self.location_due)
            .min()
            .map(|due| due.saturating_duration_since(now))
    }
    /// The form reads due now, each marked started. A read already running
    /// stays due and starts once it settles.
    pub fn due<C: Connection>(
        &mut self,
        client: &mut Client<C>,
        now: Instant,
        busy: impl Fn(PeriodicRead) -> bool,
    ) -> Vec<PeriodicRead> {
        // A pending resume starts no read; due and flagged reads stay queued.
        if client.resume_pending() {
            return Vec::new();
        }
        let hints = [
            client.take_session_form_refresh(),
            client.take_location_form_refresh(),
        ];
        let mut due = Vec::new();
        for ((read, at, held), hinted) in [
            (
                PeriodicRead::SessionForms,
                &mut self.session_due,
                &mut self.session_hint,
            ),
            (
                PeriodicRead::LocationForms,
                &mut self.location_due,
                &mut self.location_hint,
            ),
        ]
        .into_iter()
        .zip(hints)
        {
            if hinted {
                *at = Some(now);
            }
            if !at.is_some_and(|at| now >= at) {
                continue;
            }
            *at = None;
            if busy(read) {
                // Held until the running read settles, never as a deadline.
                *held = true;
            } else {
                due.push(read);
            }
        }
        due
    }

    /// Record a form read's outcome. Committed events, attachment and recovery
    /// trigger canonical reads; only a failed read schedules a retry, and an
    /// answer for a Session or location no longer current asks again at once.
    pub fn settled(
        &mut self,
        read: PeriodicRead,
        result: Result<bool, ClientError>,
        now: Instant,
    ) -> Result<bool, ClientError> {
        let (at, held) = match read {
            PeriodicRead::SessionForms => (&mut self.session_due, &mut self.session_hint),
            _ => (&mut self.location_due, &mut self.location_hint),
        };
        let hinted = std::mem::take(held);
        match result {
            Ok(true) => {
                *at = hinted.then_some(now);
                Ok(true)
            }
            Ok(false) => {
                *at = Some(now);
                Ok(false)
            }
            Err(error) => {
                *at = Some(now + INTERVAL);
                Err(error)
            }
        }
    }
}

pub fn execute<C: Connection>(client: &mut Client<C>, request: Request) -> Reply {
    let mut reply = Reply {
        target: request.target.clone(),
        form: request.form.id.clone(),
        state: None,
        note: String::new(),
        validation_refused: false,
    };
    match transact(client, &request) {
        Ok((state, note, validation_refused)) => {
            reply.state = state;
            reply.validation_refused = validation_refused;
            reply.note = note;
        }
        Err(note) => reply.note = note,
    }
    reply
}
fn state<C: Connection>(
    client: &mut Client<C>,
    request: &Request,
) -> Result<FormState, ClientError> {
    match &request.target {
        Target::Session(session) => client.form_state(session, &request.form.id),
        Target::Location(location) => client.location_form_state(location, &request.form.id),
    }
}
fn refresh<C: Connection>(client: &mut Client<C>, target: &Target) -> Result<(), ClientError> {
    match target {
        Target::Session(_) => client.refresh_forms(),
        Target::Location(_) => client.refresh_location_forms(),
    }
}
#[expect(clippy::too_many_lines, reason = "legacy")]
fn transact<C: Connection>(
    client: &mut Client<C>,
    request: &Request,
) -> Result<(Option<FormState>, String, bool), String> {
    if !request
        .target
        .active(client.selected(), client.form_location().as_ref())
    {
        return Err(match request.target {
            Target::Session(_) => "Session changed; no form response sent",
            Target::Location(_) => "Location changed; no form response sent",
        }
        .into());
    }
    if !request.target.owner_matches(&request.form) {
        return Err("Question owner does not match its address; no response sent".into());
    }
    let before = state(client, request)
        .map_err(|error| format!("State unreadable; no new response sent: {error}"))?;
    if !before.is_open() {
        return Ok((
            Some(before),
            "Question already settled; no response sent".into(),
            false,
        ));
    }
    let Some(decision) = &request.decision else {
        return Ok((
            Some(before),
            "Question is open. Draft kept; only an explicit decision sends a reply".into(),
            false,
        ));
    };
    let unchanged = match &request.target {
        Target::Session(_) => {
            client
                .refresh_forms()
                .map_err(|error| format!("Question list unreadable; no response sent: {error}"))?;
            client.forms().contains(&request.form)
        }
        Target::Location(location) => {
            client
                .location_form(location, &request.form.id)
                .map_err(|error| format!("Question unreadable; no response sent: {error}"))?
                == request.form
        }
    };
    if !unchanged {
        return Err(
            "Question changed or left the list; no response sent. Ctrl+O checks state".into(),
        );
    }
    if let Decision::Answer(answer) = decision {
        grapt_tui_forms::validate(&request.form, answer)?;
    }
    let result = match (&request.target, decision) {
        (Target::Session(session), Decision::Answer(answer)) => {
            client.answer_form(session, &request.form.id, answer)
        }
        (Target::Session(session), Decision::Cancel) => {
            client.cancel_form(session, &request.form.id)
        }
        (Target::Location(location), Decision::Answer(answer)) => {
            client.answer_location_form(location, &request.form.id, answer)
        }
        (Target::Location(location), Decision::Cancel) => {
            client.cancel_location_form(location, &request.form.id)
        }
    };
    let invalid = match &result {
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
            Some(message)
        }
        _ => None,
    };
    // A failed POST or refresh may follow a committed write. Never replay it.
    match (state(client, request), invalid) {
        (Ok(state), _) if !state.is_open() => {
            let same = match (&state, decision) {
                (FormState::Answered(actual), Decision::Answer(expected)) => actual == expected,
                (FormState::Cancelled, Decision::Cancel) => true,
                _ => false,
            };
            let note = if !same { "Question settled elsewhere; inspect the reported state" }
            else if matches!(request.target, Target::Location(_)) {
                if matches!(before, FormState::Suspended(_)) { "Decision recorded; suspended MCP wait was not resumed" }
                else { "Decision recorded; live MCP waiter delivery is unconfirmed" }
            } else if matches!(decision, Decision::Answer(_)) { "Answer confirmed" } else { "Cancellation confirmed" };
            let note = match refresh(client, &request.target) { Ok(()) => note.into(), Err(error) => format!("{note}; question list unavailable: {error}") };
            Ok((Some(state), note, false))
        }
        (Ok(state), Some(message)) => Ok((Some(state), format!("Answer refused: {message}. Edit the draft, then Ctrl+S answers"), true)),
        (Ok(state), None) => Ok((Some(state), format!("Decision not confirmed; draft kept. Ctrl+O reads; explicit retry sends the same decision. {}", result.err().map(|error| error.to_string()).unwrap_or_default()), false)),
        (Err(error), Some(message)) => Ok((None, format!("Answer refused: {message}. Draft editable; state unreadable: {error}"), true)),
        (Err(error), None) => Err(format!("Decision unconfirmed; draft kept. Ctrl+O reads; no automatic retry: {error}")),
    }
}
#[cfg(test)]
mod tests;
