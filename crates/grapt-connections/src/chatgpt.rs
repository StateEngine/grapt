//! Replace an existing profile's ChatGPT sign-in in place (C0).
//!
//! `grapt login chatgpt --profile <profile>` signs in again, possibly with a
//! different ChatGPT account, without creating a profile: only the sign-in,
//! its derived credential, the account's model listing and, when the account
//! id changed, `provider.account` in `grapt.jsonc` are replaced. The store,
//! Sessions, mail state and every other configuration byte are left alone, so
//! existing Sessions stay resumable.
//!
//! Everything is validated, then published as one recoverable switch under
//! the profile's sign-in lock, the one a running service takes before each
//! request: a failure or crash leaves the old generation or the new one. With
//! the same account id that service uses the new sign-in on its next request;
//! a different id needs a restart, because the configured account is read at
//! start.

use std::path::Path;

use grapt_config::{
    ConfigBounds, ConfigValue, MaxAncestorDepth, MaxDocumentBytes, MaxDocumentNodes, MaxDocuments,
    MaxValueDepth, Spanned,
};
use grapt_login::chatgpt::{self, SignInError, SwitchFile, Tokens};
use serde_json::Value;

mod managed;

/// The platform's own bound for a profile configuration document.
const CONFIG_BYTES: usize = 64 * 1_024;

/// First run's bound for a model listing.
const LISTING_BYTES: usize = 16 * 1024 * 1024;

/// The wire family a ChatGPT sign-in serves; absent means this one.
const CHATGPT_FAMILY: &str = "chatgpt-codex";

/// Why a sign-in was not replaced. Nothing in the profile changed.
#[derive(Debug)]
pub enum ResignError {
    /// The profile cannot take this sign-in; the text says why and what to do.
    Refused(String),
    SignIn(SignInError),
}

impl From<SignInError> for ResignError {
    fn from(error: SignInError) -> Self {
        Self::SignIn(error)
    }
}

impl std::fmt::Display for ResignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(reason) => f.write_str(reason),
            Self::SignIn(error) => error.fmt(f),
        }
    }
}

/// The current configuration name the module tests write; production resolves
/// the profile's own spelling instead.
#[cfg(test)]
const CONFIG_FILE: &str = "grapt.jsonc";

fn refused(reason: impl Into<String>) -> ResignError {
    ResignError::Refused(reason.into())
}

/// The ChatGPT sign-in a profile is configured for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Configured {
    pub account: String,
    pub model: String,
}

/// A replaced sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resigned {
    pub previous_account: String,
    pub account: String,
}

impl Resigned {
    /// A running service keeps the configured account until it restarts.
    pub fn account_changed(&self) -> bool {
        self.previous_account != self.account
    }
}

/// Whether `profile` is a ChatGPT profile this can sign in again, checked
/// before a sign-in starts so a refusal costs no browser round trip.
pub fn check(profile: &Path) -> Result<Configured, ResignError> {
    if let Some(configured) = managed::check(profile)? {
        return Ok(configured);
    }
    let raw = read_config(profile)?;
    Ok(configured(&raw)?.0)
}

