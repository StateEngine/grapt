//! Explicit endpoint coordinates share the existing bounded model check; an
//! endpoint named without a model offers every model it serves.
use super::*;

pub(super) struct ModelSetup {
    pub(super) endpoint: String,
    pub(super) model: Option<DtoString>,
    /// Present only with a model: the user's own window for it.
    pub(super) context_window: Option<u32>,
    pub(super) permits_empty_key: bool,
}

impl ModelSetup {
    pub(super) fn entered(
        provider: KeyProvider,
        custom: Option<CustomProviderRequestDto>,
        databricks: Option<DatabricksProviderRequestDto>,
    ) -> Result<Option<Self>, InvalidRequestErrorDto> {
        let setup = match (provider.id(), custom, databricks) {
            (id, Some(custom), None)
                if id == "custom"
                    || grapt_connections::catalog::endpoint_provider(id).is_some() =>
            {
                Self {
                    permits_empty_key: provider.permits_empty_local_key()
                        && custom.endpoint.as_str().starts_with("http://"),
                    endpoint: custom.endpoint.into_string(),
                    model: custom.model.into_present(),
                    context_window: custom.context_window.into_present(),
                }
            }
            ("databricks", None, Some(databricks)) => Self {
                endpoint: workspace_endpoint(databricks.workspace.as_str())?,
                model: Some(databricks.model),
                context_window: Some(databricks.context_window),
                permits_empty_key: false,
            },
            (id, _, _)
                if id == "custom"
                    || grapt_connections::catalog::endpoint_provider(id).is_some() =>
            {
                return Err(invalid("enter the API base URL"))
            }
            ("databricks", _, _) => {
                return Err(invalid(
                    "enter the Databricks workspace URL, model and context window",
                ))
            }
            (_, None, None) => return Ok(None),
            _ => {
                return Err(invalid(
                    "setup fields do not match the selected integration",
                ))
            }
        };
        if setup.context_window.is_some() && setup.model.is_none() {
            return Err(invalid(
                "a context window belongs to a named model; enter the model ID too",
            ));
        }
        if setup.model.as_ref().is_some_and(|model| {
            model.as_str().is_empty()
                || model.as_str().len() > 256
                || model.as_str().chars().any(char::is_control)
        }) || setup
            .context_window
            .is_some_and(|window| !(512..=16_777_216).contains(&window))
        {
            return Err(invalid(
                "enter a model ID and a context window between 512 and 16777216 tokens",
            ));
        }
        grapt_login::vault::ApiDestination::parse(&setup.endpoint)
            .map_err(|_| invalid("use an HTTPS endpoint or a literal loopback HTTP address"))?;
        Ok(Some(setup))
    }
}

