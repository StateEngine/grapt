//! Lost advisory reads resubscribe through the backoff and admit outage mail once.
use super::*;

#[tokio::test]
async fn failed_advisory_reads_resubscribe_and_deliver_mail_sent_while_offline() -> TestResult {
    for kind in [
        fake::PollLoss::Http(408),
        fake::PollLoss::Malformed,
        fake::PollLoss::Disconnected,
    ] {
        let (_root, application, mut driver, session) = fixture(Some(r#"{"scope":"project","project_id":"secode","agent_id":"coder","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#)).await?;
        let native = application.native_mail.as_ref().ok_or("native owner")?;
        let owner = native.snapshot()?.first().ok_or("owner")?.1.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        owner
            .context
            .lock()
            .await
            .use_test_port(NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?);
        let (failed, failure_received) = tokio::sync::oneshot::channel();
        let (mail_sent, send_received) = tokio::sync::oneshot::channel();
        let stop = async {
            failure_received
                .await
                .unwrap_or_else(|error| panic!("advisory failure absent: {error}"));
            let mut offline = false;
            for _ in 0..100 {
                if owner
                    .context
                    .try_lock()
                    .is_ok_and(|context| context.attention == MailAttentionInfo::Retrying)
                {
                    offline = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert!(
                offline,
                "lost advisory neither published offline nor scheduled recovery"
            );
            assert!(application
                .lock_inbox()
                .pending_native_mail_wake(&session)
                .unwrap_or_else(|error| panic!("outage inbox: {error}"))
                .is_none());
            // The peer keeps its snapshot empty until this send DURING outage.
            mail_sent
                .send(())
                .unwrap_or_else(|_| panic!("outage peer ended before mail send"));
            std::future::poll_fn(|_| {
                match application.lock_inbox().pending_native_mail_wake(&session) {
                    Ok(Some(_)) => Poll::Ready(()),
                    Ok(None) => Poll::Pending,
                    Err(error) => panic!("recovery admission unavailable: {error}"),
                }
            })
            .await;
            assert!(
                matches!(
                    owner.context.lock().await.attention,
                    MailAttentionInfo::Ready { .. }
                ),
                "fresh observation did not clear offline attention"
            );
        };
        let (transport, run) = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            tokio::join!(
                fake::serve_poll_loss(listener, kind, failed, send_received),
                driver.run_until(stop)
            )
        })
        .await?;
        let calls = transport?;
        run?;
        // The lost poll joins its old subscription before observing afresh.
        let snapshots: Vec<usize> = calls
            .iter()
            .enumerate()
            .filter_map(|(at, call)| (call.as_str() == "subscribe_and_snapshot").then_some(at))
            .collect();
        assert_eq!(
            snapshots.len(),
            2,
            "no fresh snapshot after loss: {calls:?}"
        );
        assert!(
            calls[snapshots[0]..snapshots[1]]
                .iter()
                .any(|call| call.as_str() == "close"),
            "fresh observation did not first close the lost subscription: {calls:?}"
        );
        assert_eq!(calls.last().map(String::as_str), Some("close"));
        assert!(Arc::ptr_eq(
            &native.snapshot()?.first().ok_or("same owner")?.1,
            &owner
        ));
        let inbox = application.lock_inbox();
        let pending = inbox.pending_inputs(&session)?;
        assert_eq!(pending.len(), 1, "outage mail admitted more than once");
        assert_settled_wake_coverage(&inbox, &pending[0])?;
    }
    Ok(())
}
