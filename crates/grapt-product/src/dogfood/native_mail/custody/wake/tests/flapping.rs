//! Quiet HTTP observation renews without taking over a healthy binding.
use super::*;
use std::sync::atomic::AtomicUsize;

#[tokio::test]
async fn a_quiet_long_poll_deadline_renews_the_same_subscription() -> TestResult {
    let (_root, application, mut driver, session) = fixture(Some(r#"{"scope":"project","project_id":"secode","agent_id":"coder","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#)).await?;
    let native = application.native_mail.as_ref().ok_or("native owner")?;
    let owner = native.snapshot()?.first().ok_or("owner")?.1.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    owner
        .context
        .lock()
        .await
        .use_test_port(NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?);
    let polls = Arc::new(AtomicUsize::new(0));
    // The source is deliberately quiet. Observe two complete negotiated
    // ten-second HTTP budgets and their replacement waits, then stop normally.
    let stop = tokio::time::sleep(std::time::Duration::from_secs(21));
    let (transport, run) = tokio::time::timeout(std::time::Duration::from_secs(25), async {
        tokio::join!(
            fake::serve_renewing(listener, polls.clone()),
            driver.run_until(stop)
        )
    })
    .await?;
    let calls = transport?;
    run?;
    assert_eq!(
        polls.load(Ordering::SeqCst),
        3,
        "quiet wait was never renewed"
    );
    for operation in ["open", "subscribe_and_snapshot", "close"] {
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.as_str() == operation)
                .count(),
            1,
            "quiet timeout replaced the binding: {operation}"
        );
    }
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.as_str() == "fetch_snapshot_page")
            .count(),
        2,
        "quiet renewal must probe once without amplifying snapshot reads"
    );
    assert!(application
        .lock_inbox()
        .pending_inputs(&session)?
        .is_empty());
    Ok(())
}
