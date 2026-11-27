//! Explicit coordinates for providers whose API address belongs to an account.
#[derive(Debug, Clone, Copy)]
pub struct EndpointProvider {
    pub id: &'static str,
    pub hint: &'static str,
    pub completion_tokens: bool,
    pub messages_model_prefix: Option<&'static str>,
    pub permits_empty_local_key: bool,
}

const PROVIDERS: &[EndpointProvider] = &[
    EndpointProvider {
        id: "snowflake-cortex",
        hint: "Use https://<account>.snowflakecomputing.com/api/v2/cortex/v1 and a programmatic access token.",
        completion_tokens: true,
        messages_model_prefix: Some("claude-"),
        permits_empty_local_key: false,
    },
    EndpointProvider {
        id: "infomaniak",
        hint: "Use https://api.infomaniak.com/2/ai/<product_id>/openai/v1 and your AI Tools API token.",
        completion_tokens: true,
        messages_model_prefix: None,
        permits_empty_local_key: false,
    },
    EndpointProvider {
        id: "neon",
        hint: "Use https://<branch-host>/v1 and a branch credential with ai_gateway:invoke access.",
        completion_tokens: false,
        messages_model_prefix: None,
        permits_empty_local_key: false,
    },
    EndpointProvider {
        id: "lmstudio",
        hint: "Use the local server's API base URL ending in /v1. An HTTP loopback server may leave the API key empty.",
        completion_tokens: false,
        messages_model_prefix: None,
        permits_empty_local_key: true,
    },
];

pub fn endpoint_provider(id: &str) -> Option<&'static EndpointProvider> {
    PROVIDERS.iter().find(|row| row.id == id)
}
