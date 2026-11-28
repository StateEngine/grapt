//! A Session's internal accounting notices stay out of its public history,
//! whatever their size.
use super::*;

/// Record a token calibration, then grow its row past the public notice bound.
fn oversized_calibration(
    app: &crate::dogfood::ServerApplication,
    fixture: &Fixture,
) -> Result<(), String> {
    let session = &fixture.session;
    let mut inbox = lock(&app.inbox);
    let ids = CountingIds(AtomicU64::new(0x7e00_0000));
    let authorities = grapt_types::DeterministicAuthorities::new(&FixedClock, &ids);
    let calibration = grapt_session::notice::TokenCalibration {
        target: grapt_session::notice::CalibrationTarget::new("a".repeat(64)).ok_or("target")?,
        confirmed: 1200,
        estimated: 1000,
    };
    grapt_session::notice::record_token_calibration(
        inbox.store_mut(),
        &authorities,
        session,
        &calibration,
    )?;
    let padding = i64::try_from(grapt_session::notice::MAX_NOTICE_DATA_BYTES + 1)
        .map_err(|error| error.to_string())?;
    drop(inbox);
    // Another writer's damage, so through a connection of its own.
    let raw = rusqlite::Connection::open(&fixture.database).map_err(|error| error.to_string())?;
    let grown = raw
        .execute(
            "UPDATE event SET data = json_set(data, '$.padding', printf('%.*c', ?1, 'x'))
               WHERE aggregate_id = ?2 AND type = 'session.notice.recorded.1'
                 AND json_extract(data, '$.kind') = 'token-calibration'",
            (padding, session.as_str()),
        )
        .map_err(|error| error.to_string())?;
    if grown != 1 {
        return Err(format!("grew {grown} calibration rows, expected 1"));
    }
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn an_oversized_calibration_notice_leaves_the_history_page_readable() -> Result<(), String> {
    let (fixture, app, _driver) = long_session("history-internal-notice")?;
    oversized_calibration(&app, &fixture)?;
    let host = grapt_api::PreparedApiHost::new(
        crate::compose_host::bind_loopback()
            .await
            .map_err(|error| error.to_string())?,
        super::read_isolation::service(&app)?,
        crate::compose_host::host_limits().map_err(|error| error.to_string())?,
        Arc::new(grapt_api::TokioDeadlineDriver),
    )
    .map_err(|error| error.to_string())?
    .start();
    let address = host.ready().observed_address();
    let path = format!("/api/session/{}/message?limit=200", fixture.session);
    let response = super::read_isolation::get(address, &path).await;
    host.stop().await;
    let response = response?;
    assert!(
        response.starts_with("HTTP/1.1 200 "),
        "an internal notice refused the history page: {response}"
    );
    assert!(
        !response.contains("token-calibration"),
        "an internal notice reached public history"
    );
    Ok(())
}
