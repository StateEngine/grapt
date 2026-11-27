//! AWS connection custody through the public configuration and renewal owners.
use grapt_connections::store::{
    AuthMethod, Connection, Connections, CredentialFormat, DefaultModel, Family, Store,
};
use grapt_login::vault::{
    ApiDestination, CredentialVault, DestinationBinding, SealedFileVault, VaultKey, VaultSecret,
};
use grapt_platform::{CredentialRefusal, CredentialResolutionRequest, PlatformOwner};
use grapt_provider::{CredentialPlacement, ProtocolFamily};
use grapt_types::{
    AccountId, CredentialVersion, ModelId, ModelRouteId, PolicyVersion, ProviderId, Timestamp,
};
use std::path::PathBuf;

type TestResult = Result<(), Box<dyn std::error::Error>>;
struct Profile(PathBuf);
impl Drop for Profile {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn aws_owner_retains_destination_selection_and_disconnect_authority() -> TestResult {
    let path = std::env::temp_dir().join(format!("grapt-aws-owner-{}", std::process::id()));
    std::fs::create_dir(&path)?;
    let root = Profile(std::fs::canonicalize(path)?);
    grapt_login::profile_security::protect_directory(&root.0)?;
    let provider = ProviderId::new("bedrock-fixture").ok_or("provider")?;
    let endpoint = "https://bedrock-runtime.us-east-1.amazonaws.com";
    let binding = DestinationBinding::key(ApiDestination::parse(endpoint)?);
    let selection = grapt_login::aws::Selection::new("us-east-1".into(), Some("selected".into()))?;
    let key = VaultKey::new(provider.clone(), "chain.credential")?;
    let mut vault = SealedFileVault::open(&root.0)?;
    vault.put(
        &key,
        &binding,
        VaultSecret::aws_chain(selection)?,
        Timestamp::from_millis(1),
    )?;
    let records = Connections {
        connections: vec![Connection {
            id: provider.clone(),
            catalog_provider: ProviderId::new("amazon-bedrock").ok_or("catalog provider")?,
            family: Family::BedrockConverse,
            method: AuthMethod::AwsChain,
            label: "AWS fixture".into(),
            endpoint: Some(endpoint.into()),
            watsonx_project: None,
            account: AccountId::new("aws-account").ok_or("account")?,
            route: ModelRouteId::new("converse").ok_or("route")?,
            credential_version: CredentialVersion::new("aws-v1").ok_or("version")?,
            policy_version: PolicyVersion::new("policy").ok_or("policy")?,
            credential_file: "chain.credential".into(),
            credential_format: CredentialFormat::AwsChain {
                binding: binding.clone(),
            },
            discovery_file: "chain-models.json".into(),
            oauth_file: None,
        }],
        default_model: Some(DefaultModel {
            provider,
            model: ModelId::new("review-model").ok_or("model")?,
            variant: None,
        }),
    };
    Store::open(&root.0)?.save(&records)?;
    std::fs::write(
        root.0.join("grapt.jsonc"),
        br#"{"store":{"authority":"initialize"},"product":{"build":"aws-owner-fixture"}}"#,
    )?;
    std::fs::write(
        root.0.join("chain-models.json"),
        br#"{"data":[{"id":"review-model","max_model_len":131072}]}"#,
    )?;
    let platform = PlatformOwner::prepare(&root.0)?;
    let (_store, targets, _build, authority, _contact) =
        platform.into_provider_parts()?.into_components();
    let target = targets.issue()?;
    let resolution = CredentialResolutionRequest::new(
        &target,
        authority.record().clone(),
        ProtocolFamily::BedrockConverse,
        CredentialPlacement::AwsSigV4,
    );
    let (initial, mut owner) = authority.into_refreshable(resolution)?;
    assert_eq!(initial.placement(), CredentialPlacement::AwsSigV4);
    assert!(owner
        .current()?
        .credential
        .bind_destination("https://foreign.example")
        .is_err());
    // Changing the stored selection cannot retarget the already-issued owner.
    vault.put(
        &key,
        &binding,
        VaultSecret::aws_chain(grapt_login::aws::Selection::new(
            "us-east-1".into(),
            Some("other".into()),
        )?)?,
        Timestamp::from_millis(2),
    )?;
    assert!(matches!(
        owner.current(),
        Err(CredentialRefusal::SourceChanged)
    ));
    assert!(owner.validate_dispatch().is_ok());
    Store::open(&root.0)?.save(&Connections {
        connections: Vec::new(),
        default_model: None,
    })?;
    assert!(matches!(
        owner.validate_dispatch(),
        Err(CredentialRefusal::Absent)
    ));
    Ok(())
}
