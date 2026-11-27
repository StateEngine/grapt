use super::*;
use crate::http::auth::{BearerFuture, BearerLease, BearerProvider};
use grapt_types::Secret;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

type LeaseStep = Option<(&'static str, &'static str, u64)>;

struct Sequence {
    steps: Mutex<VecDeque<LeaseStep>>,
    rejected: Mutex<Vec<Option<u64>>>,
}
impl Sequence {
    fn new(steps: Vec<LeaseStep>) -> Arc<Self> {
        Arc::new(Self {
            steps: Mutex::new(steps.into()),
            rejected: Mutex::new(Vec::new()),
        })
    }
}
impl BearerProvider for Sequence {
    fn acquire(&self, rejected: Option<u64>) -> BearerFuture<'_> {
        Box::pin(async move {
            self.rejected
                .lock()
                .map_err(|_| HttpError::State)?
                .push(rejected);
            self.steps
                .lock()
                .map_err(|_| HttpError::State)?
                .pop_front()
                .ok_or(HttpError::State)?
                .map(|(generation, token, revision)| {
                    Ok(BearerLease {
                        generation: generation.parse().map_err(|_| HttpError::State)?,
                        token: Secret::new(token.to_owned()),
                        revision,
                    })
                })
                .transpose()
        })
    }
}
const CALL:&[u8]=br#"{"jsonrpc":"2.0","id":17,"method":"tools/call","params":{"name":"echo","arguments":{"text":"once"}}}"#;
#[tokio::test]
async fn explicit_401_refreshes_once_with_same_generation_and_exact_request() -> TestResult {
    let (endpoint, task) = fixture(vec![
        response("401 Unauthorized", "", ""),
        response(
            "200 OK",
            "Content-Type: application/json\r\n",
            r#"{"jsonrpc":"2.0","id":17,"result":{}}"#,
        ),
    ])
    .await?;
    let provider = Sequence::new(vec![
        Some(("mcl_first", "expired", 1)),
        Some(("mcl_first", "rotated", 2)),
    ]);
    let client = HttpClient::new(&endpoint, 4096)?.with_bearer(provider.clone())?;
    let _ = client.post(CALL, BUDGET).await?;
    let requests = task.await?.map_err(|e| e.to_string())?;
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("authorization: Bearer expired\r\n"));
    assert!(requests[1].contains("authorization: Bearer rotated\r\n"));
    assert_eq!(
        requests[0].split_once("\r\n\r\n").map(|v| v.1),
        requests[1].split_once("\r\n\r\n").map(|v| v.1)
    );
    assert_eq!(
        *provider.rejected.lock().map_err(|_| "fixture lock")?,
        vec![None, Some(1)]
    );
    Ok(())
}
#[tokio::test]
async fn a_new_login_cannot_replay_a_call_approved_for_the_old_account() -> TestResult {
    let (endpoint, task) = fixture(vec![response("401 Unauthorized", "", "")]).await?;
    let provider = Sequence::new(vec![
        Some(("mcl_first", "first", 1)),
        Some(("mcl_second", "second", 2)),
    ]);
    let client = HttpClient::new(&endpoint, 4096)?.with_bearer(provider)?;
    assert!(matches!(
        client.post(CALL, BUDGET).await,
        Err(HttpError::Authentication)
    ));
    assert_eq!(task.await?.map_err(|e| e.to_string())?.len(), 1);
    Ok(())
}
#[tokio::test]
async fn anonymous_sessions_cannot_silently_acquire_a_new_login() -> TestResult {
    let (endpoint, task) = fixture(vec![response("202 Accepted", "", "")]).await?;
    let provider = Sequence::new(vec![None, Some(("mcl_new", "new-account", 1))]);
    let client = HttpClient::new(&endpoint, 4096)?.with_bearer(provider)?;
    let notification = br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    assert!(matches!(
        client.post(notification, BUDGET).await?,
        HttpReply::Accepted
    ));
    assert!(matches!(
        client.post(CALL, BUDGET).await,
        Err(HttpError::Authentication)
    ));
    assert_eq!(task.await?.map_err(|e| e.to_string())?.len(), 1);
    Ok(())
}
#[tokio::test]
async fn an_ambiguous_network_failure_never_authorizes_refresh_or_replay() -> TestResult {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/mcp", listener.local_addr()?);
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        request(&mut stream).await
    });
    let provider = Sequence::new(vec![Some(("mcl_first", "first", 1))]);
    let client = HttpClient::new(&endpoint, 4096)?.with_bearer(provider.clone())?;
    assert!(matches!(
        client.post(CALL, BUDGET).await,
        Err(HttpError::Transport)
    ));
    assert!(task
        .await?
        .map_err(|e| e.to_string())?
        .ends_with(std::str::from_utf8(CALL)?));
    assert_eq!(
        provider.rejected.lock().map_err(|_| "fixture lock")?.len(),
        1
    );
    Ok(())
}
#[tokio::test]
async fn token_acquisition_is_inside_the_original_request_deadline() -> TestResult {
    struct Waiting;
    impl BearerProvider for Waiting {
        fn acquire(&self, _: Option<u64>) -> BearerFuture<'_> {
            Box::pin(async { std::future::pending().await })
        }
    }
    let client = HttpClient::new("http://127.0.0.1:1/mcp", 4096)?.with_bearer(Arc::new(Waiting))?;
    assert!(matches!(
        tokio::time::timeout(
            Duration::from_secs(1),
            client.post(CALL, Duration::from_millis(20))
        )
        .await?,
        Err(HttpError::TimedOut)
    ));
    Ok(())
}
