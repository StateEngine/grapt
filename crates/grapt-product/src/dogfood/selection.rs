use super::*;
use grapt_session::selection::{
    select_agent_committed, select_model_committed, select_preset_committed, SelectionError,
};
use grapt_session::{SessionListProjector, SessionStoreError};

/// Composed identities accepted by explicit ID, not the default selection.
pub(crate) const PRODUCTION_AGENT_IDS: &[&str] = &["agt_live_headless1"];

/// Test-kit fixture agents. The permission witness agent exists only in test
/// builds; the shipped product never publishes or accepts it.
#[cfg(feature = "loopback-provider-testkit")]
pub(crate) const FIXTURE_AGENT_IDS: &[&str] = &["agt_dogfood_allow"];
#[cfg(not(feature = "loopback-provider-testkit"))]
pub(crate) const FIXTURE_AGENT_IDS: &[&str] = &[];

pub(crate) fn composed_agent_ids() -> impl Iterator<Item = &'static str> {
    PRODUCTION_AGENT_IDS
        .iter()
        .chain(FIXTURE_AGENT_IDS)
        .copied()
}

pub(crate) fn validate_agent(
    service: &Path,
    directory: &Path,
    agent: &str,
    child: bool,
) -> Result<(), String> {
    if !composed_agent_ids().any(|id| id == agent) {
        super::agent_definitions::Catalog::read(service, directory)?.selected_for(agent, child)?;
    }
    Ok(())
}

pub(crate) fn resolve_model(
    application: &DogfoodApplication,
    model: &Value,
) -> Result<Value, String> {
    model_inventory::resolve(&application.catalog_snapshot(), model)
        .ok_or_else(|| "selected model has no matching composed target authority".into())
}

fn failure(error: SelectionError, session: &SessionId) -> SessionLookupApplicationError {
    match error {
        SelectionError::Read(SessionStoreError::Unknown { .. }) => {
            SessionLookupApplicationError::SessionNotFound(not_found(session))
        }
        other => SessionLookupApplicationError::InvalidRequest(invalid(other)),
    }
}

pub(crate) fn subagent_get(
    application: &DogfoodApplication,
    session: SessionId,
) -> Result<grapt_api::u3a::SessionSubagentModelResponseDto, SessionLookupApplicationError> {
    let held = application.lock_inbox();
    let model = grapt_session::selection::subagent_model(held.store().connection(), &session)
        .map_err(|error| failure(SelectionError::Read(error), &session))?;
    decode(json!({"data":model}))
        .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))
}

pub(crate) fn subagent_set(
    application: &DogfoodApplication,
    session: SessionId,
    request: Option<grapt_api::u3a::SessionModelSelectDto>,
) -> Result<NoContent, SessionLookupApplicationError> {
    let model = request
        .map(|request| serde_json::to_value(request.model().get()))
        .transpose()
        .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))?;
    let model = model
        .as_ref()
        .map(|model| resolve_model(application, model))
        .transpose()
        .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))?;
    let authorities = DeterministicAuthorities::new(&*application.clock, &*application.ids);
    let mut held = application.lock_inbox();
    let seq = grapt_session::selection::select_subagent_model(
        held.store_mut(),
        &authorities,
        &session,
        model.as_ref(),
    )
    .map_err(|error| failure(error, &session))?;
    if let Some(seq) = seq {
        input_events::publish(&held, &application.hub, &session, seq);
    }
    Ok(NoContent)
}

pub(crate) fn agent(
    application: &DogfoodApplication,
    session: SessionId,
    request: grapt_api::u3a::SessionAgentSelectDto,
) -> Result<NoContent, SessionLookupApplicationError> {
    let agent = request.agent().get().as_inner();
    let authorities = DeterministicAuthorities::new(&*application.clock, &*application.ids);
    let record = SessionPorts::new(Arc::clone(&application.inbox), &authorities)
        .get(&session)
        .map_err(|error| match error {
            grapt_runtime::SessionPortError::Read { source } => {
                failure(SelectionError::Read(source), &session)
            }
            other => SessionLookupApplicationError::InvalidRequest(invalid(other)),
        })?;
    let child = {
        let inbox = application.lock_inbox();
        grapt_subagent::SubagentService::child_context_on(inbox.store(), &session)
            .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))?
            .is_some()
    };
    validate_agent(
        application.plugins.service_root(),
        Path::new(record.placement.directory()),
        agent.as_str(),
        child,
    )
    .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))?;
    let mut held = application.lock_inbox();
    let seq = select_agent_committed(
        held.store_mut(),
        &authorities,
        &session,
        agent,
        &mut [&mut SessionListProjector],
    )
    .map_err(|error| failure(error, &session))?;
    if let Some(seq) = seq {
        input_events::publish(&held, &application.hub, &session, seq);
    }
    Ok(NoContent)
}

