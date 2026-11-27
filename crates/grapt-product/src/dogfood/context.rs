//! Session context composition: non-secret entries are durable; capabilities
//! are write-only, process-private custody and are never a persisted identity.

use std::collections::BTreeMap;
use std::sync::Arc;

use grapt_api::u3a::{
    InstructionEntryKeyDto, InstructionEntryListDto, InstructionEntryPutDto, NoContent,
    SessionCapabilityPutDto, SessionLookupApplicationError,
};
use grapt_instruction::InstructionService;
use grapt_runtime::mailbox::remote::credential::ProvisionGeneration;
use grapt_runtime::{SessionPortError, SessionPorts};
use grapt_session::SessionStoreError;
use grapt_types::{
    ClockAuthority, DeterministicAuthorities, IdAuthority, InstructionKey, Secret, SessionId,
};
use serde_json::{json, Value};

use super::support::{decode, invalid, not_found};
use super::DogfoodApplication;

#[derive(Debug, Default)]
pub(in crate::dogfood) struct SessionCapabilities {
    pub(in crate::dogfood) values: BTreeMap<SessionId, Arc<Secret<String>>>,
    /// Resource ownership only, bounded by the same 128 capability Sessions.
    /// Replacing a binding invalidates even tokens held by an older refresh.
    pub(in crate::dogfood) producer_bindings:
        BTreeMap<SessionId, Arc<std::sync::atomic::AtomicBool>>,
    pub(in crate::dogfood) native_principals: BTreeMap<SessionId, grapt_api::SealedPrincipal>,
    pub(in crate::dogfood) resumes: BTreeMap<SessionId, Arc<super::stateengine_resume::Checkpoint>>,
    pub(in crate::dogfood) packets: super::stateengine_resume::packet::Cache,
    /// The authenticated provision each held capability came from, bounded by
    /// the same Sessions; minted process-wide, so a generation never repeats.
    pub(in crate::dogfood) generations: BTreeMap<SessionId, ProvisionGeneration>,
    minted: Option<ProvisionGeneration>,
    /// Where each Session's continuity level is published.
    pub(in crate::dogfood) attention: Option<Arc<super::continuity_attention::ContinuityAttention>>,
    bytes: usize,
}

impl SessionCapabilities {
    fn check_put(&self, session: &SessionId, capability: &str) -> Result<usize, String> {
        if capability.len() > 65_536 {
            return Err("StateEngine capability exceeds 64 KiB".to_owned());
        }
        let prior = self.values.get(session);
        if prior.is_none() && self.values.len() >= 128 {
            return Err("StateEngine capability session bound reached".to_owned());
        }
        let bytes = self.bytes - prior.map_or(0, |value| value.expose().len()) + capability.len();
        if bytes > 1_048_576 {
            return Err("StateEngine capability aggregate byte bound reached".to_owned());
        }
        Ok(bytes)
    }
    pub(in crate::dogfood) fn put(
        &mut self,
        session: SessionId,
        capability: &str,
    ) -> Result<(), String> {
        let bytes = self.check_put(&session, capability)?;
        // Checked before any custody changes: an exhausted counter refuses.
        let generation = match self.minted {
            None => ProvisionGeneration::FIRST,
            Some(minted) => minted
                .next()
                .ok_or_else(|| "StateEngine capability generation is exhausted".to_owned())?,
        };
        if let Some(previous) = self.producer_bindings.insert(
            session.clone(),
            Arc::new(std::sync::atomic::AtomicBool::new(true)),
        ) {
            previous.store(false, std::sync::atomic::Ordering::Release);
        }
        self.values.insert(
            session.clone(),
            Arc::new(Secret::new(capability.to_owned())),
        );
        self.generations.insert(session.clone(), generation);
        self.minted = Some(generation);
        self.native_principals.remove(&session);
        self.bytes = bytes;
        self.resumes.remove(&session);
        self.packets.remove(&session);
        Ok(())
    }
    pub(crate) fn retire_producer(&mut self, session: &SessionId) {
        if let Some(token) = self.producer_bindings.remove(session) {
            token.store(false, std::sync::atomic::Ordering::Release);
        }
    }
    pub(crate) fn remove(&mut self, session: &SessionId) {
        self.retire_producer(session);
        if let Some(capability) = self.values.remove(session) {
            self.bytes -= capability.expose().len();
        }
        self.generations.remove(session);
        self.native_principals.remove(session);
        self.resumes.remove(session);
        self.packets.remove(session);
    }
}

