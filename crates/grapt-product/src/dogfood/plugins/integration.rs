//! Bounded integration connection attempts owned by the product host.
//!
//! Every credential a plugin creates is owned by that plugin's transport
//! identity and is readable only by it. OAuth callbacks run in the process of
//! the plugin that registered the method, under the requesting call's grant.
//! A command method runs as a permission-governed product effect attributed to
//! the requesting plugin call; the guest learns only the attempt status.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use grapt_platform::{IntegrationCredentialAuthority, IntegrationCredentialOwner};
use grapt_plugin::{CallAuthority, HostOperation, PluginGenerationId, PluginHostContext};
use grapt_types::{ClockAuthority as _, IdAuthority as _, Secret};
use serde_json::{json, Value};

use super::authority::Authorized;
use super::{IntegrationMethod, ProductPluginHost};

const MAX_ATTEMPTS: usize = 32;
const ATTEMPT_LIFETIME_MS: i64 = 10 * 60 * 1_000;
const MAX_PROVIDER_LIFETIME_MS: i64 = 60 * 60 * 1_000;
const MAX_INPUT_BYTES: usize = 64 * 1_024;
const MAX_ERROR_BYTES: usize = 4 * 1_024;
const MAX_CREDENTIAL_BYTES: usize = 16 * 1_024;

#[derive(Debug, Default)]
pub(crate) struct IntegrationRuntime {
    oauth: Mutex<BTreeMap<String, OauthAttempt>>,
    commands: Arc<Mutex<BTreeMap<String, CommandAttempt>>>,
}

#[derive(Debug)]
struct OauthAttempt {
    generation: PluginGenerationId,
    requester: String,
    method_owner: String,
    integration_id: String,
    method_id: String,
    label: String,
    created: i64,
    expires: i64,
    state: OauthState,
}

#[derive(Debug)]
enum OauthState {
    PendingCode { callback: u64 },
    Completing,
    Complete,
    Failed { message: String },
    Expired,
}

#[derive(Debug)]
struct CommandAttempt {
    generation: PluginGenerationId,
    requester: String,
    integration_id: String,
    created: i64,
    expires: i64,
    state: CommandState,
    abort: Option<tokio::task::AbortHandle>,
}

#[derive(Debug)]
enum CommandState {
    Pending,
    Complete,
    Failed { message: String },
    Expired,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn bounded_error(error: impl std::fmt::Display) -> String {
    let mut value = error.to_string();
    if value.len() > MAX_ERROR_BYTES {
        let mut end = MAX_ERROR_BYTES;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
    }
    value
}

fn now(application: &super::super::DogfoodApplication) -> i64 {
    application.clock.now().as_millis()
}

fn expiry(created: i64, requested: Option<i64>) -> Result<i64, String> {
    let default = created
        .checked_add(ATTEMPT_LIFETIME_MS)
        .ok_or_else(|| "integration attempt expiry overflowed".to_owned())?;
    let maximum = created
        .checked_add(MAX_PROVIDER_LIFETIME_MS)
        .ok_or_else(|| "integration attempt expiry overflowed".to_owned())?;
    Ok(requested
        .filter(|value| *value > created && *value <= maximum)
        .unwrap_or(default))
}

fn mint(application: &super::super::DogfoodApplication, prefix: &str) -> String {
    format!("{prefix}_{}", application.ids.mint().as_str())
}

fn coordinates(payload: &Value) -> Result<(&str, &str), String> {
    let integration = payload
        .get("integrationID")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 256)
        .ok_or_else(|| "integration attempt requires a bounded integrationID".to_owned())?;
    let method = payload
        .get("methodID")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 256)
        .ok_or_else(|| "integration attempt requires a bounded methodID".to_owned())?;
    Ok((integration, method))
}

fn label(payload: &Value, fallback: &str) -> Result<String, String> {
    let label = payload
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or(fallback);
    if label.is_empty() || label.len() > 512 {
        return Err("integration label must contain 1..=512 UTF-8 bytes".to_owned());
    }
    Ok(label.to_owned())
}

fn attempt_id<'a>(payload: &'a Value, operation: &str) -> Result<&'a str, String> {
    payload
        .get("attemptID")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{operation} requires attemptID"))
}

fn owned_by<'a>(
    attempt: Option<&'a mut OauthAttempt>,
    context: &PluginHostContext,
    requester: &str,
    attempt_id: &str,
) -> Result<&'a mut OauthAttempt, String> {
    attempt
        .filter(|attempt| {
            attempt.generation == context.generation() && attempt.requester == requester
        })
        .ok_or_else(|| format!("OAuth attempt not found: {attempt_id}"))
}

