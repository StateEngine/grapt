//! Count real accepted sockets, not client handles.
use super::*;

struct KeepAlive {
    base: String,
    heads: Arc<std::sync::Mutex<Vec<(usize, String)>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for KeepAlive {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn keep_alive(statuses: &'static [u16]) -> Result<KeepAlive, String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|error| error.to_string())?;
    let base = format!(
        "http://{}",
        listener.local_addr().map_err(|e| e.to_string())?
    );
    let heads = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = Arc::clone(&heads);
    let ordinal = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn(async move {
        // Both accepted sockets and requests per socket have fixed bounds.
        let mut connections = tokio::task::JoinSet::new();
        for connection in 0..8 {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let observed = Arc::clone(&observed);
            let ordinal = Arc::clone(&ordinal);
            connections.spawn(async move {
                for _ in 0..8 {
                    let mut raw = Vec::new();
                    let mut buffer = [0_u8; 4096];
                    let head = loop {
                        let read = socket.read(&mut buffer).await.ok()?;
                        if read == 0 || raw.len() + read > 256 * 1024 {
                            return None;
                        }
                        raw.extend_from_slice(&buffer[..read]);
                        let text = String::from_utf8_lossy(&raw);
                        if let Some(end) = text.find("\r\n\r\n") {
                            let head = &text[..end];
                            let length = head.lines().find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .and_then(|value| value.parse::<usize>().ok())
                            })?;
                            if raw.len() >= end + 4 + length {
                                break head.to_owned();
                            }
                        }
                    };
                    observed.lock().ok()?.push((connection, head));
                    let at = ordinal.fetch_add(1, Ordering::SeqCst);
                    let status = statuses.get(at).copied().unwrap_or(200);
                    let body = sse(&[
                        r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
                        "[DONE]",
                    ]);
                    let head = format!(
                        "HTTP/1.1 {status} fixture\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n",
                        body.len()
                    );
                    socket.write_all(head.as_bytes()).await.ok()?;
                    socket.write_all(&body).await.ok()?;
                }
                Some(())
            });
        }
        while connections.join_next().await.is_some() {}
    });
    Ok(KeepAlive { base, heads, task })
}

async fn call(
    adapter: &ProviderAdapter,
    fixture: &KeepAlive,
    id: &str,
    account: &str,
    version: &str,
) -> Result<(), String> {
    let mut input = input(
        id,
        &fixture.base,
        ProtocolFamily::OpenAiCompatible,
        compat_request(),
    );
    input.lease = CredentialLease::issue(
        grapt_types::AccountId::new(account).ok_or("account")?,
        grapt_types::CredentialVersion::new(version).ok_or("credential version")?,
        CredentialPlacement::BearerAuthorization,
        grapt_types::Secret::new(format!("fixture-{account}-{version}")),
    )
    .map_err(|error| error.to_string())?;
    let opened = tokio::time::timeout(Duration::from_secs(5), adapter.open(input))
        .await
        .map_err(|_| "pool fixture timed out")?
        .map_err(|error| error.to_string())?;
    let _fragments = drain(opened).await;
    Ok(())
}

fn connections(fixture: &KeepAlive) -> Result<Vec<usize>, String> {
    let heads = fixture.heads.lock().map_err(|_| "heads poisoned")?;
    Ok(heads.iter().map(|(connection, _)| *connection).collect())
}

#[tokio::test]
async fn consecutive_steps_reuse_only_the_same_account_and_credential_pool() -> Result<(), String> {
    let fixture = keep_alive(&[]).await?;
    let adapter = adapter(1);
    for (id, account, version) in [
        ("pool_a1", "a", "1"),
        ("pool_a2", "a", "1"),
        ("pool_b1", "b", "1"),
        ("pool_b2", "b", "1"),
        ("pool_a3", "a", "1"),
        ("pool_a4", "a", "2"),
    ] {
        call(&adapter, &fixture, id, account, version).await?;
    }
    assert_eq!(connections(&fixture)?, [0, 0, 1, 1, 2, 3]);
    let heads = fixture.heads.lock().map_err(|_| "heads poisoned")?;
    for ((_, head), expected) in heads.iter().zip(["a-1", "a-1", "b-1", "b-1", "a-1", "a-2"]) {
        assert!(head.contains(&format!("authorization: Bearer fixture-{expected}")));
    }
    Ok(())
}

