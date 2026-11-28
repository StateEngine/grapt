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
    /// completion answers.
    pub(super) async fn verify(
        self,
        provider: KeyProvider,
        base: &str,
        secret: VaultSecret,
        id: &str,
    ) -> Result<grapt_login::key::VerifiedKey, InvalidRequestErrorDto> {
        let protocol = if provider.family() == grapt_connections::store::Family::AnthropicMessages {
            grapt_login::key::KeyProtocol::BearerMessages
        } else if grapt_connections::catalog::endpoint_provider(provider.id())
            .is_some_and(|row| row.completion_tokens)
        {
            grapt_login::key::KeyProtocol::CompletionChat
        } else {
            grapt_login::key::KeyProtocol::Chat
        };
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
            let listing = serde_json::to_vec(&serde_json::json!({"data":[{
                "id":model.as_str(), "max_model_len":window,
            }]}))
            .map_err(invalid)?;
            let verified = grapt_login::key::verify_model_listing(
                base,
                secret,
                model.as_str(),
                listing,
                protocol,
            )
            .await
            .map_err(failed)?;
            // Only a verified key reaches the optional catalogue read.
            let listing = grapt_login::key::enrich_listing(&verified, model.as_str()).await;
            return Ok(verified.with_listing(listing));
        }
        let verified =
            grapt_login::key::verify(base, grapt_login::key::KeyPlacement::Bearer, secret)
                .await
                .map_err(failed)?;
        let model = match self.model {
            Some(model) => model.into_string(),
            None => grapt_login::key::first_model(verified.listing()).ok_or_else(|| {
                endpoint_refusal(format!(
                    "the server at {base} lists no models; load or pull one, then retry"
                ))
            })?,
        };
        let verified = verified
            .check_model(base, protocol, &model)
            .await
            .map_err(failed)?;
        let listing = grapt_login::key::served_listing(&verified, &model)
            .await
            .map_err(|error| served_refusal(error, &model))?;
        Ok(verified.with_listing(listing))
    }
}

fn served_refusal(error: grapt_login::key::ServedError, model: &str) -> InvalidRequestErrorDto {
    use grapt_login::key::ServedError;
    endpoint_refusal(match error {
        ServedError::Unreadable => "the server's model list is unreadable".to_owned(),
        ServedError::NotServed => {
            format!("the server does not list {model}; choose a model it serves")
        }
        ServedError::NoWindow => format!(
            "the server reports no context window for {model}; enter the model's context window"
        ),
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