/// Delete the durable session before releasing its process-private resources.
pub(crate) fn delete_session(
    application: &DogfoodApplication,
    session: SessionId,
) -> Result<NoContent, SessionLookupApplicationError> {
    if application.get_record(&session).is_err() {
        return Err(SessionLookupApplicationError::SessionNotFound(not_found(
            &session,
        )));
    }
    application
        .executor
        .interrupt(&session, None)
        .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))?;
    let authorities = DeterministicAuthorities::new(&*application.clock, &*application.ids);
    {
        let mut inbox = application.lock_inbox();
        grapt_session::delete_session(inbox.store_mut(), &authorities, &session)
    }
    .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))?;
    application.executor.close_terminals(&session);
    remove_session(application, &session);
    Ok(NoContent)
}

fn remove_session(application: &DogfoodApplication, session: &SessionId) {
    application.mcp.end_resource_subscriptions(Some(session));
    match application.session_capabilities.lock() {
        Ok(mut custody) => custody.remove(session),
        Err(poisoned) => poisoned.into_inner().remove(session),
    }
    if let Some(native) = &application.native_mail {
        native.retire(session);
    }
}

pub(crate) fn retire_producer(application: &DogfoodApplication, session: &SessionId) {
    application.mcp.end_resource_subscriptions(Some(session));
    match application.session_capabilities.lock() {
        Ok(mut custody) => custody.retire_producer(session),
        Err(poisoned) => poisoned.into_inner().retire_producer(session),
    }
}

/// Only this product boundary knows the StateEngine metadata keys. An absent
/// process capability for a durable binding is a rebind refusal.
pub(in crate::dogfood) fn call_metadata(
    inbox: &grapt_inbox::Inbox,
    custody: &SessionCapabilities,
    session: &SessionId,
    server: &str,
) -> Result<Option<Value>, String> {
    if server != "stateengine" {
        return Ok(None);
    }
    let entries = InstructionService::entries(inbox.store(), session)
        .map_err(|_| "StateEngine binding could not be read".to_owned())?;
    let entry = entries
        .into_iter()
        .find(|entry| entry.key.as_str() == "stateengine-identity");
    let binding = entry
        .as_ref()
        .map(|entry| serde_json::from_str::<Value>(&entry.value));
    match binding {
        None => Ok(Some(json!({"stateengine.session-anonymous-v1": true}))),
        Some(Ok(value)) if value.get("scope").and_then(Value::as_str) == Some("incognito") => {
            Ok(Some(json!({"stateengine.session-anonymous-v1": true})))
        }
        Some(Ok(value))
            if value
                .get("project_id")
                .and_then(Value::as_str)
                .is_some_and(|v| !v.is_empty())
                && value
                    .get("agent_id")
                    .and_then(Value::as_str)
                    .is_some_and(|v| !v.is_empty()) =>
        {
            let capability = custody
                .values
                .get(session)
                .ok_or_else(|| "StateEngine rebind required".to_owned())?;
            Ok(Some(
                json!({"stateengine.session-capability-v1": capability.expose()}),
            ))
        }
        _ => Err("StateEngine binding is unreadable; rebind required".to_owned()),
    }
}

fn refused(error: impl ToString) -> SessionLookupApplicationError {
    SessionLookupApplicationError::InvalidRequest(invalid(error))
}

