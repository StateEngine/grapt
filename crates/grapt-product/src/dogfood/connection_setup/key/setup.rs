//! Explicit endpoint coordinates share the existing bounded model check.
use super::*;

pub(super) struct ModelSetup {
    pub(super) endpoint: String,
    pub(super) model: DtoString,
    pub(super) context_window: u32,
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
                    model: custom.model,
                    context_window: custom.context_window,
                }
            }
            ("databricks", None, Some(databricks)) => Self {
                endpoint: workspace_endpoint(databricks.workspace.as_str())?,
                model: databricks.model,
                context_window: databricks.context_window,
                permits_empty_key: false,
            },
            (id, _, _)
                if id == "custom"
                    || grapt_connections::catalog::endpoint_provider(id).is_some() =>
            {
                return Err(invalid("enter the API base URL, model and context window"))
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
        if setup.model.as_str().is_empty()
            || setup.model.as_str().len() > 256
            || setup.model.as_str().chars().any(char::is_control)
            || !(512..=16_777_216).contains(&setup.context_window)
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
