use super::*;
use crate::{
    AuthenticationPolicyVersion, MaxPrincipalMappings, SeatDirectoryEntry, SeatPrincipalDirectory,
    SeatTrustBoundary, SubjectId,
};
use grapt_types::{
    authority::DeterministicAuthorities, ClockAuthority, IdAuthority, IdToken, SeatRecipientId,
    Timestamp,
};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug)]
struct Clock;
impl ClockAuthority for Clock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1)
    }
}
#[derive(Debug)]
struct Ids;
impl IdAuthority for Ids {
    fn mint(&self) -> IdToken {
        IdToken::new("integration-api-fixture")
            .unwrap_or_else(|| panic!("invalid fixture identity"))
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ConnectionRecord {
    operation: &'static str,
    connection: Option<String>,
    key: Option<String>,
}
#[derive(Default)]
struct Owner(AtomicUsize, std::sync::Mutex<Vec<ConnectionRecord>>);
impl Owner {
    fn record(
        &self,
        operation: &'static str,
        connection: Option<super::super::DtoString>,
        key: Option<&str>,
    ) {
        self.1
            .lock()
            .unwrap_or_else(|_| panic!("connection records poisoned"))
            .push(ConnectionRecord {
                operation,
                connection: connection.map(|value| value.as_str().to_owned()),
                key: key.map(str::to_owned),
            });
    }
}
impl IntegrationApplicationPort for Owner {
    fn authentication_status(
        &self,
        _: SealedPrincipal,
        _: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<super::super::AuthenticationStatusResponseDto, InvalidRequestErrorDto>
    {
        panic!("key route dispatched authentication status")
    }

    fn connect_environment(
        &self,
        _: SealedPrincipal,
        _: IntegrationIdDto,
        _: ProjectCurrentLocationQueryDto,
        _: super::super::IntegrationEnvironmentConnectRequestDto,
        connection: Option<super::super::DtoString>,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        self.record("environment", connection, None);
        Box::pin(async { Ok(NoContent) })
    }
    fn oauth_challenge(
        &self,
        _: SealedPrincipal,
        _: IntegrationIdDto,
        _: ProjectCurrentLocationQueryDto,
        _: IntegrationAttemptIdDto,
    ) -> ApplicationFuture<super::super::IntegrationOAuthChallengeResponseDto, InvalidRequestErrorDto>
    {
        panic!("key route dispatched OAuth challenge")
    }
    fn connect_command(
        &self,
        _: SealedPrincipal,
        _: IntegrationIdDto,
        _: ProjectCurrentLocationQueryDto,
        _: super::super::IntegrationCommandConnectRequestDto,
    ) -> ApplicationFuture<super::super::IntegrationCommandConnectResponseDto, InvalidRequestErrorDto>
    {
        panic!("key route dispatched command connect")
    }
    fn command_status(
        &self,
        _: SealedPrincipal,
        _: IntegrationIdDto,
        _: ProjectCurrentLocationQueryDto,
        _: IntegrationAttemptIdDto,
    ) -> ApplicationFuture<super::super::IntegrationCommandStatusResponseDto, InvalidRequestErrorDto>
    {
        panic!("key route dispatched command status")
    }
    fn cancel_command(
        &self,
        _: SealedPrincipal,
        _: IntegrationIdDto,
        _: ProjectCurrentLocationQueryDto,
        _: IntegrationAttemptIdDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        panic!("key route dispatched command cancel")
    }
    fn remove_credential(
        &self,
        _: SealedPrincipal,
        _: super::super::CredentialIdDto,
        _: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        panic!("key route dispatched credential removal")
    }

    fn update_credential(
        &self,
        _: SealedPrincipal,
        _: super::super::CredentialIdDto,
        _: ProjectCurrentLocationQueryDto,
        _: super::super::CredentialUpdateRequestDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        panic!("key route dispatched credential metadata")
    }

    fn connect_oauth(
        &self,
        _principal: SealedPrincipal,
        _id: IntegrationIdDto,
        _query: ProjectCurrentLocationQueryDto,
        _request: IntegrationOAuthConnectRequestDto,
        connection: Option<super::super::DtoString>,
    ) -> ApplicationFuture<IntegrationOAuthConnectResponseDto, InvalidRequestErrorDto> {
        self.record("oauth", connection, None);
        Box::pin(async {
            Ok(serde_json::from_value(serde_json::json!({
            "data":{"attemptID":"fixture-attempt","url":"https://auth.example.invalid/",
                "instructions":"Use the fixture issuer", "mode":"auto", "time":{"created":1,"expires":2}},
            "location":{"directory":"/fixture", "project":{"id":"prj_fixture","directory":"/fixture"}}
        })).unwrap_or_else(|error| panic!("OAuth response fixture: {error}")))
        })
    }
    fn oauth_status(
        &self,
        _principal: SealedPrincipal,
        _id: IntegrationIdDto,
        _query: ProjectCurrentLocationQueryDto,
        _attempt: IntegrationAttemptIdDto,
    ) -> ApplicationFuture<IntegrationOAuthStatusResponseDto, InvalidRequestErrorDto> {
        panic!("key route dispatched OAuth")
    }
    fn complete_oauth(
        &self,
        _principal: SealedPrincipal,
        _id: IntegrationIdDto,
        _query: ProjectCurrentLocationQueryDto,
        _attempt: IntegrationAttemptIdDto,
        _request: IntegrationOAuthCompleteRequestDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        panic!("key route dispatched OAuth")
    }
    fn cancel_oauth(
        &self,
        _principal: SealedPrincipal,
        _id: IntegrationIdDto,
        _query: ProjectCurrentLocationQueryDto,
        _attempt: IntegrationAttemptIdDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        panic!("key route dispatched OAuth")
    }
    fn location(
        &self,
        _: SealedPrincipal,
        _: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<super::super::LocationInfoDto, InvalidRequestErrorDto> {
        panic!("key route dispatched location read")
    }
    fn list(
        &self,
        _: SealedPrincipal,
        _: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<IntegrationListResponseDto, InvalidRequestErrorDto> {
        panic!("key route dispatched integration list")
    }
    fn get(
        &self,
        _: SealedPrincipal,
        _: IntegrationIdDto,
        _: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<IntegrationGetResponseDto, InvalidRequestErrorDto> {
        panic!("key route dispatched integration get")
    }
    fn connect_key(
        &self,
        _: SealedPrincipal,
        _: IntegrationIdDto,
        _: ProjectCurrentLocationQueryDto,
        request: IntegrationKeyConnectRequestDto,
        connection: Option<super::super::DtoString>,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        self.record("key", connection, Some(request.key.as_str()));
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(NoContent) })
    }
}

#[tokio::test]
async fn key_connect_requires_verified_principal_and_transport_loopback_despite_forwarded_headers(
) -> Result<(), Box<dyn std::error::Error>> {
    let subject = SubjectId::try_new("integration-user")?;
    let directory = SeatPrincipalDirectory::configure(
        vec![SeatDirectoryEntry::new(
            subject.clone(),
            SeatRecipientId::try_new("integration-seat")?,
        )],
        MaxPrincipalMappings::try_new(1)?,
        &DeterministicAuthorities::new(&Clock, &Ids),
    )?;
    let boundary = SeatTrustBoundary::new(
        AuthenticationPolicyVersion::try_new("fixture-policy")?,
        directory,
    );
    let principal = SealedPrincipal::seal(boundary.authenticate_fixture(subject)?);
    let owner = Arc::new(Owner::default());
    for (authenticated, address, status) in [
        (false, Some("127.0.0.1:10"), StatusCode::UNAUTHORIZED),
        (true, None, StatusCode::FORBIDDEN),
        (true, Some("192.0.2.1:10"), StatusCode::FORBIDDEN),
        (true, Some("127.0.0.1:10"), StatusCode::NO_CONTENT),
    ] {
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/integration/openai/connect/key")
            .header("x-forwarded-for", "127.0.0.1")
            .body(Body::from(r#"{"key":"fixture-key"}"#))?;
        if authenticated {
            request.extensions_mut().insert(principal.clone());
        }
        if let Some(address) = address {
            request
                .extensions_mut()
                .insert(crate::admission::TransportPeer::fixture(address.parse()?));
        }
        assert_eq!(
            handle(owner.clone(), Operation::Key, request)
                .await
                .status(),
            status
        );
    }
    for operation in [
        Operation::Environment,
        Operation::OAuthChallenge,
        Operation::Command,
        Operation::CommandStatus,
        Operation::CommandCancel,
    ] {
        for (authenticated, address, expected) in [
            (false, "127.0.0.1:10", StatusCode::UNAUTHORIZED),
            (true, "192.0.2.1:10", StatusCode::FORBIDDEN),
        ] {
            let mut request = Request::builder()
                .uri("/api/integration/openai/connect/command/missing")
                .header("x-forwarded-for", "127.0.0.1")
                .body(Body::from("invalid JSON"))?;
            if authenticated {
                request.extensions_mut().insert(principal.clone());
            }
            request
                .extensions_mut()
                .insert(crate::admission::TransportPeer::fixture(address.parse()?));
            assert_eq!(
                handle(owner.clone(), operation, request).await.status(),
                expected
            );
        }
    }
    for names in [
        vec!["../foreign".to_owned()],
        vec!["x".repeat(65)],
        vec!["work".into(), "other".into()],
    ] {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/api/integration/openai/connect/key");
        for name in names {
            builder = builder.header("x-grapt-connection-id", name);
        }
        let mut request = builder.body(Body::from(r#"{"key":"fixture-key"}"#))?;
        request.extensions_mut().insert(principal.clone());
        request
            .extensions_mut()
            .insert(crate::admission::TransportPeer::fixture(
                "127.0.0.1:10".parse()?,
            ));
        assert_eq!(
            handle(owner.clone(), Operation::Key, request)
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(owner.0.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn renamed_connection_headers_preserve_targets_and_refuse_ambiguity(
) -> Result<(), Box<dyn std::error::Error>> {
    let subject = SubjectId::try_new("connection-user")?;
    let directory = SeatPrincipalDirectory::configure(
        vec![SeatDirectoryEntry::new(
            subject.clone(),
            SeatRecipientId::try_new("connection-client")?,
        )],
        MaxPrincipalMappings::try_new(1)?,
        &DeterministicAuthorities::new(&Clock, &Ids),
    )?;
    let boundary = SeatTrustBoundary::new(
        AuthenticationPolicyVersion::try_new("fixture-policy")?,
        directory,
    );
    let principal = SealedPrincipal::seal(boundary.authenticate_fixture(subject)?);
    let owner = Arc::new(Owner::default());
    for (operation, label, body) in [
        (
            Operation::Environment,
            "environment",
            r#"{"name":"SYNTHETIC_PROVIDER_KEY"}"#,
        ),
        (Operation::Key, "key", r#"{"key":"SYNTHETIC_TEST_KEY"}"#),
        (
            Operation::OAuth,
            "oauth",
            r#"{"methodID":"fixture-device","inputs":{}}"#,
        ),
    ] {
        for (headers, refusal) in connection_header_cases() {
            let named = !headers.is_empty();
            let mut builder = Request::builder()
                .method("POST")
                .uri(format!("/api/integration/openai/connect/{label}"));
            for (name, value) in &headers {
                builder = builder.header(*name, *value);
            }
            let mut request = builder.body(Body::from(body))?;
            request.extensions_mut().insert(principal.clone());
            request
                .extensions_mut()
                .insert(crate::admission::TransportPeer::fixture(
                    "127.0.0.1:10".parse()?,
                ));
            let response = handle(owner.clone(), operation, request).await;
            let records =
                std::mem::take(&mut *owner.1.lock().map_err(|_| "connection records poisoned")?);
            if let Some(reason) = refusal {
                assert_eq!(
                    response.status(),
                    StatusCode::BAD_REQUEST,
                    "{label}: {headers:?}"
                );
                let bytes = axum::body::to_bytes(response.into_body(), 4096).await?;
                let value: serde_json::Value = serde_json::from_slice(&bytes)?;
                assert_eq!(value["message"], reason, "{label}: {headers:?}");
                assert!(records.is_empty(), "refused request reached the owner");
            } else {
                let status = if label == "oauth" {
                    StatusCode::OK
                } else {
                    StatusCode::NO_CONTENT
                };
                assert_eq!(response.status(), status, "{label}: {headers:?}");
                assert_eq!(
                    records,
                    vec![ConnectionRecord {
                        operation: label,
                        connection: named.then(|| "work".into()),
                        key: (label == "key").then(|| "SYNTHETIC_TEST_KEY".into()),
                    }]
                );
            }
        }
    }
    Ok(())
}

type ConnectionHeaderCase = (Vec<(&'static str, &'static str)>, Option<&'static str>);
fn connection_header_cases() -> [ConnectionHeaderCase; 10] {
    // Literal old-client spellings keep this independent of the parser's constants.
    let current = "x-grapt-connection-id";
    let legacy = "x-secode-connection-id"; // grapt-rename: keep
    [
        (vec![], None),
        (vec![(legacy, "work")], None),
        (vec![(current, "work")], None),
        (vec![(current, "work"), (legacy, "work")], None),
        (
            vec![(current, "work"), (legacy, "other")],
            Some("conflicting connection identity"),
        ),
        (
            vec![(current, "work"), (current, "other")],
            Some("duplicate connection identity"),
        ),
        (
            vec![(legacy, "work"), (legacy, "other")],
            Some("duplicate connection identity"),
        ),
        (
            vec![(current, "work"), (legacy, "work"), (legacy, "other")],
            Some("duplicate connection identity"),
        ),
        (
            vec![(current, "../foreign")],
            Some("invalid connection identity"),
        ),
        (
            vec![(legacy, "../foreign")],
            Some("invalid connection identity"),
        ),
    ]
}
