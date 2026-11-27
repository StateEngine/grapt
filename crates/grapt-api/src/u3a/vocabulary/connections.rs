//! Bounded retained connection/status projections. Unavailable transports are
//! represented by the public failed state, never by a fabricated connection.

use super::catalog::{IntegrationIdDto, LocationInfoDto};
use super::prelude::*;

macro_rules! connection_union {
    ($name:ident, $tag:literal, $( $variant:ident($arm:ty) => $wire:literal ),+ $(,)?) => {
        #[derive(Debug, Clone, PartialEq, serde::Serialize)]
        #[serde(untagged)]
        pub enum $name { $( $variant(Box<$arm>), )+ }
        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(input: D) -> Result<Self, D::Error> {
                // Reject duplicate keys and enforce allocation/depth bounds
                // before selecting a closed arm. JSON field order is immaterial.
                let bounded = <OpenObject as serde::Deserialize>::deserialize(input)?;
                let value = serde_json::to_value(bounded).map_err(serde::de::Error::custom)?;
                match value.get($tag).and_then(serde_json::Value::as_str) {
                    $(Some($wire) => serde_json::from_value(value).map(Box::new).map(Self::$variant).map_err(serde::de::Error::custom),)+
                    _ => Err(serde::de::Error::custom("missing or unknown connection tag")),
                }
            }
        }
    };
}
pub(super) use connection_union;

string_enum! { pub enum McpConnectedStatus { Connected => "connected" } }
string_enum! { pub enum McpPendingStatus { Pending => "pending" } }
string_enum! { pub enum McpDisabledStatus { Disabled => "disabled" } }
string_enum! { pub enum McpFailedStatus { Failed => "failed" } }
string_enum! { pub enum McpNeedsAuthStatus { NeedsAuth => "needs_auth" } }
string_enum! { pub enum McpNeedsRegistrationStatus { NeedsRegistration => "needs_client_registration" } }
closed_object! { pub struct McpConnectedDto => "Mcp.Status.Connected" { required status: McpConnectedStatus => "status", } }
closed_object! { pub struct McpPendingDto => "Mcp.Status.Pending" { required status: McpPendingStatus => "status", } }
closed_object! { pub struct McpDisabledDto => "Mcp.Status.Disabled" { required status: McpDisabledStatus => "status", } }
closed_object! { pub struct McpFailedDto => "Mcp.Status.Failed" {
    required status: McpFailedStatus => "status",
    required error: DtoString => "error",
} }
closed_object! { pub struct McpNeedsAuthDto => "Mcp.Status.NeedsAuth" { required status: McpNeedsAuthStatus => "status", } }
closed_object! { pub struct McpNeedsRegistrationDto => "Mcp.Status.NeedsClientRegistration" {
    required status: McpNeedsRegistrationStatus => "status",
    required error: DtoString => "error",
} }
connection_union!(McpStatusDto, "status",
    Connected(McpConnectedDto) => "connected", Pending(McpPendingDto) => "pending",
    Disabled(McpDisabledDto) => "disabled", Failed(McpFailedDto) => "failed",
    NeedsAuth(McpNeedsAuthDto) => "needs_auth", NeedsRegistration(McpNeedsRegistrationDto) => "needs_client_registration",
);
closed_object! { pub struct McpAddRequestDto => "inline:mcp-add-request" {
    required config: OpenObject => "config",
} }
closed_object! { pub struct McpServerDto => "Mcp.Server" {
    required name: DtoString => "name",
    required status: McpStatusDto => "status",
    optional integration_id: IntegrationIdDto => "integrationID",
} }
closed_object! { pub struct McpResourceDto => "Mcp.Resource" {
    required server: DtoString => "server",
    required name: DtoString => "name",
    required uri: DtoString => "uri",
    optional description: DtoString => "description",
    optional mime_type: DtoString => "mimeType",
} }
closed_object! { pub struct McpResourceTemplateDto => "Mcp.ResourceTemplate" {
    required server: DtoString => "server",
    required name: DtoString => "name",
    required uri_template: DtoString => "uriTemplate",
    optional description: DtoString => "description",
    optional mime_type: DtoString => "mimeType",
} }
closed_object! { pub struct McpResourceCatalogDto => "Mcp.ResourceCatalog" {
    required resources: BoundedVec<McpResourceDto, MAX_CATALOG_ROWS> => "resources",
    required templates: BoundedVec<McpResourceTemplateDto, MAX_CATALOG_ROWS> => "templates",
} }
closed_object! { pub struct McpListResponseDto => "inline:mcp-list-response" {
    required data: BoundedVec<McpServerDto, MAX_CATALOG_ROWS> => "data",
    required location: LocationInfoDto => "location",
} }
closed_object! { pub struct McpResourcesResponseDto => "inline:mcp-resources-response" {
    required data: McpResourceCatalogDto => "data",
    required location: LocationInfoDto => "location",
} }

