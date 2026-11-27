//! In-app setup owns connections before the first execution application exists.
use super::{
    support::{decode, invalid},
    DogfoodApplication,
};
use grapt_api::{u3a::*, SealedPrincipal};
use grapt_connections::{
    catalog::KeyProvider,
    store::{Connections, Store},
};
use grapt_login::vault::VaultSecret;
use grapt_types::ClockAuthority;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

mod azure;
mod bedrock;
mod credential;
mod install;
mod key;
mod oauth;
mod status;
mod vertex;
mod watsonx;

#[derive(Clone)]
pub(crate) struct ConnectionSetup {
    root: PathBuf,
    activate: bool,
    external_host: bool,
    oauth: Arc<oauth::Runtime>,
    application: Arc<Mutex<Option<Arc<DogfoodApplication>>>>,
    mutation: Arc<Mutex<()>>,
    operations: Arc<tokio::sync::Semaphore>,
    ports: Arc<ApplicationSlot>,
    hub: Arc<DogfoodEventHub>,
    limits: super::production::ProductionProviderLimits,
    driver: Arc<Mutex<Option<tokio::sync::oneshot::Sender<super::prompt_driver::PromptDriver>>>>,
}

impl ConnectionSetup {
    pub(super) fn command_refusal<T: Send + 'static>(
        &self,
        principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
        message: &'static str,
    ) -> ApplicationFuture<T, InvalidRequestErrorDto> {
        let location = self.location(principal, query);
        Box::pin(async move {
            let _location = location.await?;
            Err(invalid(message))
        })
    }
    pub(crate) fn pending(
        root: &Path,
        hub: Arc<DogfoodEventHub>,
        limits: super::production::ProductionProviderLimits,
        external_host: bool,
    ) -> (
        Arc<Self>,
        tokio::sync::oneshot::Receiver<super::prompt_driver::PromptDriver>,
    ) {
        Self::pending_with_activation(root, hub, limits, true, external_host)
    }

    /// A temporary operator service can repair sign-in without opening old credentials.
    pub(crate) fn administration(
        root: &Path,
        hub: Arc<DogfoodEventHub>,
        limits: super::production::ProductionProviderLimits,
        external_host: bool,
    ) -> (
        Arc<Self>,
        tokio::sync::oneshot::Receiver<super::prompt_driver::PromptDriver>,
    ) {
        Self::pending_with_activation(root, hub, limits, false, external_host)
    }

    fn pending_with_activation(
        root: &Path,
        hub: Arc<DogfoodEventHub>,
        limits: super::production::ProductionProviderLimits,
        activate: bool,
        external_host: bool,
    ) -> (
        Arc<Self>,
        tokio::sync::oneshot::Receiver<super::prompt_driver::PromptDriver>,
    ) {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        (
            Arc::new(Self {
                root: root.into(),
                activate,
                external_host,
                oauth: Arc::new(oauth::Runtime::default()),
                application: Arc::new(Mutex::new(None)),
                mutation: Arc::new(Mutex::new(())),
                operations: Arc::new(tokio::sync::Semaphore::new(1)),
                ports: Arc::new(ApplicationSlot::default()),
                hub,
                limits,
                driver: Arc::new(Mutex::new(Some(sender))),
            }),
            receiver,
        )
    }

    pub(crate) fn configured(
        root: &Path,
        application: Arc<DogfoodApplication>,
        ports: Arc<ApplicationSlot>,
        hub: Arc<DogfoodEventHub>,
        limits: super::production::ProductionProviderLimits,
        external_host: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            root: root.into(),
            activate: true,
            external_host,
            oauth: Arc::new(oauth::Runtime::default()),
            application: Arc::new(Mutex::new(Some(application))),
            mutation: Arc::new(Mutex::new(())),
            operations: Arc::new(tokio::sync::Semaphore::new(1)),
            ports,
            hub,
            limits,
            driver: Arc::new(Mutex::new(None)),
        })
    }

    pub(crate) fn ports(&self) -> Arc<ApplicationSlot> {
        Arc::clone(&self.ports)
    }

    fn application(&self) -> Option<Arc<DogfoodApplication>> {
        match self.application.lock() {
            Ok(application) => application.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

impl IntegrationApplicationPort for ConnectionSetup {
    fn authentication_status(
        &self,
        principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<AuthenticationStatusResponseDto, InvalidRequestErrorDto> {
        self.authentication_status_owned(principal, query)
    }

    fn connect_command(
        &self,
        principal: SealedPrincipal,
        _id: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        _request: IntegrationCommandConnectRequestDto,
    ) -> ApplicationFuture<IntegrationCommandConnectResponseDto, InvalidRequestErrorDto> {
        self.command_refusal(
            principal,
            query,
            "this integration has no supported command method",
        )
    }
    fn command_status(
        &self,
        principal: SealedPrincipal,
        _id: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        _attempt: IntegrationAttemptIdDto,
    ) -> ApplicationFuture<IntegrationCommandStatusResponseDto, InvalidRequestErrorDto> {
        self.command_refusal(principal, query, "command attempt is unavailable")
    }
    fn cancel_command(
        &self,
        principal: SealedPrincipal,
        _id: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        _attempt: IntegrationAttemptIdDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        let location = self.location(principal, query);
        Box::pin(async move {
            let _location = location.await?;
            Ok(NoContent)
        })
    }
    fn remove_credential(
        &self,
        principal: SealedPrincipal,
        id: CredentialIdDto,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        self.remove_credential_owned(principal, id, query)
    }

    fn update_credential(
        &self,
        principal: SealedPrincipal,
        id: CredentialIdDto,
        query: ProjectCurrentLocationQueryDto,
        request: CredentialUpdateRequestDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        self.update_credential_owned(principal, id, query, request)
    }

    fn connect_oauth(
        &self,
        principal: SealedPrincipal,
        id: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        request: IntegrationOAuthConnectRequestDto,
        connection: Option<DtoString>,
    ) -> ApplicationFuture<IntegrationOAuthConnectResponseDto, InvalidRequestErrorDto> {
        self.connect_oauth_owned(principal, id, query, request, connection)
    }
    fn oauth_status(
        &self,
        principal: SealedPrincipal,
        id: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        attempt: IntegrationAttemptIdDto,
    ) -> ApplicationFuture<IntegrationOAuthStatusResponseDto, InvalidRequestErrorDto> {
        self.oauth_status_owned(principal, id, query, attempt)
    }
    fn complete_oauth(
        &self,
        principal: SealedPrincipal,
        id: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        attempt: IntegrationAttemptIdDto,
        request: IntegrationOAuthCompleteRequestDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        self.complete_oauth_owned(principal, id, query, attempt, request)
    }
    fn cancel_oauth(
        &self,
        principal: SealedPrincipal,
        id: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        attempt: IntegrationAttemptIdDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        self.cancel_oauth_owned(principal, id, query, attempt)
    }
    fn location(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<LocationInfoDto, InvalidRequestErrorDto> {
        let root = self.root.clone();
        Box::pin(async move {
            let location = super::locations::resolve_default(&root.to_string_lossy(), &query)
                .await
                .map_err(invalid)?;
            decode(location.wire()).map_err(invalid)
        })
    }
    fn list(
        &self,
        _principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<IntegrationListResponseDto, InvalidRequestErrorDto> {
        if let Some(application) = self.application() {
            return super::connections::integrations(&application, query);
        }
        let root = self.root.clone();
        Box::pin(async move {
            let location = super::locations::resolve_default(&root.to_string_lossy(), &query)
                .await
                .map_err(invalid)?;
            let records = Store::read_snapshot(&root)
                .map_err(invalid)?
                .ok_or_else(|| invalid("setup connection metadata is missing"))?;
            let data = integration_rows(&records, None).map_err(invalid)?;
            decode(serde_json::json!({"location":location.wire(),"data":data})).map_err(invalid)
        })
    }

    fn get(
        &self,
        principal: SealedPrincipal,
        id: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<IntegrationGetResponseDto, InvalidRequestErrorDto> {
        let listed = self.list(principal, query);
        Box::pin(async move {
            let listed = listed.await?;
            let mut value = serde_json::to_value(listed).map_err(invalid)?;
            value["data"] = value["data"]
                .as_array()
                .and_then(|rows| {
                    rows.iter()
                        .find(|row| row["id"].as_str() == Some(id.as_str()))
                })
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            decode(value).map_err(invalid)
        })
    }

    fn connect_key(
        &self,
        _principal: SealedPrincipal,
        id: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        request: IntegrationKeyConnectRequestDto,
        connection: Option<DtoString>,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        self.connect_key_owned(
            id,
            query,
            key::KeyRequest::Entered(Box::new(request)),
            connection,
        )
    }
    fn connect_environment(
        &self,
        _principal: SealedPrincipal,
        id: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        request: IntegrationEnvironmentConnectRequestDto,
        connection: Option<DtoString>,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        self.connect_key_owned(id, query, key::KeyRequest::Environment(request), connection)
    }
    fn oauth_challenge(
        &self,
        principal: SealedPrincipal,
        id: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        attempt: IntegrationAttemptIdDto,
    ) -> ApplicationFuture<IntegrationOAuthChallengeResponseDto, InvalidRequestErrorDto> {
        self.oauth_challenge_owned(principal, id, query, attempt)
    }
}

fn authorization_failed() -> InvalidRequestErrorDto {
    super::support::static_dto(serde_json::json!({
        "_tag":"InvalidRequestError", "kind":"integration_authorization", "message":"Authentication failed",
    }))
}

pub(super) fn integration_rows(
    records: &Connections,
    legacy: Option<(&str, &str)>,
) -> Result<serde_json::Value, String> {
    use serde_json::json;
    let mut rows = std::collections::BTreeMap::new();
    for integration in grapt_connections::catalog::integrations()? {
        let mut methods = Vec::new();
        if let Some(provider) = integration.key {
            methods.push(if provider.family().is_vertex() {
                json!({"type":"key","label":"Service-account JSON"})
            } else {
                json!({"type":"key"})
            });
            let names: Vec<_> = provider
                .environment_names()?
                .into_iter()
                .filter(|name| std::env::var_os(name).is_some())
                .collect();
            if !names.is_empty() {
                methods.push(json!({"type":"env","names":names}));
            }
        }
        rows.insert(
            integration.id.to_owned(),
            json!({"id":integration.id,"name":integration.name,
            "methods":methods,"connections":[]}),
        );
    }
    if let Some(openai) = rows.get_mut("openai") {
        openai["methods"]
            .as_array_mut()
            .ok_or("invalid OpenAI authentication methods")?
            .push(json!({"type":"oauth","id":"chatgpt-device","label":"ChatGPT (device code)"}));
        openai["methods"]
            .as_array_mut()
            .ok_or("invalid OpenAI authentication methods")?
            .insert(
                0,
                json!({"type":"oauth","id":"chatgpt-browser","label":"ChatGPT (browser)"}),
            );
    }
    for record in &records.connections {
        let id = record.catalog_provider.as_str();
        let row = rows
            .entry(id.into())
            .or_insert_with(|| json!({"id":id,"name":id,"methods":[],"connections":[]}));
        let credential_id = legacy
            .filter(|(provider, _)| {
                *provider == record.id.as_str()
                    && record.credential_format
                        == grapt_connections::store::CredentialFormat::LegacyFile
            })
            .map(|(_, id)| id.to_owned())
            .unwrap_or_else(|| format!("cred_{}", record.id.as_str()));
        let connection = match &record.method {
            grapt_connections::store::AuthMethod::Env { name } => {
                json!({"type":"env","name":name})
            }
            _ => json!({"type":"credential","id":credential_id,"label":record.label}),
        };
        row["connections"]
            .as_array_mut()
            .ok_or("invalid integration projection")?
            .push(connection);
    }
    let mut rows: Vec<_> = rows.into_values().collect();
    rows.sort_by_cached_key(|row| row["name"].as_str().unwrap_or_default().to_lowercase());
    Ok(serde_json::Value::Array(rows))
}
