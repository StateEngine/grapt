//! An advisory cannot turn a completed stop into fresh execution authority.
use super::*;

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn seat03_cancelled_wake_survives_duplicate_advisory_and_explicit_resume_works() -> TestResult
{
    for user_first in [false, true] {
        let (_root, application, mut driver, session) = fixture(Some(r#"{"scope":"project","project_id":"secode","agent_id":"coder","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#)).await?;
        let native = application.native_mail.as_ref().ok_or("native owner")?;
        let owners = native.snapshot()?;
        let owner = owners.first().ok_or("native owner")?.1.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        owner
            .context
            .lock()
            .await
            .use_test_port(NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?);
        let (started, _accepted) = crate::dogfood::prompt_driver::Started::channel();
        let (_cancel, cancelled) = tokio::sync::watch::channel(false);
        let reservation = application.wake_slots.reserve(&session)?;
        let mut stop_signal = reservation.cancellation();
        let active = reservation
            .enter(&started, cancelled)
            .await?
            .ok_or("active custody")?;
        let (entered, poll) = tokio::sync::oneshot::channel();
        let (release, advisory) = tokio::sync::oneshot::channel();
        let (rescanned, rescan) = tokio::sync::oneshot::channel();
        let (finish, finished) = tokio::sync::oneshot::channel();
        let provider = crate::dogfood::test_provider::server(&application)?;
        provider.hold_response();
        let witness = async {
            poll.await
                .map_err(|_| "initial native poll was not observed")?;
            for _ in 0..1024 {
                if owner.wake_scheduled.load(Ordering::SeqCst) {
                    break;
                }
                tokio::task::yield_now().await;
            }
            let original = application
                .lock_inbox()
                .pending_native_mail_wake(&session)?
                .ok_or("queued wake")?;
            assert!(
                owner.wake_scheduled.load(Ordering::SeqCst),
                "native wake never queued behind busy Session"
            );
            let release_active = async {
                stop_signal.changed().await?;
                drop(active);
                Ok::<_, Box<dyn std::error::Error>>(())
            };
            let path = format!("/api/session/{session}/interrupt");
            let (response, released) = tokio::join!(
                ingress::call(&application, "POST", &path, "{}"),
                release_active
            );
            released?;
            let response = response?;
            assert!(
                response.starts_with(b"HTTP/1.1 204"),
                "authenticated interrupt failed"
            );
            assert!(
                application
                    .lock_inbox()
                    .pending_native_mail_wake(&session)?
                    .is_none(),
                "cancelled native input still authorized automatic recovery"
            );
            for _ in 0..1024 {
                if !owner.wake_scheduled.load(Ordering::SeqCst) {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert!(
                !owner.wake_scheduled.load(Ordering::SeqCst),
                "cancelled queued job did not settle"
            );
            release
                .send(())
                .map_err(|_| "advisory consumer disappeared")?;
            rescan
                .await
                .map_err(|_| "duplicate advisory was not rescanned")?;
            for _ in 0..128 {
                tokio::task::yield_now().await;
            }
            assert!(
                provider.request()?.is_none(),
                "duplicate advisory restarted cancelled native work"
            );
            assert!(
                !owner.wake_scheduled.load(Ordering::SeqCst),
                "advisory recreated a cancelled job"
            );
            assert_eq!(
                application.lock_inbox().pending_inputs(&session)?,
                vec![original.clone()],
                "cancelled input was lost or rewritten"
            );
            // An explicit new prompt goes through ordinary admission and owns the
            // next promotion. The held input was never removed from that fold.
            let response = ingress::call(
                &application,
                "POST",
                &format!("/api/session/{session}/prompt"),
                r#"{"text":"resume pending work"}"#,
            )
            .await?;
            assert!(
                response.starts_with(b"HTTP/1.1 200"),
                "explicit resume was refused"
            );
            while provider.request()?.is_none() {
                {
                    let held = application.lock_inbox();
                    let notices = grapt_session::notice::not_run_yet_notices(
                        held.store().connection(),
                        &session,
                    )?;
                    if let Some(notice) = notices.last() {
                        return Err(format!("provider not reached: {}", notice.reason).into());
                    }
                }
                tokio::task::yield_now().await;
            }
            assert!(
                application
                    .lock_inbox()
                    .promoted_cursor(&session, &original.input)?
                    .is_some(),
                "explicit resume did not promote the held input"
            );
            if user_first {
                let response = ingress::call(
                    &application,
                    "POST",
                    &format!("/api/session/{session}/interrupt"),
                    "{}",
                )
                .await?;
                assert!(response.starts_with(b"HTTP/1.1 204"));
            }
            finish
                .send(())
                .map_err(|_| "driver stopped before explicit resume")?;
            Ok::<_, Box<dyn std::error::Error>>(())
        };
        let (transport, run, witnessed) =
            tokio::time::timeout(std::time::Duration::from_secs(20), async {
                tokio::join!(
                    fake::serve_with_advisory(listener, entered, advisory, rescanned),
                    driver.run_until_shutdown(async {
                        let _finished = finished.await;
                        crate::dogfood::lifecycle::StopReason::ServeWindowExpired
                    }),
                    witness
                )
            })
            .await?;
        if let Err(error) = witnessed {
            return Err(format!(
                "wake witness failed: {error}; transport={transport:?}; driver={run:?}"
            )
            .into());
        }
        transport?;
        run?;
        let held = application.lock_inbox();
        let payload: String = held.store().connection().query_row(
            "SELECT data FROM event WHERE aggregate_id=?1 AND type IN ('session.step.failed.2','session.step.failed.3') ORDER BY seq DESC LIMIT 1",
            [session.as_str()], |row| row.get(0),
        )?;
        let fact = grapt_event::session_step_failed::decode(&serde_json::from_str(&payload)?)?;
        assert_eq!(
            fact.reason(),
            &grapt_types::StepTerminalReason::Interrupted {
                by: if user_first {
                    grapt_types::InterruptionSource::User
                } else {
                    grapt_types::InterruptionSource::ManagedShutdown
                },
            }
        );
    }
    Ok(())
}
