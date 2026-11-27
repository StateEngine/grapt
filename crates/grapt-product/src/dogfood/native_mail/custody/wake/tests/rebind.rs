//! Refused authentication parks the existing owner until authenticated ingress.
use super::*;
use std::sync::atomic::AtomicUsize;

#[tokio::test]
async fn refused_subscription_needs_rebind_before_any_further_subscribe() -> TestResult {
    unauthenticated("subscribe_and_snapshot", false).await
}

#[tokio::test]
async fn revoked_idle_subscription_needs_rebind_without_a_takeover_retry() -> TestResult {
    unauthenticated("next_change", false).await
}

#[tokio::test]
async fn authentication_refused_during_preparation_stays_parked_between_prompts() -> TestResult {
    unauthenticated("subscribe_and_snapshot", true).await
}

#[tokio::test]
async fn idle_project_mismatch_refuses_later_prompt_without_retry() -> TestResult {
    let (_root, application, _driver, session) = fixture(Some(
        r#"{"scope":"project","project_id":"secode","agent_id":"coder","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#,
    )).await?;
    let native = application.native_mail.as_ref().ok_or("native owner")?;
    let owner = native.snapshot()?.first().ok_or("owner")?.1.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    owner
        .context
        .lock()
        .await
        .use_test_port(NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?);
    let attempts = Arc::new(AtomicUsize::new(0));
    let (served, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            fake::serve_unauthenticated(
                listener,
                fake::Refusals {
                    operation: "next_change",
                    cause: "ForeignProjectMismatch",
                    remaining: 1,
                    skip: 0,
                    attempts: attempts.clone(),
                }
            ),
            async {
                while attempts.load(Ordering::SeqCst) == 0 {
                    native.drive_one(session.clone(), owner.clone()).await;
                }
                native.close_pending().await;
            }
        )
    })
    .await?;
    let calls = served?;
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(calls.iter().filter(|call| *call == "open").count(), 1);
    for _ in 0..2 {
        let mut plan = native.plan(&session)?;
        let prepared = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            plan.refresh(StepNumber::new(1).ok_or("step")?),
        )
        .await?;
        assert_eq!(prepared, Err(E::Principal));
        plan.finish(false).await?;
    }
    assert_eq!(
        owner.context.lock().await.attention,
        MailAttentionInfo::RebindRequired
    );
    assert!(!owner.recovery.lock().map_err(|_| "recovery")?.recovering());
    Ok(())
}

async fn unauthenticated(operation: &'static str, preparation: bool) -> TestResult {
    let (_root, application, mut driver, session) = fixture(Some(r#"{"scope":"project","project_id":"secode","agent_id":"coder","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#)).await?;
    let native = application.native_mail.as_ref().ok_or("native owner")?;
    let owner = native.snapshot()?.first().ok_or("owner")?.1.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    owner
        .context
        .lock()
        .await
        .use_test_port(NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?);
    let attempts = Arc::new(AtomicUsize::new(0));
    let (transport, prepared) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            fake::serve_unauthenticated(
                listener,
                fake::Refusals {
                    operation,
                    cause: "Unauthenticated",
                    remaining: 1,
                    skip: 0,
                    attempts: attempts.clone(),
                }
            ),
            async {
                if preparation {
                    native.drive_one(session.clone(), owner.clone()).await;
                    let mut plan = native.plan(&session)?;
                    plan.refresh(StepNumber::new(1).ok_or("step")?)
                        .await
                        .map_err(|e| e.to_string())?;
                    plan.finish(false).await?;
                } else {
                    while attempts.load(Ordering::SeqCst) == 0 {
                        native.drive_one(session.clone(), owner.clone()).await;
                    }
                }
                native.close_pending().await;
                Ok::<(), Box<dyn std::error::Error>>(())
            }
        )
    })
    .await?;
    prepared?;
    let calls = transport?;
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "authentication refusal was retried"
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| *call == "subscribe_and_snapshot")
            .count(),
        1
    );
    assert_eq!(calls.iter().filter(|call| *call == "open").count(), 1);
    assert_parked(native, &owner, &session).await?;
    resume(application.clone(), &mut driver, &owner, &session).await?;
    Ok(())
}

