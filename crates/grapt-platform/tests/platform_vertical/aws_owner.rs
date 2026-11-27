//! Real platform-owned AWS capabilities, without resolving ambient AWS credentials.
use super::shared::*;
use grapt_connections::store::{AuthMethod, Connections, CredentialFormat, Family, Store};
use grapt_login::vault::{
    ApiDestination, CredentialVault, DestinationBinding, SealedFileVault, VaultKey, VaultSecret,
};
use grapt_provider::{CredentialError, CredentialPlacement, ProtocolFamily};
use grapt_types::Timestamp;

#[test]
fn runtime_aws_owner_retains_destination_and_refuses_dispatch_after_disconnect() -> TestResult {
    let root = scratch("aws-owner-disconnect")?;
    grapt_login::profile_security::protect_directory(&root)?;
    let config = config("open-ready", "aws-owner", &[], None);
    std::fs::write(root.join("grapt.jsonc"), &config)?;
    let binding = DestinationBinding::key(ApiDestination::parse(
        "https://bedrock-runtime.us-east-1.amazonaws.com",
    )?);
    let mut records = Store::open(&root)?.migrate(config.as_bytes())?;
    let connection = records.connections.first_mut().ok_or("connection")?;
    connection.family = Family::BedrockConverse;
    connection.method = AuthMethod::AwsChain;
    connection.endpoint = Some(binding.api_origin().into());
    connection.credential_format = CredentialFormat::AwsChain {
        binding: binding.clone(),
    };
    let provider = connection.id.clone();
    let key = VaultKey::new(provider.clone(), &connection.credential_file)?;
    SealedFileVault::open(&root)?.put(
        &key,
        &binding,
        VaultSecret::aws_chain(grapt_login::aws::Selection::new(
            "us-east-1".into(),
            Some("synthetic-unresolved-profile".into()),
        )?)?,
        Timestamp::from_millis(10),
    )?;
    Store::open(&root)?.save(&records)?;
    grapt_login::profile_security::replace_private_file(
        &root,
        "account-models.json",
        br#"{"data":[{"id":"gpt-5.6"}]}"#,
    )?;
    ready_database(&root)?;
    let (authority, target) = credential_authority_and_target(&root)?;
    let record = authority.record().clone();
    let (initial, mut owner) =
        authority.into_refreshable(grapt_platform::CredentialResolutionRequest::new(
            &target,
            record,
            ProtocolFamily::BedrockConverse,
            CredentialPlacement::AwsSigV4,
        ))?;
    assert!(matches!(
        initial.bind_destination("https://foreign.example"),
        Err(CredentialError::DestinationMismatch)
    ));
    assert!(matches!(
        owner
            .current()?
            .credential
            .bind_destination("https://foreign.example"),
        Err(CredentialError::DestinationMismatch)
    ));
    owner.validate_dispatch()?;
    let (_, commit) = Store::open(&root)?.remove_with(
        &provider,
        &Connections {
            connections: Vec::new(),
            default_model: None,
        },
        || Ok(()),
    )?;
    assert!(commit.durability_error.is_none());
    assert!(commit.cleanup_error.is_none());
    assert!(matches!(
        owner.validate_dispatch(),
        Err(grapt_platform::CredentialRefusal::Absent)
    ));
    std::fs::remove_dir_all(root)?;
    Ok(())
}
