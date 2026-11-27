//! An accepted opener retains its version's API key across bounded slot reuse.
use super::*;
use grapt_connections::store::{
    next_key_files, AuthMethod, Connection, Connections, CredentialFormat, DefaultModel, Family,
    Store,
};
use grapt_login::vault::{ApiDestination, DestinationBinding, VaultSecret};
use grapt_types::{AccountId, CredentialVersion, ModelRouteId, PolicyVersion, Timestamp};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
struct Profile(PathBuf);
impl Drop for Profile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn retained_openers_keep_their_key_and_version_after_three_connection_installs() -> TestResult
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let mut observed = Vec::new();
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await?;
            let mut wire = Vec::new();
            let header = loop {
                let mut chunk = [0; 1024];
                let n = socket.read(&mut chunk).await?;
                if n == 0 || wire.len() + n > 64 * 1024 {
                    return Err(std::io::Error::other("provider request exceeded its bound"));
                }
                wire.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&wire);
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length = head
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|n| n.parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if body.len() >= length {
                        break head.to_owned();
                    }
                }
            };
            observed.push(
                header
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("authorization: ")
                            .or_else(|| line.strip_prefix("Authorization: "))
                    })
                    .ok_or_else(|| std::io::Error::other("credential header missing"))?
                    .to_owned(),
            );
            let body = concat!("data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n", "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n");
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
        }
        Ok::<_, std::io::Error>(observed)
    });
    let profile =
        Profile(std::env::temp_dir().join(format!("grapt-retained-key-{}", std::process::id())));
    std::fs::create_dir(&profile.0)?;
    grapt_login::profile_security::protect_directory(&profile.0)?;
    std::fs::write(
        profile.0.join("grapt.jsonc"),
        br#"{"store":{"authority":"open-ready"},"product":{"build":"retained-key-test"}}"#,
    )?;
    std::fs::write(
        profile.0.join("model-catalog.json"),
        grapt_connections::catalog::BUNDLED,
    )?;
    let provider = ProviderId::new("openai").ok_or("provider identity")?;
    let model = ModelId::new("gpt-5.6").ok_or("model identity")?;
    let binding = DestinationBinding::key(ApiDestination::parse(&base)?);
    let clock = Arc::new(SystemClock);
    let (dispatch, _samples) = grapt_provider::DispatchObserver::bounded(clock.clone());
    let capacity = std::num::NonZeroUsize::new(1).ok_or("provider capacity")?;
    let limits = ProductionProviderLimits {
        connect: std::time::Duration::from_secs(5),
        response_head: std::time::Duration::from_secs(5),
        active_attempts: capacity,
        child_attempts: capacity,
    };
    let endpoint = LiveEndpoint::Production(grapt_provider::EndpointConfig::new());
    let mut records = Connections {
        connections: Vec::new(),
        default_model: Some(DefaultModel {
            provider: provider.clone(),
            model: model.clone(),
            variant: None,
        }),
    };
    let mut generations = Vec::new();
    for index in 1..=3 {
        let (credential_file, discovery_file) =
            next_key_files(&provider, records.connections.first(), "retained-fixture")?;
        records.connections = vec![Connection {
            id: provider.clone(),
            catalog_provider: provider.clone(),
            family: Family::OpenaiResponses,
            method: AuthMethod::Key,
            label: "Fixture".into(),
            endpoint: Some(base.clone()),
            watsonx_project: None,
            account: AccountId::new("fixture").ok_or("account")?,
            route: ModelRouteId::new("responses").ok_or("route")?,
            credential_version: CredentialVersion::new(format!("key-version-{index}"))
                .ok_or("version")?,
            policy_version: PolicyVersion::new("fixture-policy").ok_or("policy")?,
            credential_file,
            credential_format: CredentialFormat::Vault {
                binding: binding.clone(),
            },
            discovery_file,
            oauth_file: None,
        }];
        let mut store = Store::open(&profile.0)?;
        let (generation, commit) = store.install_key_with(
            &records,
            &provider,
            VaultSecret::api_key(format!("fixture-key-{index}").into())?,
            &binding,
            br#"{"data":[{"id":"gpt-5.6"}]}"#,
            Timestamp::from_millis(index),
            || {
                let parts = grapt_platform::PreparedProviderInputs::prepare_pending_connections(
                    &profile.0, &records,
                )
                .and_then(|inputs| inputs.into_components().map_err(Into::into))
                .map_err(std::io::Error::other)?;
                let removed_target = parts.targets.issue().map_err(std::io::Error::other)?;
                let (target, openers) = open(
                    &parts.targets,
                    parts.credential,
                    parts.secondary_credentials,
                    &endpoint,
                    limits,
                    dispatch.clone(),
                    grapt_provider::ProviderErrorText::disabled(),
                    clock.clone(),
                )
                .map_err(std::io::Error::other)?;
                Ok((target, removed_target, openers))
            },
        )?;
        assert!(commit.durability_error.is_none() && commit.cleanup_error.is_none());
        generations.push(generation);
    }
    let mut retained = Vec::new();
    for (index, (target, removed_target, openers)) in generations
        .into_iter()
        .enumerate()
        .filter(|(index, _)| *index != 1)
    {
        assert_eq!(
            target.credential_version().as_str(),
            format!("key-version-{}", index + 1)
        );
        let opener = openers.get(&provider).ok_or("retained opener missing")?;
        let mut builder = grapt_provider::PhysicalAttemptAuthoritySnapshot::builder();
        builder.model(target)?;
        let authority = builder.build()?;
        let request = grapt_provider::ProviderRequest::OpenAiResponses(
            grapt_provider::OpenAiResponsesRequest {
                model: authority.model().model_ref().clone(),
                input: Vec::new(),
                prompt_cache_key: None,
                max_output_tokens: None,
                tools: Vec::new(),
                web_search: None,
                tool_choice: None,
            },
        );
        let attempt = grapt_types::PhysicalAttemptId::new(format!("pat_retained_{index}"))
            .ok_or("attempt")?;
        let (_custody, mut reader) = opener
            .open(attempt, authority, request)
            .map_err(|error| format!("opener refused: {error:?}"))?
            .into_parts();
        let mut stopped = false;
        while let Some(fragment) =
            tokio::time::timeout(std::time::Duration::from_secs(10), reader.next_fragment()).await?
        {
            stopped |= matches!(
                fragment,
                grapt_provider::AttemptFragment::ProviderTerminal {
                    finish: grapt_provider::FinishReason::Stop,
                    ..
                }
            );
        }
        assert!(stopped, "retained credential did not finish its request");
        retained.push((removed_target, openers));
    }
    let observed = tokio::time::timeout(std::time::Duration::from_secs(10), server).await???;
    assert_eq!(observed, ["Bearer fixture-key-1", "Bearer fixture-key-3"]);
    // Reproduce a visible disconnect whose file cleanup is still pending.
    // Both keys remain present, so only connection authority can refuse them.
    Store::open(&profile.0)?.save(&Connections {
        connections: Vec::new(),
        default_model: None,
    })?;
    for (index, (target, openers)) in retained.into_iter().enumerate() {
        let mut builder = grapt_provider::PhysicalAttemptAuthoritySnapshot::builder();
        builder.model(target)?;
        let authority = builder.build()?;
        let request = grapt_provider::ProviderRequest::OpenAiResponses(
            grapt_provider::OpenAiResponsesRequest {
                model: authority.model().model_ref().clone(),
                input: Vec::new(),
                prompt_cache_key: None,
                max_output_tokens: None,
                tools: Vec::new(),
                web_search: None,
                tool_choice: None,
            },
        );
        let result = openers
            .get(&provider)
            .ok_or("retained opener absent")?
            .open(
                grapt_types::PhysicalAttemptId::new(format!("pat_removed_{index}"))
                    .ok_or("attempt")?,
                authority,
                request,
            );
        assert!(
            matches!(
                result,
                Err(grapt_provider::AttemptOpenRefusal::Credential(
                    grapt_provider::CredentialError::Revoked
                ))
            ),
            "removed key retained future dispatch authority"
        );
    }
    assert!(profile
        .0
        .join("connect-retained-fixture-a.credential")
        .exists());
    assert!(profile
        .0
        .join("connect-retained-fixture-b.credential")
        .exists());

    Ok(())
}
