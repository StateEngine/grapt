//! A reused OAuth slot retires the original opener and fences old refresh writes.
use super::*;
use grapt_connections::store::{
    next_key_files, AuthMethod, Connection, Connections, CredentialFormat, DefaultModel, Family,
    Store,
};
use grapt_login::vault::{
    ApiDestination, DestinationBinding, OAuthRefreshDestination, VaultSecret,
};
use grapt_provider::{AttemptOpener, CodexDeadlines, CodexLimits, FinishReason};
use grapt_types::{
    AccountId, CredentialVersion, ModelId, ModelRouteId, PolicyVersion, ProviderId,
    ResolvedModelTarget,
};
use std::{io, path::Path};

struct Held {
    target: ResolvedModelTarget,
    opener: AttemptOpener,
    owner: Arc<LiveCredentials>,
}
#[path = "generation/removal_race.rs"]
mod removal_race;

fn prepare(profile: &Path, records: &Connections, issuer: &Issuer) -> TestResult<Held> {
    prepare_with(profile, records, issuer, |authority| authority)
}
fn prepare_with(
    profile: &Path,
    records: &Connections,
    issuer: &Issuer,
    wrap: impl FnOnce(
        Arc<dyn grapt_login::chatgpt::SignInLockAuthority>,
    ) -> Arc<dyn grapt_login::chatgpt::SignInLockAuthority>,
) -> TestResult<Held> {
    let parts =
        grapt_platform::PreparedProviderInputs::prepare_pending_connections(profile, records)?
            .into_components()?;
    let target = parts.targets.issue()?;
    let request = CredentialResolutionRequest::new(
        &target,
        parts.credential.record().clone(),
        ProtocolFamily::ChatGptCodex,
        parts.credential.placement(),
    );
    let (credential, mut source) = parts.credential.into_refreshable(request)?;
    let _baseline = source.current()?;
    let (root, files) = source.chatgpt_sign_in().ok_or("sign-in owner missing")?;
    let session = Session::open_with_files(root, target.account().as_str(), files.clone())?
        .ok_or("sign-in state missing")?;
    let session = match source.sign_in_lock_authority() {
        Some(authority) => session.with_lock_authority(wrap(authority)),
        None => session,
    };
    let owner = Arc::new(LiveCredentials {
        state: Arc::new(Mutex::new(Some(State {
            source,
            cloud: None,
            sign_in: Some(SignIn {
                session,
                client: client(Some(Duration::from_secs(30)))?,
                issuer: issuer.base.clone(),
            }),
            unaccepted_refresh: None,
            accepted_revision: None,
        }))),
        gate: Arc::new(tokio::sync::Semaphore::new(1)),
        clock: Arc::new(Fixed),
    });
    let capacity = std::num::NonZeroUsize::new(1).ok_or("provider capacity")?;
    let opener = AttemptOpener::codex(
        CodexDeadlines::new(Duration::from_secs(15), Duration::from_secs(15))?,
        CodexLimits::new(capacity),
        records.connections[0]
            .endpoint
            .as_deref()
            .ok_or("API endpoint missing")?,
        target.route().clone(),
        "grapt-generation-test",
        credential,
    )
    .map_err(|error| format!("opener refused: {error:?}"))?
    .with_credential_owner(owner.clone());
    Ok(Held {
        target,
        opener,
        owner,
    })
}
fn proposed(
    mut records: Connections,
    binding: &DestinationBinding,
    api: &str,
    generation: usize,
) -> TestResult<Connections> {
    let id = ProviderId::new("openai").ok_or("provider")?;
    let (credential_file, discovery_file) =
        next_key_files(&id, records.connections.first(), "oauth-generation")?;
    records.connections = vec![Connection {
        id: id.clone(),
        catalog_provider: id.clone(),
        family: Family::ChatgptCodex,
        method: AuthMethod::OauthDevice,
        label: "ChatGPT fixture".into(),
        endpoint: Some(api.into()),
        watsonx_project: None,
        account: AccountId::new("account-a").ok_or("account")?,
        route: ModelRouteId::new("responses").ok_or("route")?,
        credential_version: CredentialVersion::new(format!("oauth-generation-{generation}"))
            .ok_or("version")?,
        policy_version: PolicyVersion::new("fixture-policy").ok_or("policy")?,
        oauth_file: Some(format!(
            "{}.oauth.json",
            credential_file.trim_end_matches(".credential")
        )),
        credential_file,
        discovery_file,
        credential_format: CredentialFormat::VaultedOAuth {
            binding: binding.clone(),
        },
    }];
    records.default_model = Some(DefaultModel {
        provider: id,
        model: ModelId::new("gpt-5.6").ok_or("model")?,
        variant: None,
    });
    Ok(records)
}
fn install(
    profile: &Path,
    records: &Connections,
    issuer: &Issuer,
    binding: &DestinationBinding,
    access: &str,
    expiry: i64,
) -> io::Result<Held> {
    let id = &records.connections[0].id;
    Store::open(profile)?.install_oauth_with(records, id,
        VaultSecret::oauth(tokens("account-a", access, expiry)).map_err(io::Error::other)?, binding, &issuer.base,
        br#"{"models":[{"slug":"gpt-5.6","context_window":272000,"supported_reasoning_levels":[]}]}"#,
        Timestamp::from_millis(NOW), || prepare(profile, records, issuer).map_err(|error| io::Error::other(error.to_string())))
        .map(|(held, commit)| { assert!(commit.durability_error.is_none() && commit.cleanup_error.is_none()); held })
}
async fn poll(
    held: Held,
    name: &str,
) -> TestResult<Result<FinishReason, grapt_provider::ProviderFailureClass>> {
    let mut builder = grapt_provider::PhysicalAttemptAuthoritySnapshot::builder();
    builder.model(held.target)?;
    let authority = builder.build()?;
    let request =
        grapt_provider::ProviderRequest::ChatGptCodex(grapt_provider::ChatGptCodexRequest {
            model: authority.model().model_ref().clone(),
            input: Vec::new(),
            prompt_cache_key: None,
            tools: Vec::new(),
            web_search: None,
            tool_choice: None,
            reasoning: Default::default(),
            turn: Default::default(),
        });
    let (_custody, mut reader) = held
        .opener
        .open(
            grapt_types::PhysicalAttemptId::new(name).ok_or("attempt")?,
            authority,
            request,
        )
        .map_err(|error| format!("opener refused: {error:?}"))?
        .into_parts();
    // Protected native credential IO is outside the provider's request deadline.
    // Keep observing that deadline's actual result instead of ending the fixture first.
    tokio::time::timeout(Duration::from_secs(120), async {
        for _ in 0..32 {
            match reader.next_fragment().await {
                Some(grapt_provider::AttemptFragment::ProviderTerminal { finish, .. }) => {
                    return Ok(Ok(finish))
                }
                Some(grapt_provider::AttemptFragment::ProviderFailure { class, .. }) => {
                    return Ok(Err(class))
                }
                Some(_) => (),
                None => break,
            }
        }
        Err("provider did not terminate".into())
    })
    .await
    .map_err(|_| format!("credential dispatch fixture did not finish: {name}"))?
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn three_oauth_installs_retire_the_old_opener_and_refuse_an_interleaved_refresh_overwrite(
) -> TestResult {
    let profile = Profile(
        std::env::temp_dir().join(format!("grapt-oauth-generations-{}", std::process::id())),
    );
    std::fs::create_dir(&profile.0)?;
    grapt_login::profile_security::protect_directory(&profile.0)?;
    std::fs::write(
        profile.0.join("grapt.jsonc"),
        br#"{"store":{"authority":"open-ready"},"product":{"build":"oauth-generation-test"}}"#,
    )?;
    std::fs::write(
        profile.0.join("model-catalog.json"),
        grapt_connections::catalog::BUNDLED,
    )?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let api = format!("http://{}", listener.local_addr()?);
    let wire = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0; 1024];
            let count = socket.read(&mut chunk).await?;
            if count == 0 || bytes.len() + count > 32 * 1024 {
                return Err(io::Error::other("provider request exceeded its bound"));
            }
            bytes.extend_from_slice(&chunk[..count]);
            let text = String::from_utf8_lossy(&bytes);
            if let Some((head, body)) = text.split_once("\r\n\r\n") {
                let length = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|length| length.parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if body.len() >= length {
                    break;
                }
            }
        }
        let request = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
        assert!(request.contains("authorization: bearer third-token\r\n"));
        let body = "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n";
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await
    });
    let consumed = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let issuer = Issuer::with_hold(
        vec![answer(NOW + 72 * HOUR)],
        Some((consumed.clone(), release.clone())),
    )
    .await?;
    let binding = DestinationBinding::oauth(
        ApiDestination::parse(&api)?,
        OAuthRefreshDestination::parse(&issuer.base)?,
    );
    let first_records = proposed(
        Connections {
            connections: Vec::new(),
            default_model: None,
        },
        &binding,
        &api,
        1,
    )?;
    let first = install(
        &profile.0,
        &first_records,
        &issuer,
        &binding,
        "first-token",
        NOW + 10_000,
    )?;
    let second_records = proposed(first_records, &binding, &api, 2)?;
    let third_records = proposed(second_records.clone(), &binding, &api, 3)?;
    {
        let refresh = first.owner.current();
        tokio::pin!(refresh);
        tokio::select! { () = consumed.notified() => (), result = &mut refresh => return Err(format!("refresh did not reach held issuer: {}", result.is_ok()).into()) }
        let _second = install(
            &profile.0,
            &second_records,
            &issuer,
            &binding,
            "second-token",
            NOW + 72 * HOUR,
        )?;
        assert_eq!(
            install(
                &profile.0,
                &third_records,
                &issuer,
                &binding,
                "third-token",
                NOW + 72 * HOUR
            )
            .err()
            .ok_or("installation overwrote in-flight refresh")?
            .kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(
            Store::read_snapshot(&profile.0)?,
            Some(second_records.clone())
        );
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(120), &mut refresh)
            .await
            .map_err(|_| "released OAuth refresh did not finish protected credential IO")??;
    }
    assert_eq!(Store::read_snapshot(&profile.0)?, Some(second_records));
    let third = install(
        &profile.0,
        &third_records,
        &issuer,
        &binding,
        "third-token",
        NOW + 72 * HOUR,
    )?;
    assert_eq!(
        first.target.credential_version().as_str(),
        "oauth-generation-1"
    );
    assert_eq!(
        third.target.credential_version().as_str(),
        "oauth-generation-3"
    );
    assert!(
        first.owner.current().await.is_err(),
        "retired grant adopted the third sign-in"
    );
    assert_eq!(issuer.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        poll(first, "pat_retired_oauth").await?,
        Err(grapt_provider::ProviderFailureClass::Authentication)
    );
    let current_owner = Arc::clone(&third.owner);
    assert_eq!(
        poll(third, "pat_current_oauth").await?,
        Ok(FinishReason::Stop)
    );
    tokio::time::timeout(Duration::from_secs(120), wire)
        .await
        .map_err(|_| "current OAuth dispatch did not reach the wire fixture")???;
    assert_eq!(
        Store::read_snapshot(&profile.0)?,
        Some(third_records.clone())
    );
    // Visible removal must retire refresh admission even while physical
    // cleanup is pending and the encrypted current snapshot is still present.
    Store::open(&profile.0)?.save(&Connections {
        connections: Vec::new(),
        default_model: None,
    })?;
    assert!(
        current_owner.current().await.is_err(),
        "pending deletion retained OAuth refresh authority"
    );
    Store::open(&profile.0)?.save(&third_records)?;
    let removed = Connections {
        connections: Vec::new(),
        default_model: None,
    };
    Store::open(&profile.0)?.remove_with(
        &ProviderId::new("openai").ok_or("provider")?,
        &removed,
        || Ok(()),
    )?;
    assert!(
        current_owner.current().await.is_err(),
        "removed OAuth owner restored its sign-in"
    );
    assert_eq!(issuer.calls.load(Ordering::SeqCst), 1);
    assert_eq!(Store::read_snapshot(&profile.0)?, Some(removed));
    for slot in ["a", "b"] {
        assert!(
            !profile
                .0
                .join(format!("connect-oauth-generation-{slot}.oauth.json.lock"))
                .exists(),
            "retired OAuth lock remains"
        );
    }
    Ok(())
}
