//! Owner-held request authentication through the native wire consumer.
use super::*;
use crate::{
    CredentialError, ProviderCredential, RequestAuthenticationFuture, RequestAuthenticator,
};
use std::sync::Mutex;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Signer {
    refuse: bool,
    calls: AtomicUsize,
    body: Mutex<Vec<u8>>,
}
impl RequestAuthenticator for Signer {
    fn authenticate(&self, mut request: reqwest::Request) -> RequestAuthenticationFuture<'_> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.refuse {
                return Err(CredentialError::Revoked);
            }
            *self
                .body
                .lock()
                .map_err(|_| CredentialError::InvalidRequest)? = request
                .body()
                .and_then(reqwest::Body::as_bytes)
                .ok_or(CredentialError::InvalidRequest)?
                .to_vec();
            request.headers_mut().insert(
                reqwest::header::AUTHORIZATION,
                reqwest::header::HeaderValue::from_static("synthetic-owner-signature"),
            );
            Ok(request)
        })
    }
}

async fn exercise(wrong_destination: bool, refuse: bool) -> TestResult {
    let fixture = serve(
        200,
        &[("content-type", "application/vnd.amazon.eventstream")],
        vec![
            frame("messageStart", serde_json::json!({"role":"assistant"})),
            frame("messageStop", serde_json::json!({"stopReason":"end_turn"})),
            frame(
                "metadata",
                serde_json::json!({"usage":{"inputTokens":1,"outputTokens":1}}),
            ),
        ],
    )
    .await;
    let signer = Arc::new(Signer {
        refuse,
        calls: AtomicUsize::new(0),
        body: Mutex::new(Vec::new()),
    });
    let request = crate::ProviderRequest::BedrockConverse(crate::BedrockConverseRequest {
        model: model("amazon-bedrock", "anthropic.claude-sonnet-5-5"),
        messages: vec![user("Signed body").into()],
        max_tokens: crate::MaxOutputTokens::new(128).ok_or("output bound")?,
        tools: Vec::new(),
        tool_choice: None,
    });
    let mut attempt = input(
        "bedrock-owner-auth",
        &fixture.base,
        ProtocolFamily::BedrockConverse,
        request,
    );
    attempt.lease = ProviderCredential::inject_aws(
        attempt.lease.account().clone(),
        attempt.lease.credential_version().clone(),
        signer.clone(),
        if wrong_destination {
            "https://foreign.example"
        } else {
            &fixture.base
        },
    )?
    .lease()?;
    let provider = adapter(1);
    let refused = wrong_destination || refuse;
    let fragments = match provider.open(attempt).await {
        Ok(open) => drain(open).await,
        Err(crate::CodexError::RequestBuild { reason }) if wrong_destination => {
            assert_eq!(reason, CredentialError::DestinationMismatch.to_string());
            Vec::new()
        }
        Err(error) => return Err(error.into()),
    };
    let signs = signer.calls.load(Ordering::SeqCst);
    let requests = fixture.requests.load(Ordering::SeqCst);
    assert_eq!(signs, usize::from(!wrong_destination));
    assert_eq!(requests, usize::from(!refused));
    assert_eq!(provider.active_len(), 0);
    if refuse {
        assert!(fragments.iter().any(|fragment| matches!(
            fragment,
            AttemptFragment::ProviderFailure {
                class: crate::ProviderFailureClass::Authentication,
                ..
            }
        )));
    } else if !wrong_destination {
        assert_eq!(finish_of(&fragments), Some(FinishReason::Stop));
        let signed = signer.body.lock().map_err(|_| "signed body lock")?;
        let sent = fixture.body.lock().map_err(|_| "wire body lock")?;
        assert_eq!(signed.as_slice(), sent.as_bytes());
        let body: serde_json::Value = serde_json::from_slice(&signed)?;
        assert_eq!(body["messages"][0]["content"][0]["text"], "Signed body");
        assert_eq!(body["inferenceConfig"]["maxTokens"], 128);
        let head = fixture.head.lock().map_err(|_| "wire head lock")?;
        assert!(head.contains("authorization: synthetic-owner-signature"));
    }
    Ok(())
}

#[tokio::test]
async fn aws_wrong_destination_refuses_before_invoking_signer() -> TestResult {
    exercise(true, false).await
}

#[tokio::test]
async fn aws_owner_refusal_sends_no_http() -> TestResult {
    exercise(false, true).await
}

#[tokio::test]
async fn aws_signs_final_staged_body_and_sends_exactly_once() -> TestResult {
    exercise(false, false).await
}