/// Replace the profile's sign-in with `tokens`, whose account offers the
/// models in `listing` (the account's model listing, as fetched at
/// `fetched_at_ms`). The profile's configured model must still be offered.
pub async fn resign(
    profile: &Path,
    tokens: &Tokens,
    listing: &[u8],
    fetched_at_ms: i64,
) -> Result<Resigned, ResignError> {
    // Refuse incompatible profiles before acquiring or creating sign-in state.
    // The publication callback validates the current generation again.
    check(profile)?;
    let (listing, offered) = stamped(listing, fetched_at_ms)?;
    // The sign-in owner is acquired asynchronously first. The callback then
    // holds the connection writer only through synchronous journal publication.
    let (resigned, _store) = chatgpt::replace_sign_in(profile, tokens, |profile| {
        if crate::store::Store::read_snapshot(profile)
            .map_err(managed::storage)?
            .is_none()
        {
            // In-place legacy sign-in authorizes making its credential directory
            // private. Existing managed storage must pass strict checks first.
            grapt_login::profile_security::protect_directory(profile).map_err(managed::storage)?;
        }
        let store = crate::store::Store::open(profile).map_err(managed::storage)?;
        if let Some(mut records) = store.load().map_err(managed::storage)? {
            let (resigned, files) =
                managed::prepare(profile, &mut records, tokens, listing, &offered)?;
            return Ok(((resigned, store), files));
        }
        let raw = read_config(profile)?;
        let (configured, account) = configured(&raw)?;
        if !offered.contains(&configured.model) {
            return Err(refused(format!(
                "this ChatGPT account does not offer the profile's model {}; nothing was changed",
                configured.model
            )));
        }
        let mut files = vec![SwitchFile::Listing(listing)];
        if configured.account != tokens.account {
            let edited = with_account(&raw, account, &tokens.account)?;
            files.push(SwitchFile::Config(edited));
        }
        let resigned = Resigned {
            previous_account: configured.account,
            account: tokens.account.clone(),
        };
        Ok(((resigned, store), files))
    })
    .await?;
    Ok(resigned)
}

fn read_config(profile: &Path) -> Result<Vec<u8>, ResignError> {
    match std::fs::symlink_metadata(profile) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(refused("the profile must be a real directory")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(refused(format!(
                "no profile exists at {}; run grapt code to create one",
                profile.display()
            )))
        }
        Err(error) => {
            return Err(refused(format!(
                "the profile is unreadable: {:?}",
                error.kind()
            )))
        }
    }
    // A profile from before the rename keeps reading and writing its own file.
    let spelling = grapt_login::profile_files::profile_spelling(profile)
        .map_err(|error| {
            refused(format!(
                "the profile configuration is unreadable: {:?}",
                error.kind()
            ))
        })?
        .spelling;
    let (json, jsonc) = (spelling.config_json(), spelling.config_jsonc());
    if std::fs::symlink_metadata(profile.join(json)).is_ok() {
        return Err(refused(format!(
            "the profile has both {json} and {jsonc}; keep one, then sign in again"
        )));
    }
    let path = profile.join(jsonc);
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
        refused(format!(
            "the profile's {jsonc} is unreadable: {:?}",
            error.kind()
        ))
    })?;
    if !metadata.is_file() || metadata.len() > CONFIG_BYTES as u64 {
        return Err(refused(format!(
            "the profile's {jsonc} must be a regular file of at most 64 KiB"
        )));
    }
    let bytes = std::fs::read(&path).map_err(|error| {
        refused(format!(
            "the profile's {jsonc} is unreadable: {:?}",
            error.kind()
        ))
    })?;
    if bytes.len() > CONFIG_BYTES {
        return Err(refused(format!("the profile's {jsonc} grew past 64 KiB")));
    }
    Ok(bytes)
}

fn bounds() -> Result<ConfigBounds, ResignError> {
    let invalid = |_| refused("configuration bounds are invalid");
    Ok(ConfigBounds::new(
        MaxDocumentBytes::try_new(CONFIG_BYTES).map_err(invalid)?,
        MaxAncestorDepth::try_new(1).map_err(invalid)?,
        MaxDocuments::try_new(1).map_err(invalid)?,
        MaxValueDepth::try_new(16).map_err(invalid)?,
        MaxDocumentNodes::try_new(4_096).map_err(invalid)?,
    ))
}

fn decode(raw: &[u8]) -> Result<Spanned<ConfigValue>, ResignError> {
    grapt_config::decode(raw, bounds()?)
        .map_err(|_| refused("the profile's grapt.jsonc is not valid JSONC; repair it first"))
}