pub(crate) fn require_session(
    application: &DogfoodApplication,
    session: &SessionId,
) -> Result<(), SessionLookupApplicationError> {
    session_record(application, session).map(|_| ())
}

pub(crate) fn session_record(
    application: &DogfoodApplication,
    session: &SessionId,
) -> Result<grapt_session::SessionRecord, SessionLookupApplicationError> {
    let record = stored_record(application, session)?;
    application.verify_session_carry(session).map_err(refused)?;
    Ok(record)
}

/// A read route's lookup: the record, with the structural carry check only.
/// Every carried byte the route serves is digested by the fetch that reads it.
pub(crate) fn require_readable_session(
    application: &DogfoodApplication,
    session: &SessionId,
) -> Result<(), SessionLookupApplicationError> {
    stored_record(application, session)?;
    application
        .verify_session_carry_for_read(session)
        .map_err(refused)
}

fn stored_record(
    application: &DogfoodApplication,
    session: &SessionId,
) -> Result<grapt_session::SessionRecord, SessionLookupApplicationError> {
    let authorities = DeterministicAuthorities::new(&*application.clock, &*application.ids);
    SessionPorts::new(Arc::clone(&application.inbox), &authorities)
        .get(session)
        .map_err(|error| match error {
            SessionPortError::Read {
                source: SessionStoreError::Unknown { .. },
            } => SessionLookupApplicationError::SessionNotFound(not_found(session)),
            other => refused(other),
        })
}

pub(crate) fn capability(
    application: &DogfoodApplication,
    principal: grapt_api::SealedPrincipal,
    session: SessionId,
    request: SessionCapabilityPutDto,
) -> Result<NoContent, SessionLookupApplicationError> {
    require_session(application, &session)?;
    application
        .ensure_session_writer(&session)
        .map_err(refused)?;
    super::host_control::prepare_binding(application, &session).map_err(refused)?;
    // Existing mail readers take inbox before capability custody too. Keep that
    // order while publishing the durable selection and process-private secret.
    let mut inbox = application.lock_inbox();
    let mut held = application
        .session_capabilities
        .lock()
        .map_err(|_| refused("StateEngine capability custody lock is poisoned"))?;
    held.check_put(&session, request.capability().get().expose())
        .map_err(refused)?;
    if let Some(path) = request.config_path().as_present() {
        let (project, agent) =
            super::session_config::identity(&inbox, &session).map_err(refused)?;
        let binding = grapt_session::config_binding::ConfigBinding {
            path: path.as_str().to_owned(),
            project,
            agent,
        };
        let record = grapt_session::SessionStore::over(inbox.store())
            .record(&session)
            .map_err(refused)?;
        super::session_config::document(
            &binding,
            &session,
            std::path::Path::new(record.placement.directory()),
            std::path::Path::new(&application.catalog_snapshot().directory),
        )
        .map_err(refused)?;
        let authorities = DeterministicAuthorities::new(&*application.clock, &*application.ids);
        grapt_session::config_binding::bind(inbox.store_mut(), &authorities, &session, binding)
            .map_err(refused)?;
        application.mcp.end_resource_subscriptions(Some(&session));
    }
    revoke_native_target(application, &mut inbox, &session)?;
    super::host_control::select_binding(application, &mut inbox, &session).map_err(refused)?;
    held.put(session.clone(), request.capability().get().expose())
        .map_err(refused)?;
    held.native_principals
        .insert(session.clone(), principal.clone());
    let attention = held.attention.clone();
    drop(held);
    application.executor.0.invalidate_config(&session);
    drop(inbox);
    if let Some(attention) = attention {
        attention.clear(&session);
    }
    native_binding_changed(application, &session);
    Ok(NoContent)
}