impl IntegrationRuntime {
    pub(crate) fn generation_closed(&self, generation: PluginGenerationId) {
        lock(&self.oauth).retain(|_, attempt| attempt.generation != generation);
        lock(&self.commands).retain(|_, attempt| {
            if attempt.generation != generation {
                return true;
            }
            if let Some(abort) = &attempt.abort {
                abort.abort();
            }
            false
        });
    }
}

impl ProductPluginHost {
    pub(crate) fn integration_credentials(
        &self,
        operation: HostOperation,
        application: &super::super::DogfoodApplication,
        authorized: &Authorized<'_>,
        payload: &Value,
    ) -> Result<Value, String> {
        let authority = IntegrationCredentialAuthority::new();
        match operation {
            HostOperation::IntegrationConnectKey => {
                authorized.call()?;
                let integration_id = payload
                    .get("integrationID")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "integration.connect.key requires integrationID".to_owned())?;
                let key = payload
                    .get("key")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "integration.connect.key requires key".to_owned())?;
                let registry = application.plugins.integration_registry()?;
                if !registry.methods.iter().any(|method| {
                    matches!(method, IntegrationMethod::Key { integration_id: held } if held == integration_id)
                }) {
                    return Err(format!("key method is unavailable: {integration_id}"));
                }
                authority
                    .connect_key(
                        application.lock_inbox().store_mut(),
                        IntegrationCredentialOwner {
                            plugin: authorized.owner,
                            now_ms: now(application),
                        },
                        mint(application, "crd"),
                        integration_id.to_owned(),
                        label(payload, integration_id)?,
                        Secret::new(key.to_owned()),
                    )
                    .map_err(|error| error.to_string())?;
                Ok(Value::Null)
            }
            HostOperation::IntegrationConnectionActive => {
                let integration_id = payload
                    .get("id")
                    .or_else(|| payload.get("integrationID"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        "integration.connection.active requires integration id".to_owned()
                    })?;
                Ok(authority
                    .active(application.lock_inbox().store(), authorized.owner, integration_id)
                    .map_err(|error| error.to_string())?
                    .map(|connection| {
                        json!({"type":"credential","id":connection.id(),"label":connection.label()})
                    })
                    .unwrap_or(Value::Null))
            }
            HostOperation::IntegrationConnectionResolve => {
                let connection = payload.get("connection").ok_or_else(|| {
                    "integration.connection.resolve requires connection".to_owned()
                })?;
                if connection.get("type").and_then(Value::as_str) != Some("credential") {
                    return Ok(Value::Null);
                }
                let id = connection
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "integration credential connection requires id".to_owned())?;
                Ok(authority
                    .resolve(application.lock_inbox().store(), authorized.owner, id)
                    .map_err(|error| error.to_string())?
                    .map(|credential| credential.into_secret().into_inner())
                    .unwrap_or(Value::Null))
            }
            _ => Err("integration credential dispatch changed".to_owned()),
        }
    }

    pub(crate) fn integration_oauth(
        &self,
        context: &PluginHostContext,
        operation: HostOperation,
        application: &super::super::DogfoodApplication,
        authorized: &Authorized<'_>,
        payload: &Value,
    ) -> Result<Value, String> {
        match operation {
            HostOperation::IntegrationOauthConnect => {
                self.oauth_connect(context, application, authorized, payload)
            }
            HostOperation::IntegrationOauthStatus => {
                self.oauth_status(context, application, authorized, payload)
            }
            HostOperation::IntegrationOauthComplete => {
                self.oauth_complete(context, application, authorized, payload)
            }
            HostOperation::IntegrationOauthCancel => {
                let attempt_id = attempt_id(payload, "OAuth cancel")?;
                let mut attempts = lock(&self.integration.oauth);
                owned_by(
                    attempts.get_mut(attempt_id),
                    context,
                    authorized.owner,
                    attempt_id,
                )?;
                attempts.remove(attempt_id);
                Ok(Value::Null)
            }
            _ => Err("OAuth integration dispatch changed".to_owned()),
        }
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn oauth_connect(
        &self,
        context: &PluginHostContext,
        application: &super::super::DogfoodApplication,
        authorized: &Authorized<'_>,
        payload: &Value,
    ) -> Result<Value, String> {
        let call = authorized.call()?;
        let (integration_id, method_id) = coordinates(payload)?;
        let (method_owner, authorize) = application
            .plugins
            .integration_registry()?
            .methods
            .iter()
            .find_map(|method| match method {
                IntegrationMethod::Oauth {
                    integration_id: held_integration,
                    method_id: held_method,
                    plugin_id,
                    authorize,
                } if held_integration == integration_id && held_method == method_id => {
                    Some((plugin_id.clone(), *authorize))
                }
                _ => None,
            })
            .ok_or_else(|| format!("OAuth method not found: {integration_id}/{method_id}"))?;
        let inputs = payload.get("inputs").cloned().unwrap_or_else(|| json!({}));
        if serde_json::to_vec(&inputs)
            .map_err(|_| "OAuth inputs could not be encoded".to_owned())?
            .len()
            > MAX_INPUT_BYTES
        {
            return Err(format!(
                "OAuth inputs exceed the {MAX_INPUT_BYTES}-byte bound"
            ));
        }
        if lock(&self.integration.oauth).len() >= MAX_ATTEMPTS {
            return Err(format!(
                "OAuth attempts exceed the {MAX_ATTEMPTS}-attempt bound"
            ));
        }
        let attempt_id = mint(application, "oauth");
        let authorization = application
            .plugins
            .supervisor()?
            .invoke_callback(
                context.generation(),
                &method_owner,
                authorize,
                inputs,
                CallAuthority::Inherit {
                    parent: call.grant.clone(),
                    call: attempt_id.clone(),
                },
            )
            .map_err(|error| error.to_string())?;
        let created = now(application);
        let expires = expiry(
            created,
            authorization.get("expiresAt").and_then(Value::as_i64),
        )?;
        let mode = authorization
            .get("mode")
            .and_then(Value::as_str)
            .ok_or_else(|| "OAuth authorization mode is absent".to_owned())?;
        let public = json!({
            "attemptID": attempt_id,
            "url": authorization.get("url").cloned().unwrap_or(Value::Null),
            "instructions": authorization.get("instructions").cloned().unwrap_or(Value::Null),
            "mode": mode,
            "time": {"created": created, "expires": expires},
        });
        let mut attempt = OauthAttempt {
            generation: context.generation(),
            requester: authorized.owner.to_owned(),
            method_owner,
            integration_id: integration_id.to_owned(),
            method_id: method_id.to_owned(),
            label: label(payload, integration_id)?,
            created,
            expires,
            state: OauthState::Completing,
        };
        match mode {
            "code" => {
                let callback = authorization
                    .get("credentialCallback")
                    .and_then(Value::as_u64)
                    .filter(|callback| *callback > 0)
                    .ok_or_else(|| "OAuth credential callback is absent".to_owned())?;
                attempt.state = OauthState::PendingCode { callback };
            }
            "auto" => {
                let credential = authorization
                    .get("credential")
                    .cloned()
                    .ok_or_else(|| "auto OAuth authorization omitted its credential".to_owned())?;
                attempt.state = match self.persist_oauth(application, &attempt, credential) {
                    Ok(()) => OauthState::Complete,
                    Err(message) => OauthState::Failed { message },
                };
            }
            other => return Err(format!("unsupported OAuth authorization mode: {other}")),
        }
        let mut attempts = lock(&self.integration.oauth);
        if attempts.len() >= MAX_ATTEMPTS {
            return Err(format!(
                "OAuth attempts exceed the {MAX_ATTEMPTS}-attempt bound"
            ));
        }
        attempts.insert(attempt_id, attempt);
        Ok(public)
    }

    fn oauth_complete(
        &self,
        context: &PluginHostContext,
        application: &super::super::DogfoodApplication,
        authorized: &Authorized<'_>,
        payload: &Value,
    ) -> Result<Value, String> {
        let call = authorized.call()?;
        let attempt_id = attempt_id(payload, "OAuth completion")?.to_owned();
        let code = payload
            .get("code")
            .and_then(Value::as_str)
            .filter(|code| !code.is_empty() && code.len() <= 16 * 1_024)
            .ok_or_else(|| format!("OAuth code required: {attempt_id}"))?
            .to_owned();
        let (owner, callback) = {
            let mut attempts = lock(&self.integration.oauth);
            let attempt = owned_by(
                attempts.get_mut(&attempt_id),
                context,
                authorized.owner,
                &attempt_id,
            )?;
            if now(application) >= attempt.expires {
                attempt.state = OauthState::Expired;
                return Err(format!("OAuth attempt expired: {attempt_id}"));
            }
            let OauthState::PendingCode { callback } = attempt.state else {
                return Err(format!("OAuth attempt is not pending: {attempt_id}"));
            };
            attempt.state = OauthState::Completing;
            (attempt.method_owner.clone(), callback)
        };
        let credential = application
            .plugins
            .supervisor()?
            .invoke_callback(
                context.generation(),
                &owner,
                callback,
                json!(code),
                CallAuthority::Inherit {
                    parent: call.grant.clone(),
                    call: attempt_id.clone(),
                },
            )
            .map_err(|error| error.to_string());
        let mut attempts = lock(&self.integration.oauth);
        let attempt = attempts
            .get_mut(&attempt_id)
            .ok_or_else(|| format!("OAuth attempt disappeared during completion: {attempt_id}"))?;
        let outcome =
            credential.and_then(|credential| self.persist_oauth(application, attempt, credential));
        match outcome {
            Ok(()) => {
                attempt.state = OauthState::Complete;
                Ok(Value::Null)
            }
            Err(message) => {
                let message = bounded_error(message);
                attempt.state = OauthState::Failed {
                    message: message.clone(),
                };
                Err(message)
            }
        }
    }

    fn persist_oauth(
        &self,
        application: &super::super::DogfoodApplication,
        attempt: &OauthAttempt,
        credential: Value,
    ) -> Result<(), String> {
        let encoded = serde_json::to_vec(&credential)
            .map_err(|_| "OAuth credential could not be encoded".to_owned())?;
        if encoded.len() > MAX_CREDENTIAL_BYTES {
            return Err(format!(
                "OAuth credential exceeds the {MAX_CREDENTIAL_BYTES}-byte bound"
            ));
        }
        IntegrationCredentialAuthority::new()
            .connect_oauth(
                application.lock_inbox().store_mut(),
                IntegrationCredentialOwner {
                    plugin: &attempt.requester,
                    now_ms: now(application),
                },
                grapt_platform::IntegrationOAuthWrite {
                    id: mint(application, "crd"),
                    integration_id: attempt.integration_id.clone(),
                    label: attempt.label.clone(),
                    method_id: attempt.method_id.clone(),
                    credential: Secret::new(credential),
                },
            )
            .map(|_| ())
            .map_err(bounded_error)
    }

    fn oauth_status(
        &self,
        context: &PluginHostContext,
        application: &super::super::DogfoodApplication,
        authorized: &Authorized<'_>,
        payload: &Value,
    ) -> Result<Value, String> {
        let attempt_id = attempt_id(payload, "OAuth status")?;
        let mut attempts = lock(&self.integration.oauth);
        let attempt = owned_by(
            attempts.get_mut(attempt_id),
            context,
            authorized.owner,
            attempt_id,
        )?;
        if now(application) >= attempt.expires
            && matches!(attempt.state, OauthState::PendingCode { .. })
        {
            attempt.state = OauthState::Expired;
        }
        let (status, message) = match &attempt.state {
            OauthState::PendingCode { .. } | OauthState::Completing => ("pending", None),
            OauthState::Complete => ("complete", None),
            OauthState::Failed { message } => ("failed", Some(message.clone())),
            OauthState::Expired => ("expired", None),
        };
        let mut value = json!({
            "status": status,
            "time": {"created": attempt.created, "expires": attempt.expires},
        });
        if let Some(message) = message {
            value["message"] = json!(message);
        }
        Ok(value)
    }

    pub(crate) fn integration_command(
        &self,
        context: &PluginHostContext,
        operation: HostOperation,
        application: &super::super::DogfoodApplication,
        authorized: &Authorized<'_>,
        payload: &Value,
    ) -> Result<Value, String> {
        match operation {
            HostOperation::IntegrationCommandConnect => {
                self.command_connect(context, application, authorized, payload)
            }
            HostOperation::IntegrationCommandStatus => {
                let attempt_id = attempt_id(payload, "command status")?;
                let mut attempts = lock(&self.integration.commands);
                let attempt = attempts
                    .get_mut(attempt_id)
                    .filter(|attempt| {
                        attempt.generation == context.generation()
                            && attempt.requester == authorized.owner
                    })
                    .ok_or_else(|| format!("command attempt not found: {attempt_id}"))?;
                if now(application) >= attempt.expires
                    && matches!(attempt.state, CommandState::Pending)
                {
                    if let Some(abort) = attempt.abort.take() {
                        abort.abort();
                    }
                    attempt.state = CommandState::Expired;
                }
                let (status, message) = match &attempt.state {
                    CommandState::Pending => ("pending", None),
                    CommandState::Complete => ("complete", None),
                    CommandState::Failed { message } => ("failed", Some(message.clone())),
                    CommandState::Expired => ("expired", None),
                };
                let mut value = json!({
                    "status": status,
                    "integrationID": attempt.integration_id,
                    "time": {"created": attempt.created, "expires": attempt.expires},
                });
                if let Some(message) = message {
                    value["message"] = json!(message);
                }
                Ok(value)
            }
            HostOperation::IntegrationCommandCancel => {
                let attempt_id = attempt_id(payload, "command cancel")?;
                let mut attempts = lock(&self.integration.commands);
                let owned = attempts.get(attempt_id).is_some_and(|attempt| {
                    attempt.generation == context.generation()
                        && attempt.requester == authorized.owner
                });
                if !owned {
                    return Err(format!("command attempt not found: {attempt_id}"));
                }
                if let Some(attempt) = attempts.remove(attempt_id) {
                    if let Some(abort) = attempt.abort {
                        abort.abort();
                    }
                }
                Ok(Value::Null)
            }
            _ => Err("command integration dispatch changed".to_owned()),
        }
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn command_connect(
        &self,
        context: &PluginHostContext,
        application: &super::super::DogfoodApplication,
        authorized: &Authorized<'_>,
        payload: &Value,
    ) -> Result<Value, String> {
        let scope = authorized.call()?.scope;
        let message = scope.message.clone().ok_or_else(|| {
            "integration.command.connect requires a plugin tool call to attribute its effect"
                .to_owned()
        })?;
        let (integration_id, method_id) = coordinates(payload)?;
        let argv = application
            .plugins
            .integration_registry()?
            .methods
            .iter()
            .find_map(|method| match method {
                IntegrationMethod::Command {
                    integration_id: held_integration,
                    method_id: held_method,
                    command,
                } if held_integration == integration_id && held_method == method_id => {
                    Some(command.clone())
                }
                _ => None,
            })
            .ok_or_else(|| format!("Command method not found: {integration_id}/{method_id}"))?;
        let created = now(application);
        let expires = expiry(created, None)?;
        let attempt_id = mint(application, "command");
        let wake = application.wake_slots.reserve(&scope.session)?;
        {
            let mut attempts = lock(&self.integration.commands);
            if attempts.len() >= MAX_ATTEMPTS {
                return Err(format!(
                    "command attempts exceed the {MAX_ATTEMPTS}-attempt bound"
                ));
            }
            attempts.insert(
                attempt_id.clone(),
                CommandAttempt {
                    generation: context.generation(),
                    requester: authorized.owner.to_owned(),
                    integration_id: integration_id.to_owned(),
                    created,
                    expires,
                    state: CommandState::Pending,
                    abort: None,
                },
            );
        }
        let attempts = Arc::clone(&self.integration.commands);
        let weak = Arc::downgrade(&self.application()?);
        let id = attempt_id.clone();
        let integration = integration_id.to_owned();
        let owner = authorized.owner.to_owned();
        let attempt_label = label(payload, integration_id)?;
        let session = scope.session.clone();
        let agent = scope.agent.clone();
        let principal = scope.principal.clone();
        let task = self.handle.spawn(async move {
            let _completion = wake.completion();
            let outcome = wake
                .run_activity(async move {
                    let application = weak.upgrade().ok_or_else(|| {
                        "integration command application is unavailable".to_owned()
                    })?;
                    let credential = application
                        .executor
                        .0
                        .plugin_integration_command(session, message, agent, principal, argv)
                        .await?;
                    let credential = credential.trim();
                    if credential.is_empty() || credential.len() > MAX_CREDENTIAL_BYTES {
                        return Err("integration command returned no bounded credential".to_owned());
                    }
                    let result = IntegrationCredentialAuthority::new()
                        .connect_key(
                            application.lock_inbox().store_mut(),
                            IntegrationCredentialOwner {
                                plugin: &owner,
                                now_ms: now(&application),
                            },
                            mint(&application, "crd"),
                            integration,
                            attempt_label,
                            Secret::new(credential.to_owned()),
                        )
                        .map(|_| ())
                        .map_err(bounded_error);
                    result
                })
                .await;
            let mut attempts = lock(&attempts);
            let Some(attempt) = attempts.get_mut(&id) else {
                return;
            };
            if !matches!(attempt.state, CommandState::Pending) {
                return;
            }
            attempt.abort = None;
            attempt.state = match outcome {
                Ok(()) => CommandState::Complete,
                Err(message) => CommandState::Failed { message },
            };
        });
        if let Some(attempt) = lock(&self.integration.commands).get_mut(&attempt_id) {
            if matches!(attempt.state, CommandState::Pending) {
                attempt.abort = Some(task.abort_handle());
            }
        }
        Ok(json!({"attemptID": attempt_id, "time": {"created": created, "expires": expires}}))
    }
}
