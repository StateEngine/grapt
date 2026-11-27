//! Explicit AWS credential-chain selection and Bedrock request signing.
use aws_credential_types::provider::ProvideCredentials;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use grapt_types::{ClockAuthority, Timestamp};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};

/// Persist the selection, never the rotating credentials resolved from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    region: String,
    profile: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum AwsError {
    #[error("invalid AWS region or profile")]
    Invalid,
    #[error("AWS credentials are unavailable; configure the selected AWS profile or credential chain and retry")]
    Unavailable,
    #[error("AWS profile configuration could not be used; credential_process is unsupported; use environment credentials, a shared static profile, SSO, web identity or an IAM role")]
    ProfileConfiguration,
    #[error("AWS request signing failed; check the AWS credentials and system clock, then retry")]
    Signing,
}

impl Selection {
    pub fn new(region: String, profile: Option<String>) -> Result<Self, AwsError> {
        let selection = Self { region, profile };
        if !selection.valid() {
            return Err(AwsError::Invalid);
        }
        Ok(selection)
    }

    pub(crate) fn valid(&self) -> bool {
        !self.region.is_empty()
            && self.region.len() <= 64
            && !self.region.starts_with('-')
            && !self.region.ends_with('-')
            && !self.region.contains("--")
            && self
                .region
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && self.profile.as_ref().is_none_or(|profile| {
                !profile.is_empty()
                    && profile.len() <= 256
                    && !profile.chars().any(char::is_control)
            })
    }

    /// A fresh chain per request observes rotated environment, file and SSO credentials.
    /// The SDK also owns web identity, containers and instance roles.
    /// Profiles requiring credential_process refuse without starting a helper.
    pub async fn sign(
        &self,
        request: reqwest::Request,
        clock: &(dyn ClockAuthority + Sync),
    ) -> Result<reqwest::Request, AwsError> {
        if !self.valid() {
            return Err(AwsError::Invalid);
        }
        let http = aws_smithy_http_client::Builder::new()
            .tls_provider(aws_smithy_http_client::tls::Provider::Rustls(
                aws_smithy_http_client::tls::rustls_provider::CryptoMode::Ring,
            ))
            .build_https();
        let region = aws_types::region::Region::new(self.region.clone());
        let config = aws_config::provider_config::ProviderConfig::without_region()
            .with_http_client(http)
            .with_region(Some(region.clone()));
        let credentials = tokio::time::timeout(Duration::from_secs(15), async {
            if let Some(profile) = &self.profile {
                // Explicit selection must not fall back to an ambient account.
                aws_config::profile::ProfileFileCredentialsProvider::builder()
                    .configure(&config)
                    .profile_name(profile)
                    .build()
                    .provide_credentials()
                    .await
            } else {
                aws_config::default_provider::credentials::DefaultCredentialsChain::builder()
                    .configure(config)
                    .region(region)
                    .build()
                    .await
                    .provide_credentials()
                    .await
            }
        })
        .await
        .map_err(|_| AwsError::Unavailable)?
        .map_err(|error| match error {
            aws_credential_types::provider::error::CredentialsError::InvalidConfiguration(_) => {
                AwsError::ProfileConfiguration
            }
            _ => AwsError::Unavailable,
        })?;
        sign_request(request, credentials, &self.region, clock.now())
    }
}

fn sign_request(
    mut request: reqwest::Request,
    credentials: aws_credential_types::Credentials,
    region: &str,
    now: Timestamp,
) -> Result<reqwest::Request, AwsError> {
    for name in [
        "authorization",
        "x-amz-date",
        "x-amz-security-token",
        "x-amz-content-sha256",
    ] {
        request.headers_mut().remove(name);
    }
    let body = request
        .body()
        .and_then(reqwest::Body::as_bytes)
        .ok_or(AwsError::Signing)?;
    let identity = credentials.into();
    let milliseconds = u64::try_from(now.as_millis()).map_err(|_| AwsError::Signing)?;
    let time = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_millis(milliseconds))
        .ok_or(AwsError::Signing)?;
    let params = aws_sigv4::sign::v4::SigningParams::builder()
        .identity(&identity)
        .region(region)
        .name("bedrock")
        .time(time)
        .settings(SigningSettings::default())
        .build()
        .map_err(|_| AwsError::Signing)?
        .into();
    let headers = request
        .headers()
        .iter()
        .map(|(name, value)| {
            value
                .to_str()
                .map(|value| (name.as_str(), value))
                .map_err(|_| AwsError::Signing)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let signable = SignableRequest::new(
        request.method().as_str(),
        request.url().as_str(),
        headers.into_iter(),
        SignableBody::Bytes(body),
    )
    .map_err(|_| AwsError::Signing)?;
    let (instructions, _) = sign(signable, &params)
        .map_err(|_| AwsError::Signing)?
        .into_parts();
    let (headers, query) = instructions.into_parts();
    if !query.is_empty() {
        return Err(AwsError::Signing);
    }
    for header in headers {
        let name = reqwest::header::HeaderName::from_bytes(header.name().as_bytes())
            .map_err(|_| AwsError::Signing)?;
        let mut value = reqwest::header::HeaderValue::from_str(header.value())
            .map_err(|_| AwsError::Signing)?;
        value.set_sensitive(true);
        request.headers_mut().insert(name, value);
    }
    Ok(request)
}
