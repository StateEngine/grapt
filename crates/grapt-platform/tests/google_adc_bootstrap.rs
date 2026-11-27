//! Store-free provider preparation remains synchronous and does not renew tokens.
use grapt_connections::store::{
    AuthMethod, Connection, Connections, CredentialFormat, DefaultModel, Family, Store,
};
use grapt_login::vault::{
    ApiDestination, CredentialVault, DestinationBinding, SealedFileVault, VaultKey, VaultSecret,
};
use grapt_types::{
    AccountId, CredentialVersion, ModelId, ModelRouteId, PolicyVersion, ProviderId, Timestamp,
};
use std::{path::PathBuf, process::Command};
type TestResult = Result<(), Box<dyn std::error::Error>>;
struct Profile(PathBuf);
impl Drop for Profile {
    fn drop(&mut self) {
        let _cleanup = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn configured_adc_prepares_without_an_async_runtime_or_token_exchange() -> TestResult {
    const CHILD: &str = "GRAPT_ADC_BOOTSTRAP_CHILD";
    if std::env::var(CHILD).as_deref() == Ok("1") {
        let root = std::env::current_dir()?;
        let prepared = grapt_platform::PreparedProviderInputs::prepare(&root)?.into_components()?;
        assert_eq!(
            prepared.credential.placement(),
            grapt_provider::CredentialPlacement::VertexAdc
        );
        return Ok(());
    }
    let path = std::env::temp_dir().join(format!("grapt-adc-bootstrap-{}", std::process::id()));
    std::fs::create_dir(&path)?;
    let root = Profile(std::fs::canonicalize(path)?);
    grapt_login::profile_security::protect_directory(&root.0)?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    std::fs::write(
        root.0.join("adc.json"),
        serde_json::to_vec(&serde_json::json!({
            "type":"authorized_user", "client_id":"fixture-client", "client_secret":"synthetic-secret",
            "refresh_token":"synthetic-refresh", "token_uri":format!("http://{}/token", listener.local_addr()?)
        }))?,
    )?;
    let provider = ProviderId::new("google-fixture").ok_or("provider")?;
    let endpoint =
        "https://aiplatform.googleapis.com/v1/projects/fixture/locations/global/publishers/google";
    let binding = DestinationBinding::key(ApiDestination::parse(endpoint)?);
    let key = VaultKey::new(provider.clone(), "adc.credential")?;
    SealedFileVault::open(&root.0)?.put(
        &key,
        &binding,
        VaultSecret::vertex_adc(),
        Timestamp::from_millis(1),
    )?;
    let model = "gemini-3.1-pro-preview-customtools";
    let records = Connections {
        connections: vec![Connection {
            id: provider.clone(),
            catalog_provider: ProviderId::new("google-vertex").ok_or("catalog")?,
            family: Family::VertexGemini,
            method: AuthMethod::GoogleAdc,
            label: "Google fixture".into(),
            endpoint: Some(endpoint.into()),
            watsonx_project: None,
            account: AccountId::new("google-account").ok_or("account")?,
            route: ModelRouteId::new("vertex").ok_or("route")?,
            credential_version: CredentialVersion::new("adc-v1").ok_or("version")?,
            policy_version: PolicyVersion::new("policy").ok_or("policy")?,
            credential_file: "adc.credential".into(),
            credential_format: CredentialFormat::GoogleAdc { binding },
            discovery_file: "adc-models.json".into(),
            oauth_file: None,
        }],
        default_model: Some(DefaultModel {
            provider,
            model: ModelId::new(model).ok_or("model")?,
            variant: None,
        }),
    };
    Store::open(&root.0)?.save(&records)?;
    std::fs::write(
        root.0.join("grapt.jsonc"),
        br#"{"store":{"authority":"open-ready"},"product":{"build":"adc-fixture"}}"#,
    )?;
    let listing = grapt_connections::catalog::KeyProvider::from_id("google-vertex")?
        .ok_or("catalog")?
        .vertex_listing(model)?;
    std::fs::write(root.0.join("adc-models.json"), listing)?;
    let mut child = Command::new(std::env::current_exe()?);
    child.env_clear();
    for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(key) {
            child.env(key, value);
        }
    }
    let status = child
        .args([
            "--exact",
            "configured_adc_prepares_without_an_async_runtime_or_token_exchange",
            "--nocapture",
        ])
        .current_dir(&root.0)
        .env(CHILD, "1")
        .env("HOME", &root.0)
        .env("USERPROFILE", &root.0)
        .env("CLOUDSDK_CONFIG", &root.0)
        .env("GOOGLE_APPLICATION_CREDENTIALS", root.0.join("adc.json"))
        .status()?;
    assert!(status.success(), "synchronous ADC preparation failed");
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "preparation initiated token exchange"
    );
    Ok(())
}
