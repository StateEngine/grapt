use super::*;
use grapt_client::{TransportError, WireResponse};
use std::collections::VecDeque;

#[derive(Debug)]
struct Peer(VecDeque<(Operation, WireResponse)>);
impl Transport for Peer {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        let (operation, response) = self.0.pop_front().ok_or(TransportError::NotConnected)?;
        assert_eq!(request.operation(), operation);
        assert_eq!(
            request.query(),
            &[("location[directory]".into(), "/work/my project".into())]
        );
        assert!(!request.path().contains('{'));
        if operation == Operation::IntegrationKeyConnect {
            assert_eq!(operation.method().as_str(), "POST");
            let body: IntegrationKeyConnectRequestDto =
                serde_json::from_slice(request.body().ok_or(TransportError::NotConnected)?)
                    .map_err(|_| TransportError::NotConnected)?;
            if body.bedrock.as_present().is_some() {
                assert!(body.key.as_str().is_empty());
            } else {
                assert_eq!(body.key.as_str(), "private-fixture-key");
            }
        }
        Ok(response)
    }
}
fn reply(
    operation: Operation,
    body: serde_json::Value,
) -> Result<(Operation, WireResponse), serde_json::Error> {
    Ok((
        operation,
        WireResponse {
            status: 200,
            body: serde_json::to_vec(&body)?,
        },
    ))
}
#[test]
fn engine_key_and_oauth_results_refresh_connections_and_never_echo_a_refused_secret(
) -> Result<(), Box<dyn std::error::Error>> {
    let location = serde_json::json!({"directory":"/work/my project","project":{"id":"prj_000000000001","directory":"/work/my project"}});
    let catalog = serde_json::json!({"data":[],"location":location});
    let mut api = Api {
        directory: Some("/work/my project".into()),
        attempt: None,
        browser: None,
        peer: Peer(VecDeque::from([
            reply(Operation::IntegrationKeyConnect, serde_json::Value::Null)?,
            reply(Operation::IntegrationList, catalog.clone())?,
            reply(
                Operation::IntegrationOAuthConnect,
                serde_json::json!({"data":{"attemptID":"attempt-one","url":"https://example.test/auth","instructions":"Sign in","mode":"auto","time":{"created":1,"expires":100}},"location":location}),
            )?,
            reply(
                Operation::IntegrationOAuthStatus,
                serde_json::json!({"data":{"status":"pending","time":{"created":1,"expires":100}},"location":location}),
            )?,
            reply(
                Operation::IntegrationOAuthStatus,
                serde_json::json!({"data":{"status":"complete","time":{"created":1,"expires":100}},"location":location}),
            )?,
            reply(Operation::IntegrationList, catalog)?,
            (
                Operation::IntegrationKeyConnect,
                WireResponse {
                    status: 401,
                    body: b"private-fixture-key".to_vec(),
                },
            ),
        ])),
    };
    let id = IntegrationIdDto::try_new("openai")?;
    let body = || {
        serde_json::from_value::<IntegrationKeyConnectRequestDto>(
            serde_json::json!({"key":"private-fixture-key"}),
        )
    };
    assert!(matches!(
        api.execute(Request::Key(id.clone(), Box::new(body()?)))?,
        Some(Reply::Catalog(_, true))
    ));
    let oauth =
        serde_json::from_value(serde_json::json!({"methodID":"chatgpt-device","inputs":{}}))?;
    assert!(matches!(
        api.execute(Request::OAuth(id.clone(), oauth))?,
        Some(Reply::Attempt(_))
    ));
    assert!(api.poll()?.is_none());
    assert!(matches!(api.poll()?, Some(Reply::Catalog(_, true))));
    assert!(api.attempt.is_none());
    let error = match api.execute(Request::Key(id, Box::new(body()?))) {
        Err(e) => e,
        _ => return Err("refused key was accepted".into()),
    };
    assert!(error.contains("401") && !error.contains("private-fixture-key"));
    Ok(())
}

#[test]
fn aws_chain_refusals_keep_safe_causes_without_refreshing_or_echoing_backend_text(
) -> Result<(), Box<dyn std::error::Error>> {
    for (kind, expected) in [
        (
            grapt_api::u3a::INTEGRATION_AWS_CREDENTIALS_KIND,
            grapt_api::u3a::INTEGRATION_AWS_CREDENTIALS_MESSAGE,
        ),
        (
            grapt_api::u3a::INTEGRATION_AWS_PROFILE_KIND,
            grapt_api::u3a::INTEGRATION_AWS_PROFILE_MESSAGE,
        ),
    ] {
        let mut api = Api {
            directory: Some("/work/my project".into()),
            attempt: None,
            browser: None,
            peer: Peer(VecDeque::from([(
                Operation::IntegrationKeyConnect,
                WireResponse {
                    status: 400,
                    body: serde_json::to_vec(&serde_json::json!({
                        "_tag":"InvalidRequestError", "kind":kind, "message":"private-backend-body"
                    }))?,
                },
            )])),
        };
        let body = serde_json::from_value(serde_json::json!({
            "key":"", "bedrock":{"region":"us-east-1", "model":"fixture-model", "auth":{"type":"aws-chain"}}
        }))?;
        let result = api.execute(Request::Key(
            IntegrationIdDto::try_new("amazon-bedrock")?,
            Box::new(body),
        ));
        let error = match result {
            Err(error) => error,
            _ => return Err("AWS refusal accepted".into()),
        };
        assert_eq!(error, expected);
        assert!(!error.contains("private-backend-body"));
        assert!(api.peer.0.is_empty());
    }
    Ok(())
}
