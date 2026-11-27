//! Re-sign migrated profiles through the same recoverable sign-in owner.
use super::*;
use crate::store::{AuthMethod, Connection, Connections, Family, Store};

pub(super) fn storage(error: std::io::Error) -> ResignError {
    refused(format!(
        "connection storage refused: {}",
        grapt_login::profile_security::failure_diagnostic(&error)
    ))
}

fn selected<'a>(
    profile: &Path,
    records: &'a Connections,
) -> Result<(&'a Connection, String), ResignError> {
    // C0 owns the historical sign-in pair; C2 names new connection IDs.
    let connection = records
        .connections
        .iter()
        .find(|connection| {
            connection.family == Family::ChatgptCodex
                && connection.method == AuthMethod::OauthDevice
                && connection.credential_file == "provider.credential"
                && connection.oauth_file.as_deref() == Some("chatgpt.oauth.json")
        })
        .ok_or_else(|| refused("this profile has no migrated ChatGPT sign-in to replace"))?;
    let effective = crate::store::selection::profile(records, profile).map_err(storage)?;
    let model = effective
        .as_ref()
        .filter(|selected| selected.provider == connection.id)
        .map(|selected| selected.model.as_str().to_owned())
        .unwrap_or_default();
    Ok((connection, model))
}

pub(super) fn check(profile: &Path) -> Result<Option<Configured>, ResignError> {
    let Some(records) = Store::read_snapshot(profile).map_err(storage)? else {
        return Ok(None);
    };
    let (connection, model) = selected(profile, &records)?;
    Ok(Some(Configured {
        account: connection.account.as_str().to_owned(),
        model,
    }))
}

pub(super) fn prepare(
    profile: &Path,
    records: &mut Connections,
    tokens: &Tokens,
    listing: Vec<u8>,
    offered: &[String],
) -> Result<(Resigned, Vec<SwitchFile>), ResignError> {
    let (current, model) = selected(profile, records)?;
    if offered.is_empty() || !model.is_empty() && !offered.contains(&model) {
        return Err(refused(
            "this ChatGPT account does not offer the selected model; nothing was changed",
        ));
    }
    let id = current.id.clone();
    let resigned = Resigned {
        previous_account: current.account.as_str().into(),
        account: tokens.account.clone(),
    };
    let account = grapt_types::AccountId::new(tokens.account.clone())
        .ok_or_else(|| refused("ChatGPT account is empty"))?;
    let connection = records
        .connections
        .iter_mut()
        .find(|connection| connection.id == id)
        .ok_or_else(|| refused("ChatGPT connection disappeared"))?;
    connection.account = account;
    let encoded = records.to_vec().map_err(storage)?;
    Ok((
        resigned,
        vec![
            SwitchFile::Connections(encoded),
            SwitchFile::Listing(listing),
        ],
    ))
}