/// Read selection and effective trust from the same resolver used at execution
/// admission. Release each store read before looking up an ancestor.
pub(crate) fn preset_get(
    application: &DogfoodApplication,
    session: SessionId,
) -> Result<grapt_api::u3a::SessionPresetResponseDto, SessionLookupApplicationError> {
    let authorities = DeterministicAuthorities::new(&*application.clock, &*application.ids);
    let ports = SessionPorts::new(Arc::clone(&application.inbox), &authorities);
    let record = ports.get(&session).map_err(|error| match error {
        grapt_runtime::SessionPortError::Read { source } => {
            failure(SelectionError::Read(source), &session)
        }
        other => SessionLookupApplicationError::InvalidRequest(invalid(other)),
    })?;
    let policy = permission_preset::effective(application.plugins.service_root(), &record, |id| {
        ports.get(id).map_err(|error| error.to_string())
    })
    .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))?;
    decode(json!({"data":{"selected":record.preset,"effective":policy.preset.name()}}))
        .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))
}

/// Set the Session's permission preset. A mistyped name says which exist,
/// rather than leaving the user believing a trust level is in force. The
/// selection has no live event: clients read it from the Session.
pub(crate) fn preset(
    application: &DogfoodApplication,
    session: SessionId,
    request: grapt_api::u3a::SessionPresetSelectDto,
) -> Result<NoContent, SessionLookupApplicationError> {
    use grapt_permission::preset::Preset;
    let name = request.preset().get().as_str();
    let preset = Preset::parse(name).ok_or_else(|| {
        let available: Vec<&str> = Preset::ALL.iter().map(|preset| preset.name()).collect();
        SessionLookupApplicationError::InvalidRequest(invalid(format!(
            "Unknown permission preset: {name}. Available: {}",
            available.join(", ")
        )))
    })?;
    let authorities = DeterministicAuthorities::new(&*application.clock, &*application.ids);
    let mut held = application.lock_inbox();
    select_preset_committed(
        held.store_mut(),
        &authorities,
        &session,
        Some(preset.name()),
        &mut [&mut SessionListProjector],
    )
    .map_err(|error| failure(error, &session))?;
    Ok(NoContent)
}

pub(crate) fn model(
    application: &DogfoodApplication,
    session: SessionId,
    request: grapt_api::u3a::SessionModelSelectDto,
) -> Result<NoContent, SessionLookupApplicationError> {
    let model = serde_json::to_value(request.model().get())
        .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))?;
    let model = resolve_model(application, &model)
        .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))?;
    let authorities = DeterministicAuthorities::new(&*application.clock, &*application.ids);
    let mut held = application.lock_inbox();
    let seq = select_model_committed(
        held.store_mut(),
        &authorities,
        &session,
        &model,
        &mut [&mut SessionListProjector],
    )
    .map_err(|error| failure(error, &session))?;
    if let Some(seq) = seq {
        input_events::publish(&held, &application.hub, &session, seq);
    }
    Ok(NoContent)
}

/// The product's agent inventory, shared by the HTTP route and plugin hosts so
/// there is exactly one list to maintain. Internal identities stay hidden;
/// explicit selections can still resolve them.
pub(crate) fn agent_inventory(
    catalog: &CatalogProjection,
    service: &Path,
    directory: &Path,
) -> Result<Value, String> {
    let mut agents = vec![json!({
        "id": "agt_live_headless1",
        "name": "Live Headless",
        "model": {"providerID": catalog.provider, "id": catalog.model},
        "request": {"settings": {}, "headers": {}, "body": {}},
        "mode": "primary",
        "hidden": true,
        "permissions": [],
    })];
    agents.extend(fixture_agents(catalog));
    agents.extend(super::agent_definitions::Catalog::read(service, directory)?.inventory()?);
    // Custom definitions may sort before the general-purpose agent.
    agents.sort_by_key(|agent| agent["id"] != "default");
    Ok(Value::Array(agents))
}

#[cfg(feature = "loopback-provider-testkit")]
fn fixture_agents(catalog: &CatalogProjection) -> Vec<Value> {
    vec![json!({
        "id": "agt_dogfood_allow",
        "name": "Dogfood Allow Witness",
        "model": {"providerID": catalog.provider, "id": catalog.model},
        "request": {"settings": {}, "headers": {}, "body": {}},
        "mode": "subagent",
        "hidden": false,
        "permissions": [{"action": "write", "resource": "*", "effect": "allow"}],
    })]
}

#[cfg(not(feature = "loopback-provider-testkit"))]
fn fixture_agents(_catalog: &CatalogProjection) -> Vec<Value> {
    Vec::new()
}

#[cfg(test)]
mod agent_inventory_tests {
    use super::*;

    #[test]
    fn shipped_inventory_omits_the_permission_witness_and_the_test_kit_injects_it() {
        assert!(!PRODUCTION_AGENT_IDS.contains(&"agt_dogfood_allow"));
        let composed = composed_agent_ids().collect::<Vec<_>>();
        assert!(composed.contains(&"agt_live_headless1"));
        assert_eq!(
            composed.contains(&"agt_dogfood_allow"),
            cfg!(feature = "loopback-provider-testkit"),
            "the witness agent must be present exactly in test-kit builds"
        );
    }
}
