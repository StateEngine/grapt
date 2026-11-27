//! Secret-free Azure form validation before credential work.
use grapt_api::u3a::AzureProviderRequestDto;
use grapt_connections::catalog::KeyProvider;

pub(super) struct Prepared {
    pub(super) base: String,
    pub(super) deployment: String,
    pub(super) listing: Vec<u8>,
    pub(super) cli: bool,
}

pub(super) fn prepare(
    provider: KeyProvider,
    native: Option<&AzureProviderRequestDto>,
) -> Result<Option<Prepared>, String> {
    if !provider.is_azure() {
        return if native.is_some() {
            Err("Azure setup is only valid for Azure connections".into())
        } else {
            Ok(None)
        };
    }
    let native = native.ok_or("Azure requires an endpoint, deployment and catalog model")?;
    let base = grapt_login::key::azure::api_base(native.endpoint.as_str())
        .map_err(|_| "use an HTTPS Azure resource endpoint or its /openai/v1 API base")?;
    let cli = matches!(
        native.auth.as_present(),
        Some(grapt_api::u3a::AzureAuthRequestDto::AzureCli)
    );
    if cli {
        grapt_login::azure::validate_endpoint(&base).map_err(|error| error.to_string())?;
    }
    let deployment = native.deployment.as_str().to_owned();
    let listing = provider.azure_listing(&deployment, native.model.as_str())?;
    Ok(Some(Prepared {
        base,
        deployment,
        listing,
        cli,
    }))
}
