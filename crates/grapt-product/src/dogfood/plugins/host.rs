use super::authority::{authorize, Authorized};
use super::*;

#[derive(Debug)]
pub(super) struct ProductPluginHost {
    pub(super) application: Weak<super::DogfoodApplication>,
    pub(super) handle: tokio::runtime::Handle,
    subscriptions: Mutex<BTreeMap<u64, PluginEventSubscription>>,
    next_subscription: Mutex<u64>,
    pub(super) integration: integration::IntegrationRuntime,
}

#[derive(Debug)]
struct PluginEventSubscription {
    generation: PluginGenerationId,
    process: u64,
    session: grapt_types::SessionId,
    stream: Arc<grapt_api::DogfoodEventSubscription>,
}

impl ProductPluginHost {
    pub(super) fn new(
        application: Weak<super::DogfoodApplication>,
        handle: tokio::runtime::Handle,
    ) -> Self {
        Self {
            application,
            handle,
            subscriptions: Mutex::new(BTreeMap::new()),
            next_subscription: Mutex::new(1),
            integration: integration::IntegrationRuntime::default(),
        }
    }

    pub(super) fn application(&self) -> Result<Arc<super::DogfoodApplication>, String> {
        self.application
            .upgrade()
            .ok_or_else(|| "plugin host application is no longer active".to_owned())
    }

    fn query() -> Result<grapt_api::u3a::ProjectCurrentLocationQueryDto, String> {
        super::support::decode(json!({}))
    }

    pub(super) fn encode<T: serde::Serialize>(value: T) -> Result<Value, String> {
        serde_json::to_value(value).map_err(|error| error.to_string())
    }

    pub(super) fn located(application: &super::DogfoodApplication, data: Value) -> Value {
        json!({
            "location": {
                "directory": application.catalog_snapshot().directory,
                "project": application.catalog_snapshot().project.as_str(),
            },
            "data": data,
        })
    }

    fn session_get(
        application: &super::DogfoodApplication,
        authorized: &Authorized<'_>,
    ) -> Result<Value, String> {
        let record = application.get_record(&authorized.call()?.scope.session)?;
        Self::encode(
            application
                .session_response(&record)
                .map_err(|error| format!("{error:?}"))?,
        )
    }

    /// Create a Session inside the calling Session's project, directory, and
    /// workspace. A guest may not choose another location or parent.
    fn session_create(
        &self,
        application: &super::DogfoodApplication,
        authorized: &Authorized<'_>,
        payload: &Value,
    ) -> Result<Value, String> {
        let scope = authorized.call()?.scope;
        if payload.get("id").is_some_and(|value| !value.is_null()) {
            return Err("client-selected Session ids are not yet admitted".to_owned());
        }
        let caller = application.get_record(&scope.session)?;
        if let Some(location) = payload.get("location").filter(|value| !value.is_null()) {
            let directory = location.get("directory").and_then(Value::as_str);
            let workspace = location.get("workspaceID").and_then(Value::as_str);
            if directory.is_some_and(|directory| directory != caller.placement.directory())
                || workspace != caller.workspace.as_ref().map(|id| id.as_str())
            {
                return Err(
                    "plugin session.create is confined to the calling Session's location"
                        .to_owned(),
                );
            }
        }
        if let Some(parent) = payload.get("parentID").filter(|value| !value.is_null()) {
            if parent.as_str() != Some(scope.session.as_str()) {
                return Err("plugin session.create may only parent the calling Session".to_owned());
            }
        }
        let mut seed = super::SessionSeed::new(
            caller.project.clone(),
            caller.placement.directory().to_owned(),
            "New session",
        )
        .map_err(|error| error.to_string())?;
        if let Some(workspace) = caller.workspace.clone() {
            seed = seed
                .with_workspace(workspace)
                .map_err(|error| error.to_string())?;
        }
        if payload
            .get("parentID")
            .is_some_and(|value| !value.is_null())
        {
            seed = seed.with_parent(scope.session.clone());
        }
        if let Some(agent) = payload.get("agent").and_then(Value::as_str) {
            super::selection::validate_agent(
                application.plugins.service_root(),
                Path::new(caller.placement.directory()),
                agent,
                // This creates a parented primary Session, not a delegated run.
                false,
            )?;
            seed = seed.with_agent(agent).map_err(|error| error.to_string())?;
        }
        if let Some(model) = payload.get("model").filter(|value| !value.is_null()) {
            let model = super::selection::resolve_model(application, model)?;
            seed = seed.with_model(model).map_err(|error| error.to_string())?;
        }
        let created = {
            let authorities =
                super::DeterministicAuthorities::new(&*application.clock, &*application.ids);
            super::SessionPorts::new(Arc::clone(&application.inbox), &authorities)
                .create(&seed)
                .map_err(|error| error.to_string())?
        };
        let record = application.get_record(&created.session)?;
        self.handle
            .block_on(super::super::sandbox_worktree_config::observe(
                application,
                &created.session,
                Path::new(record.placement.directory()),
            ));
        Self::encode(
            application
                .session_response(&record)
                .map_err(|error| format!("{error:?}"))?,
        )
    }

