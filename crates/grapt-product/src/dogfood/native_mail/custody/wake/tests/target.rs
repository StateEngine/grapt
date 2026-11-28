//! Authenticated rebinding selects one target; retries cannot win it back.
use super::*;

const IDENTITY: &str = r#"{"scope":"project","project_id":"secode","agent_id":"coder","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#;

async fn another(
    application: &Arc<ServerApplication>,
    root: &Root,
    identity: &str,
) -> Result<SessionId, Box<dyn std::error::Error>> {
    let authorities = DeterministicAuthorities::new(&*application.clock, &*application.ids);
    let seed = grapt_session::SessionSeed::new(
        grapt_types::ProjectId::new("prj_native_wake").ok_or("project")?,
        root.0.to_str().ok_or("directory")?,
        "replacement seat",
    )?
    .with_agent("default")?;
    let session = grapt_runtime::SessionPorts::new(application.inbox.clone(), &authorities)
        .create(&seed)?
        .session;
    InstructionService::put_entry(
        application.lock_inbox().store_mut(),
        &session,
        &InstructionKey::new("stateengine-identity").ok_or("key")?,
        identity,
        1,
    )?;
    bind_capability(application, &session).await?;
    Ok(session)
}

#[tokio::test]
async fn same_seat_takeover_retires_old_owner_and_retry_does_not_reclaim() -> TestResult {
    let (root, application, _driver, old) = fixture(Some(IDENTITY)).await?;
    let native = application.native_mail.as_ref().ok_or("native owner")?;
    let replacement = another(&application, &root, IDENTITY).await?;
    let owners = native.snapshot()?;
    let old_owner = owners
        .iter()
        .find(|(s, _)| s == &old)
        .ok_or("old owner")?
        .1
        .clone();
    let new_owner = owners
        .iter()
        .find(|(s, _)| s == &replacement)
        .ok_or("new owner")?
        .1
        .clone();
    assert!(old_owner.superseded.load(Ordering::SeqCst));
    assert!(!new_owner.superseded.load(Ordering::SeqCst));
    assert!(!application.lock_inbox().native_mail_wakes_available(&old)?);
    assert!(application
        .lock_inbox()
        .native_mail_wakes_available(&replacement)?);
    // The prompt/reconnect path cannot reclaim the retired delivery authority.
    let mut old_plan = native.plan(&old)?;
    old_plan
        .refresh(StepNumber::new(1).ok_or("step")?)
        .await
        .map_err(|e| e.to_string())?;
    assert!(old_plan.take_batch().map_err(|e| e.to_string())?.is_none());
    old_plan.finish(false).await?;
    assert!(old_owner.superseded.load(Ordering::SeqCst));
    assert!(!new_owner.superseded.load(Ordering::SeqCst));
    // A new authenticated subscription is an explicit transfer in either direction.
    bind_capability(&application, &old).await?;
    {
        let inbox = application.lock_inbox();
        assert!(inbox.native_notice_target(&old)?.is_some());
        assert!(inbox.native_notice_target(&replacement)?.is_none());
    }
    assert!(new_owner.superseded.load(Ordering::SeqCst));
    Ok(())
}

#[tokio::test]
async fn distinct_project_scope_keeps_its_own_target() -> TestResult {
    let (root, application, _driver, first) = fixture(Some(IDENTITY)).await?;
    let identity = IDENTITY.replace(
        "\"project_id\":\"secode\"",
        "\"project_id\":\"another-project\"",
    );
    let second = another(&application, &root, &identity).await?;
    let native = application.native_mail.as_ref().ok_or("native owner")?;
    for (_, owner) in native.snapshot()? {
        assert!(!owner.superseded.load(Ordering::SeqCst));
    }
    assert!(application
        .lock_inbox()
        .native_mail_wakes_available(&first)?);
    assert!(application
        .lock_inbox()
        .native_mail_wakes_available(&second)?);
    Ok(())
}

#[tokio::test]
async fn interrupt_before_mail_arrives_keeps_the_actual_idle_driver_out_of_provider() -> TestResult
{
    let (_root, application, mut driver, session) = fixture(Some(IDENTITY)).await?;
    let native = application.native_mail.as_ref().ok_or("native owner")?;
    let owner = native.snapshot()?.first().ok_or("owner")?.1.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    owner
        .context
        .lock()
        .await
        .use_test_port(NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?);
    let (entered, poll) = tokio::sync::oneshot::channel();
    let stop = async {
        let response = ingress::call(
            &application,
            "POST",
            &format!("/api/session/{session}/interrupt"),
            "{}",
        )
        .await;
        assert!(
            matches!(&response, Ok(response) if response.starts_with(b"HTTP/1.1 204")),
            "authenticated interrupt failed: {response:?}"
        );
        assert!(poll.await.is_ok(), "native poll not reached");
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
    };
    let (transport, run) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(
            fake::serve(listener, Some(entered), 2),
            driver.run_until(stop)
        )
    })
    .await?;
    transport?;
    run?;
    assert!(application
        .lock_inbox()
        .pending_inputs(&session)?
        .is_empty());
    assert!(!owner.wake_scheduled.load(Ordering::SeqCst));
    assert!(crate::dogfood::test_provider::server(&application)?
        .request()?
        .is_none());
    assert!(!application
        .lock_inbox()
        .native_mail_wakes_available(&session)?);
    Ok(())
}

#[tokio::test]
async fn old_cleanup_failure_cannot_lose_the_new_target_bookkeeping() -> TestResult {
    let (root, application, _driver, old) = fixture(Some(IDENTITY)).await?;
    let native = application.native_mail.as_ref().ok_or("native owner")?;
    let old_owner = native
        .snapshot()?
        .into_iter()
        .find(|(session, _)| session == &old)
        .ok_or("old owner")?
        .1;
    let poisoned_owner = old_owner.clone();
    let injected: std::thread::Result<()> =
        std::thread::spawn(move || match poisoned_owner.wait.lock() {
            Ok(_held) => panic!("injected old wait cleanup failure"),
            Err(_) => panic!("fixture wait was already poisoned"),
        })
        .join();
    assert!(injected.is_err(), "cleanup failure was not injected");
    let middle = another(&application, &root, IDENTITY).await?;
    let newest = another(&application, &root, IDENTITY).await?;
    let owners = native.snapshot()?;
    let middle_owner = owners
        .iter()
        .find(|(s, _)| s == &middle)
        .ok_or("middle owner")?
        .1
        .clone();
    let newest_owner = owners
        .iter()
        .find(|(s, _)| s == &newest)
        .ok_or("newest owner")?
        .1
        .clone();
    assert!(old_owner.superseded.load(Ordering::SeqCst));
    assert!(
        middle_owner.superseded.load(Ordering::SeqCst),
        "cleanup error left an untracked live target"
    );
    assert!(!newest_owner.superseded.load(Ordering::SeqCst));
    assert!(!application
        .lock_inbox()
        .native_mail_wakes_available(&middle)?);
    assert!(application
        .lock_inbox()
        .native_mail_wakes_available(&newest)?);
    assert!(
        native.close().await.is_err(),
        "retirement lost the earlier failed wait cleanup"
    );
    Ok(())
}
