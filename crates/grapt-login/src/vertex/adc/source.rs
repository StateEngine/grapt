//! Select and bound local inputs without activating SDK token renewal.
use super::AdcError;
use std::{io::Read, path::PathBuf};
use zeroize::Zeroizing;

const ADC_BYTES: u64 = 64 * 1024;

pub(super) struct Source {
    document: Option<serde_json::Value>,
    quota: Option<String>,
}
impl Source {
    pub(super) fn open() -> Result<Self, AdcError> {
        let document = adc_file()?.map(from_file).transpose()?;
        let quota = match std::env::var_os("GOOGLE_CLOUD_QUOTA_PROJECT") {
            Some(value) => Some(value.into_string().map_err(|_| AdcError::InvalidSource)?),
            None => document
                .as_ref()
                .and_then(|value| value.get("quota_project_id"))
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .ok_or(AdcError::InvalidSource)
                })
                .transpose()?,
        };
        if quota.as_ref().is_some_and(|value| {
            value.is_empty() || value.len() > 256 || !value.bytes().all(|b| b.is_ascii_graphic())
        }) {
            return Err(AdcError::InvalidSource);
        }
        Ok(Self { document, quota })
    }
    pub(super) fn document(&self) -> Option<&serde_json::Value> {
        self.document.as_ref()
    }
    pub(super) fn quota(&self) -> Option<&str> {
        self.quota.as_deref()
    }
}

fn adc_file() -> Result<Option<PathBuf>, AdcError> {
    // An explicitly selected file cannot fall back to another identity.
    if let Some(path) = std::env::var_os("GOOGLE_APPLICATION_CREDENTIALS") {
        if path.is_empty() {
            return Err(AdcError::InvalidSource);
        }
        return Ok(Some(PathBuf::from(path)));
    }
    let directory = if let Some(directory) = std::env::var_os("CLOUDSDK_CONFIG") {
        let directory = directory
            .into_string()
            .map_err(|_| AdcError::InvalidSource)?;
        if directory.is_empty() || directory.len() > 4096 || directory.chars().any(char::is_control)
        {
            return Err(AdcError::InvalidSource);
        }
        if directory == "~" || directory.starts_with("~/") {
            let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .ok_or(AdcError::Missing)?;
            Some(PathBuf::from(home).join(directory.strip_prefix("~/").unwrap_or("")))
        } else {
            Some(PathBuf::from(directory))
        }
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(|root| PathBuf::from(root).join("gcloud"))
    } else {
        std::env::var_os("HOME").map(|root| PathBuf::from(root).join(".config/gcloud"))
    };
    let Some(directory) = directory else {
        return Ok(None);
    };
    let path = directory.join("application_default_credentials.json");
    match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => Ok(Some(path)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(AdcError::Unreadable),
        _ => Err(AdcError::InvalidSource),
    }
}

fn check_source(mut value: &serde_json::Value) -> Result<(), AdcError> {
    for _ in 0..8 {
        match value["type"].as_str() {
            Some("service_account" | "authorized_user") => return Ok(()),
            Some("impersonated_service_account") => {
                value = value
                    .get("source_credentials")
                    .ok_or(AdcError::InvalidSource)?;
            }
            // The SDK rereads these sources on refresh without our byte/lifecycle bounds.
            Some("external_account") => return Err(AdcError::UnsupportedSource),
            _ => return Err(AdcError::InvalidSource),
        }
    }
    Err(AdcError::InvalidSource)
}

fn io_error(error: std::io::Error) -> AdcError {
    if error.kind() == std::io::ErrorKind::NotFound {
        AdcError::Missing
    } else {
        AdcError::Unreadable
    }
}

fn from_file(path: PathBuf) -> Result<serde_json::Value, AdcError> {
    let mut raw = Zeroizing::new(Vec::new());
    if !std::fs::metadata(&path).map_err(io_error)?.is_file() {
        return Err(AdcError::InvalidSource);
    }
    let file = std::fs::File::open(path).map_err(io_error)?;
    if !file.metadata().map_err(io_error)?.is_file() {
        return Err(AdcError::InvalidSource);
    }
    file.take(ADC_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(io_error)?;
    if raw.len() as u64 > ADC_BYTES {
        return Err(AdcError::InvalidSource);
    }
    let value: serde_json::Value =
        serde_json::from_slice(&raw).map_err(|_| AdcError::InvalidSource)?;
    check_source(&value)?;
    Ok(value)
}
