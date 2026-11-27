//! Secret-free Bedrock form validation; credentials stay in the shared key owner.
use grapt_api::u3a::BedrockProviderRequestDto;
use grapt_connections::{catalog::KeyProvider, store::Family};

#[derive(Debug)]
pub(super) struct Prepared {
    pub(super) base: String,
    pub(super) model: String,
    pub(super) listing: Vec<u8>,
    pub(super) aws: Option<grapt_login::aws::Selection>,
}

pub(super) fn prepare(
    provider: KeyProvider,
    native: Option<&BedrockProviderRequestDto>,
) -> Result<Option<Prepared>, String> {
    if provider.family() != Family::BedrockConverse {
        return if native.is_some() {
            Err("Bedrock setup is only valid for Amazon Bedrock".into())
        } else {
            Ok(None)
        };
    }
    let native = native.ok_or("Bedrock requires a region and model in the Connect form")?;
    let region = native.region.as_str();
    if region.is_empty()
        || region.len() > 64
        || region.starts_with('-')
        || region.ends_with('-')
        || region.contains("--")
        || !region
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err("invalid Bedrock region".into());
    }
    let aws = match native.auth.as_present() {
        Some(grapt_api::u3a::BedrockAuthRequestDto::AwsChain { profile }) => Some(
            grapt_login::aws::Selection::new(
                region.into(),
                profile.as_present().map(|p| p.as_str().into()),
            )
            .map_err(|error| error.to_string())?,
        ),
        Some(grapt_api::u3a::BedrockAuthRequestDto::Bearer) | None => None,
    };
    let base = native
        .endpoint
        .as_present()
        .map(|s| s.as_str().to_owned())
        .unwrap_or_else(|| {
            let domain = if region.starts_with("cn-") {
                "amazonaws.com.cn"
            } else {
                "amazonaws.com"
            };
            format!("https://bedrock-runtime.{region}.{domain}")
        });
    grapt_login::vault::ApiDestination::parse(&base).map_err(|_| "invalid Bedrock endpoint")?;
    let model = native.model.as_str().to_owned();
    let listing = provider.bedrock_listing(&model)?;
    Ok(Some(Prepared {
        base,
        model,
        listing,
        aws,
    }))
}
