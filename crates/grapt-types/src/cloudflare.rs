//! Validated Cloudflare gateway coordinates and their fixed API destinations.

macro_rules! coordinate {
    ($name:ident, $valid:expr, $message:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct $name(String);
        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl TryFrom<String> for $name {
            type Error = String;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                if ($valid)(&value) {
                    Ok(Self(value))
                } else {
                    Err($message.into())
                }
            }
        }
    };
}

coordinate!(
    CloudflareAccountId,
    |value: &str| value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
    "enter the 32-character lowercase hexadecimal Cloudflare account ID"
);
coordinate!(
    CloudflareGatewayId,
    |value: &str| !value.is_empty() && value.len() <= 64
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
        && !matches!(value, "." | ".."),
    "enter a Cloudflare gateway ID of 1..=64 URL-safe letters, digits, hyphens, dots, underscores or tildes"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudflareProviderId {
    Openai,
    Anthropic,
    Google,
    Xai,
    Alibaba,
    Meta,
    Unbiased,
    Moonshotai,
    Fireworks,
    ReflectionAi,
    Minimax,
    Deepseek,
}
impl CloudflareProviderId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
            Self::Google => "google",
            Self::Xai => "xai",
            Self::Alibaba => "alibaba",
            Self::Meta => "meta",
            Self::Unbiased => "unbiased",
            Self::Moonshotai => "moonshotai",
            Self::Fireworks => "fireworks",
            Self::ReflectionAi => "reflection-ai",
            Self::Minimax => "minimax",
            Self::Deepseek => "deepseek",
        }
    }
}
impl TryFrom<String> for CloudflareProviderId {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "openai" => Ok(Self::Openai),
            "anthropic" => Ok(Self::Anthropic),
            "google" => Ok(Self::Google),
            "xai" => Ok(Self::Xai),
            "alibaba" => Ok(Self::Alibaba),
            "meta" => Ok(Self::Meta),
            "unbiased" => Ok(Self::Unbiased),
            "moonshotai" => Ok(Self::Moonshotai),
            "fireworks" => Ok(Self::Fireworks),
            "reflection-ai" => Ok(Self::ReflectionAi),
            "minimax" => Ok(Self::Minimax),
            "deepseek" => Ok(Self::Deepseek),
            _ => Err("choose a model provider listed for Cloudflare AI Gateway".into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudflareWire {
    Responses,
    Messages,
    Chat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudflareGatewayScope {
    account: CloudflareAccountId,
    gateway: CloudflareGatewayId,
    provider: CloudflareProviderId,
}
impl CloudflareGatewayScope {
    pub fn new(
        account: CloudflareAccountId,
        gateway: CloudflareGatewayId,
        provider: CloudflareProviderId,
    ) -> Self {
        Self {
            account,
            gateway,
            provider,
        }
    }
    pub fn account(&self) -> &CloudflareAccountId {
        &self.account
    }
    pub fn gateway(&self) -> &CloudflareGatewayId {
        &self.gateway
    }
    pub fn provider(&self) -> CloudflareProviderId {
        self.provider
    }
    pub fn wire(&self) -> CloudflareWire {
        match self.provider {
            CloudflareProviderId::Openai => CloudflareWire::Responses,
            CloudflareProviderId::Anthropic => CloudflareWire::Messages,
            _ => CloudflareWire::Chat,
        }
    }
    pub fn api_base(&self) -> String {
        format!("{}{}", self.api_origin(), self.api_path())
    }
    pub fn api_origin(&self) -> &'static str {
        match self.wire() {
            CloudflareWire::Responses | CloudflareWire::Messages => {
                "https://gateway.ai.cloudflare.com"
            }
            CloudflareWire::Chat => "https://api.cloudflare.com",
        }
    }
    pub fn api_path(&self) -> String {
        match self.wire() {
            CloudflareWire::Responses => format!(
                "/v1/{}/{}/openai",
                self.account.as_str(),
                self.gateway.as_str()
            ),
            CloudflareWire::Messages => format!(
                "/v1/{}/{}/anthropic/v1",
                self.account.as_str(),
                self.gateway.as_str()
            ),
            CloudflareWire::Chat => format!("/client/v4/accounts/{}/ai/v1", self.account.as_str()),
        }
    }
    pub fn request_path(&self) -> &'static str {
        match self.wire() {
            CloudflareWire::Responses => "/responses",
            CloudflareWire::Messages => "/messages",
            CloudflareWire::Chat => "/chat/completions",
        }
    }
    pub fn wire_model(&self, model: &crate::ModelId) -> Result<crate::ModelId, String> {
        let prefix = format!("{}/", self.provider.as_str());
        let native = model
            .as_str()
            .strip_prefix(&prefix)
            .filter(|id| !id.is_empty())
            .ok_or("the model does not belong to the selected Cloudflare provider")?;
        let value = match self.wire() {
            CloudflareWire::Responses => native.to_owned(),
            CloudflareWire::Messages => native.replace('.', "-"),
            CloudflareWire::Chat => model.as_str().to_owned(),
        };
        crate::ModelId::new(value).ok_or_else(|| "invalid Cloudflare model ID".into())
    }
}
