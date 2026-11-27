use super::*;
use grapt_types::{
    authority::DeterministicAuthorities, ClockAuthority, IdAuthority, IdToken, SeatRecipientId,
    Timestamp,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
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
        IdToken::new("plugin-fixture").unwrap_or_else(|| panic!("fixture identity invalid"))
    }
}
struct Installer(AtomicUsize);
impl ClaudePluginApplicationPort for Installer {
    fn install(
        &self,
        _: crate::SealedPrincipal,
        _: super::super::ProjectCurrentLocationQueryDto,
        request: ClaudePluginRequestDto,
    ) -> super::super::ApplicationFuture<
        ClaudePluginResponseDto,
        super::super::InvalidRequestErrorDto,
    > {
        assert!(matches!(request, ClaudePluginRequestDto::Install { .. }));
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Ok(ClaudePluginResponseDto {
                message: BoundedString::try_new("installed")
                    .unwrap_or_else(|_| panic!("fixture response invalid")),
            })
        })
    }
}
#[tokio::test]
async fn installer_requires_authenticated_transport_loopback_before_dispatch(
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::{
        AuthenticationPolicyVersion, MaxPrincipalMappings, SealedPrincipal, SeatDirectoryEntry,
        SeatPrincipalDirectory, SeatTrustBoundary, SubjectId,
    };
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    let subject = SubjectId::try_new("plugin-user")?;
    let directory = SeatPrincipalDirectory::configure(
        vec![SeatDirectoryEntry::new(
            subject.clone(),
            SeatRecipientId::try_new("plugin-seat")?,
        )],
        MaxPrincipalMappings::try_new(1)?,
        &DeterministicAuthorities::new(&Clock, &Ids),
    )?;
    let boundary = SeatTrustBoundary::new(
        AuthenticationPolicyVersion::try_new("fixture-policy")?,
        directory,
    );
    let principal = SealedPrincipal::seal(boundary.authenticate_fixture(subject)?);
    let owner = Arc::new(Installer(AtomicUsize::new(0)));
    for (authenticated, address, status) in [
        (false, Some("127.0.0.1:10"), StatusCode::UNAUTHORIZED),
        (true, None, StatusCode::FORBIDDEN),
        (true, Some("192.0.2.1:10"), StatusCode::FORBIDDEN),
        (true, Some("127.0.0.1:10"), StatusCode::OK),
    ] {
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/plugin/claude")
            .header("x-forwarded-for", "127.0.0.1")
            .body(Body::from(r#"{"action":"install","plugin":"review@team"}"#))?;
        if authenticated {
            request.extensions_mut().insert(principal.clone());
        }
        if let Some(address) = address {
            request
                .extensions_mut()
                .insert(crate::admission::TransportPeer::fixture(address.parse()?));
        }
        assert_eq!(handle(owner.clone(), request).await.status(), status);
    }
    assert_eq!(owner.0.load(Ordering::SeqCst), 1);
    Ok(())
}