/// The configured ChatGPT sign-in and the byte span of its account value.
fn configured(raw: &[u8]) -> Result<(Configured, (usize, usize)), ResignError> {
    let document = decode(raw)?;
    let provider = document
        .value()
        .get("provider")
        .filter(|provider| provider.value().as_object().is_some())
        .ok_or_else(|| refused("the profile configures no provider"))?;
    let text = |field: &str| {
        provider
            .value()
            .get(field)
            .and_then(|value| value.value().as_str())
    };
    let family = text("protocol_family").unwrap_or(CHATGPT_FAMILY);
    if text("credential_version") != Some(chatgpt::SIGN_IN_CREDENTIAL_VERSION)
        || family != CHATGPT_FAMILY
        || provider.value().get("credential").is_some()
    {
        return Err(refused(
            "this profile is not signed in with ChatGPT; grapt login chatgpt replaces an existing ChatGPT sign-in",
        ));
    }
    let account = provider
        .value()
        .get("account")
        .filter(|account| {
            account
                .value()
                .as_str()
                .is_some_and(|text| !text.is_empty())
        })
        .ok_or_else(|| refused("the profile's provider names no ChatGPT account"))?;
    let model = text("model")
        .filter(|model| !model.is_empty())
        .ok_or_else(|| refused("the profile's provider names no model"))?;
    let span = (account.span().start(), account.span().end());
    let account = account.value().as_str().unwrap_or_default().to_owned();
    Ok((
        Configured {
            account,
            model: model.to_owned(),
        },
        span,
    ))
}

/// The configuration with only the account value replaced; comments,
/// formatting and every other value keep their bytes.
fn with_account(raw: &[u8], span: (usize, usize), account: &str) -> Result<Vec<u8>, ResignError> {
    let encoded = serde_json::to_string(account)
        .map_err(|_| refused("the new account id cannot be encoded"))?;
    let mut text = String::from_utf8(raw.to_vec())
        .map_err(|_| refused("the profile's grapt.jsonc is not UTF-8"))?;
    let (start, end) = span;
    if end > text.len() || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return Err(refused("the profile's account value could not be located"));
    }
    text.replace_range(start..end, &encoded);
    if text.len() > CONFIG_BYTES {
        return Err(refused("the updated grapt.jsonc would exceed 64 KiB"));
    }
    let bytes = text.into_bytes();
    let (updated, _) = configured(&bytes)?;
    if updated.account != account {
        return Err(refused("the account update did not read back"));
    }
    Ok(bytes)
}

/// The listing stamped with its fetch time, as first run stores it, and the
/// models it offers: rows the account shows or leaves unclassified, never
/// `hide` or an unknown visibility.
fn stamped(listing: &[u8], fetched_at_ms: i64) -> Result<(Vec<u8>, Vec<String>), ResignError> {
    if listing.len() > LISTING_BYTES {
        return Err(refused(
            "the account's model listing exceeds its byte bound",
        ));
    }
    let mut value: Value = serde_json::from_slice(listing)
        .map_err(|_| refused("the account's model listing is not valid JSON"))?;
    let offered = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| refused("the account's model listing has no models"))?
        .iter()
        .filter(|row| match row.get("visibility") {
            None => true,
            Some(Value::String(visibility)) => matches!(visibility.as_str(), "list" | "none"),
            Some(_) => false,
        })
        .filter_map(|row| row.get("slug").and_then(Value::as_str).map(str::to_owned))
        .collect();
    value
        .as_object_mut()
        .ok_or_else(|| refused("the account's model listing is not an object"))?
        .insert("secode_fetched_at_ms".into(), fetched_at_ms.into());
    let bytes = serde_json::to_vec(&value)
        .map_err(|_| refused("the account's model listing cannot be encoded"))?;
    if bytes.len() > LISTING_BYTES {
        return Err(refused("the stamped model listing exceeds its byte bound"));
    }
    Ok((bytes, offered))
}

#[cfg(test)]
mod tests;
