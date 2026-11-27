//! Restart the actual TCP door during an idle Session, after fast retries.
use super::*;

#[tokio::test]
async fn a_restarted_door_resubscribes_after_fast_retries_without_relaunch() -> TestResult {
    let (_root, application, mut driver, session) = fixture(Some(r#"{"scope":"project","project_id":"secode","agent_id":"coder","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#)).await?;
    let native = application.native_mail.as_ref().ok_or("native owner")?;
    let owner = native.snapshot()?.first().ok_or("owner")?.1.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    owner
        .context
        .lock()
        .await
        .use_test_port(NonZeroU16::new(address.port()).ok_or("port")?);
    let (entered, poll) = tokio::sync::oneshot::channel();
    let mut validated_fence = None;
    let stop = async {
        let input = std::future::poll_fn(|_| {
            match application.lock_inbox().pending_native_mail_wake(&session) {
                Ok(Some(input)) => Poll::Ready(input.input),
                Ok(None) => Poll::Pending,
                Err(error) => panic!("restart admission evidence unavailable: {error}"),
            }
        })
        .await;
        let context = owner.context.lock().await;
        assert!(context.close_failed, "old close evidence was erased");
        assert!(
            !context.close_cleanup_failed,
            "a lost door receipt was treated as unfinished local cleanup"
        );
        let capability = match &context.binding {
            Some(binding) => binding.capability.clone(),
            None => panic!("restored binding unavailable"),
        };
        let revision = match owner.revision() {
            Ok(revision) => revision,
            Err(error) => panic!("restored revision unavailable: {error}"),
        };
        let fence = TransportWakeFence {
            owner: owner.clone(),
            revision,
            capability,
            input,
            notify: false,
        };
        drop(context);
        assert_eq!(
            fence.validate_after_wait().await,
            Ok(()),
            "fresh observation stayed deaf behind historical close evidence"
        );
        // Keep this probe until the driver settles the actual queued fence.
        validated_fence = Some(fence);
    };
    let door = async {
        let mut serving = Box::pin(fake::serve_quiet(listener, entered));
        tokio::select! {
            result = &mut serving => return Err(format!("initial door exited before its idle poll: {result:?}")),
            result = poll => result.map_err(|e| e.to_string())?,
        }
        // The door future owns every listener and socket. Dropping it closes
        // the real pending HTTP operation; no synthetic failure tag is used.
        drop(serving);
        for _ in 0..5000 {
            let attempts = owner
                .recovery
                .lock()
                .map_err(|_| "recovery custody")?
                .attempts();
            if attempts >= 4 {
                let replacement = tokio::net::TcpListener::bind(address)
                    .await
                    .map_err(|e| e.to_string())?;
                let calls = fake::serve_restarted(replacement)
                    .await
                    .map_err(|e| e.to_string())?;
                return Ok(calls);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        Err("session stopped reconnecting before the slow background phase".to_owned())
    };
    let completed = tokio::time::timeout(std::time::Duration::from_secs(55), async {
        tokio::join!(door, driver.run_until(stop))
    })
    .await
    .map_err(|_| "restarted fixture door never restored the same Session")?;
    drop(validated_fence);
    let calls = completed.0?;
    assert!(calls.iter().any(|call| call == "open"));
    assert!(calls.iter().any(|call| call == "subscribe_and_snapshot"));
    assert!(Arc::ptr_eq(
        &native.snapshot()?.first().ok_or("same owner")?.1,
        &owner
    ));
    let pending = application.lock_inbox().pending_inputs(&session)?;
    assert_eq!(
        pending.len(),
        1,
        "recovery duplicated the canonical mail wake"
    );
    assert_settled_wake_coverage(&application.lock_inbox(), &pending[0])?;
    assert!(calls.iter().any(|call| call == "close"));
    completed.1?;
    // Current cleanup joined successfully; the old missing receipt stays history.
    assert!(owner.context.lock().await.close_failed);
    Ok(())
}
