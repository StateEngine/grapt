//! Retained secondary repair uses its original receipt across a source rescan.
use super::*;

#[tokio::test]
async fn original_secondary_repair_survives_rescan_and_settles_through_http() -> TestResult {
    let (_root, application, _driver, session) = fixture(Some(r#"{"scope":"project","project_id":"secode","agent_id":"coder","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#)).await?;
    let native = application.native_mail.as_ref().ok_or("native owners")?;
    let owner = native.snapshot()?.first().ok_or("owner")?.1.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    owner
        .context
        .lock()
        .await
        .use_test_port(NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?);
    let exercise = async {
        crate::dogfood::native_mail::custody::tests::drive_to_ready(native, &session).await?;
        let (scope, original) = {
            let context = owner.context.lock().await;
            let scope = context
                .current_binding()
                .map_err(|error| error.to_string())?
                .ok_or("binding")?
                .scope;
            let mut inbox = application.lock_inbox();
            let page = inbox.store().read_remote_mailbox_for_binding(
                &scope,
                grapt_types::MailboxCursor::START,
                64,
                1_048_576,
            )?;
            let original = page
                .records
                .iter()
                .find(|row| row.ack().source_message().as_str() == "mail-b")
                .ok_or("original row")?
                .ack()
                .clone();
            // Model a previously measured durable secondary refusal through the
            // owning typed store API. The actual HTTP consumer performs repair.
            inbox.store_mut().settle_remote_mailbox_delivery(
                &scope,
                &original,
                Some(grapt_types::remote_mail::SecondaryProjectionState::Refused),
            )?;
            (scope, original)
        };
        owner.cancel_wait()?;
        {
            let mut context = owner.context.lock().await;
            context
                .runtime
                .as_mut()
                .ok_or("live runtime")?
                .request_snapshot();
            let inbox = application.lock_inbox();
            let debt = inbox
                .store()
                .remote_mailbox_message(&scope, original.local_message())?;
            assert!(
                !debt.delivery_pending() && debt.repair_pending(),
                "measured secondary debt disappeared before rescan"
            );
        }
        let mut settled = false;
        for _ in 0..32 {
            native.drive_one(session.clone(), owner.clone()).await;
            let inbox = application.lock_inbox();
            let row = inbox
                .store()
                .remote_mailbox_message(&scope, original.local_message())?;
            assert!(
                row.ack().is_exact_repeat_of(&original),
                "repair changed original delivery coordinates"
            );
            if !row.repair_pending() && !row.delivery_pending() {
                settled = true;
                break;
            }
        }
        assert!(settled, "rescan stranded original secondary repair");
        native.close().await?;
        assert!(!owner.context.lock().await.owns_runtime());
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    let (peer, proof) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(fake::serve(listener, None, 2), exercise)
    })
    .await?;
    proof?;
    let calls = peer?;
    assert!(
        calls
            .iter()
            .filter(|call| call.as_str() == "subscribe_and_snapshot")
            .count()
            >= 2
    );
    assert!(calls.iter().any(|call| call == "acknowledge_delivery"));
    Ok(())
}

#[tokio::test]
async fn an_owned_partial_snapshot_never_satisfies_the_execution_fence() -> TestResult {
    let (_root, application, _driver, session) = fixture(Some(r#"{"scope":"project","project_id":"secode","agent_id":"coder","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#)).await?;
    let native = application.native_mail.as_ref().ok_or("native owners")?;
    let owner = native.snapshot()?.first().ok_or("owner")?.1.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    owner
        .context
        .lock()
        .await
        .use_test_port(NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?);
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let exercise = async {
        tokio::select! {
            _ = native.drive_one(session.clone(),owner.clone()) => return Err("initial held snapshot unexpectedly completed".into()),
            () = async { while attempts.load(Ordering::SeqCst)==0 {tokio::task::yield_now().await;} } => {}
        }
        let context = owner.context.lock().await;
        assert!(
            context.advance.is_some() && context.runtime.is_none(),
            "test did not retain actual unfinished HTTP work"
        );
        let fence = TransportWakeFence {
            owner: owner.clone(),
            revision: owner.revision()?,
            capability: context
                .binding
                .as_ref()
                .ok_or("binding")?
                .capability
                .clone(),
            input: "msg_unadmitted_partial_probe".parse()?,
            notify: false,
        };
        drop(context);
        assert_eq!(
            fence.validate_after_wait().await,
            Err(NativeWakeRefusal::Connection),
            "partial source observation acquired live execution authority"
        );
        assert!(crate::dogfood::test_provider::server(&application)?
            .request()?
            .is_none());
        native.close().await?;
        assert!(!owner.context.lock().await.owns_runtime());
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    let (peer, proof) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(
            fake::serve_refusing(
                listener,
                fake::Refusals {
                    operation: "subscribe_and_snapshot",
                    cause: "Deadline",
                    remaining: 1,
                    skip: 0,
                    attempts: attempts.clone()
                }
            ),
            exercise
        )
    })
    .await?;
    proof?;
    peer?;
    Ok(())
}
