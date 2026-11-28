//! Explicit coordinates for providers whose API address belongs to an account
//! or to a model server on the user's own machine.
#[derive(Debug, Clone, Copy)]
pub struct EndpointProvider {
    pub id: &'static str,
    pub hint: &'static str,
    pub completion_tokens: bool,
    pub messages_model_prefix: Option<&'static str>,
    pub permits_empty_local_key: bool,
    /// A model server people run on their own machine: its usual loopback
    /// address, and the name to show when no bundled catalogue row names it.
    pub local: Option<LocalServer>,
}

/// The coordinates a local model server listens on unless told otherwise.
#[derive(Debug, Clone, Copy)]
pub struct LocalServer {
    pub name: &'static str,
    pub base: &'static str,
}

const PROVIDERS: &[EndpointProvider] = &[
    EndpointProvider {
        id: "snowflake-cortex",
        hint: "Use https://<account>.snowflakecomputing.com/api/v2/cortex/v1 and a programmatic access token.",
        completion_tokens: true,
        messages_model_prefix: Some("claude-"),
        permits_empty_local_key: false,
        local: None,
    },
    EndpointProvider {
        id: "infomaniak",
        hint: "Use https://api.infomaniak.com/2/ai/<product_id>/openai/v1 and your AI Tools API token.",
        completion_tokens: true,
        messages_model_prefix: None,
        permits_empty_local_key: false,
        local: None,
    },
    EndpointProvider {
        id: "neon",
        hint: "Use https://<branch-host>/v1 and a branch credential with ai_gateway:invoke access.",
        completion_tokens: false,
        messages_model_prefix: None,
        permits_empty_local_key: false,
        local: None,
    },
    local("lmstudio", "LM Studio", "http://127.0.0.1:1234/v1"),
    local("ollama", "Ollama", "http://127.0.0.1:11434/v1"),
    local("vllm", "vLLM", "http://127.0.0.1:8000/v1"),
    local("llamacpp", "llama.cpp server", "http://127.0.0.1:8080/v1"),
];

const fn local(id: &'static str, name: &'static str, base: &'static str) -> EndpointProvider {
    EndpointProvider {
        id,
        hint: "Leave the fields empty to use the server's usual address and every model it serves. An HTTP loopback server may leave the API key empty.",
        completion_tokens: false,
        messages_model_prefix: None,
        permits_empty_local_key: true,
        local: Some(LocalServer { name, base }),
    }
}

pub fn endpoint_provider(id: &str) -> Option<&'static EndpointProvider> {
    PROVIDERS.iter().find(|row| row.id == id)
}

/// Every local model server with a preset, in the order a picker shows them.
pub fn local_servers() -> impl Iterator<Item = &'static EndpointProvider> {
    PROVIDERS.iter().filter(|row| row.local.is_some())
}