string_enum! { pub enum McpConfigScope { User => "user", Project => "project" } }
// One declared server, as the configuration route reports it: the document
// class it came from (user or repository) and its disabled state. The route
// is configuration-only, so it never needs the live registry and starts no
// peer.
closed_object! { pub struct McpConfigurationServerDto => "inline:mcp-configuration-server" {
    required name: DtoString => "name",
    required scope: McpConfigScope => "scope",
    required disabled: bool => "disabled",
} }
closed_object! { pub struct McpConfigurationListResponseDto => "inline:mcp-configuration-list-response" {
    required data: BoundedVec<McpConfigurationServerDto, MAX_CATALOG_ROWS> => "data",
    required location: LocationInfoDto => "location",
} }

bounded_id! { pub struct CredentialIdDto => "cred" }
string_enum! { pub enum CredentialConnectionType { Credential => "credential" } }
string_enum! { pub enum EnvConnectionType { Env => "env" } }
string_enum! { pub enum KeyMethodType { Key => "key" } }
closed_object! { pub struct CredentialConnectionDto => "Connection.CredentialInfo" {
    required kind: CredentialConnectionType => "type",
    required id: CredentialIdDto => "id",
    required label: DtoString => "label",
} }
closed_object! { pub struct EnvConnectionDto => "Connection.EnvInfo" {
    required kind: EnvConnectionType => "type",
    required name: DtoString => "name",
} }
connection_union!(ConnectionInfoDto, "type", Credential(CredentialConnectionDto) => "credential", Env(EnvConnectionDto) => "env");
closed_object! { pub struct KeyMethodDto => "Integration.KeyMethod" {
    required kind: KeyMethodType => "type",
    optional label: DtoString => "label",
} }
closed_object! { pub struct EnvMethodDto => "Integration.EnvMethod" {
    required kind: EnvConnectionType => "type",
    required names: BoundedVec<DtoString, MAX_NESTED_ITEMS> => "names",
} }
connection_union!(IntegrationMethodDto, "type",
    Key(KeyMethodDto) => "key", Env(EnvMethodDto) => "env",
    OAuth(super::IntegrationOAuthMethodDto) => "oauth",
    Command(super::IntegrationCommandMethodDto) => "command",
);
closed_object! { pub struct IntegrationInfoDto => "Integration.Info" {
    required id: IntegrationIdDto => "id",
    required name: DtoString => "name",
    required methods: BoundedVec<IntegrationMethodDto, MAX_NESTED_ITEMS> => "methods",
    required connections: BoundedVec<ConnectionInfoDto, MAX_NESTED_ITEMS> => "connections",
} }
closed_object! { pub struct IntegrationListResponseDto => "inline:integration-list-response" {
    required data: BoundedVec<IntegrationInfoDto, MAX_CATALOG_ROWS> => "data",
    required location: LocationInfoDto => "location",
} }

string_enum! { pub enum PluginOriginDto {
    UserGlobal => "user-global",
    LaunchExplicit => "launch-explicit",
    Project => "project",
    Wellknown => "wellknown",
    Unknown => "unknown"
} }
string_enum! { pub enum PluginStatusDto { Active => "active", Blocked => "blocked" } }
closed_object! { pub struct PluginInfoDto => "Plugin.Info" {
    required id: DtoString => "id",
    required exported_id: DtoString => "exportedID",
    required declared_target: DtoString => "declaredTarget",
    required origin: PluginOriginDto => "origin",
    required source: DtoString => "source",
    required status: PluginStatusDto => "status",
    optional reason: DtoString => "reason",
    optional generation: NonNegativeU64 => "generation",
} }
closed_object! { pub struct PluginListResponseDto => "inline:plugin-list-response" {
    required data: BoundedVec<PluginInfoDto, MAX_CATALOG_ROWS> => "data",
    required location: LocationInfoDto => "location",
} }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_mcp_requires_a_reason_and_cannot_be_decoded_as_connected(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let status: McpStatusDto =
            serde_json::from_str(r#"{"error":"transport absent","status":"failed"}"#)?;
        assert!(matches!(status, McpStatusDto::Failed(_)));
        for invalid in [
            r#"{"status":"failed"}"#,
            r#"{"status":"connected","error":"lost"}"#,
            r#"{"status":"disabled","status":"connected"}"#,
        ] {
            assert!(
                serde_json::from_str::<McpStatusDto>(invalid).is_err(),
                "accepted {invalid}"
            );
        }
        let connection: ConnectionInfoDto = serde_json::from_str(
            r#"{"id":"cred_local_dogfood_provider_file_v1","label":"Configured product credential","type":"credential"}"#,
        )?;
        assert!(matches!(connection, ConnectionInfoDto::Credential(_)));
        Ok(())
    }
}

// Native inspection leaves the retained, closed Mcp.Server response unchanged.
closed_object! { pub struct McpInspectionToolDto => "inline:mcp-inspection-tool" {
    required name: DtoString => "name",
    optional omission: DtoString => "omission",
} }
closed_object! { pub struct McpInspectionServerDto => "inline:mcp-inspection-server" {
    required server: McpServerDto => "server",
    required tools: BoundedVec<McpInspectionToolDto, MAX_CATALOG_ROWS> => "tools",
} }
closed_object! { pub struct McpInspectionResponseDto => "inline:mcp-inspection-response" {
    optional subscriptions: BoundedVec<super::McpResourceWatchDto, 1024> => "subscriptions",
    required data: BoundedVec<McpInspectionServerDto, MAX_CATALOG_ROWS> => "data",
    required location: LocationInfoDto => "location",
} }
