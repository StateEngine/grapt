use super::*;
use grapt_types::Timestamp;
type TestResult = Result<(), Box<dyn std::error::Error>>;
#[derive(Debug)]
struct Clock;
impl ClockAuthority for Clock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1000)
    }
}
struct Profile(std::path::PathBuf);
impl Profile {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!(
            "grapt-mcp-auth-{}",
            grapt_store::OsRandomIds::seed()?.instance()
        ));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn fixture(profile: &Profile) -> Result<Arc<AuthManager>, Box<dyn std::error::Error>> {
    let store = grapt_store::EventStore::open_in_memory()?;
    let inbox = Arc::new(Mutex::new(Inbox::new(store)?));
    Ok(AuthManager::new(&profile.0, inbox, Arc::new(Clock))?)
}
fn credential(
    resource: &str,
    generation: &str,
) -> Result<OAuthCredential, Box<dyn std::error::Error>> {
    use grapt_mcp_client::oauth::{tokens::TokenGrant, OAuthMetadata};
    let metadata=OAuthMetadata::parse(br#"{"issuer":"https://auth.example","authorization_endpoint":"https://auth.example/authorize","token_endpoint":"https://auth.example/token","code_challenge_methods_supported":["S256"]}"#,"https://auth.example")?;
    Ok(OAuthCredential::from_response(&metadata,resource,"public-client",&Secret::new(br#"{"token_type":"Bearer","access_token":"held-access","refresh_token":"held-refresh"}"#.to_vec()),1000,TokenGrant::Login(generation.parse()?))?)
}
#[tokio::test]
async fn anonymous_access_does_not_discover_or_start_sign_in_and_logout_prevents_resurrection(
) -> TestResult {
    let profile = Profile::new()?;
    let manager = fixture(&profile)?;
    let resource = "https://mcp.invalid/mcp";
    assert!(manager
        .bearer("docs", resource)
        .acquire(None)
        .await?
        .is_none());
    let key = AuthManager::key("docs", resource)?;
    let stale = manager.read(&key)?.revision;
    manager.logout("docs", resource).await?;
    let logged_out = manager.read(&key)?;
    assert_eq!(logged_out.revision, McpCredentialRevision::Stored(1));
    assert!(logged_out.value.is_none());
    assert!(manager
        .write(
            &key,
            stale,
            Some(credential(resource, "mcl_late")?.encode()?)
        )
        .is_err());
    assert!(manager
        .bearer("docs", resource)
        .acquire(None)
        .await?
        .is_none());
    Ok(())
}
#[tokio::test]
async fn committed_credentials_have_exact_alias_resource_and_login_generation() -> TestResult {
    let profile = Profile::new()?;
    let manager = fixture(&profile)?;
    let resource = "https://mcp.invalid/mcp?tenant=first";
    let key = AuthManager::key("docs", resource)?;
    let credential = credential(resource, "mcl_first")?;
    manager.write(
        &key,
        McpCredentialRevision::Missing,
        Some(credential.encode()?),
    )?;
    let held = manager
        .bearer("docs", resource)
        .acquire(None)
        .await?
        .ok_or("missing held credential")?;
    assert_eq!(held.token.expose(), "held-access");
    assert_eq!(held.generation.as_str(), "mcl_first");
    assert_eq!(held.revision, 1);
    assert!(!format!("{held:?}").contains("held-access"));
    assert!(manager
        .bearer("other", resource)
        .acquire(None)
        .await?
        .is_none());
    assert!(manager
        .bearer("docs", "https://mcp.invalid/mcp?tenant=second")
        .acquire(None)
        .await?
        .is_none());
    manager.logout("docs", resource).await?;
    assert!(manager
        .bearer("docs", resource)
        .acquire(None)
        .await?
        .is_none());
    Ok(())
}
#[tokio::test]
async fn corrupt_stored_credentials_are_errors_and_can_be_explicitly_signed_out() -> TestResult {
    let profile = Profile::new()?;
    let manager = fixture(&profile)?;
    let resource = "https://mcp.invalid/mcp";
    let key = AuthManager::key("docs", resource)?;
    manager.write(
        &key,
        McpCredentialRevision::Missing,
        Some(Secret::new("not-json-private".into())),
    )?;
    let error = manager
        .bearer("docs", resource)
        .acquire(None)
        .await
        .err()
        .ok_or("corrupt credential treated as absence")?;
    assert!(!error.to_string().contains("not-json-private"));
    manager.logout("docs", resource).await?;
    assert!(manager.read(&key)?.value.is_none());
    Ok(())
}

#[tokio::test]
async fn a_misbound_stored_credential_never_releases_its_access_token() -> TestResult {
    let profile = Profile::new()?;
    let manager = fixture(&profile)?;
    let resource = "https://mcp.invalid/mcp";
    let key = AuthManager::key("docs", resource)?;
    manager.write(
        &key,
        McpCredentialRevision::Missing,
        Some(credential("https://other.invalid/mcp", "mcl_other")?.encode()?),
    )?;
    assert!(manager
        .bearer("docs", resource)
        .acquire(None)
        .await
        .is_err());
    Ok(())
}

#[path = "tests/flow.rs"]
mod flow;