/// Inspect the last durable capture without resampling files or changing epochs.
/// Session-entry values (including private seat packets) are deliberately absent.
pub(crate) fn inspect(
    application: &DogfoodApplication,
    session: SessionId,
) -> Result<grapt_api::u3a::InstructionInspectionDto, SessionLookupApplicationError> {
    require_session(application, &session)?;
    let inbox = application.lock_inbox();
    let current = InstructionService::current(inbox.store(), &session).map_err(refused)?;
    super::instructions::warn_skipped(&current.skipped);
    let current = current.values;
    let captured = current.is_some();
    let mut ambient = String::new();
    let mut keys = Vec::new();
    for entry in current.into_iter().flatten() {
        if entry.key.as_str() == "core/instructions" {
            ambient = entry.value;
        }
        keys.push(entry.key.as_str().to_owned());
    }
    decode(json!({"captured":captured,"ambient":ambient,"keys":keys})).map_err(refused)
}

pub(crate) fn entries(
    application: &DogfoodApplication,
    session: SessionId,
) -> Result<InstructionEntryListDto, SessionLookupApplicationError> {
    require_session(application, &session)?;
    let inbox = application.lock_inbox();
    let entries = InstructionService::entries(inbox.store(), &session).map_err(refused)?;
    let mut data = Vec::with_capacity(entries.len());
    for entry in entries {
        // A corrupt stored JSON value is not an absent entry or an empty list.
        let value: grapt_api::u3a::InstructionJsonDto =
            serde_json::from_str(&entry.value).map_err(refused)?;
        data.push(json!({"key":entry.key.as_str(), "value":value}));
    }
    decode(json!({"data":data})).map_err(refused)
}

pub(crate) fn put(
    application: &DogfoodApplication,
    session: SessionId,
    key: InstructionEntryKeyDto,
    request: InstructionEntryPutDto,
) -> Result<NoContent, SessionLookupApplicationError> {
    require_session(application, &session)?;
    let key =
        InstructionKey::new(key.as_str()).ok_or_else(|| refused("instruction key invalid"))?;
    let value = request
        .value()
        .as_value()
        .map_or(Value::Null, |value| value.value().clone());
    let value = serde_json::to_string(&value).map_err(refused)?;
    let updated_ms = application.clock.now().as_millis();
    let mut inbox = application.lock_inbox();
    if key.as_str() == "stateengine-identity" {
        revoke_native_target(application, &mut inbox, &session)?;
    }
    InstructionService::put_entry(inbox.store_mut(), &session, &key, &value, updated_ms)
        .map_err(refused)?;
    if key.as_str() == "stateengine-identity" {
        retire_producer(application, &session);
    }
    drop(inbox);
    if key.as_str() == "stateengine-identity" {
        native_binding_changed(application, &session);
    }
    Ok(NoContent)
}

pub(crate) fn remove(
    application: &DogfoodApplication,
    session: SessionId,
    key: InstructionEntryKeyDto,
) -> Result<NoContent, SessionLookupApplicationError> {
    require_session(application, &session)?;
    let key =
        InstructionKey::new(key.as_str()).ok_or_else(|| refused("instruction key invalid"))?;
    let updated_ms = application.clock.now().as_millis();
    let mut inbox = application.lock_inbox();
    if key.as_str() == "stateengine-identity" {
        revoke_native_target(application, &mut inbox, &session)?;
    }
    InstructionService::remove_entry(inbox.store_mut(), &session, &key, updated_ms)
        .map_err(refused)?;
    if key.as_str() == "stateengine-identity" {
        retire_producer(application, &session);
    }
    drop(inbox);
    if key.as_str() == "stateengine-identity" {
        native_binding_changed(application, &session);
    }
    Ok(NoContent)
}

fn revoke_native_target(
    application: &DogfoodApplication,
    inbox: &mut grapt_inbox::Inbox,
    session: &SessionId,
) -> Result<(), SessionLookupApplicationError> {
    if application.native_mail.is_some() || application.external_host {
        let event = format!("evt_{}", application.ids.mint().as_str())
            .parse()
            .map_err(refused)?;
        let created_ms = application.clock.now().as_millis();
        inbox
            .revoke_native_mail_target(session, &event, created_ms)
            .map_err(refused)?;
    }
    Ok(())
}

