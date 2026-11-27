//! The generated user configuration declares the existing loopback mail door.
use crate::dogfood::catalog_config::{user_owned, CatalogDocument};
use grapt_types::SessionId;
use std::{fmt, num::NonZeroU16, path::Path, sync::Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    Missing,
    Unreadable,
    Invalid,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Missing => "NativeMailEndpointNotConfigured",
            Self::Unreadable => "NativeMailEndpointConfigurationUnreadable",
            Self::Invalid => "NativeMailEndpointInvalid",
        })
    }
}

pub(super) fn read(
    inbox: &Mutex<grapt_inbox::Inbox>,
    session: &SessionId,
    directory: &Path,
    service_root: &Path,
) -> Result<NonZeroU16, Refusal> {
    let sources = crate::dogfood::session_config::sources(inbox, session, service_root, directory)
        .map_err(|_| Refusal::Unreadable)?;
    configured(&sources.documents)
}

fn configured(documents: &[CatalogDocument]) -> Result<NonZeroU16, Refusal> {
    // An explicitly selected file is the credential destination declaration.
    // Its missing endpoint cannot inherit another file's service-wide target.
    let documents = documents
        .iter()
        .rfind(|document| document.origin == grapt_plugin::PluginOrigin::LaunchExplicit)
        .map(std::slice::from_ref)
        .unwrap_or(documents);
    let mut selected = None;
    for document in documents
        .iter()
        .filter(|document| user_owned(document.origin))
    {
        let Some(stateengine) = document.value.get("stateengine") else {
            continue;
        };
        let stateengine = stateengine.value();
        if stateengine.as_object().is_none() {
            return Err(Refusal::Invalid);
        }
        if let Some(endpoint) = stateengine.get("native_mail_endpoint") {
            selected = Some(parse(endpoint.value().as_str().ok_or(Refusal::Invalid)?)?);
        }
    }
    selected.ok_or(Refusal::Missing)
}

fn parse(endpoint: &str) -> Result<NonZeroU16, Refusal> {
    // The sealed transport connects only to IPv4 loopback. Do not widen its
    // credential destination to arbitrary hosts, URLs, paths or redirects.
    let port = endpoint
        .strip_prefix("http://127.0.0.1:")
        .ok_or(Refusal::Invalid)?;
    let port = port.strip_suffix('/').unwrap_or(port);
    if port.is_empty() || port.len() > 5 || !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Refusal::Invalid);
    }
    port.parse::<u16>()
        .ok()
        .and_then(NonZeroU16::new)
        .ok_or(Refusal::Invalid)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod session_tests;
