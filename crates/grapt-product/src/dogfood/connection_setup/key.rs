//! Both key inputs share verification, source approval and generation publication.
use super::*;
mod cloudflare;
mod setup;
#[cfg(test)]
mod tests;

pub(super) enum KeyRequest {
    Entered(Box<IntegrationKeyConnectRequestDto>),
    Environment(IntegrationEnvironmentConnectRequestDto),
}
enum SecretInput {
    ApiKey(VaultSecret),
    VertexAdc,
    Vertex(grapt_login::vertex::ServiceAccount),
    Watsonx(grapt_login::watsonx::ApiKey),
}
impl ConnectionSetup {
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(super) fn connect_key_owned(
        &self,
        id: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        request: KeyRequest,
        target: Option<DtoString>,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        let owner = self.clone();
        Box::pin(async move {
            let mut provider = KeyProvider::from_id(id.as_str())
                .map_err(invalid)?
                .ok_or_else(|| invalid("this integration has no supported API key method"))?;
            let bedrock = match &request {
                KeyRequest::Entered(request) => {
                    super::bedrock::prepare(provider, request.bedrock.as_present())
                        .map_err(invalid)?
                }
                KeyRequest::Environment(_)
                    if provider.family() == grapt_connections::store::Family::BedrockConverse =>
                {
                    return Err(invalid("Bedrock requires the in-app Connect form"))
                }
                KeyRequest::Environment(_) => None,
            };
            let azure = match &request {
                KeyRequest::Entered(request) => {
                    super::azure::prepare(provider, request.azure.as_present()).map_err(invalid)?
                }
                KeyRequest::Environment(_) if provider.is_azure() => {
                    return Err(invalid("Azure requires the in-app Connect form"));
                }
                KeyRequest::Environment(_) => None,
            };
            let mut vertex = None;
            let mut watsonx = None;
            let (key, label, environment, setup, cloudflare) = match request {
                KeyRequest::Entered(request) => {
                    let request = *request;
                    if provider.family().is_vertex() {
                        vertex = Some(
                            super::vertex::Setup::new(
                                provider,
                                request.vertex.into_present().ok_or_else(|| {
                                    invalid(
                                        "enter the Vertex project, location and model in Connect",
                                    )
                                })?,
                            )
                            .map_err(invalid)?,
                        );
                    } else if !request.vertex.is_absent() {
                        return Err(invalid("Vertex setup belongs to a Vertex connection"));
                    }
                    if provider.family() == grapt_connections::store::Family::WatsonxChat {
                        watsonx = Some(
                            super::watsonx::Setup::new(
                                provider,
                                request.watsonx.into_present().ok_or_else(|| {
                                    invalid("enter the IBM region, project and model in Connect")
                                })?,
                            )
                            .map_err(invalid)?,
                        );
                    } else if !request.watsonx.is_absent() {
                        return Err(invalid("watsonx setup belongs to a watsonx connection"));
                    }
                    let cloudflare =
                        cloudflare::scope(id.as_str(), request.cloudflare.into_present())?;
                    let setup = setup::ModelSetup::entered(
                        provider,
                        request.custom.into_present(),
                        request.databricks.into_present(),
                    )?;
                    let mut key = request.key.into_string();
                    if key.is_empty() && setup.as_ref().is_some_and(|setup| setup.permits_empty_key)
                    {
                        // The endpoint check above limits plaintext to literal loopback.
                        // Local servers commonly ignore the required bearer field.
                        key = "local".into();
                    }
                    let key = if vertex
                        .as_ref()
                        .is_some_and(|setup| setup.application_default)
                    {
                        if !key.is_empty() {
                            return Err(invalid("choose application-default credentials or a service-account key, not both"));
                        }
                        SecretInput::VertexAdc
                    } else if let Some(selection) =
                        bedrock.as_ref().and_then(|prepared| prepared.aws.as_ref())
                    {
                        if !key.is_empty() {
                            return Err(invalid(
                                "choose AWS credential chain or a Bedrock bearer token, not both",
                            ));
                        }
                        SecretInput::ApiKey(
                            VaultSecret::aws_chain(selection.clone()).map_err(invalid)?,
                        )
                    } else if azure.as_ref().is_some_and(|prepared| prepared.cli) {
                        if !key.is_empty() {
                            return Err(invalid(
                                "choose Azure CLI authorization or an API key, not both",
                            ));
                        }
                        SecretInput::ApiKey(VaultSecret::azure_cli())
                    } else if watsonx.is_some() {
                        SecretInput::Watsonx(
                            grapt_login::watsonx::ApiKey::new(key)
                                .map_err(|_| authorization_failed())?,
                        )
                    } else if vertex.is_some() {
                        SecretInput::Vertex(grapt_login::vertex::ServiceAccount::new(key).map_err(
                            |_| invalid("paste a valid Google service-account JSON key"),
                        )?)
                    } else {
                        SecretInput::ApiKey(VaultSecret::api_key(key.into()).map_err(|_| {
                            cloudflare::key_error(id.as_str(), grapt_login::key::KeyError::Invalid)
                        })?)
                    };
                    (
                        key,
                        request
                            .label
                            .into_present()
                            .map(|label| label.into_string()),
                        None,
                        setup,
                        cloudflare,
                    )
                }
                KeyRequest::Environment(request) => {
                    let name = request.name.into_string();
                    if !provider.allows_environment(&name).map_err(invalid)? {
                        return Err(invalid(
                            "this variable is not a supported credential source for the provider",
                        ));
                    }
                    let value = std::env::var_os(&name).and_then(|value| value.into_string().ok())
                        .ok_or_else(|| invalid("credential variable is not available to the service; restart with it exported or use --key-stdin"))?;
                    (
                        SecretInput::ApiKey(
                            VaultSecret::api_key(value.into())
                                .map_err(|_| authorization_failed())?,
                        ),
                        request
                            .label
                            .into_present()
                            .map(|label| label.into_string()),
                        Some(name),
                        None,
                        None,
                    )
                }
            };
            if let Some(model) = setup.as_ref().and_then(|setup| setup.model.as_ref()) {
                provider = provider.for_model(model.as_str());
            }
            if let Some(scope) = &cloudflare {
                provider = provider.with_cloudflare(scope).map_err(invalid)?;
            }
            let permit = Arc::clone(&owner.operations)
                .try_acquire_owned()
                .map_err(|_| invalid("connections are being changed; retry shortly"))?;
            super::super::locations::resolve_default(&owner.root.to_string_lossy(), &query)
                .await
                .map_err(invalid)?;
            let root = owner.root.clone();
            let (before, permit) = tokio::task::spawn_blocking(move || {
                let before = Store::read_snapshot(&root)?;
                grapt_platform::profile_configuration::ConnectionChange::check_source(
                    &root,
                    before.as_ref(),
                )?;
                Ok::<_, std::io::Error>((before, permit))
            })
            .await
            .map_err(|_| invalid("connection configuration owner stopped"))?
            .map_err(invalid)?;
            // An endpoint's wire is known only once its model is checked, so
            // its connection identity is settled after the check, against the
            // wire the check chose.
            let known = if setup.is_none() {
                Some(
                    install::connection_id(
                        before.as_ref(),
                        provider,
                        target.as_ref().map(DtoString::as_str),
                    )
                    .map_err(invalid)?,
                )
            } else {
                None
            };
            let base = cloudflare
                .as_ref()
                .map(|scope| scope.api_base())
                .or_else(|| {
                    azure
                        .as_ref()
                        .map(|prepared| prepared.base.clone())
                        .or_else(|| setup.as_ref().map(|setup| setup.endpoint.clone()))
                        .or_else(|| bedrock.as_ref().map(|prepared| prepared.base.clone()))
                })
                .or_else(|| {
                    before
                        .as_ref()
                        .and_then(|records| {
                            records
                                .connections
                                .iter()
                                .find(|record| Some(&record.id) == known.as_ref())
                        })
                        .and_then(|record| record.endpoint.clone())
                })
                .unwrap_or_else(|| provider.api_base().into());
            let base = vertex.as_ref().map_or(base, |setup| setup.base.clone());
            let base = watsonx.as_ref().map_or(base, |setup| setup.base.clone());
            #[cfg(feature = "loopback-provider-testkit")]
            let base = if let Some(fixture) = std::env::var_os("GRAPT_LIVE_HEADLESS_LOOPBACK") {
                let fixture = fixture
                    .into_string()
                    .map_err(|_| invalid("invalid loopback fixture endpoint"))?;
                grapt_provider::LoopbackBase::parse(&fixture)
                    .map_err(|_| invalid("invalid loopback fixture endpoint"))?;
                if let Some(scope) = &cloudflare {
                    grapt_login::key::cloudflare::fixture_base(scope, &fixture)
                        .map_err(|_| invalid("invalid Cloudflare fixture origin"))?
                } else {
                    fixture
                }
            } else {
                base
            };
            let label = label.unwrap_or_else(|| match &cloudflare {
                Some(scope) => format!("{} ({})", provider.name(), scope.provider().as_str()),
                None => provider.name().into(),
            });
            let checked_completion = setup.is_some()
                || azure.is_some()
                || bedrock.is_some()
                || cloudflare.is_some()
                || vertex.is_some()
                || watsonx.is_some();
            let verified = match (vertex, watsonx, key) {
                (None, Some(setup), SecretInput::Watsonx(key)) => grapt_login::key::verify_watsonx(&base, key, setup.project, &setup.model, setup.listing, &grapt_store::SystemClock).await.map_err(|error| match error {
                    grapt_login::key::KeyError::Unreachable => invalid("watsonx verification is unavailable; retry Connect shortly"),
                    _ => invalid("watsonx authorization failed; check the IBM Cloud API key, project permissions, region and model, then retry Connect"),
                })?,
                (Some(setup), None, SecretInput::VertexAdc) => grapt_login::key::verify_vertex_adc(
                    &base, &setup.model,
                    if provider.family() == grapt_connections::store::Family::VertexAnthropic {
                        grapt_login::key::VertexProtocol::Anthropic
                    } else { grapt_login::key::VertexProtocol::Gemini },
                    setup.listing,
                ).await.map_err(|_| crate::dogfood::support::static_dto(serde_json::json!({
                    "_tag":"InvalidRequestError", "kind":grapt_api::u3a::INTEGRATION_GOOGLE_ADC_KIND,
                    "message":grapt_api::u3a::INTEGRATION_GOOGLE_ADC_MESSAGE,
                })))?,
                (Some(setup), None, SecretInput::Vertex(key)) => grapt_login::key::verify_vertex(
                    &base,
                    key,
                    &setup.model,
                    if provider.family() == grapt_connections::store::Family::VertexAnthropic {
                        grapt_login::key::VertexProtocol::Anthropic
                    } else {
                        grapt_login::key::VertexProtocol::Gemini
                    },
                    setup.listing,
                    &grapt_store::SystemClock,
                )
                .await
                .map_err(|_| invalid("Vertex authorization failed; check the service-account key, project permissions, model location and system clock, then retry Connect"))?,
                (None, None, SecretInput::ApiKey(secret)) if setup.is_some() => match setup {
                    Some(setup) => {
                        // Reconnecting a named connection keeps its wire.
                        let wire = target.as_ref().and_then(|target| {
                            before.as_ref()?.connections.iter().find_map(|record| {
                                (record.id.as_str() == target.as_str()
                                    && record.catalog_provider.as_str() == provider.id())
                                .then_some(record.family)
                            })
                        });
                        let (verified, checked) =
                            setup.verify(provider, wire, &base, secret, id.as_str()).await?;
                        // The checked model's wire, which a discovered model
                        // may select, is the wire the connection installs.
                        provider = checked;
                        verified
                    }
                    None => return Err(invalid("enter the API base URL")),
                },
                (None, None, SecretInput::ApiKey(secret)) => {
                    if let Some(scope) = cloudflare {
                        let listing = provider.cloudflare_listing().map_err(invalid)?;
                        let model =
                            grapt_types::ModelId::new(provider.default_model(&listing).map_err(invalid)?)
                                .ok_or_else(|| invalid("invalid Cloudflare default model"))?;
                        grapt_login::key::cloudflare::verify(&base, secret, scope, model, listing).await
                    } else if let Some(azure) = azure {
                        grapt_login::key::azure::verify(&base, secret, &azure.deployment, azure.listing)
                            .await
                    } else if let Some(super::bedrock::Prepared { model, listing, .. }) = bedrock {
                        grapt_login::key::bedrock::verify(&base, &model, secret, listing, &grapt_store::SystemClock).await
                    } else {
                        grapt_login::key::verify(&base, provider.placement(), secret).await
                    }
                    .map_err(|error| verification_failed(id.as_str(), error))?
                }
                _ => return Err(invalid("credential does not match this integration")),
            };
            let verified = if !checked_completion {
                let listing = provider
                    .supported_listing(verified.listing())
                    .map_err(invalid)?;
                let model = provider.default_model(&listing).map_err(invalid)?;
                let protocol = match provider.family() {
                    grapt_connections::store::Family::GoogleGemini => {
                        grapt_login::key::verify_completion(&base, &verified, &model)
                            .await
                            .map_err(|error| verification_failed(id.as_str(), error))?;
                        None
                    }
                    grapt_connections::store::Family::CohereChat => {
                        grapt_login::key::verify_cohere_completion(&base, &verified, &model)
                            .await
                            .map_err(|error| verification_failed(id.as_str(), error))?;
                        None
                    }
                    grapt_connections::store::Family::OpenaiResponses => {
                        Some(grapt_login::key::KeyProtocol::Responses)
                    }
                    grapt_connections::store::Family::AnthropicMessages => {
                        Some(grapt_login::key::KeyProtocol::Messages)
                    }
                    grapt_connections::store::Family::OpenaiCompatible => {
                        Some(if provider.id() == "groq" {
                            grapt_login::key::KeyProtocol::GroqChat
                        } else {
                            grapt_login::key::KeyProtocol::Chat
                        })
                    }
                    grapt_connections::store::Family::BedrockConverse => {
                        return Err(invalid("Bedrock requires its native model verification"))
                    }
                    grapt_connections::store::Family::VertexGemini
                    | grapt_connections::store::Family::VertexAnthropic
                    | grapt_connections::store::Family::WatsonxChat => {
                        return Err(invalid(
                            "this provider requires its native model verification",
                        ))
                    }
                    grapt_connections::store::Family::ChatgptCodex => {
                        return Err(invalid("use the ChatGPT sign-in method"))
                    }
                };
                if let Some(protocol) = protocol {
                    verified
                        .check_model(&base, protocol, &model)
                        .await
                        .map_err(|error| verification_failed(id.as_str(), error))?;
                    verified
                } else {
                    verified
                }
            } else {
                verified
            };
            let connection = match known {
                Some(connection) => connection,
                None => install::connection_id(
                    before.as_ref(),
                    provider,
                    target.as_ref().map(DtoString::as_str),
                )
                .map_err(invalid)?,
            };
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let source = match environment {
                    Some(name) => install::KeySource::Environment { name, verified },
                    None => install::KeySource::Vault(verified),
                };
                owner.install(provider, &base, label, before, source, connection)
            })
            .await
            .map_err(|_| invalid("connection storage owner stopped"))?
            .map_err(invalid)?;
            Ok(NoContent)
        })
    }
}

