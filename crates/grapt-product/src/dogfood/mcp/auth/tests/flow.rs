//! Exercise the real login owner across setup, browser callback and token persistence.
use super::*;
use grapt_mcp_client::oauth::http::testkit::{client, Recorded, Reply, Server};
use serde_json::json;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn callback(authorization: &Secret<String>, code: &str) -> TestResult {
    let authorization = url::Url::parse(authorization.expose())?;
    let pairs: std::collections::BTreeMap<_, _> = authorization
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let mut target = url::Url::parse(pairs.get("redirect_uri").ok_or("no callback URI")?)?;
    let issuer = format!("{}/issuer", authorization.origin().ascii_serialization());
    target
        .query_pairs_mut()
        .append_pair("code", code)
        .append_pair("state", pairs.get("state").ok_or("no state")?)
        .append_pair("iss", &issuer);
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut stream =
            tokio::net::TcpStream::connect(("127.0.0.1", target.port().ok_or("no callback port")?))
                .await?;
        let query = target.query().ok_or("no callback query")?;
        let request = format!(
            "GET {}?{query} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            target.path(),
            target.port().ok_or("no callback port")?
        );
        stream.write_all(request.as_bytes()).await?;
        let mut reply = Vec::new();
        stream.take(4096).read_to_end(&mut reply).await?;
        if !reply.starts_with(b"HTTP/1.1 200 OK\r\n") {
            return Err("callback was not accepted".into());
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    })
    .await??;
    Ok(())
}

async fn settled(
    manager: &AuthManager,
    resource: &str,
    id: &McpLoginId,
) -> Result<LoginState, Box<dyn std::error::Error>> {
    Ok(tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let state = manager.status("docs", resource, id)?;
            if state != LoginState::Pending {
                return Ok::<_, AuthError>(state);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await??)
}

fn reply(base: &str, request: &Recorded) -> Reply {
    match request.path.as_str() {
        "/mcp" => Reply {
            status: "401 Unauthorized",
            headers: vec![(
                "WWW-Authenticate".into(),
                format!("Bearer resource_metadata=\"{base}/resource-meta\""),
            )],
            body: Vec::new(),
            delay: Duration::ZERO,
        },
        "/resource-meta" => Reply::json(
            json!({"resource":format!("{base}/mcp"),"authorization_servers":[format!("{base}/issuer")]}),
        ),
        "/.well-known/oauth-authorization-server/issuer" => Reply::json(
            json!({"issuer":format!("{base}/issuer"),"authorization_endpoint":format!("{base}/authorize"),"token_endpoint":format!("{base}/token"),"code_challenge_methods_supported":["S256"],"authorization_response_iss_parameter_supported":true}),
        ),
        "/token" => {
            let pairs: std::collections::BTreeMap<_, _> =
                url::form_urlencoded::parse(request.body.expose())
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect();
            let Some(code) = pairs.get("code") else {
                return Reply {
                    status: "400 Bad Request",
                    headers: Vec::new(),
                    body: Vec::new(),
                    delay: Duration::ZERO,
                };
            };
            Reply::json(
                json!({"token_type":"Bearer","access_token":code,"refresh_token":format!("refresh-{code}"),"expires_in":3600}),
            )
        }
        _ => Reply {
            status: "404 Not Found",
            headers: Vec::new(),
            body: Vec::new(),
            delay: Duration::ZERO,
        },
    }
}

struct LoginTask(tokio::task::JoinHandle<Result<Secret<String>, AuthError>>);
impl Drop for LoginTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[expect(clippy::too_many_lines, reason = "legacy")]
async fn setup_race(replacement: bool) -> TestResult {
    let profile = Profile::new()?;
    let database = profile.0.join("credentials.sqlite");
    let inbox = Arc::new(Mutex::new(Inbox::new(grapt_store::EventStore::open(
        &database,
        env!("CARGO_PKG_VERSION"),
    )?)?));
    let manager = AuthManager::new(&profile.0, inbox, Arc::new(Clock))?;
    manager
        .http
        .set(Ok(client()?))
        .map_err(|_| "fixture HTTP client was already installed")?;
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let entered = Mutex::new(Some(entered_tx));
    let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(1);
    let resume = Mutex::new(resume_rx);
    let first = AtomicBool::new(true);
    let server = Server::new(if replacement { 8 } else { 4 }, move |base, request| {
        if request.path == "/mcp" && first.swap(false, Ordering::AcqRel) {
            if let Some(sender) = entered.lock().ok().and_then(|mut entered| entered.take()) {
                let _ = sender.send(());
            }
            let released = resume
                .lock()
                .is_ok_and(|receiver| receiver.recv_timeout(Duration::from_secs(5)).is_ok());
            if !released {
                return Reply {
                    status: "500 Setup Gate Timed Out",
                    headers: Vec::new(),
                    body: Vec::new(),
                    delay: Duration::ZERO,
                };
            }
        }
        reply(base, request)
    })?;
    let resource = format!("{}/mcp", server.base);
    let old_id: McpLoginId = "mcl_older".parse()?;
    let new_id: McpLoginId = "mcl_newer".parse()?;
    let older_manager = manager.clone();
    let older_resource = resource.clone();
    let older_id = old_id.clone();
    let mut older = LoginTask(tokio::spawn(async move {
        older_manager
            .login(
                "docs".to_owned(),
                older_resource,
                older_id,
                Some("fixture-client".to_owned()),
                None,
                std::sync::Weak::new(),
            )
            .await
    }));
    tokio::time::timeout(Duration::from_secs(3), entered_rx).await??;
    if replacement {
        let authorization = tokio::time::timeout(
            Duration::from_secs(4),
            manager.login(
                "docs".to_owned(),
                resource.clone(),
                new_id.clone(),
                Some("fixture-client".to_owned()),
                None,
                std::sync::Weak::new(),
            ),
        )
        .await??;
        callback(&authorization, "new-code").await?;
        assert_eq!(
            settled(&manager, &resource, &new_id).await?,
            LoginState::Complete
        );
    } else {
        tokio::time::timeout(Duration::from_secs(3), manager.logout("docs", &resource)).await??;
    }
    let key = AuthManager::key("docs", &resource)?;
    let preserved = manager.read(&key)?;
    assert_eq!(preserved.revision, McpCredentialRevision::Stored(1));
    resume_tx.try_send(())?;
    let authorization = tokio::time::timeout(Duration::from_secs(4), &mut older.0).await???;
    callback(&authorization, "old-code").await?;
    assert_eq!(
        settled(&manager, &resource, &old_id).await?,
        LoginState::Failed,
        "older setup adopted a newer committed revision"
    );
    let current = manager.read(&key)?;
    assert_eq!(current.revision, preserved.revision);
    let reopened = grapt_store::EventStore::open(&database, env!("CARGO_PKG_VERSION"))?
        .mcp_credential(&key)?;
    assert_eq!(reopened.revision, preserved.revision);
    if replacement {
        let credential =
            AuthManager::decode(&reopened)?.ok_or("replacement credential was lost")?;
        assert_eq!(credential.generation(), &new_id);
        assert_eq!(credential.access_token().expose(), "new-code");
    } else {
        assert!(
            reopened.value.is_none(),
            "older setup resurrected a committed logout"
        );
    }
    server.finish()?;
    Ok(())
}

#[tokio::test]
async fn login_setup_cannot_resurrect_a_committed_logout() -> TestResult {
    setup_race(false).await
}
#[tokio::test]
async fn login_setup_cannot_replace_a_completed_newer_login() -> TestResult {
    setup_race(true).await
}
