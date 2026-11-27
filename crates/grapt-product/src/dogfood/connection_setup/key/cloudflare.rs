//! Typed gateway setup before any network request or credential publication.
use super::*;
use grapt_api::u3a::IntegrationCloudflareGatewayConnectDto;
use grapt_types::cloudflare::CloudflareGatewayScope;

pub(super) fn scope(
    id: &str,
    entered: Option<IntegrationCloudflareGatewayConnectDto>,
) -> Result<Option<CloudflareGatewayScope>, InvalidRequestErrorDto> {
    match (id == "cloudflare-ai-gateway", entered) {
        (true, Some(entered)) => Ok(Some(CloudflareGatewayScope::new(
            entered.account.into_string().try_into().map_err(invalid)?,
            entered.gateway.into_string().try_into().map_err(invalid)?,
            entered.provider.into_string().try_into().map_err(invalid)?,
        ))),
        (true, None) => Err(invalid(
            "enter the Cloudflare account ID, gateway ID and model provider",
        )),
        (false, Some(_)) => Err(invalid(
            "gateway coordinates apply only to Cloudflare AI Gateway",
        )),
        (false, None) => Ok(None),
    }
}

pub(super) fn key_error(id: &str, error: grapt_login::key::KeyError) -> InvalidRequestErrorDto {
    if id != "cloudflare-ai-gateway" {
        return authorization_failed();
    }
    match error {
        grapt_login::key::KeyError::Unreachable => {
            invalid("Cloudflare is unavailable; check connectivity and retry Connect")
        }
        _ => crate::dogfood::support::static_dto(serde_json::json!({
            "_tag": "InvalidRequestError",
            "kind": "integration_authorization",
            "message": "Cloudflare verification failed; check the account, gateway, token permissions and provider keys or credits, then retry Connect",
        })),
    }
}
