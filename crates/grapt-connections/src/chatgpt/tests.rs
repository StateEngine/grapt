use std::path::PathBuf;

use grapt_types::Timestamp;

use super::*;

const NOW: i64 = 1_800_000_000_000;

const LISTING_FILE: &str = "account-models.json";

const CONFIG: &str = r#"{
  // Signed in by first run; the comment and layout must survive.
  "catalog": {"source": "models.dev-chatgpt"},
  "provider": {
    "id": "openai",
    "model": "gpt-6.1-sol",
    "account": "workspace-a",
    "credential_version": "chatgpt-oauth-v1",
    "route": "responses"
  },
  "web_search": "live"
}
"#;

/// A scratch profile directory, removed on drop.
struct Profile(PathBuf);

impl Profile {
    fn new(case: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("grapt-connections-{case}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir(&path).unwrap_or_else(|error| panic!("profile: {error}"));
        Self(path)
    }

    /// A ChatGPT profile signed in to `workspace-a`, with a store beside it.
    fn signed_in(case: &str, config: &str) -> Self {
        let profile = Self::new(case);
        profile.write(CONFIG_FILE, config.as_bytes());
        profile.write(LISTING_FILE, br#"{"models":[{"slug":"gpt-6.1-sol"}]}"#);
        profile.write(
            "database.sqlite3",
            b"store bytes the sign-in must not touch",
        );
        chatgpt::save(&profile.0, &tokens("workspace-a", "old"))
            .unwrap_or_else(|error| panic!("first sign-in: {error}"));
        profile
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.0.join(name), bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
    }

    fn read(&self, name: &str) -> Vec<u8> {
        std::fs::read(self.0.join(name)).unwrap_or_else(|error| panic!("{name}: {error}"))
    }

    fn credential(&self) -> String {
        let stored = self.read(chatgpt::CREDENTIAL_FILE);
        let bytes = grapt_login::profile_security::open(&stored, 4096)
            .unwrap_or_else(|error| panic!("credential: {error}"));
        String::from_utf8(bytes).unwrap_or_else(|error| panic!("credential: {error}"))
    }

    fn signed_in_account(&self) -> String {
        chatgpt::load(&self.0)
            .unwrap_or_else(|error| panic!("load: {error}"))
            .map(|tokens| tokens.account)
            .unwrap_or_default()
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tokens(account: &str, generation: &str) -> Tokens {
    Tokens {
        access: format!("access-{account}-{generation}"),
        refresh: format!("refresh-{account}-{generation}"),
        expires_at: Timestamp::from_millis(NOW + 3_600_000),
        account: account.to_owned(),
    }
}

const LISTING: &[u8] =
    br#"{"models":[{"slug":"gpt-6.1-sol","visibility":"list"},{"slug":"hidden","visibility":"hide"}]}"#;

#[tokio::test]
async fn a_new_sign_in_on_the_same_account_replaces_only_the_credential_and_listing() {
    let profile = Profile::signed_in("same-account", CONFIG);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&profile.0, std::fs::Permissions::from_mode(0o755))
            .unwrap_or_else(|error| panic!("legacy permissions: {error}"));
    }
    assert!(check(&profile.0).is_ok());
    let store = profile.read("database.sqlite3");
    let resigned = resign(&profile.0, &tokens("workspace-a", "new"), LISTING, NOW)
        .await
        .unwrap_or_else(|error| panic!("resign: {error}"));
    assert!(!resigned.account_changed());
    assert_eq!(profile.credential(), "access-workspace-a-new");
    assert_eq!(profile.read(CONFIG_FILE), CONFIG.as_bytes());
    assert_eq!(profile.read("database.sqlite3"), store);
    let listing: Value = serde_json::from_slice(&profile.read(LISTING_FILE))
        .unwrap_or_else(|error| panic!("listing: {error}"));
    assert_eq!(listing["secode_fetched_at_ms"], NOW);
    assert_eq!(listing["models"][1]["slug"], "hidden");
}

#[tokio::test]
async fn a_different_account_updates_only_the_configured_account_value() {
    let profile = Profile::signed_in("new-account", CONFIG);
    let store = profile.read("database.sqlite3");
    let resigned = resign(&profile.0, &tokens("workspace-b", "new"), LISTING, NOW)
        .await
        .unwrap_or_else(|error| panic!("resign: {error}"));
    assert!(resigned.account_changed());
    assert_eq!(resigned.previous_account, "workspace-a");
    assert_eq!(profile.signed_in_account(), "workspace-b");
    assert_eq!(profile.credential(), "access-workspace-b-new");
    let expected = CONFIG.replace("\"workspace-a\"", "\"workspace-b\"");
    assert_eq!(
        String::from_utf8(profile.read(CONFIG_FILE)).unwrap_or_default(),
        expected
    );
    assert_eq!(profile.read("database.sqlite3"), store);
}

#[tokio::test]
async fn a_refused_sign_in_changes_nothing() {
    // The account no longer offers the profile's model.
    let profile = Profile::signed_in("model-gone", CONFIG);
    let other = br#"{"models":[{"slug":"gpt-6.1-sol","visibility":"hide"}]}"#;
    let refusal = resign(&profile.0, &tokens("workspace-b", "new"), other, NOW).await;
    assert!(
        matches!(refusal, Err(ResignError::Refused(_))),
        "{refusal:?}"
    );
    assert_eq!(profile.credential(), "access-workspace-a-old");
    assert_eq!(profile.signed_in_account(), "workspace-a");
    assert_eq!(profile.read(CONFIG_FILE), CONFIG.as_bytes());

    // The account edit would push grapt.jsonc past its bound: refused
    // before anything, the listing included, is published.
    let padding = "x".repeat(CONFIG_BYTES - CONFIG.len() - 4);
    let full = format!("{CONFIG}// {padding}\n");
    let crowded = Profile::signed_in("config-full", &full);
    let listing = crowded.read(LISTING_FILE);
    let longer = tokens("workspace-with-a-much-longer-id", "new");
    let refusal = resign(&crowded.0, &longer, LISTING, NOW).await;
    assert!(
        matches!(refusal, Err(ResignError::Refused(_))),
        "{refusal:?}"
    );
    assert_eq!(crowded.read(LISTING_FILE), listing);
    assert_eq!(crowded.read(CONFIG_FILE), full.as_bytes());
    assert_eq!(crowded.credential(), "access-workspace-a-old");

    // An API-key profile is not a ChatGPT sign-in, and says so before any
    // browser round trip.
    let api_key = CONFIG
        .replace("chatgpt-oauth-v1", "api-key-v1")
        .replace("\"workspace-a\"", "\"api-key\"");
    let keyed = Profile::new("api-key");
    keyed.write(CONFIG_FILE, api_key.as_bytes());
    assert!(matches!(check(&keyed.0), Err(ResignError::Refused(_))));
    assert!(matches!(
        resign(&keyed.0, &tokens("workspace-b", "new"), LISTING, NOW).await,
        Err(ResignError::Refused(_))
    ));
    assert!(!keyed.0.join(chatgpt::CREDENTIAL_FILE).exists());
}

#[tokio::test]
async fn a_migrated_sign_in_updates_the_managed_account_and_retains_user_configuration(
) -> Result<(), Box<dyn std::error::Error>> {
    let legacy = CONFIG.replace(
        "\"route\": \"responses\"",
        "\"route\": \"responses\", \"policy_version\": \"p1\"",
    );
    let profile = Profile::signed_in("managed-switch", &legacy);
    grapt_login::profile_security::protect_directory(&profile.0)?;
    let mut store = crate::store::Store::open(&profile.0)?;
    let before = store.migrate(legacy.as_bytes())?;
    drop(store);
    let config = profile.read(CONFIG_FILE);
    let database = profile.read("database.sqlite3");
    let resigned = resign(&profile.0, &tokens("workspace-b", "managed"), LISTING, NOW)
        .await
        .map_err(|error| error.to_string())?;
    assert!(resigned.account_changed());
    grapt_login::profile_security::read_owner_only_file(
        &profile.0.join(crate::store::FILE),
        64 * 1024,
    )?;
    assert_eq!(profile.signed_in_account(), "workspace-b");
    assert_eq!(
        check(&profile.0)
            .map_err(|error| error.to_string())?
            .account,
        "workspace-b"
    );
    assert_eq!(profile.read(CONFIG_FILE), config);
    assert_eq!(profile.read("database.sqlite3"), database);
    let after =
        crate::store::Store::read_snapshot(&profile.0)?.ok_or("missing managed generation")?;
    assert_eq!(after.default_model, before.default_model);
    assert_eq!(
        after.connections.first().ok_or("connection")?.id.as_str(),
        "openai"
    );
    assert_eq!(
        after
            .connections
            .first()
            .ok_or("connection")?
            .account
            .as_str(),
        "workspace-b"
    );
    let rejected = br#"{"models":[{"slug":"other-model"}]}"#;
    assert!(resign(
        &profile.0,
        &tokens("workspace-c", "rejected"),
        rejected,
        NOW
    )
    .await
    .is_err());
    assert_eq!(crate::store::Store::read_snapshot(&profile.0)?, Some(after));
    assert_eq!(profile.signed_in_account(), "workspace-b");
    // Interrupt after account metadata commits, before token publication.
    let state = profile.0.join(chatgpt::STATE_FILE);
    let retained = profile.0.join("retained-state");
    std::fs::rename(&state, &retained)?;
    std::fs::create_dir(&state)?;
    assert!(resign(
        &profile.0,
        &tokens("workspace-c", "interrupted"),
        LISTING,
        NOW
    )
    .await
    .is_err());
    assert!(
        matches!(crate::store::Store::open(&profile.0), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    std::fs::remove_dir(&state)?;
    std::fs::rename(&retained, &state)?;
    assert_eq!(profile.signed_in_account(), "workspace-b");
    assert!(chatgpt::recover_pending_switch(&profile.0).await?);
    assert_eq!(profile.signed_in_account(), "workspace-c");
    assert_eq!(
        check(&profile.0)
            .map_err(|error| error.to_string())?
            .account,
        "workspace-c"
    );
    assert!(!chatgpt::has_pending_switch(&profile.0)?);
    assert_eq!(profile.read(CONFIG_FILE), config);
    Ok(())
}

#[tokio::test]
async fn migrated_replacement_validates_the_user_selection_before_publication(
) -> Result<(), Box<dyn std::error::Error>> {
    let legacy = CONFIG.replace(
        "\"route\": \"responses\"",
        "\"route\": \"responses\", \"policy_version\": \"p1\"",
    );
    let profile = Profile::signed_in("managed-selection", &legacy);
    grapt_login::profile_security::protect_directory(&profile.0)?;
    let mut store = crate::store::Store::open(&profile.0)?;
    let before = store.migrate(legacy.as_bytes())?;
    drop(store);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&profile.0, std::fs::Permissions::from_mode(0o755))?;
        assert!(check(&profile.0).is_err());
        assert!(
            resign(&profile.0, &tokens("workspace-b", "insecure"), LISTING, NOW)
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::metadata(&profile.0)?.permissions().mode() & 0o777,
            0o755
        );
        std::fs::set_permissions(&profile.0, std::fs::Permissions::from_mode(0o700))?;
    }
    let overridden = legacy.replace("\"web_search\": \"live\"", "\"web_search\": \"live\", \"default_model\":{\"providerID\":\"openai\",\"id\":\"selected-model\"}");
    profile.write(CONFIG_FILE, overridden.as_bytes());
    assert_eq!(
        check(&profile.0).map_err(|error| error.to_string())?.model,
        "selected-model"
    );
    assert!(resign(
        &profile.0,
        &tokens("workspace-b", "wrong-model"),
        LISTING,
        NOW
    )
    .await
    .is_err());
    assert_eq!(
        crate::store::Store::read_snapshot(&profile.0)?,
        Some(before)
    );
    assert_eq!(profile.signed_in_account(), "workspace-a");
    let selected = br#"{"models":[{"slug":"selected-model"}]}"#;
    resign(
        &profile.0,
        &tokens("workspace-b", "selected"),
        selected,
        NOW,
    )
    .await
    .map_err(|error| error.to_string())?;
    assert_eq!(profile.signed_in_account(), "workspace-b");
    assert_eq!(profile.read(CONFIG_FILE), overridden.as_bytes());
    Ok(())
}
