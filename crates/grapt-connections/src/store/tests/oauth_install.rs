//! OAuth publication shares the key install's crash boundary and vault owner.
use super::*;
use grapt_login::{
    chatgpt::{SignInFiles, Tokens},
    vault::{
        ApiDestination, CredentialVault, DestinationBinding, OAuthRefreshDestination,
        SealedFileVault, VaultKey, VaultSecret,
    },
};
use grapt_types::Timestamp;

#[test]
fn oauth_install_pins_account_and_issuer_and_recovers_before_and_after_commit(
) -> Result<(), Box<dyn std::error::Error>> {
    let profile = Profile::new("oauth-install")?;
    let mut store = Store::open(&profile.0)?;
    let before = store.migrate(&legacy("openai-responses", None))?;
    let mut records = before.clone();
    let connection = records.connections[0].id.clone();
    let issuer = "https://issuer.example";
    let binding = DestinationBinding::oauth(
        ApiDestination::parse("https://api.example/v1")?,
        OAuthRefreshDestination::parse(issuer)?,
    );
    let (credential, discovery) =
        next_key_files(&connection, records.connections.first(), "oauth")?;
    let state = format!("{}.oauth.json", credential.trim_end_matches(".credential"));
    let selected = &mut records.connections[0];
    selected.method = AuthMethod::OauthDevice;
    selected.family = Family::ChatgptCodex;
    selected.credential_file = credential.clone();
    selected.discovery_file = discovery.clone();
    selected.oauth_file = Some(state.clone());
    selected.credential_format = CredentialFormat::VaultedOAuth {
        binding: binding.clone(),
    };
    let tokens = Tokens {
        access: "private-access".into(),
        refresh: "private-refresh".into(),
        account: selected.account.as_str().into(),
        expires_at: Timestamp::from_millis(100_000),
    };
    let listing = br#"{"models":[{"slug":"retained-model"}]}"#;
    for (account, destination) in [
        ("another-account", issuer),
        (tokens.account.as_str(), "https://another-issuer.example"),
    ] {
        let mut wrong = tokens.clone();
        wrong.account = account.into();
        assert!(store
            .install_oauth_with(
                &records,
                &connection,
                VaultSecret::oauth(wrong)?,
                &binding,
                destination,
                listing,
                Timestamp::from_millis(1),
                || Ok(())
            )
            .is_err());
        assert!(!profile.0.join("connection-install.json").exists());
    }
    assert!(store
        .install_oauth_with(
            &records,
            &connection,
            VaultSecret::oauth(tokens.clone())?,
            &binding,
            issuer,
            listing,
            Timestamp::from_millis(1),
            || Err::<(), _>(io::Error::other("preparation refused"))
        )
        .is_err());
    drop(store);
    let mut store = Store::open(&profile.0)?;
    let vault = SealedFileVault::open(&profile.0)?;
    let key = VaultKey::new(connection.clone(), &state)?;
    assert_eq!(store.load()?, Some(before));
    assert!(vault.get(&key, &binding)?.is_none());
    assert!(!profile.0.join(&discovery).exists());
    let files = SignInFiles::new(&state, &credential)?
        .with_vault(connection.clone(), binding.clone())?
        .with_generation(records.connections[0].credential_version.clone())?;
    store.failure = Some(install::Failure::MetadataSync);
    let (prepared, commit) = store.install_oauth_with(
        &records,
        &connection,
        VaultSecret::oauth(tokens.clone())?,
        &binding,
        issuer,
        listing,
        Timestamp::from_millis(2),
        || {
            assert_eq!(
                files.load(&profile.0).map_err(io::Error::other)?,
                Some(tokens.clone())
            );
            Ok("accepted generation")
        },
    )?;
    assert_eq!(prepared, "accepted generation");
    assert!(commit.durability_error.is_some());
    assert!(profile.0.join("connection-install.json").is_file());
    assert!(!profile.0.join(&credential).exists());
    drop(store);
    let store = Store::open(&profile.0)?;
    assert_eq!(store.load()?, Some(records));
    assert_eq!(files.load(&profile.0)?, Some(tokens));
    assert!(vault.get(&key, &binding)?.is_some());
    assert!(!profile.0.join("connection-install.json").exists());
    Ok(())
}