#[tokio::test]
async fn restored_session_without_capability_parks_before_open_then_rebinds() -> TestResult {
    const IDENTITY: &str = r#"{"scope":"project","project_id":"secode","agent_id":"coder","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#;
    // A restored identity starts with no process-private capability or owner.
    let (_root, application, mut driver, session) = unbound_fixture(Some(IDENTITY)).await?;
    let native = application.native_mail.as_ref().ok_or("native owner")?;
    unbound_preparation(native, &session, Err(E::Capability)).await?;
    let route = format!("/api/session/{session}/instructions/entries/stateengine-identity");
    for disabled in [None, Some(r#"{"value":{"scope":"incognito"}}"#), None] {
        let (method, body) = disabled.map_or(("DELETE", ""), |body| ("PUT", body));
        let response = ingress::call(&application, method, &route, body).await?;
        assert!(response.starts_with(b"HTTP/1.1 204"));
        unbound_preparation(native, &session, Ok(())).await?;
        let response = ingress::call(
            &application,
            "PUT",
            &route,
            &format!("{{\"value\":{IDENTITY}}}"),
        )
        .await?;
        assert!(response.starts_with(b"HTTP/1.1 204"));
        // Metadata changes revoke the durable target. Re-enabling identity
        // alone keeps preparation quiet until authenticated ingress selects it.
        unbound_preparation(native, &session, Ok(())).await?;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), native.drive())
                .await
                .is_err(),
            "metadata alone started a subscription"
        );
    }
    bind_capability(&application, &session).await?;
    let rebound = native.plan(&session)?;
    assert!(rebound.service_owned);
    let owner = rebound.owner.clone();
    drop(rebound);
    assert_eq!(native.snapshot()?.len(), 1);
    drive_rebound(application.clone(), &mut driver, &owner, &session).await?;
    Ok(())
}

async fn unbound_preparation(
    native: &NativeMailOwners,
    session: &SessionId,
    expected: Result<(), E>,
) -> TestResult {
    let mut plan = native.plan(session)?;
    assert!(!plan.service_owned);
    let prepared = plan.refresh(StepNumber::new(1).ok_or("step")?).await;
    assert_eq!(prepared, expected);
    match expected {
        Ok(()) => {
            // A revoked target has no retained attention projection. Its
            // consumer must receive no batch and cannot request a wake.
            assert!(matches!(plan.take_batch(), Ok(None)));
            assert!(!native
                .inbox
                .lock()
                .map_err(|_| "inbox")?
                .native_mail_wakes_available(session)?);
        }
        Err(E::Capability) => {
            assert_eq!(
                plan.lease.as_ref().ok_or("prepared context")?.attention,
                MailAttentionInfo::RebindRequired
            );
        }
        Err(_) => return Err("unexpected unbound refusal".into()),
    }
    let context = plan.lease.as_ref().ok_or("prepared context")?;
    assert!(context.runtime.is_none(), "opened without a capability");
    assert!(!context.subscription_open);
    assert!(context.batch.is_none());
    assert!(!native
        .custody
        .lock()
        .map_err(|_| "custody")?
        .values
        .contains_key(session));
    plan.shutdown().await?;
    assert!(
        native.snapshot()?.is_empty(),
        "unbound plan spent registry capacity"
    );
    assert!(native.hydrate().is_empty());
    Ok(())
}