    fn session_prompt(
        &self,
        application: &Arc<super::DogfoodApplication>,
        authorized: &Authorized<'_>,
        payload: &Value,
    ) -> Result<Value, String> {
        let scope = authorized.call()?.scope;
        let mut request = payload.clone();
        request
            .as_object_mut()
            .ok_or_else(|| "session.prompt payload is malformed".to_owned())?
            .remove("sessionID");
        let request = super::support::decode::<grapt_api::u3a::PromptRequestDto>(request)?;
        let session =
            grapt_api::u3a::WireId::parse(scope.session.as_str().to_owned()).ok_or_else(|| {
                "session.prompt sessionID cannot be represented on the wire".to_owned()
            })?;
        let response = self
            .handle
            .block_on(application.session_prompt(scope.principal.clone(), session, request))
            .map_err(|error| format!("{error:?}"))?;
        Self::encode(response)
    }

    fn session_synthetic(
        &self,
        application: &Arc<super::DogfoodApplication>,
        authorized: &Authorized<'_>,
        payload: &Value,
    ) -> Result<Value, String> {
        let scope = authorized.call()?.scope;
        let text = payload
            .get("text")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 256 * 1_024)
            .ok_or_else(|| "session.synthetic requires bounded text".to_owned())?
            .to_owned();
        let description = payload
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let metadata = synthetic_metadata(payload)?;
        let delivery = match payload.get("delivery").and_then(Value::as_str) {
            Some("steer") | None => grapt_types::DeliveryMode::Steer,
            Some("queue") => grapt_types::DeliveryMode::Queue,
            Some(_) => return Err("session.synthetic delivery is invalid".to_owned()),
        };
        let resume = payload
            .get("resume")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let input = match payload.get("id").and_then(Value::as_str) {
            Some(raw) => raw
                .parse::<grapt_types::MessageId>()
                .map_err(|_| "session.synthetic id is invalid".to_owned())?,
            None => format!(
                "msg_{}",
                grapt_types::IdAuthority::mint(application.ids.as_ref()).as_str()
            )
            .parse()
            .map_err(|_| "minted synthetic id is invalid".to_owned())?,
        };
        self.handle.block_on(application.submit_synthetic(
            scope.principal.clone(),
            scope.session.clone(),
            input,
            text,
            description,
            metadata,
            delivery,
            resume,
        ))
    }

    fn event_subscribe(
        &self,
        context: &PluginHostContext,
        application: &super::DogfoodApplication,
        authorized: &Authorized<'_>,
        payload: &Value,
    ) -> Result<Value, String> {
        const MAX_PLUGIN_SUBSCRIPTIONS: usize = 64;
        let action = payload
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| "event.subscribe requires an action".to_owned())?;
        let owned = |subscription: &PluginEventSubscription| {
            subscription.generation == context.generation()
                && subscription.process == authorized.process
        };
        match action {
            "open" => {
                let session = authorized.call()?.scope.session.clone();
                let mut subscriptions = self
                    .subscriptions
                    .lock()
                    .map_err(|_| "plugin event subscriptions are poisoned".to_owned())?;
                if subscriptions.len() >= MAX_PLUGIN_SUBSCRIPTIONS {
                    return Err("plugin event subscription bound is exhausted".to_owned());
                }
                let stream = Arc::new(
                    application
                        .hub
                        .subscribe_host()
                        .map_err(|error| error.to_string())?,
                );
                let mut next = self
                    .next_subscription
                    .lock()
                    .map_err(|_| "plugin event subscription identity is poisoned".to_owned())?;
                let id = *next;
                *next = next
                    .checked_add(1)
                    .ok_or_else(|| "plugin event subscription identity overflowed".to_owned())?;
                subscriptions.insert(
                    id,
                    PluginEventSubscription {
                        generation: context.generation(),
                        process: authorized.process,
                        session,
                        stream,
                    },
                );
                Ok(json!({"subscription":id}))
            }
            "next" => {
                let id = payload
                    .get("subscription")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "event.subscribe next requires subscription".to_owned())?;
                let (stream, session) = {
                    let subscriptions = self
                        .subscriptions
                        .lock()
                        .map_err(|_| "plugin event subscriptions are poisoned".to_owned())?;
                    let subscription = subscriptions
                        .get(&id)
                        .filter(|subscription| owned(subscription))
                        .ok_or_else(|| {
                            "plugin event subscription is absent or belongs to another plugin"
                                .to_owned()
                        })?;
                    (
                        Arc::clone(&subscription.stream),
                        subscription.session.clone(),
                    )
                };
                loop {
                    let Some(frame) = stream.next_frame().map_err(|error| error.to_string())?
                    else {
                        return Ok(json!({"done":true}));
                    };
                    let value = event_json(&frame)?;
                    if delivered_to(&value, &session) {
                        return Ok(json!({"done":false,"value":value}));
                    }
                }
            }
            "cancel" => {
                let id = payload
                    .get("subscription")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "event.subscribe cancel requires subscription".to_owned())?;
                let mut subscriptions = self
                    .subscriptions
                    .lock()
                    .map_err(|_| "plugin event subscriptions are poisoned".to_owned())?;
                if !subscriptions.get(&id).is_some_and(owned) {
                    return Err(
                        "plugin event subscription is absent or belongs to another plugin"
                            .to_owned(),
                    );
                }
                if let Some(subscription) = subscriptions.remove(&id) {
                    subscription.stream.cancel();
                }
                Ok(Value::Null)
            }
            _ => Err("event.subscribe action is unsupported".to_owned()),
        }
    }
}

