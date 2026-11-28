//! Naming follows successful admission; it never changes admission's outcome.
//! A live catalog hint follows each committed title change.
use super::*;
use grapt_session::title::{name_from_first_prompt, rename_session, TitleError};
use grapt_session::SessionStoreError;

pub(crate) fn automatic(application: &ServerApplication, session: &SessionId) {
    let authorities = DeterministicAuthorities::new(&*application.clock, &*application.ids);
    let mut held = application.lock_inbox();
    let result = name_from_first_prompt(held.store_mut(), &authorities, session);
    drop(held);
    match result {
        Ok(Some(_)) => announce(application, session),
        Ok(None) => {}
        Err(error) => {
            // The prompt is already admitted; its independent name can retry.
            let reason: String = error.to_string().chars().take(512).collect();
            crate::diagnostics::report(format!(
                "session {session}: admitted prompt title could not be saved: {reason}"
            ));
        }
    }
}

pub(crate) fn manual(
    application: &ServerApplication,
    session: SessionId,
    request: grapt_api::u3a::SessionRenameRequestDto,
) -> Result<NoContent, SessionLookupApplicationError> {
    let authorities = DeterministicAuthorities::new(&*application.clock, &*application.ids);
    let mut held = application.lock_inbox();
    let changed = rename_session(
        held.store_mut(),
        &authorities,
        &session,
        request.title().get().as_str(),
    )
    .map_err(|error| match error {
        TitleError::Read(SessionStoreError::Unknown { .. }) => {
            SessionLookupApplicationError::SessionNotFound(not_found(&session))
        }
        other => SessionLookupApplicationError::InvalidRequest(invalid(other)),
    })?;
    drop(held);
    if changed.is_some() {
        announce(application, &session);
    }
    Ok(NoContent)
}

fn announce(application: &ServerApplication, session: &SessionId) {
    use grapt_api::u3a::{DogfoodEvent, FiniteF64, LiveEvent, SessionTitleChangedData, WireId};
    let event = (|| -> Result<_, String> {
        let id = WireId::parse(format!("evt_{}", application.ids.mint().as_str()))
            .ok_or("title notice identity was refused")?;
        let session_id = WireId::parse(session.as_str().to_owned())
            .ok_or("title notice Session identity was refused")?;
        let created = FiniteF64::try_new(application.clock.now().as_millis() as f64)
            .map_err(|error| error.to_string())?;
        Ok(DogfoodEvent::SessionTitleChanged(LiveEvent::new(
            id,
            created,
            SessionTitleChangedData { session_id },
        )))
    })();
    // The write succeeded. A missed advisory must never invite a write retry.
    if let Err(error) = event.and_then(|event| {
        application
            .hub
            .publish(&event)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }) {
        crate::diagnostics::report(format!(
            "session {session}: title saved but refresh notice unavailable: {error}"
        ));
    }
}
