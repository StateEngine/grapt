//! Secret-free status observed under the profile's connection writer.
use crate::store::{AuthMethod, Connection, CredentialFormat, Store};
use grapt_login::{
    profile_security,
    vault::{BackendKind, CredentialVault, CredentialView, SealedFileVault, VaultKey},
};
use grapt_types::Timestamp;
use serde::Serialize;
use std::path::Path;
use zeroize::Zeroizing;

#[derive(Debug, Serialize)]
pub struct ConnectionStatus {
    pub id: String,
    pub label: String,
    pub provider: String,
    pub backend: &'static str,
    pub last4: Option<String>,
    pub status: &'static str,
}

/// The caller supplies time and environment from the service that dispatches.
/// Opening the store also recovers interrupted publication before observation.
pub fn read(
    root: &Path,
    now: Timestamp,
    environment: impl Fn(&str) -> Option<String>,
) -> Result<Vec<ConnectionStatus>, String> {
    let store = Store::open(root).map_err(|error| error.to_string())?;
    let records = store
        .load()
        .map_err(|error| error.to_string())?
        .ok_or("connection metadata is absent")?;
    let mut result = Vec::with_capacity(records.connections.len());
    for record in records.connections {
        let (backend, last4, status) = if let AuthMethod::Env { name } = &record.method {
            let present = environment(name).is_some_and(|value| {
                grapt_login::vault::VaultSecret::api_key(value.into()).is_ok()
            });
            (
                "environment",
                None,
                if present { "ok" } else { "env-missing" },
            )
        } else {
            describe(root, &record, now)?
        };
        result.push(ConnectionStatus {
            id: record.id.as_str().into(),
            label: record.label,
            provider: record.catalog_provider.as_str().into(),
            backend,
            last4,
            status,
        });
    }
    Ok(result)
}

fn backend(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::WindowsCurrentUserEncryption => "windows-current-user-encryption",
        BackendKind::OwnerOnlyLegacyPlaintext => "owner-only-legacy-plaintext",
    }
}
fn expiry(expires: Timestamp, now: Timestamp) -> &'static str {
    let remaining = expires.as_millis().saturating_sub(now.as_millis());
    if remaining <= 0 {
        "expired"
    } else if remaining <= 48 * 60 * 60 * 1000 {
        "renewal-due"
    } else {
        "ok"
    }
}
fn describe(
    root: &Path,
    record: &Connection,
    now: Timestamp,
) -> Result<(&'static str, Option<String>, &'static str), String> {
    if let Some(binding) = record.credential_format.binding() {
        let file = match &record.credential_format {
            CredentialFormat::VaultedOAuth { .. } => record
                .oauth_file
                .as_deref()
                .ok_or("OAuth state reference is absent")?,
            _ => &record.credential_file,
        };
        let key = VaultKey::new(record.id.clone(), file).map_err(|error| error.to_string())?;
        let vault = SealedFileVault::open(root).map_err(|error| error.to_string())?;
        let description = vault
            .describe(&key, binding)
            .map_err(|error| error.to_string())?
            .ok_or("connected credential is absent")?;
        let status = if matches!(
            record.credential_format,
            CredentialFormat::VaultedOAuth { .. }
        ) {
            let secret = vault
                .get(&key, binding)
                .map_err(|error| error.to_string())?
                .ok_or("connected credential is absent")?;
            match secret.expose() {
                CredentialView::OAuth { expires_at, .. } => expiry(expires_at, now),
                CredentialView::AzureCli
                | CredentialView::VertexAdc
                | CredentialView::AwsChain(_)
                | CredentialView::ApiKey(_)
                | CredentialView::VertexServiceAccount { .. }
                | CredentialView::WatsonxIam { .. } => {
                    return Err("OAuth credential contains an API key".into())
                }
            }
        } else {
            "ok"
        };
        return Ok((backend(description.backend), description.last4, status));
    }
    if let Some(state) = &record.oauth_file {
        let files = grapt_login::chatgpt::SignInFiles::new(state, &record.credential_file)
            .map_err(|error| error.to_string())?;
        let tokens = files
            .load(root)
            .map_err(|error| error.to_string())?
            .ok_or("connected OAuth state is absent")?;
        let stored = Zeroizing::new(
            profile_security::read_owner_only_file(
                &root.join(state),
                profile_security::stored_bound(32 * 1024),
            )
            .map_err(|error| error.to_string())?,
        );
        let kind = if profile_security::is_protected(&stored) {
            "windows-current-user-encryption"
        } else {
            "owner-only-legacy-plaintext"
        };
        return Ok((kind, None, expiry(tokens.expires_at, now)));
    }
    let stored = Zeroizing::new(
        profile_security::read_owner_only_file(
            &root.join(&record.credential_file),
            profile_security::stored_bound(4096),
        )
        .map_err(|error| error.to_string())?,
    );
    let kind = if profile_security::is_protected(&stored) {
        "windows-current-user-encryption"
    } else {
        "owner-only-legacy-plaintext"
    };
    let plaintext =
        Zeroizing::new(profile_security::open(&stored, 4096).map_err(|error| error.to_string())?);
    let value = std::str::from_utf8(&plaintext)
        .map_err(|_| "legacy API key is malformed")?
        .trim_end_matches(['\r', '\n']);
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err("legacy API key is malformed".into());
    }
    let last4 = if value.chars().count() > 4 {
        let mut last: Vec<_> = value.chars().rev().take(4).collect();
        last.reverse();
        Some(last.into_iter().collect())
    } else {
        None
    };
    Ok((kind, last4, "ok"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expiry_distinguishes_expired_renewal_due_and_usable_without_renewing() {
        let now = Timestamp::from_millis(100);
        assert_eq!(expiry(Timestamp::from_millis(100), now), "expired");
        assert_eq!(expiry(Timestamp::from_millis(101), now), "renewal-due");
        assert_eq!(
            expiry(Timestamp::from_millis(100 + 48 * 60 * 60 * 1000), now),
            "renewal-due"
        );
        assert_eq!(
            expiry(Timestamp::from_millis(101 + 48 * 60 * 60 * 1000), now),
            "ok"
        );
    }
}