impl ModelSetup {
    /// Check the key with one completed request, then publish what the
    /// endpoint serves. A named model with its window is that one model; any
    /// other connect offers every listed model, the checked one first. The
    /// key travels only to this base, and to nothing before its model list or
    /// completion answers. Returns the provider as the checked model's wire
    /// selects it.
    pub(super) async fn verify(
        self,
        provider: KeyProvider,
        base: &str,
        secret: VaultSecret,
        id: &str,
    ) -> Result<(grapt_login::key::VerifiedKey, KeyProvider), InvalidRequestErrorDto> {
        let failed = |error| match error {
            grapt_login::key::KeyError::Unreachable => endpoint_refusal(format!(
                "could not reach {base}; start the server there or enter its API base URL"
            )),
            grapt_login::key::KeyError::Listing => endpoint_refusal(format!(
                "the server at {base} lists no models; load or pull one, then retry"
            )),
            error => super::verification_failed(id, error),
        };
        if let (Some(model), Some(window)) = (&self.model, self.context_window) {
            let provider = provider.for_model(model.as_str());
            let listing = serde_json::to_vec(&serde_json::json!({"data":[{
                "id":model.as_str(), "max_model_len":window,
            }]}))
            .map_err(invalid)?;
            let verified = grapt_login::key::verify_model_listing(
                base,
                secret,
                model.as_str(),
                listing,
                protocol(provider),
            )
            .await
            .map_err(failed)?;
            // Only a verified key reaches the optional catalogue read.
            let listing = grapt_login::key::enrich_listing(&verified, model.as_str()).await;
            return Ok((verified.with_listing(listing), provider));
        }
        let verified =
            grapt_login::key::verify(base, grapt_login::key::KeyPlacement::Bearer, secret)
                .await
                .map_err(failed)?;
        // A named model is the one candidate. Otherwise the listing names the
        // candidates; a model that cannot complete or states no window gives
        // way to the next, so an embedding model listed first does not stop
        // the connect.
        let named = self.model.is_some();
        let candidates = match self.model {
            Some(model) => vec![model.into_string()],
            None => grapt_login::key::candidate_models(verified.listing()),
        };
        let mut refusal = endpoint_refusal(format!(
            "the server at {base} lists no models; load or pull one, then retry"
        ));
        for model in candidates {
            let provider = provider.for_model(&model);
            let protocol = protocol(provider);
            if protocol == grapt_login::key::KeyProtocol::BearerMessages {
                grapt_login::vault::MessagesDestination::parse(base)
                    .map_err(|_| invalid("use the provider's Messages API base URL"))?;
            }
            refusal = match verified.check_model(base, protocol, &model).await {
                Ok(()) => match grapt_login::key::served_listing(&verified, &model).await {
                    Ok(listing) => return Ok((verified.with_listing(listing), provider)),
                    Err(error) => served_refusal(error, named.then_some(model.as_str())),
                },
                Err(grapt_login::key::KeyError::Completion) if !named => endpoint_refusal(
                    "no model the server lists completed a check request; connect one by name"
                        .to_owned(),
                ),
                Err(error) => return Err(failed(error)),
            };
            if named {
                break;
            }
        }
        Err(refusal)
    }
}

/// The completion check's wire for this provider and model.
fn protocol(provider: KeyProvider) -> grapt_login::key::KeyProtocol {
    if provider.family() == grapt_connections::store::Family::AnthropicMessages {
        grapt_login::key::KeyProtocol::BearerMessages
    } else if grapt_connections::catalog::endpoint_provider(provider.id())
        .is_some_and(|row| row.completion_tokens)
    {
        grapt_login::key::KeyProtocol::CompletionChat
    } else {
        grapt_login::key::KeyProtocol::Chat
    }
}

/// Why a served listing cannot back a model. Only a model the user named is
/// spelled out: an identity the endpoint returned is its data, not ours, and
/// never reaches a refusal.
fn served_refusal(
    error: grapt_login::key::ServedError,
    named: Option<&str>,
) -> InvalidRequestErrorDto {
    use grapt_login::key::ServedError;
    let model = named
        .filter(|model| {
            model.len() <= 128
                && model
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._:/@+-".contains(c))
        })
        .map_or_else(|| "the model".to_owned(), |model| format!("model {model}"));
    endpoint_refusal(match error {
        ServedError::Unreadable => "the server's model list is unreadable".to_owned(),
        ServedError::NotServed => format!("the server does not list {model}; choose a model it serves"),
        ServedError::NoWindow if named.is_some() => format!(
            "the server reports no context window for {model}; enter the model's context window"
        ),
        ServedError::NoWindow => "the server reports no context window for the models it lists; load one, or connect it by name with its context window".to_owned(),
    })
}

/// A refusal in the product's own words: only the entered address and model
/// identities appear in it, never a provider's response body.
fn endpoint_refusal(message: String) -> InvalidRequestErrorDto {
    serde_json::from_value(serde_json::json!({
        "_tag": "InvalidRequestError",
        "kind": grapt_api::u3a::INTEGRATION_ENDPOINT_KIND,
        "message": message,
    }))
    .unwrap_or_else(|_| invalid("the endpoint connection could not be completed"))
}

fn workspace_endpoint(workspace: &str) -> Result<String, InvalidRequestErrorDto> {
    grapt_login::vault::ApiDestination::parse(workspace)
        .map_err(|_| invalid("use an HTTPS Databricks workspace URL"))?;
    let mut endpoint =
        url::Url::parse(workspace).map_err(|_| invalid("use an HTTPS Databricks workspace URL"))?;
    if endpoint.path() != "/" {
        return Err(invalid(
            "enter the Databricks workspace URL without an API path",
        ));
    }
    endpoint.set_path("/ai-gateway/mlflow/v1");
    Ok(endpoint.into())
}
