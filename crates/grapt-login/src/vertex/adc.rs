//! Google application-default credentials stay in the authentication owner.
use google_cloud_auth::credentials::{self, CacheableResource, Credentials};
use std::{sync::OnceLock, time::Duration};

mod source;
use source::Source;

const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum AdcError {
    #[error("the selected Google application-default credential file is absent")]
    Missing,
    #[error("the selected Google application-default credential file could not be read")]
    Unreadable,
    #[error("Google application-default configuration is malformed or exceeds its input bound")]
    InvalidSource,
    #[error("external-account credential sources are unsupported; run gcloud auth application-default login or select a service-account JSON file")]
    UnsupportedSource,
    #[error("Google application-default authorization is unavailable; check the selected account and permissions, then retry Connect")]
    Unavailable,
}

/// Source admission is synchronous; SDK renewal starts on the first async request.
#[allow(
    missing_debug_implementations,
    reason = "the session owns credential material"
)]
pub struct Session {
    source: Source,
    credentials: OnceLock<Result<Credentials, AdcError>>,
}
impl Session {
    pub fn open() -> Result<Self, AdcError> {
        Ok(Self {
            source: Source::open()?,
            credentials: OnceLock::new(),
        })
    }

    fn initialize(&self) -> Result<Credentials, AdcError> {
        // The SDK's local signer uses the process provider, independently of TLS clients.
        if rustls::crypto::CryptoProvider::get_default().is_none()
            && rustls::crypto::ring::default_provider()
                .install_default()
                .is_err()
            && rustls::crypto::CryptoProvider::get_default().is_none()
        {
            return Err(AdcError::Unavailable);
        }
        let quota = self.source.quota();
        let Some(value) = self.source.document() else {
            let mut builder = credentials::mds::Builder::default().with_scopes([SCOPE]);
            if let Some(quota) = quota {
                builder = builder.with_quota_project_id(quota);
            }
            return builder
                .build_access_token_credentials()
                .map(Credentials::from)
                .map_err(|_| AdcError::InvalidSource);
        };
        match value["type"].as_str() {
            Some("service_account") => {
                let mut builder = credentials::service_account::Builder::new(value.clone())
                    .with_access_specifier(
                        credentials::service_account::AccessSpecifier::from_scopes([SCOPE]),
                    )
                    .with_universe_domain("googleapis.com");
                if let Some(quota) = quota {
                    builder = builder.with_quota_project_id(quota);
                }
                builder
                    .build_access_token_credentials()
                    .map(Credentials::from)
            }
            Some("authorized_user") => {
                let mut builder =
                    credentials::user_account::Builder::new(value.clone()).with_scopes([SCOPE]);
                if let Some(quota) = quota {
                    builder = builder.with_quota_project_id(quota);
                }
                builder
                    .build_access_token_credentials()
                    .map(Credentials::from)
            }
            Some("impersonated_service_account") => {
                let mut builder =
                    credentials::impersonated::Builder::new(value.clone()).with_scopes([SCOPE]);
                if let Some(quota) = quota {
                    builder = builder.with_quota_project_id(quota);
                }
                builder
                    .build_access_token_credentials()
                    .map(Credentials::from)
            }
            _ => return Err(AdcError::UnsupportedSource),
        }
        .map_err(|_| AdcError::InvalidSource)
    }

    pub async fn authorize_builder(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, AdcError> {
        let (client, request) = request.build_split();
        let request = self
            .authorize(request.map_err(|_| AdcError::Unavailable)?)
            .await?;
        Ok(reqwest::RequestBuilder::from_parts(client, request))
    }

    /// Only the two Google-owned authentication headers may enter a Vertex request.
    pub async fn authorize(
        &self,
        mut request: reqwest::Request,
    ) -> Result<reqwest::Request, AdcError> {
        let credentials = self
            .credentials
            .get_or_init(|| self.initialize())
            .as_ref()
            .map_err(|error| *error)?;
        let headers = tokio::time::timeout(
            Duration::from_secs(15),
            credentials.headers(http::Extensions::new()),
        )
        .await
        .map_err(|_| AdcError::Unavailable)?
        .map_err(|_| AdcError::Unavailable)?;
        let CacheableResource::New { data, .. } = headers else {
            return Err(AdcError::Unavailable);
        };
        let access = data
            .get(http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or(AdcError::Unavailable)?;
        if access.is_empty()
            || access.len() > 4096
            || !access.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(AdcError::Unavailable);
        }
        for name in ["authorization", "x-goog-user-project"] {
            request.headers_mut().remove(name);
            if let Some(value) = data.get(name) {
                let mut value = value.clone();
                value.set_sensitive(true);
                request
                    .headers_mut()
                    .insert(reqwest::header::HeaderName::from_static(name), value);
            }
        }
        Ok(request)
    }
}