fn verification_failed(id: &str, error: grapt_login::key::KeyError) -> InvalidRequestErrorDto {
    if matches!(error, grapt_login::key::KeyError::AzureCredentials) {
        return crate::dogfood::support::static_dto(serde_json::json!({
            "_tag":"InvalidRequestError", "kind":grapt_api::u3a::INTEGRATION_AZURE_CREDENTIALS_KIND,
            "message":grapt_api::u3a::INTEGRATION_AZURE_CREDENTIALS_MESSAGE,
        }));
    }
    if matches!(error, grapt_login::key::KeyError::AwsProfileConfiguration) {
        return crate::dogfood::support::static_dto(serde_json::json!({
            "_tag":"InvalidRequestError", "kind":grapt_api::u3a::INTEGRATION_AWS_PROFILE_KIND,
            "message":grapt_api::u3a::INTEGRATION_AWS_PROFILE_MESSAGE,
        }));
    }
    if matches!(error, grapt_login::key::KeyError::AwsCredentials) {
        return crate::dogfood::support::static_dto(serde_json::json!({
            "_tag":"InvalidRequestError", "kind":grapt_api::u3a::INTEGRATION_AWS_CREDENTIALS_KIND,
            "message":grapt_api::u3a::INTEGRATION_AWS_CREDENTIALS_MESSAGE,
        }));
    }
    if matches!(error, grapt_login::key::KeyError::Refused) {
        return crate::dogfood::support::static_dto(serde_json::json!({
            "_tag":"InvalidRequestError",
            "kind":grapt_api::u3a::INTEGRATION_KEY_REJECTED_KIND,
            "message":"the provider rejected this key",
        }));
    }
    cloudflare::key_error(id, error)
}