/// A plugin subscription observes only events of its granted Session; events
/// without a Session are never delivered to plugins.
fn delivered_to(event: &Value, session: &grapt_types::SessionId) -> bool {
    event
        .get("data")
        .and_then(|data| data.get("sessionID"))
        .and_then(Value::as_str)
        == Some(session.as_str())
}

/// Synthetic metadata must be absent or an object so durable admission never
/// stores a shape the history API cannot return.
fn synthetic_metadata(payload: &Value) -> Result<Option<Value>, String> {
    match payload.get("metadata") {
        None | Some(Value::Null) => Ok(None),
        Some(value @ Value::Object(_)) => Ok(Some(value.clone())),
        Some(_) => Err("session.synthetic metadata must be an object".to_owned()),
    }
}

fn event_json(frame: &[u8]) -> Result<Value, String> {
    let marker = b"\ndata: ";
    let start = frame
        .windows(marker.len())
        .position(|window| window == marker)
        .map(|index| index + marker.len())
        .ok_or_else(|| "event owner returned a malformed SSE frame".to_owned())?;
    let end = frame
        .get(start..)
        .and_then(|data| data.windows(2).position(|window| window == b"\n\n"))
        .map(|index| start + index)
        .ok_or_else(|| "event owner returned an unterminated SSE frame".to_owned())?;
    serde_json::from_slice::<Value>(&frame[start..end])
        .map_err(|error| format!("event owner returned invalid JSON: {error}"))
}

