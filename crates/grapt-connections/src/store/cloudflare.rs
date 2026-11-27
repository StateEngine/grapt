//! Decode persisted coordinates through their owning types' validation.
use grapt_types::cloudflare::CloudflareGatewayScope;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Coordinates {
    account: String,
    gateway: String,
    provider: String,
}

pub(super) fn serialize<S: Serializer>(
    scope: &CloudflareGatewayScope,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    Coordinates {
        account: scope.account().as_str().into(),
        gateway: scope.gateway().as_str().into(),
        provider: scope.provider().as_str().into(),
    }
    .serialize(serializer)
}

pub(super) fn deserialize<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<CloudflareGatewayScope, D::Error> {
    let coordinates = Coordinates::deserialize(deserializer)?;
    Ok(CloudflareGatewayScope::new(
        coordinates
            .account
            .try_into()
            .map_err(serde::de::Error::custom)?,
        coordinates
            .gateway
            .try_into()
            .map_err(serde::de::Error::custom)?,
        coordinates
            .provider
            .try_into()
            .map_err(serde::de::Error::custom)?,
    ))
}