#[tokio::test]
async fn a_new_origin_drops_the_previous_idle_pool() -> Result<(), String> {
    let first = keep_alive(&[]).await?;
    let second = keep_alive(&[]).await?;
    let adapter = adapter(1);
    call(&adapter, &first, "pool_origin_a1", "a", "1").await?;
    call(&adapter, &first, "pool_origin_a2", "a", "1").await?;
    call(&adapter, &second, "pool_origin_b1", "a", "1").await?;
    call(&adapter, &first, "pool_origin_a3", "a", "1").await?;
    assert_eq!(connections(&first)?, [0, 0, 1]);
    assert_eq!(connections(&second)?, [0]);
    Ok(())
}

#[tokio::test]
async fn an_authentication_refusal_drops_idle_connections() -> Result<(), String> {
    let fixture = keep_alive(&[200, 401, 200, 403, 200]).await?;
    let adapter = adapter(1);
    for index in 0..5 {
        call(
            &adapter,
            &fixture,
            &format!("pool_revoke_{index}"),
            "a",
            "1",
        )
        .await?;
    }
    assert_eq!(connections(&fixture)?, [0, 0, 1, 1, 2]);
    Ok(())
}

struct RevocableOwner(std::sync::atomic::AtomicBool);

impl crate::CredentialOwner for RevocableOwner {
    fn current(&self) -> crate::CredentialRenewalFuture<'_, crate::CurrentCredential> {
        Box::pin(async move {
            if self.0.load(Ordering::SeqCst) {
                return Err(crate::CredentialRenewalRefusal::SourceAbsent);
            }
            let unavailable = crate::CredentialRenewalRefusal::Unavailable;
            let credential = crate::ProviderCredential::inject(
                grapt_types::AccountId::new("a").ok_or(unavailable)?,
                grapt_types::CredentialVersion::new("1").ok_or(unavailable)?,
                CredentialPlacement::BearerAuthorization,
                grapt_types::Secret::new("fixture-current-token".to_owned()),
            )
            .map_err(|_| unavailable)?;
            Ok(crate::CurrentCredential {
                credential,
                revision: crate::CredentialRevision::new(1),
            })
        })
    }

    fn recover(&self, _: crate::CredentialRevision) -> crate::CredentialRenewalFuture<'_, bool> {
        Box::pin(async { Ok(false) })
    }

    fn accepted(&self, _: crate::CredentialRevision) -> crate::CredentialRenewalFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn a_revoked_credential_source_cannot_resurrect_its_idle_pool() -> Result<(), String> {
    let fixture = keep_alive(&[]).await?;
    let owner = Arc::new(RevocableOwner(std::sync::atomic::AtomicBool::new(false)));
    let adapter = adapter(1).with_credential_owner(owner.clone());
    call(&adapter, &fixture, "pool_source_present", "a", "1").await?;
    owner.0.store(true, Ordering::SeqCst);
    call(&adapter, &fixture, "pool_source_revoked", "a", "1").await?;
    assert_eq!(connections(&fixture)?, [0]);
    owner.0.store(false, Ordering::SeqCst);
    call(&adapter, &fixture, "pool_source_restored", "a", "1").await?;
    assert_eq!(connections(&fixture)?, [0, 1]);
    Ok(())
}

#[test]
fn preconfigured_tls_negotiates_http2_with_http1_fallback() -> Result<(), String> {
    let config = crate::transport::client::tls_config().map_err(|error| error.to_string())?;
    assert_eq!(
        config.alpn_protocols,
        [b"h2".to_vec(), b"http/1.1".to_vec()]
    );
    Ok(())
}