impl PluginHost for ProductPluginHost {
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn call(
        &self,
        context: &PluginHostContext,
        operation: HostOperation,
        payload: Value,
    ) -> Result<Value, String> {
        let authorized = authorize(context, operation, &payload)?;
        let application = self.application()?;
        match operation {
            HostOperation::NetworkFetch => {
                Err("plugin network requests are owned by the process sandbox broker".into())
            }
            HostOperation::AgentList => Ok(Self::located(
                &application,
                super::super::selection::agent_inventory(
                    &application.catalog_snapshot(),
                    application.plugins.service_root(),
                    Path::new(&application.catalog_snapshot().directory),
                )?,
            )),
            HostOperation::AgentGet => {
                let id = payload
                    .get("agentID")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "agent.get requires agentID".to_owned())?;
                let agent = super::super::selection::agent_inventory(
                    &application.catalog_snapshot(),
                    application.plugins.service_root(),
                    Path::new(&application.catalog_snapshot().directory),
                )?
                .as_array()
                .and_then(|agents| agents.iter().find(|agent| agent["id"] == id))
                .cloned()
                .ok_or_else(|| format!("Agent not found: {id}"))?;
                Ok(Self::located(&application, agent))
            }
            HostOperation::CatalogProviderList => Ok(Self::located(
                &application,
                super::model_inventory::providers(&application.catalog_snapshot()),
            )),
            HostOperation::CatalogProviderGet => {
                let id = payload
                    .get("providerID")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "catalog.provider.get requires providerID".to_owned())?;
                let provider = super::model_inventory::providers(&application.catalog_snapshot())
                    .as_array()
                    .and_then(|providers| providers.iter().find(|provider| provider["id"] == id))
                    .cloned()
                    .ok_or_else(|| format!("Provider not found: {id}"))?;
                Ok(Self::located(&application, provider))
            }
            HostOperation::CatalogModelList => Ok(Self::located(
                &application,
                super::model_inventory::models(&application.catalog_snapshot()),
            )),
            HostOperation::CatalogModelDefault => Ok(Self::located(
                &application,
                super::model_inventory::default_model(&application.catalog_snapshot()),
            )),
            HostOperation::CommandList
            | HostOperation::SkillList
            | HostOperation::ReferenceList => {
                let kind = match operation {
                    HostOperation::CommandList => super::catalogs::Kind::Commands,
                    HostOperation::SkillList => super::catalogs::Kind::Skills,
                    _ => super::catalogs::Kind::References,
                };
                self.handle
                    .block_on(super::catalogs::response::<Value>(
                        Arc::clone(&application.catalog_snapshot()),
                        Self::query()?,
                        kind,
                    ))
                    .map_err(|error| format!("{error:?}"))
            }
            HostOperation::IntegrationList | HostOperation::IntegrationGet => {
                let response = self
                    .handle
                    .block_on(super::connections::response::<Value>(
                        application.as_ref().clone(),
                        Self::query()?,
                        super::connections::Kind::Integrations,
                    ))
                    .map_err(|error| format!("{error:?}"))?;
                if operation == HostOperation::IntegrationList {
                    return Ok(response);
                }
                let id = payload
                    .get("integrationID")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "integration.get requires integrationID".to_owned())?;
                let integration = response["data"]
                    .as_array()
                    .and_then(|items| items.iter().find(|item| item["id"] == id))
                    .cloned();
                Ok(Self::located(
                    &application,
                    integration.unwrap_or(Value::Null),
                ))
            }
            HostOperation::IntegrationConnectKey
            | HostOperation::IntegrationConnectionActive
            | HostOperation::IntegrationConnectionResolve => {
                self.integration_credentials(operation, &application, &authorized, &payload)
            }
            HostOperation::IntegrationOauthConnect
            | HostOperation::IntegrationOauthStatus
            | HostOperation::IntegrationOauthComplete
            | HostOperation::IntegrationOauthCancel => {
                self.integration_oauth(context, operation, &application, &authorized, &payload)
            }
            HostOperation::IntegrationCommandConnect
            | HostOperation::IntegrationCommandStatus
            | HostOperation::IntegrationCommandCancel => {
                self.integration_command(context, operation, &application, &authorized, &payload)
            }
            HostOperation::EventSubscribe => {
                self.event_subscribe(context, &application, &authorized, &payload)
            }
            HostOperation::WebsearchProviders
            | HostOperation::WebsearchSelectionGet
            | HostOperation::WebsearchSelectionPut
            | HostOperation::WebsearchQuery
            | HostOperation::WebsearchReload => {
                self.websearch(operation, &application, &authorized, &payload)
            }
            HostOperation::PluginList => Ok(Self::located(
                &application,
                Value::Array(
                    application
                        .plugins
                        .active()
                        .into_iter()
                        .map(|plugin| {
                            json!({
                                "id":plugin.id,
                                "exportedID":plugin.id,
                                "declaredTarget":plugin.target,
                                "origin":plugin.origin,
                                "source":plugin.source,
                                "status":plugin.status,
                            })
                        })
                        .collect(),
                ),
            )),
            HostOperation::AgentReload
            | HostOperation::CatalogReload
            | HostOperation::CommandReload
            | HostOperation::IntegrationReload
            | HostOperation::ReferenceReload
            | HostOperation::SkillReload => Ok(Value::Null),
            HostOperation::SessionCreate => {
                self.session_create(&application, &authorized, &payload)
            }
            HostOperation::SessionGet => Self::session_get(&application, &authorized),
            HostOperation::SessionPrompt => {
                self.session_prompt(&application, &authorized, &payload)
            }
            HostOperation::SessionGenerate => {
                self.session_generate(context, &application, &authorized, &payload)
            }
            HostOperation::SessionCommand => {
                self.session_command(&application, &authorized, &payload)
            }
            HostOperation::SessionSynthetic => {
                self.session_synthetic(&application, &authorized, &payload)
            }
            HostOperation::SessionInterrupt => {
                let session = &authorized.call()?.scope.session;
                application.executor.interrupt(session, None)?;
                Ok(Value::Null)
            }
        }
    }

    fn generation_closed(&self, generation: PluginGenerationId) {
        self.integration.generation_closed(generation);
        let Ok(mut subscriptions) = self.subscriptions.lock() else {
            return;
        };
        let stale = subscriptions
            .iter()
            .filter_map(|(id, subscription)| (subscription.generation == generation).then_some(*id))
            .collect::<Vec<_>>();
        for id in stale {
            if let Some(subscription) = subscriptions.remove(&id) {
                subscription.stream.cancel();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_event_delivery_is_confined_to_the_granted_session() {
        let granted = "ses_granted"
            .parse::<grapt_types::SessionId>()
            .unwrap_or_else(|_| panic!("literal session id was refused"));
        assert!(delivered_to(
            &json!({"type":"session.text.delta","data":{"sessionID":"ses_granted"}}),
            &granted
        ));
        assert!(
            !delivered_to(
                &json!({"type":"session.text.delta","data":{"sessionID":"ses_other"}}),
                &granted
            ),
            "a plugin subscription observed another Session's event"
        );
        assert!(
            !delivered_to(&json!({"type":"server.connected","data":{}}), &granted),
            "a plugin subscription observed a Session-less event"
        );
    }

    #[test]
    fn synthetic_metadata_must_be_an_object_before_admission() {
        assert_eq!(synthetic_metadata(&json!({})), Ok(None));
        assert_eq!(
            synthetic_metadata(&json!({"metadata":{"source":"plugin"}})),
            Ok(Some(json!({"source":"plugin"})))
        );
        for invalid in [json!("text"), json!(7), json!([1])] {
            assert_eq!(
                synthetic_metadata(&json!({"metadata": invalid})),
                Err("session.synthetic metadata must be an object".to_owned()),
                "non-object synthetic metadata reached admission"
            );
        }
    }
}
