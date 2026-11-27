//! Preserve typed bearer and cached cloud material, without an exchange.
use super::*;

#[test]
fn managed_import_preserves_bearer_destination_and_cached_cloud_material() -> TestResult {
    for cloud in [false, true] {
        let fixture = Fixture::new(if cloud {
            "managed-import-cloud"
        } else {
            "managed-import-bearer"
        })?;
        let mut records = fixture.managed(true, None)?;
        let mut record = records.connections[0].clone();
        record.id = ProviderId::new("secondary").ok_or("fixture id")?;
        record.catalog_provider = ProviderId::new(if cloud { "watsonx" } else { "anthropic" })
            .ok_or("fixture catalog")?;
        record.method = AuthMethod::Key;
        record.oauth_file = None;
        record.endpoint = Some("https://secondary.example/custom".into());
        let binding =
            DestinationBinding::key(ApiDestination::parse("https://secondary.example/custom")?);
        record.family = if cloud {
            Family::WatsonxChat
        } else {
            Family::AnthropicMessages
        };
        let secret = if cloud {
            record.watsonx_project = Some(
                grapt_types::WatsonxProjectId::new("synthetic-project").ok_or("fixture project")?,
            );
            record.credential_format = CredentialFormat::Vault {
                binding: binding.clone(),
            };
            VaultSecret::watsonx(
                KEY.to_owned().into(),
                grapt_login::watsonx::Token::from_cached(
                    ACCESS.to_owned().into(),
                    Timestamp::from_millis(2_000_000_000_000),
                )?,
            )?
        } else {
            record.credential_format = CredentialFormat::MessagesBearer {
                binding: binding.clone(),
                destination: grapt_login::vault::MessagesDestination::parse(
                    "https://secondary.example/custom",
                )?,
            };
            VaultSecret::api_key(KEY.to_owned().into())?
        };
        (record.credential_file, record.discovery_file) =
            next_key_files(&record.id, None, "typed")?;
        records.connections.push(record.clone());
        let commit = Store::open(&fixture.source())?.install_key(
            &records,
            &record.id,
            secret,
            &binding,
            br#"{"data":[{"id":"secondary-model"}]}"#,
            Timestamp::from_millis(789),
        )?;
        assert!(commit.cleanup_error.is_none() && commit.durability_error.is_none());
        let output = fixture.initialize()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        no_secret(&output)?;
        let imported = Store::read_snapshot(&fixture.target())?.ok_or("records absent")?;
        let after = imported
            .connections
            .iter()
            .find(|item| item.id == record.id)
            .ok_or("typed connection absent")?;
        assert_eq!(after.credential_format, record.credential_format);
        assert_eq!(after.endpoint, record.endpoint);
        assert_eq!(after.watsonx_project, record.watsonx_project);
        let vault = SealedFileVault::open(&fixture.target())?;
        let key = VaultKey::new(after.id.clone(), &after.credential_file)?;
        let secret = vault.get(&key, &binding)?.ok_or("typed material lost")?;
        if cloud {
            assert!(
                matches!(secret.expose(), CredentialView::WatsonxIam { key, access, expires_at }
                if key == KEY && access == ACCESS && expires_at.as_millis() == 2_000_000_000_000)
            );
        } else {
            assert!(matches!(secret.expose(), CredentialView::ApiKey(value) if value == KEY));
        }
        assert_eq!(
            vault
                .describe(&key, &binding)?
                .ok_or("description absent")?
                .updated
                .as_millis(),
            789
        );
        assert!(vault
            .get(
                &key,
                &DestinationBinding::key(ApiDestination::parse("https://different.example")?)
            )
            .is_err());
    }
    Ok(())
}
