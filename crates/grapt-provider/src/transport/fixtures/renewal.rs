use super::*;
use crate::{CredentialOwner, CredentialRenewalFuture, CredentialRevision, CurrentCredential};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Current,
    WrongAccount,
    WrongVersion,
    WrongPlacement,
    DropBinding,
    Bound,
    Stalled,
}

struct Owner {
    placement: CredentialPlacement,
    mode: Mode,
    recoverable: bool,
    destination: Option<String>,
    recovered: AtomicUsize,
    accepted: AtomicUsize,
}

impl Owner {
    fn new(placement: CredentialPlacement, mode: Mode, recoverable: bool) -> Arc<Self> {
        Arc::new(Self {
            placement,
            mode,
            recoverable,
            destination: None,
            recovered: AtomicUsize::new(0),
            accepted: AtomicUsize::new(0),
        })
    }
}

impl CredentialOwner for Owner {
    fn current(&self) -> CredentialRenewalFuture<'_, CurrentCredential> {
        Box::pin(async move {
            if self.mode == Mode::Stalled {
                return std::future::pending().await;
            }
            let account = grapt_types::AccountId::new(if self.mode == Mode::WrongAccount {
                "acct_other"
            } else {
                "acct_fixture"
            })
            .ok_or(crate::CredentialRenewalRefusal::Unavailable)?;
            let version = grapt_types::CredentialVersion::new(if self.mode == Mode::WrongVersion {
                "8"
            } else {
                "7"
            })
            .ok_or(crate::CredentialRenewalRefusal::Unavailable)?;
            let placement = if self.mode == Mode::WrongPlacement {
                CredentialPlacement::AnthropicApiKey
            } else {
                self.placement
            };
            let credential = crate::ProviderCredential::inject(
                account,
                version,
                placement,
                grapt_types::Secret::new("rotated-token".to_owned()),
            )
            .map_err(|_| crate::CredentialRenewalRefusal::Unavailable)?;
            let credential = if self.mode != Mode::DropBinding {
                match &self.destination {
                    Some(origin) => credential
                        .bind_destination(origin)
                        .map_err(|_| crate::CredentialRenewalRefusal::Unavailable)?,
                    None => credential,
                }
            } else {
                credential
            };
            Ok(CurrentCredential {
                credential,
                revision: CredentialRevision::new(5),
            })
        })
    }
    fn recover(&self, _: CredentialRevision) -> CredentialRenewalFuture<'_, bool> {
        Box::pin(async move {
            self.recovered.fetch_add(1, Ordering::SeqCst);
            Ok(self.recoverable)
        })
    }
    fn accepted(&self, _: CredentialRevision) -> CredentialRenewalFuture<'_, ()> {
        Box::pin(async move {
            self.accepted.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

#[tokio::test]
async fn renewal_replaces_the_staged_header_on_each_credential_placement() {
    for (family, placement, header) in [
        (
            ProtocolFamily::OpenAiCompatible,
            CredentialPlacement::BearerAuthorization,
            "authorization:",
        ),
        (
            ProtocolFamily::AnthropicMessages,
            CredentialPlacement::AnthropicApiKey,
            "x-api-key:",
        ),
    ] {
        let fixture = serve(401, &[], Vec::new()).await;
        let owner = Owner::new(placement, Mode::Current, false);
        let request = if family == ProtocolFamily::AnthropicMessages {
            anthropic_request()
        } else {
            compat_request()
        };
        let open = adapter(1)
            .with_credential_owner(owner.clone())
            .open(input("att_renewal_header", &fixture.base, family, request))
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        let _fragments = drain(open).await;
        let head = fixture
            .head
            .lock()
            .unwrap_or_else(|_| panic!("fixture head lock"))
            .clone();
        assert_eq!(
            head.matches(header).count(),
            1,
            "renewal appended an authentication header"
        );
        assert!(head.contains("rotated-token"));
        assert!(!head.contains("token-value-never-printed"));
        assert_eq!(fixture.requests.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn only_a_recovered_401_has_a_retry_hint_and_the_transport_never_resends() {
    for (status, recoverable, hint, recoveries) in [
        (401, true, true, 1),
        (401, false, false, 1),
        (403, true, false, 0),
    ] {
        let fixture = serve(status, &[], Vec::new()).await;
        let owner = Owner::new(
            CredentialPlacement::BearerAuthorization,
            Mode::Current,
            recoverable,
        );
        let adapter = adapter(1).with_credential_owner(owner.clone());
        let open = adapter
            .open(input(
                "att_renewal_401",
                &fixture.base,
                ProtocolFamily::OpenAiCompatible,
                compat_request(),
            ))
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        let fragments = drain(open).await;
        let classification = fragments
            .iter()
            .find_map(|fragment| match fragment {
                AttemptFragment::ProviderFailure { classification, .. } => Some(*classification),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing refusal"));
        assert_eq!(
            classification,
            hint.then_some(crate::FailureClassification::CredentialRefreshed)
        );
        assert_eq!(owner.recovered.load(Ordering::SeqCst), recoveries);
        assert_eq!(
            fixture.requests.load(Ordering::SeqCst),
            1,
            "one attempt sent more than once"
        );
        assert_eq!(adapter.active_len(), 0);
    }
}

#[tokio::test]
async fn renewal_preserves_bound_origins_and_refuses_coordinate_changes(
) -> Result<(), Box<dyn std::error::Error>> {
    for mode in [
        Mode::WrongAccount,
        Mode::WrongVersion,
        Mode::WrongPlacement,
        Mode::Bound,
        Mode::DropBinding,
    ] {
        let fixture = serve(
            200,
            &[],
            vec![sse(&[
                r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            ])],
        )
        .await;
        let mut owner = Owner::new(CredentialPlacement::BearerAuthorization, mode, false);
        let mut request = input(
            "att_renewal_binding",
            &fixture.base,
            ProtocolFamily::OpenAiCompatible,
            compat_request(),
        );
        if matches!(mode, Mode::Bound | Mode::DropBinding) {
            Arc::get_mut(&mut owner)
                .ok_or("fixture owner shared too early")?
                .destination = Some(fixture.base.clone());
            request.lease = crate::ProviderCredential::inject(
                request.lease.account().clone(),
                request.lease.credential_version().clone(),
                request.lease.placement(),
                grapt_types::Secret::new("original-bound-token".into()),
            )?
            .bind_destination(&fixture.base)?
            .lease()?;
        }
        let adapter = adapter(1).with_credential_owner(owner);
        let fragments = drain(adapter.open(request).await?).await;
        assert_eq!(
            failure_of(&fragments),
            (mode != Mode::Bound).then_some(ProviderFailureClass::Authentication),
            "renewal changed or dropped credential authority"
        );
        assert_eq!(
            fixture.requests.load(Ordering::SeqCst),
            usize::from(mode == Mode::Bound)
        );
        assert_eq!(adapter.active_len(), 0);
    }
    Ok(())
}

#[tokio::test]
async fn a_stalled_credential_owner_is_bounded_and_releases_attempt_capacity() {
    let fixture = serve(200, &[], Vec::new()).await;
    let owner = Owner::new(
        CredentialPlacement::BearerAuthorization,
        Mode::Stalled,
        false,
    );
    let short = CodexDeadlines::new(Duration::from_millis(20), Duration::from_millis(20))
        .unwrap_or_else(|error| panic!("deadlines: {error}"));
    let adapter = ProviderAdapter::new(crate::transport::client::build_loopback_client, short, 1)
        .with_credential_owner(owner);
    let open = tokio::time::timeout(
        Duration::from_secs(1),
        adapter.open(input(
            "att_renewal_stall",
            &fixture.base,
            ProtocolFamily::OpenAiCompatible,
            compat_request(),
        )),
    )
    .await
    .unwrap_or_else(|_| panic!("credential renewal outlived its bound"))
    .unwrap_or_else(|error| panic!("open: {error}"));
    assert_eq!(
        failure_of(&drain(open).await),
        Some(ProviderFailureClass::Authentication)
    );
    assert_eq!(fixture.requests.load(Ordering::SeqCst), 0);
    assert_eq!(adapter.active_len(), 0);
}