async fn assert_parked(
    native: &NativeMailOwners,
    owner: &Arc<Owner>,
    session: &SessionId,
) -> TestResult {
    let revision = owner.revision()?;
    {
        let context = owner.context.lock().await;
        assert_eq!(context.attention, MailAttentionInfo::RebindRequired);
        assert!(context
            .blocked_revision
            .as_ref()
            .is_some_and(|r| Arc::ptr_eq(r, &revision)));
    }
    assert!(!owner.recovery.lock().map_err(|_| "recovery")?.recovering());
    for cancelled in [false, true, false] {
        let application = native
            .wake_application
            .get()
            .ok_or("application")?
            .upgrade()
            .ok_or("live application")?;
        let route = format!("/api/session/{session}/instructions/entries/stateengine-identity");
        let (method, body) = if cancelled {
            ("PUT", r#"{"value":{"scope":"incognito"}}"#)
        } else {
            ("DELETE", "")
        };
        let disabled = ingress::call(&application, method, &route, body).await?;
        assert!(
            disabled.starts_with(b"HTTP/1.1 204"),
            "identity disable refused"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), native.drive())
                .await
                .is_err()
        );
        assert_eq!(
            owner.context.lock().await.attention,
            MailAttentionInfo::Disabled
        );
        // Re-enabling the same identity must not resurrect its refused secret.
        let previous = owner.revision()?;
        let response = ingress::call(&native.wake_application.get().ok_or("application")?
            .upgrade().ok_or("live application")?, "PUT",
            &format!("/api/session/{session}/instructions/entries/stateengine-identity"),
            r#"{"value":{"scope":"project","project_id":"secode","agent_id":"coder","mail_transport_protocol":"remote-seat-mail-transport/v2"}}"#).await?;
        assert!(
            response.starts_with(b"HTTP/1.1 204"),
            "metadata refresh refused"
        );
        assert!(!Arc::ptr_eq(&previous, &owner.revision()?));
        // Updating identity metadata supplies no new capability and cannot
        // resume the refused subscription at the newer binding revision.
        let mut metadata_plan = native.plan(session)?;
        metadata_plan
            .refresh(StepNumber::new(1).ok_or("step")?)
            .await
            .map_err(|e| e.to_string())?;
        metadata_plan.finish(false).await?;
        // Driver wakeups and ordinary prompt preparation cannot resume subscription.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), native.drive())
                .await
                .is_err()
        );
        let mut plan = native.plan(session)?;
        assert!(Arc::ptr_eq(&plan.owner, owner));
        plan.refresh(StepNumber::new(1).ok_or("step")?)
            .await
            .map_err(|e| e.to_string())?;
        plan.finish(cancelled).await?;
        let frame = native
            .hydrate()
            .into_iter()
            .next()
            .ok_or("attention frame")?;
        assert_eq!(
            serde_json::to_value(frame)?["data"]["status"]["type"],
            "rebind_required"
        );
    }
    Ok(())
}

async fn resume(
    application: Arc<DogfoodApplication>,
    driver: &mut crate::dogfood::prompt_driver::PromptDriver,
    owner: &Arc<Owner>,
    session: &SessionId,
) -> TestResult {
    let native = application.native_mail.as_ref().ok_or("native owner")?;
    let old_revision = owner.revision()?;
    // The real authenticated capability route changes the generation even when
    // the supplied credential bytes happen to be identical to the old value.
    bind_capability(&application, session).await?;
    assert!(!Arc::ptr_eq(&old_revision, &owner.revision()?));
    assert!(Arc::ptr_eq(owner, &native.plan(session)?.owner));
    drive_rebound(application, driver, owner, session).await
}

async fn drive_rebound(
    application: Arc<DogfoodApplication>,
    driver: &mut crate::dogfood::prompt_driver::PromptDriver,
    owner: &Arc<Owner>,
    session: &SessionId,
) -> TestResult {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    owner
        .context
        .lock()
        .await
        .use_test_port(NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?);
    let stop = std::future::poll_fn(|_| {
        match application.lock_inbox().pending_native_mail_wake(session) {
            Ok(Some(_)) => Poll::Ready(()),
            Ok(None) => Poll::Pending,
            Err(error) => panic!("rebind wake evidence unavailable: {error}"),
        }
    });
    let (transport, run) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(fake::serve(listener, None, 2), driver.run_until(stop))
    })
    .await?;
    run?;
    let calls = transport?;
    assert_eq!(calls.iter().filter(|call| *call == "open").count(), 1);
    assert_eq!(
        calls
            .iter()
            .filter(|call| *call == "subscribe_and_snapshot")
            .count(),
        1
    );
    assert_eq!(application.lock_inbox().pending_inputs(session)?.len(), 1);
    Ok(())
}
