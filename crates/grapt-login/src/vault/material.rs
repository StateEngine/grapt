use super::*;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

fn write_secret<S: Serializer>(
    value: &Zeroizing<String>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(value.as_str())
}
fn read_secret<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Zeroizing<String>, D::Error> {
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = Zeroizing<String>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a credential string")
        }
        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
            Ok(Zeroizing::new(value.to_owned()))
        }
        fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
            Ok(Zeroizing::new(value))
        }
    }
    deserializer.deserialize_string(Visitor)
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub(super) enum Material {
    AzureCli,
    VertexAdc,
    AwsChain {
        selection: crate::aws::Selection,
    },
    ApiKey {
        #[serde(serialize_with = "write_secret", deserialize_with = "read_secret")]
        value: Zeroizing<String>,
    },
    VertexServiceAccount {
        #[serde(serialize_with = "write_secret", deserialize_with = "read_secret")]
        key: Zeroizing<String>,
        #[serde(serialize_with = "write_secret", deserialize_with = "read_secret")]
        access: Zeroizing<String>,
        expires_at_ms: i64,
    },
    WatsonxIam {
        #[serde(serialize_with = "write_secret", deserialize_with = "read_secret")]
        key: Zeroizing<String>,
        #[serde(serialize_with = "write_secret", deserialize_with = "read_secret")]
        access: Zeroizing<String>,
        expires_at_ms: i64,
    },
    OAuth {
        #[serde(serialize_with = "write_secret", deserialize_with = "read_secret")]
        access: Zeroizing<String>,
        #[serde(serialize_with = "write_secret", deserialize_with = "read_secret")]
        refresh: Zeroizing<String>,
        expires_at_ms: i64,
        account: String,
    },
}

fn valid_secret(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4096 && value.bytes().all(|byte| byte.is_ascii_graphic())
}
pub(super) fn key(value: Zeroizing<String>) -> Result<Material, VaultError> {
    if !valid_secret(&value) {
        return Err(VaultError::InvalidInput);
    }
    Ok(Material::ApiKey { value })
}
pub(super) fn oauth(tokens: crate::chatgpt::Tokens) -> Result<Material, VaultError> {
    let material = Material::OAuth {
        access: Zeroizing::new(tokens.access),
        refresh: Zeroizing::new(tokens.refresh),
        expires_at_ms: tokens.expires_at.as_millis(),
        account: tokens.account,
    };
    if !material.valid() {
        return Err(VaultError::InvalidInput);
    }
    Ok(material)
}
pub(super) fn vertex(
    key: Zeroizing<String>,
    token: crate::vertex::Token,
) -> Result<Material, VaultError> {
    crate::vertex::validate_key(&key).map_err(|_| VaultError::InvalidInput)?;
    Ok(Material::VertexServiceAccount {
        key,
        access: Zeroizing::new(token.access().to_owned()),
        expires_at_ms: token.expires_at().as_millis(),
    })
}
pub(super) fn watsonx(
    key: Zeroizing<String>,
    token: crate::watsonx::Token,
) -> Result<Material, VaultError> {
    if !crate::watsonx::valid(&key) {
        return Err(VaultError::InvalidInput);
    }
    Ok(Material::WatsonxIam {
        key,
        access: token.access().to_owned().into(),
        expires_at_ms: token.expires_at().as_millis(),
    })
}
impl Material {
    pub(super) fn valid(&self) -> bool {
        match self {
            Self::AzureCli => true,
            Self::VertexAdc => true,
            Self::AwsChain { selection } => selection.valid(),
            Self::ApiKey { value } => valid_secret(value),
            Self::VertexServiceAccount {
                key,
                access,
                expires_at_ms,
            } => {
                crate::vertex::validate_key(key).is_ok()
                    && valid_secret(access)
                    && *expires_at_ms > 0
            }
            Self::WatsonxIam {
                key,
                access,
                expires_at_ms,
            } => crate::watsonx::valid(key) && valid_secret(access) && *expires_at_ms > 0,
            Self::OAuth {
                access,
                refresh,
                account,
                ..
            } => {
                valid_secret(access)
                    && valid_secret(refresh)
                    && !account.is_empty()
                    && account.len() <= 256
                    && !account.chars().any(char::is_control)
            }
        }
    }
    pub(super) fn oauth(&self) -> bool {
        matches!(self, Self::OAuth { .. })
    }
    pub(super) fn view(&self) -> CredentialView<'_> {
        match self {
            Self::AzureCli => CredentialView::AzureCli,
            Self::VertexAdc => CredentialView::VertexAdc,
            Self::AwsChain { selection } => CredentialView::AwsChain(selection),
            Self::ApiKey { value } => CredentialView::ApiKey(value),
            Self::VertexServiceAccount {
                key,
                access,
                expires_at_ms,
            } => CredentialView::VertexServiceAccount {
                key,
                access,
                expires_at: Timestamp::from_millis(*expires_at_ms),
            },
            Self::WatsonxIam {
                key,
                access,
                expires_at_ms,
            } => CredentialView::WatsonxIam {
                key,
                access,
                expires_at: Timestamp::from_millis(*expires_at_ms),
            },
            Self::OAuth {
                access,
                refresh,
                expires_at_ms,
                account,
            } => CredentialView::OAuth {
                access,
                refresh,
                expires_at: Timestamp::from_millis(*expires_at_ms),
                account,
            },
        }
    }
    pub(super) fn last4(&self) -> Option<String> {
        match self {
            Self::ApiKey { value } if value.len() > 4 => Some(value[value.len() - 4..].to_owned()),
            _ => None,
        }
    }
}