fn native_binding_changed(application: &DogfoodApplication, session: &SessionId) {
    if let Some(native) = &application.native_mail {
        native.invalidate(session);
        // Registration never reverses the authenticated route's committed binding.
        // Missing capability or capacity is explicit unavailable on preparation;
        // no remote operation is performed by this synchronous route.
        let _registered = native.plan(session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scc_metadata_custody_never_falls_back_after_restart_or_crosses_sessions(
    ) -> Result<(), String> {
        let store = grapt_store::EventStore::open(std::path::Path::new(":memory:"), "scc-metadata")
            .map_err(|error| error.to_string())?;
        let mut inbox = grapt_inbox::Inbox::new(store).map_err(|error| error.to_string())?;
        let session = "ses_scc_bound"
            .parse::<SessionId>()
            .map_err(|error| error.to_string())?;
        let other = "ses_scc_other"
            .parse::<SessionId>()
            .map_err(|error| error.to_string())?;
        let key = InstructionKey::new("stateengine-identity").ok_or("key")?;
        // Initialize through the instruction owner, including a removed entry.
        InstructionService::put_entry(
            inbox.store_mut(),
            &session,
            &key,
            r#"{"scope":"incognito"}"#,
            1,
        )
        .map_err(|error| error.to_string())?;
        let mut custody = SessionCapabilities::default();
        custody.put(session.clone(), "SCC_PRIVATE_CAPABILITY")?;
        let anonymous = Some(json!({"stateengine.session-anonymous-v1": true}));
        assert_eq!(
            call_metadata(&inbox, &custody, &session, "stateengine")?,
            anonymous
        );
        assert_eq!(
            call_metadata(&inbox, &custody, &other, "stateengine")?,
            anonymous
        );
        assert_eq!(
            call_metadata(&inbox, &custody, &session, "unrelated")?,
            None
        );
        InstructionService::put_entry(
            inbox.store_mut(),
            &session,
            &key,
            r#"{"scope":"project","project_id":"fixture","agent_id":"agt_fixture"}"#,
            2,
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(
            call_metadata(&inbox, &custody, &session, "stateengine")?,
            Some(json!({"stateengine.session-capability-v1":"SCC_PRIVATE_CAPABILITY"}))
        );
        assert!(call_metadata(
            &inbox,
            &SessionCapabilities::default(),
            &session,
            "stateengine"
        )
        .is_err_and(|error| error == "StateEngine rebind required"));
        InstructionService::put_entry(
            inbox.store_mut(),
            &session,
            &key,
            r#"{"scope":"project"}"#,
            3,
        )
        .map_err(|error| error.to_string())?;
        let error = call_metadata(&inbox, &custody, &session, "stateengine")
            .err()
            .ok_or("malformed binding passed")?;
        assert!(error.contains("rebind required"));
        assert!(!error.contains("SCC_PRIVATE_CAPABILITY"));
        Ok(())
    }

    #[test]
    fn capability_replacement_is_bounded_redacted_and_not_resurrected() {
        let session = "ses_context"
            .parse::<SessionId>()
            .unwrap_or_else(|error| panic!("{error}"));
        let mut custody = SessionCapabilities::default();
        custody
            .put(session.clone(), "secret-first")
            .unwrap_or_else(|error| panic!("{error}"));
        custody
            .put(session.clone(), "secret-next")
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(custody.values.len(), 1);
        assert_eq!(custody.bytes, "secret-next".len());
        assert!(!format!("{custody:?}").contains("secret-next"));
        assert!(custody.put(session.clone(), &"x".repeat(65_537)).is_err());
        assert_eq!(
            custody
                .values
                .get(&session)
                .map(|value| value.expose().as_str()),
            Some("secret-next")
        );
        drop(custody);
        assert!(SessionCapabilities::default().values.is_empty());
    }
}
